use std::{fmt::Write as _, fs};

use ring::signature::{Ed25519KeyPair, KeyPair as _};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    time::timeout,
};

use super::*;

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn metadata(bytes: &[u8]) -> serde_json::Value {
    let sha256 = Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut text, byte| {
            write!(text, "{byte:02x}").unwrap();
            text
        });
    serde_json::json!({
        "schema": "speakeasy.update.v1",
        "version": "1.2.3",
        "target": "windows-x64",
        "file": manifest::NAME,
        "size": bytes.len(),
        "sha256": sha256,
    })
}

fn authenticate(value: &serde_json::Value) -> anyhow::Result<manifest::Artifact> {
    let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let bytes = serde_json::to_vec(value)?;
    manifest::verify(
        &bytes,
        key.sign(&bytes).as_ref(),
        key.public_key().as_ref().try_into().unwrap(),
        version("1.2.3"),
        version("1.2.2"),
    )
}

#[test]
fn signed_metadata_binds_version_target_name_and_bounded_size() {
    let original = metadata(b"installer");
    assert!(authenticate(&original).is_ok());
    for (field, value) in [
        ("schema", serde_json::json!("another.update.v1")),
        ("version", serde_json::json!("1.2.2")),
        ("version", serde_json::json!("1.2.4")),
        ("target", serde_json::json!("macos")),
        ("file", serde_json::json!("../other.exe")),
        ("size", serde_json::json!(0)),
        ("size", serde_json::json!(manifest::MAX_INSTALLER + 1)),
        ("sha256", serde_json::json!("00")),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        assert!(authenticate(&changed).is_err(), "{field}");
    }
    let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let bytes = serde_json::to_vec(&original).unwrap();
    for installed in [version("1.2.3"), version("2.0.0")] {
        assert!(
            manifest::verify(
                &bytes,
                key.sign(&bytes).as_ref(),
                key.public_key().as_ref().try_into().unwrap(),
                version("1.2.3"),
                installed
            )
            .is_err()
        );
    }
}

#[test]
fn tampered_metadata_wrong_keys_and_invalid_signature_lengths_are_refused() {
    let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let other = Ed25519KeyPair::from_seed_unchecked(&[8; 32]).unwrap();
    let bytes = serde_json::to_vec(&metadata(b"installer")).unwrap();
    let signature = key.sign(&bytes);
    let check = |bytes: &[u8], signature: &[u8], key: &[u8]| {
        manifest::verify(
            bytes,
            signature,
            key.try_into().unwrap(),
            version("1.2.3"),
            version("1.2.2"),
        )
    };
    assert!(check(&bytes, signature.as_ref(), other.public_key().as_ref()).is_err());
    assert!(check(&bytes, &signature.as_ref()[..63], key.public_key().as_ref()).is_err());
    let mut changed = bytes;
    changed.push(b' ');
    assert!(check(&changed, signature.as_ref(), key.public_key().as_ref()).is_err());
    let oversized = vec![b' '; manifest::MAX_MANIFEST + 1];
    assert!(
        check(
            &oversized,
            key.sign(&oversized).as_ref(),
            key.public_key().as_ref()
        )
        .is_err()
    );
}

#[test]
fn release_redirects_cannot_change_scheme_credentials_port_or_host() {
    for value in [
        "http://github.com/file",
        "https://github.com:444/file",
        "https://user@github.com/file",
        "https://github.com.attacker.invalid/file",
        "https://127.0.0.1/file",
    ] {
        assert!(!trusted_redirect(&Url::parse(value).unwrap()));
    }
    for host in [
        "github.com",
        "release-assets.githubusercontent.com",
        "objects.githubusercontent.com",
    ] {
        assert!(trusted_redirect(
            &Url::parse(&format!("https://{host}/file")).unwrap()
        ));
    }
    assert_eq!(
        asset_url(version("1.2.3"), manifest::NAME),
        "https://github.com/KrishRVH/speakeasy/releases/download/v1.2.3/speakeasy-windows-x64-setup.exe"
    );
}

async fn serve(response: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 2048];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream.write_all(&response).await.unwrap();
    });
    (url, task)
}

fn progress() -> watch::Sender<Progress> {
    watch::channel(Progress {
        stage: "Test",
        done: 0,
        total: 0,
    })
    .0
}

#[tokio::test]
async fn verified_download_retains_the_file_until_its_owner_is_released() {
    let root = tempfile::tempdir().unwrap();
    let artifact = authenticate(&metadata(b"installer")).unwrap();
    let client = client(client_builder(Trust::Nobody).unwrap().no_proxy()).unwrap();
    let (url, served) = serve(
        b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\ninstaller".to_vec(),
    )
    .await;
    let observed = progress();
    let installer = download(&client, &url, &artifact, root.path(), &observed)
        .await
        .unwrap();
    served.await.unwrap();
    assert_eq!(installer.version, version("1.2.3"));
    assert_eq!(installer.file.metadata().unwrap().len(), 9);
    assert_eq!(observed.borrow().stage, "Verifying update");
    assert_eq!(observed.borrow().done, observed.borrow().total);
    let path = installer.directory.path().join(manifest::NAME);
    #[cfg(target_os = "windows")]
    {
        assert!(File::create(&path).is_err());
        assert!(fs::remove_file(&path).is_err());
        assert!(fs::rename(&path, path.with_extension("other")).is_err());
    }
    assert!(path.is_file());
    drop(installer);
    assert_eq!(
        fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        [std::ffi::OsString::from(".lock")]
    );
}

#[tokio::test]
async fn interrupted_oversized_and_corrupt_downloads_leave_no_installer() {
    let root = tempfile::tempdir().unwrap();
    let artifact = authenticate(&metadata(b"installer")).unwrap();
    let client = client(client_builder(Trust::Nobody).unwrap().no_proxy()).unwrap();
    for response in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\ninst".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\ninstalleX".to_vec(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\na\r\ninstallerX\r\n0\r\n\r\n".to_vec(),
    ] {
        let (url, served) = serve(response).await;
        assert!(download(&client, &url, &artifact, root.path(), &progress()).await.is_err());
        served.await.unwrap();
        assert_eq!(fs::read_dir(root.path()).unwrap().map(|entry| entry.unwrap().file_name()).collect::<Vec<_>>(), [std::ffi::OsString::from(".lock")]);
    }
}

#[tokio::test]
async fn an_oversized_chunk_is_rejected_before_the_server_finishes() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 2048];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\na\r\ninstallerX\r\n")
            .await
            .unwrap();
        match stream.read(&mut request).await {
            Ok(count) => assert_eq!(count, 0),
            Err(error) => assert!(matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            )),
        }
    });
    let artifact = authenticate(&metadata(b"installer")).unwrap();
    let client = client(client_builder(Trust::Nobody).unwrap().no_proxy()).unwrap();
    assert!(
        timeout(
            Duration::from_secs(3),
            download(&client, &url, &artifact, root.path(), &progress())
        )
        .await
        .unwrap()
        .is_err()
    );
    timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn cancellation_closes_the_download_before_reporting_completion() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().to_path_buf();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap());
    let (sent, received) = async_channel::bounded(1);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 2048];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\ninst")
            .await
            .unwrap();
        sent.send(()).await.unwrap();
        match stream.read(&mut request).await {
            Ok(count) => assert_eq!(count, 0),
            Err(error) => assert!(matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            )),
        }
    });
    let artifact = authenticate(&metadata(b"installer")).unwrap();
    let operation = Job::spawn("cancel-update-test", move || async move {
        let client = client(client_builder(Trust::Nobody)?.no_proxy())?;
        download(&client, &url, &artifact, &directory, &progress())
            .await
            .map(Some)
    })
    .unwrap();
    timeout(Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    operation.request_stop();
    assert!(
        timeout(Duration::from_secs(5), operation.outcome())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    assert!(operation.is_finished());
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        [std::ffi::OsString::from(".lock")]
    );
}
