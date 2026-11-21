use std::{fmt::Write as _, time::Duration};

use ring::signature::{Ed25519KeyPair, KeyPair as _};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    time::timeout,
};

use super::*;
use crate::http::{Trust, client_builder};

const INSTALLER_HEAD: &str = "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n";

fn signed(installer: &[u8]) -> (Vec<u8>, Vec<u8>, [u8; 32]) {
    let sha256 = Sha256::digest(installer)
        .iter()
        .fold(String::new(), |mut text, byte| {
            write!(text, "{byte:02x}").unwrap();
            text
        });
    let metadata = serde_json::to_vec(&serde_json::json!({
        "schema": "speakeasy.update.v1",
        "version": "1.2.3",
        "target": "windows-x64",
        "file": manifest::NAME,
        "size": installer.len(),
        "sha256": sha256,
    }))
    .unwrap();
    let key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
    let signature = key.sign(&metadata).as_ref().to_vec();
    (
        metadata,
        signature,
        key.public_key().as_ref().try_into().unwrap(),
    )
}

fn artifact() -> manifest::Artifact {
    let (metadata, signature, key) = signed(b"installer");
    manifest::verify(&metadata, &signature, &key).unwrap()
}

#[test]
fn only_metadata_signed_by_the_embedded_key_names_an_installer() {
    let (metadata, signature, key) = signed(b"installer");
    let artifact = manifest::verify(&metadata, &signature, &key).unwrap();
    assert_eq!(artifact.size, 9);
    assert_eq!(
        artifact.sha256,
        <[u8; 32]>::from(Sha256::digest(b"installer"))
    );
    let other = Ed25519KeyPair::from_seed_unchecked(&[8; 32]).unwrap();
    let other: [u8; 32] = other.public_key().as_ref().try_into().unwrap();
    assert!(manifest::verify(&metadata, &signature, &other).is_err());
    let mut tampered = metadata;
    tampered.push(b' ');
    assert!(manifest::verify(&tampered, &signature, &key).is_err());
}

async fn answer(listener: &TcpListener, response: &[u8]) -> TcpStream {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut request = [0; 2048];
    assert!(stream.read(&mut request).await.unwrap() > 0);
    stream.write_all(response).await.unwrap();
    stream
}

async fn serve(response: String) -> (String, tokio::task::JoinHandle<TcpStream>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { answer(&listener, response.as_bytes()).await });
    (url, task)
}

async fn hung_up(mut stream: TcpStream) {
    match stream.read(&mut [0; 2048]).await {
        Ok(count) => assert_eq!(count, 0, "The client kept reading the download"),
        Err(error) => assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ),
            "{error}"
        ),
    }
}

fn cache_entries(root: &Path) -> Vec<std::ffi::OsString> {
    fs::read_dir(root.join("updates"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect()
}

fn progress() -> watch::Sender<Progress> {
    watch::channel(Progress::Step("Test")).0
}

fn loopback() -> Client {
    super::super::client(client_builder(Trust::Nobody).unwrap().no_proxy()).unwrap()
}

#[tokio::test]
async fn a_verified_installer_stays_staged_until_its_owner_lets_go() {
    let root = tempfile::tempdir().unwrap();
    let (url, served) = serve(format!("{INSTALLER_HEAD}installer")).await;
    let staged = download(&loopback(), &url, &artifact(), root.path(), &progress())
        .await
        .unwrap();
    served.await.unwrap();
    assert_eq!(
        fs::read(staged.path().join(manifest::NAME)).unwrap(),
        b"installer"
    );
    drop(staged);
    assert_eq!(cache_entries(root.path()), Vec::<std::ffi::OsString>::new());
}

#[tokio::test]
async fn an_installer_that_differs_from_its_signed_hash_is_discarded() {
    let root = tempfile::tempdir().unwrap();
    let (url, served) = serve(format!("{INSTALLER_HEAD}INSTALLER")).await;
    let error = download(&loopback(), &url, &artifact(), root.path(), &progress())
        .await
        .err()
        .unwrap();
    served.await.unwrap();
    assert_eq!(error.to_string(), INVALID_FILE);
    assert_eq!(cache_entries(root.path()), Vec::<std::ffi::OsString>::new());
}

#[tokio::test]
async fn cancellation_closes_the_download_before_reporting_completion() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().to_path_buf();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/file", listener.local_addr().unwrap());
    let (sent, received) = async_channel::bounded(1);
    let server = tokio::spawn(async move {
        let stream = answer(&listener, format!("{INSTALLER_HEAD}inst").as_bytes()).await;
        sent.send(()).await.unwrap();
        hung_up(stream).await;
    });
    let operation = Job::spawn("cancel-update-test", move || async move {
        download(&loopback(), &url, &artifact(), &directory, &progress())
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
    assert_eq!(cache_entries(root.path()), Vec::<std::ffi::OsString>::new());
}
