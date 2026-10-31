//! An update response may choose a version, but never a host, executable, or arbitrary URL.

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
    let target = TcpListener::bind("127.0.0.1:0").await?;
    let location = format!("http://{}/", target.local_addr()?);
    let answer = async move {
        let (mut stream, _) = target.accept().await?;
        let mut request = [0; 2048];
        assert!(stream.read(&mut request).await? > 0);
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 21\r\nConnection: close\r\n\r\n{\"tag_name\":\"v9.9.9\"}").await
    };
    let redirects = tokio::spawn(answer);
    for response in [
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
        "HTTP/1.1 200 OK\r\nContent-Length: 2097153\r\nConnection: close\r\n\r\n".into(),
        format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n200001\r\n{}\r\n0\r\n\r\n",
            "x".repeat(MAX_REPLY.saturating_add(1))
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/", listener.local_addr()?);
        let server = async move {
            let (mut stream, _) = listener.accept().await?;
            let mut request = [0; 2048];
            assert!(stream.read(&mut request).await? > 0);
            // The bounded client may close early, so a broken pipe is expected for a large body.
            match stream.write_all(response.as_bytes()).await {
                Ok(()) => Ok(()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                    ) =>
                {
                    Ok(())
                },
                Err(error) => Err(error),
            }
        };
        let (result, response_sent) = timeout(Duration::from_secs(3), async {
            tokio::join!(fetch(&client, &url), server)
        })
        .await?;
        response_sent?;
        assert!(result.is_err());
    }
    redirects.abort();
    assert!(
        redirects.await.unwrap_err().is_cancelled(),
        "Followed an untrusted redirect"
    );
    Ok(())
}
