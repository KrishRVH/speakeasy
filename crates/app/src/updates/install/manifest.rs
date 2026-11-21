use anyhow::Context as _;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::Deserialize;

pub(super) const NAME: &str = "speakeasy-windows-x64-setup.exe";
const UNTRUSTED: &str = "This update could not be authenticated. Nothing was installed. Open the release page or try again later.";

pub(super) struct Artifact {
    pub(super) size: u64,
    pub(super) sha256: [u8; 32],
}

#[derive(Deserialize)]
struct Manifest {
    size: u64,
    sha256: String,
}

#[cfg(target_os = "windows")]
pub(super) fn key() -> anyhow::Result<[u8; 32]> {
    hex(include_str!("../../../../../packaging/update-key.pub").trim()).context(
        "Verified updates are not configured in this build. Open the release page to update manually.",
    )
}

pub(super) fn verify(bytes: &[u8], signature: &[u8], key: &[u8; 32]) -> anyhow::Result<Artifact> {
    UnparsedPublicKey::new(&ED25519, key)
        .verify(bytes, signature)
        .ok()
        .context(UNTRUSTED)?;
    let manifest: Manifest = serde_json::from_slice(bytes).context(UNTRUSTED)?;
    Ok(Artifact {
        size: manifest.size,
        sha256: hex(&manifest.sha256).context(UNTRUSTED)?,
    })
}

fn hex(text: &str) -> Option<[u8; 32]> {
    let (pairs, []) = text.as_bytes().as_chunks::<2>() else {
        return None;
    };
    let bytes = pairs
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    bytes.try_into().ok()
}
