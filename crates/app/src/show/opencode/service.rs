use std::{
    fs,
    io::Read as _,
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use reqwest::{Client, RequestBuilder, Response, Url, redirect::Policy};
use serde::{Deserialize, de::DeserializeOwned};
use tokio::time::{Instant, timeout_at};
use zeroize::Zeroizing;

use super::{CANCELLED, Cancellation, Listing, Outcome, Request, Session, payload};
use crate::http::{Trust, client_builder};

const DEADLINE: Duration = Duration::from_secs(30);
const REGISTRATION_LIMIT: u64 = 64 * 1024;
const REPLY_LIMIT: usize = 1024 * 1024;
const DISCOVERY: &str = "Cannot discover OpenCode. Start its local service and try again.";
const CONNECTION: &str =
    "Cannot reach the registered OpenCode service. Check that it is running and try again.";
const INVALID: &str = "OpenCode returned an incompatible response. Update OpenCode and try again.";
const SELECTED: &str = "The selected OpenCode session is unavailable. Choose a session again.";

#[derive(Deserialize)]
struct Registration {
    url: String,
    pid: u32,
    version: Option<String>,
    password: Option<String>,
}

struct Service {
    client: Client,
    url: Url,
    password: Option<Zeroizing<String>>,
}

#[derive(Deserialize)]
struct Health {
    pid: u32,
    version: String,
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct Page {
    data: Vec<Row>,
    cursor: Cursor,
}

#[derive(Deserialize)]
struct Cursor {
    next: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    id: String,
    title: Option<String>,
    #[serde(rename = "parentID")]
    parent_id: Option<String>,
    location: Location,
    time: Time,
}

#[derive(Deserialize)]
struct Location {
    directory: String,
}

#[derive(Deserialize)]
struct Time {
    updated: u64,
    archived: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Admission {
    id: String,
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "type")]
    kind: String,
    delivery: String,
}

pub(super) fn registration_path() -> Result<PathBuf, &'static str> {
    let absolute = |name| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let state = absolute("XDG_STATE_HOME").or_else(|| {
        absolute(if cfg!(target_os = "windows") {
            "USERPROFILE"
        } else {
            "HOME"
        })
        .map(|home| home.join(".local/state"))
    });
    state
        .map(|state| state.join("opencode/service.json"))
        .ok_or(DISCOVERY)
}

fn registration(path: &Path) -> Result<Registration, &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_error| DISCOVERY)?;
    if !metadata.is_file() || metadata.len() > REGISTRATION_LIMIT {
        return Err(DISCOVERY);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    fs::File::open(path)
        .map_err(|_error| DISCOVERY)?
        .take(REGISTRATION_LIMIT.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_error| DISCOVERY)?;
    if bytes.len() > usize::try_from(REGISTRATION_LIMIT).unwrap_or(usize::MAX) {
        return Err(DISCOVERY);
    }
    let registration: Registration = serde_json::from_slice(&bytes).map_err(|_error| DISCOVERY)?;
    if registration.pid == 0 {
        return Err(DISCOVERY);
    }
    Ok(registration)
}

fn endpoint(text: &str) -> Result<Url, &'static str> {
    let mut url = Url::parse(text).map_err(|_error| DISCOVERY)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("OpenCode must register a loopback HTTP service without URL credentials.");
    }
    if url.host_str() == Some("localhost") {
        url.set_host(Some("127.0.0.1"))
            .map_err(|_error| DISCOVERY)?;
    }
    let host = url.host_str().ok_or(DISCOVERY)?;
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if !host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
        return Err("OpenCode must register a loopback service. Remote handoff is not supported.");
    }
    Ok(url)
}

impl Service {
    async fn discover(path: &Path) -> Result<Self, &'static str> {
        let registration = registration(path)?;
        let url = endpoint(&registration.url)?;
        let client = client_builder(Trust::Nobody)
            .map_err(|_error| CONNECTION)?
            .no_proxy()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(2))
            .timeout(DEADLINE)
            .build()
            .map_err(|_error| CONNECTION)?;
        let service = Self {
            client,
            url,
            password: registration.password.map(Zeroizing::new),
        };
        let health: Health = service.get("/api/info", &[]).await?;
        if health.pid != registration.pid
            || registration
                .version
                .is_some_and(|version| version != health.version)
        {
            return Err(
                "OpenCode's registration is stale. Restart its service and choose a session again.",
            );
        }
        Ok(service)
    }

    fn authorize(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.password {
            Some(password) => request.basic_auth("opencode", Some(password.as_str())),
            None => request,
        }
    }

    fn address(&self, path: &str) -> Url {
        let mut url = self.url.clone();
        url.set_path(path);
        url
    }

    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, &'static str> {
        let mut url = self.address(path);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query.iter().copied());
        }
        let response = self
            .authorize(self.client.get(url))
            .send()
            .await
            .map_err(|_error| CONNECTION)?;
        if !response.status().is_success() {
            return Err("OpenCode refused the lookup. Check its service and selected session.");
        }
        decode(response, REPLY_LIMIT).await
    }

    async fn selected(&self, id: &str) -> Result<(), &'static str> {
        if !valid_id(id, "ses_") {
            return Err(SELECTED);
        }
        let row: Envelope<Row> = self.get(&format!("/api/session/{id}"), &[]).await?;
        if row.data.id != id || row.data.parent_id.is_some() || row.data.time.archived.is_some() {
            return Err(SELECTED);
        }
        Ok(())
    }
}

pub(super) async fn list(path: &Path, cancel: &Cancellation) -> Result<Listing, &'static str> {
    let deadline = Instant::now().checked_add(DEADLINE).ok_or(CONNECTION)?;
    let operation = async {
        let service = Service::discover(path).await?;
        let mut sessions = Vec::new();
        let mut cursor = None;
        for _ in 0..5 {
            let mut query = vec![
                ("parentID", "null"),
                ("archived", "false"),
                ("limit", "100"),
            ];
            if let Some(cursor) = cursor.as_deref() {
                query.push(("cursor", cursor));
            }
            let page: Page = service.get("/api/session", &query).await?;
            if page.data.len() > 100 {
                return Err(INVALID);
            }
            for row in page.data {
                if !valid_id(&row.id, "ses_") {
                    return Err(INVALID);
                }
                if row.parent_id.is_none()
                    && row.time.archived.is_none()
                    && !sessions
                        .iter()
                        .any(|session: &Session| session.id == row.id)
                {
                    sessions.push(Session {
                        id: row.id,
                        title: row.title,
                        directory: row.location.directory,
                        updated: row.time.updated,
                    });
                }
            }
            cursor = page.cursor.next.filter(|cursor| !cursor.is_empty());
            if cursor.is_none() {
                break;
            }
        }
        Ok(Listing {
            endpoint: service.url.to_string(),
            sessions,
            truncated: cursor.is_some(),
        })
    };
    tokio::select! {
        biased;
        () = cancel.wait() => Err(CANCELLED),
        result = timeout_at(deadline, operation) => result.unwrap_or(Err(CONNECTION)),
    }
}

pub(super) async fn send(request: Request, path: &Path, cancel: &Cancellation) -> Outcome {
    let Some(deadline) = Instant::now().checked_add(DEADLINE) else {
        return Outcome::refused(CONNECTION);
    };
    let preparation = async {
        if !valid_id(&request.session, "ses_") {
            return Err(SELECTED);
        }
        let payload = payload::prepare(&request.directory, cancel)?;
        let service = Service::discover(path).await?;
        if request
            .expected_endpoint
            .as_ref()
            .is_some_and(|expected| expected != service.url.as_str())
        {
            return Err(
                "The OpenCode endpoint changed. Open the chooser and select the session again.",
            );
        }
        service.selected(&request.session).await?;
        Ok((service, payload))
    };
    let prepared = tokio::select! {
        biased;
        () = cancel.wait() => Err(CANCELLED),
        result = timeout_at(deadline, preparation) => result.unwrap_or(Err(CONNECTION)),
    };
    let (service, payload) = match prepared {
        Ok(prepared) => prepared,
        Err(reason) => return Outcome::refused(reason),
    };
    if cancel.cancelled() || Instant::now() >= deadline {
        return Outcome::refused(CANCELLED);
    }
    let bytes = payload.bytes.len();
    let cap = bytes.saturating_mul(2).saturating_add(REPLY_LIMIT);
    let submission = async {
        let response = service
            .authorize(
                service
                    .client
                    .post(service.address(&format!("/api/session/{}/prompt", request.session))),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload.bytes)
            .send()
            .await
            .map_err(|_error| ())?;
        if response.status().is_client_error() || response.status().is_redirection() {
            return Ok(Outcome::refused(
                "OpenCode rejected this handoff. Check the selected session and service permissions.",
            ));
        }
        if !response.status().is_success() {
            return Err(());
        }
        let reply: Envelope<Admission> = decode(response, cap).await.map_err(|_reason| ())?;
        let admission = reply.data;
        if !valid_id(&admission.id, "msg_")
            || admission.session_id != request.session
            || admission.kind != "user"
            || admission.delivery != "queue"
        {
            return Err(());
        }
        Ok(Outcome::Queued {
            session: admission.session_id,
            message: admission.id,
            attachments: payload.attachments,
            bytes,
        })
    };
    timeout_at(deadline, submission)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_else(Outcome::unknown)
}

async fn decode<T: DeserializeOwned>(
    mut response: Response,
    limit: usize,
) -> Result<T, &'static str> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_error| CONNECTION)? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(INVALID);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_private_error| INVALID)
}

fn valid_id(id: &str, prefix: &str) -> bool {
    id.len() > prefix.len()
        && id.len() <= 256
        && id.starts_with(prefix)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests;
