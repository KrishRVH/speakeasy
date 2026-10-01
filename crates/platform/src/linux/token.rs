//! A portal restore token is a one-use credential, separate from preferences.
use anyhow::ensure;
use std::{
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub(super) struct TokenStore(PathBuf);
impl TokenStore {
    pub(super) fn open() -> Option<Self> {
        let root = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })?;
        Self::at(&root.join("speakeasy")).ok()
    }
    fn at(directory: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(directory)?;
        ensure!(
            !std::fs::symlink_metadata(directory)?
                .file_type()
                .is_symlink(),
            "Portal state must be a private directory"
        );
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self(directory.join("desktop-token")))
    }
    #[expect(
        clippy::verbose_bit_mask,
        reason = "The octal mode mask directly names forbidden group and other permissions"
    )]
    pub(super) fn take(&self) -> anyhow::Result<Option<String>> {
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.0)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.permissions().mode() & 0o077 == 0
                && metadata.len() <= 4096,
            "Portal token must be a small private file"
        );
        let mut token = String::new();
        file.take(4097).read_to_string(&mut token)?;
        ensure!(
            token.len() <= 4096 && !token.contains('\0'),
            "Invalid portal token"
        );
        // Remove before presenting it: a cancelled restore may consume it too.
        std::fs::remove_file(&self.0)?;
        Ok((!token.is_empty()).then_some(token))
    }
    pub(super) fn save(&self, token: &str) -> anyhow::Result<()> {
        ensure!(
            !token.is_empty() && token.len() <= 4096 && !token.contains('\0'),
            "Invalid portal token"
        );
        let directory = self
            .0
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Portal state has no directory"))?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(token.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(&self.0)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_are_private_rotated_and_consumed_once() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let store = TokenStore::at(&directory.path().join("state"))?;
        store.save("fixture-one")?;
        assert_eq!(store.0.metadata()?.permissions().mode() & 0o777, 0o600);
        store.save("fixture-two")?;
        assert_eq!(store.take()?.as_deref(), Some("fixture-two"));
        assert!(store.take()?.is_none());
        store.save("fixture-three")?;
        std::fs::set_permissions(&store.0, std::fs::Permissions::from_mode(0o644))?;
        assert!(store.take().is_err());
        Ok(())
    }
}
