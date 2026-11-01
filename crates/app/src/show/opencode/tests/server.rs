use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    task::JoinHandle,
};

use super::*;

pub(super) struct Step {
    path: String,
    status: &'static str,
    body: Option<Value>,
    gate: Option<(Sender<()>, Receiver<()>)>,
}

impl Step {
    pub(super) fn reply(path: impl Into<String>, body: Value) -> Self {
        Self {
            path: path.into(),
            status: "200 OK",
            body: Some(body),
            gate: None,
        }
    }

    pub(super) fn rejection(path: impl Into<String>, status: &'static str) -> Self {
        Self {
            path: path.into(),
            status,
            body: Some(json!({"private": "PRIVATE_SERVER_ERROR"})),
            gate: None,
        }
    }

    pub(super) fn close(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            status: "200 OK",
            body: None,
            gate: None,
        }
    }

    pub(super) fn held(mut self, reached: Sender<()>, gate: Receiver<()>) -> Self {
        self.gate = Some((reached, gate));
        self
    }
}

pub(super) struct Call {
    pub headers: String,
    pub body: Vec<u8>,
}

pub(super) struct Server {
    pub endpoint: String,
    pub registration: PathBuf,
    thread: JoinHandle<anyhow::Result<Vec<Call>>>,
    _directory: tempfile::TempDir,
}

impl Server {
    pub(super) async fn start(steps: Vec<Step>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/", listener.local_addr()?);
        let directory = tempfile::tempdir()?;
        let registration = directory.path().join("service.json");
        fs::write(
            &registration,
            serde_json::to_vec(&json!({
                "url": endpoint, "pid": 42, "version": "public-test", "password": "PUBLIC_TEST_PASSWORD",
            }))?,
        )?;
        let thread = tokio::spawn(async move {
            let mut calls = Vec::new();
            for step in steps {
                let (mut socket, _) = tokio::time::timeout(PATIENCE, listener.accept()).await??;
                let mut bytes = Vec::new();
                loop {
                    let byte = tokio::time::timeout(PATIENCE, socket.read_u8()).await??;
                    bytes.push(byte);
                    if bytes.ends_with(b"\r\n\r\n") {
                        break;
                    }
                    anyhow::ensure!(bytes.len() <= 64 * 1024, "Fixture header is too large");
                }
                let headers = String::from_utf8(bytes)?;
                let path = headers.split_whitespace().nth(1).unwrap();
                assert_eq!(path, step.path);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                let mut body = vec![0; length];
                tokio::time::timeout(PATIENCE, socket.read_exact(&mut body)).await??;
                calls.push(Call { headers, body });
                if let Some((reached, gate)) = step.gate {
                    reached.send(()).await?;
                    tokio::time::timeout(PATIENCE, gate.recv()).await??;
                }
                if let Some(body) = step.body {
                    let body = serde_json::to_vec(&body)?;
                    let headers = format!(
                        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        step.status,
                        body.len()
                    );
                    socket.write_all(headers.as_bytes()).await?;
                    socket.write_all(&body).await?;
                    socket.shutdown().await?;
                }
            }
            Ok(calls)
        });
        Ok(Self {
            endpoint,
            registration,
            thread,
            _directory: directory,
        })
    }

    pub(super) async fn finish(self) -> anyhow::Result<Vec<Call>> {
        tokio::time::timeout(PATIENCE, self.thread).await??
    }
}
