use anyhow::{Context as _, ensure};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::Deserialize;

use super::super::Version;

pub(super) const NAME: &str = "speakeasy-windows-x64-setup.exe";
pub(super) const MAX_INSTALLER: u64 = 512 * 1024 * 1024;
pub(super) const MAX_MANIFEST: usize = 16 * 1024;
const UNTRUSTED: &str = "This update could not be authenticated. Nothing was installed. Open the release page or try again later.";

pub(super) struct Artifact {
    pub(super) version: Version,
    pub(super) size: u64,
    pub(super) sha256: [u8; 32],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    version: String,
    target: String,
    file: String,
    size: u64,
    sha256: String,
}

#[cfg(target_os = "windows")]
pub(super) fn key() -> anyhow::Result<[u8; 32]> {
    hex(include_str!("../../../../../packaging/update-key.pub").trim()).context(
        "Verified updates are not configured in this build. Open the release page to update manually.",
    )
}

pub(super) fn verify(
    bytes: &[u8],
    signature: &[u8],
    key: &[u8; 32],
    requested: Version,
    installed: Version,
) -> anyhow::Result<Artifact> {
    ensure!(
        bytes.len() <= MAX_MANIFEST && signature.len() == 64,
        UNTRUSTED
    );
    UnparsedPublicKey::new(&ED25519, key)
        .verify(bytes, signature)
        .map_err(|_signature| anyhow::anyhow!(UNTRUSTED))?;
    let manifest: Manifest = serde_json::from_slice(bytes).context(UNTRUSTED)?;
    let version = Version::parse(&manifest.version).context(UNTRUSTED)?;
    ensure!(
        manifest.schema == "speakeasy.update.v1"
            && manifest.target == "windows-x64"
            && manifest.file == NAME
            && version == requested
            && version > installed
            && (1..=MAX_INSTALLER).contains(&manifest.size),
        UNTRUSTED
    );
    Ok(Artifact {
        version,
        size: manifest.size,
        sha256: hex(&manifest.sha256).context(UNTRUSTED)?,
    })
}

fn hex(text: &str) -> anyhow::Result<[u8; 32]> {
    ensure!(
        text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()),
        UNTRUSTED
    );
    let mut bytes = [0; 32];
    for (value, pair) in bytes.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
        let pair = std::str::from_utf8(pair).context(UNTRUSTED)?;
        *value = u8::from_str_radix(pair, 16).context(UNTRUSTED)?;
    }
    Ok(bytes)
}
