use std::io::{self, ErrorKind};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};

use super::*;

#[test]
fn release_links_are_built_only_from_a_newer_numeric_version() {
    let body = br#"{"tag_name":"v1.10.0","html_url":"https://untrusted.invalid/install"}"#;
    let version = newer(body, "1.9.99").unwrap().unwrap();
    assert_eq!(
        version.release_url(),
        "https://github.com/KrishRVH/speakeasy/releases/tag/v1.10.0"
    );
    assert!(newer(body, "1.10.0").unwrap().is_none());
    assert!(newer(body, "2.0.0").unwrap().is_none());
    for tag in [
        "1.2.3",
        "v1.2",
        "v1.2.3.4",
        "v1.02.3",
        "v1.2.3/../../other",
        "v1.2.3?url=bad",
        "v1.2.3-beta",
        "v4294967296.0.0",
        "v١.2.3",
    ] {
        let body = serde_json::to_vec(&serde_json::json!({"tag_name":tag})).unwrap();
        assert!(newer(&body, "0.7.0").is_err(), "{tag}");
    }
}

#[tokio::test]
async fn redirects_and_oversized_responses_are_not_followed_or_accepted() -> anyhow::Result<()> {
    let client = client(client_builder(Trust::Nobody)?.no_proxy())?;
    let (location, target) = serve(
        "HTTP/1.1 200 OK\r\nContent-Length: 21\r\nConnection: close\r\n\r\n{\"tag_name\":\"v9.9.9\"}"
            .into(),
    )
    .await?;
    tokio::spawn(target);
    let oversized = MAX_REPLY + 1;
    for (fault, response) in [
        (
            "Followed an untrusted redirect",
            format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
        ),
        (
            "Ignored a declared length over the limit",
            format!("HTTP/1.1 200 OK\r\nContent-Length: {oversized}\r\nConnection: close\r\n\r\n"),
        ),
        (
            "Accepted a chunked body over the limit",
            format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{oversized:x}\r\n{}\r\n0\r\n\r\n",
                "x".repeat(oversized)
            ),
        ),
    ] {
        let (url, reply) = serve(response).await?;
        let (result, served) = timeout(Duration::from_secs(3), async {
            tokio::join!(fetch(&client, &url, MAX_REPLY), reply)
        })
        .await
        .context(fault)?;
        served?;
        assert!(result.is_err(), "{fault}");
    }
    Ok(())
}

async fn serve(response: String) -> io::Result<(String, impl Future<Output = io::Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/", listener.local_addr()?);
    let served = async move {
        let (mut stream, _) = listener.accept().await?;
        let mut request = [0; 2048];
        assert!(stream.read(&mut request).await? > 0);
        stream
            .write_all(response.as_bytes())
            .await
            .or_else(tolerate_hang_up)?;
        stream
            .read_to_end(&mut Vec::new())
            .await
            .map(drop)
            .or_else(tolerate_hang_up)
    };
    Ok((url, served))
}

fn tolerate_hang_up(error: io::Error) -> io::Result<()> {
    match error.kind() {
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => Ok(()),
        _ => Err(error),
    }
}
