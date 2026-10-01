//! A portal restore token is a one-use credential, separate from preferences.

use std::{
    fs,
    io::{ErrorKind, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};

const MAX_TOKEN_LEN: usize = 4096;
/// One byte past the limit, so a file that grew after its size check fails validation instead of
/// being silently truncated.
const READ_LIMIT: u64 = MAX_TOKEN_LEN as u64 + 1;
const GROUP_OR_OTHER_ACCESS: u32 = 0o077;

pub(super) struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    /// Opens the per-user token store, or `None` when it is unusable; without one, the portal asks
    /// for consent again on each launch.
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
        fs::create_dir_all(directory)?;
        ensure!(
            !fs::symlink_metadata(directory)?.file_type().is_symlink(),
            "Portal state must be a private directory"
        );
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            path: directory.join("desktop-token"),
        })
    }

    pub(super) fn consume(&self) -> anyhow::Result<Option<String>> {
        let file = match fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.permissions().mode() & GROUP_OR_OTHER_ACCESS == 0
                && metadata.len() <= MAX_TOKEN_LEN as u64,
            "Portal token must be a small private file"
        );
        let mut token = String::new();
        file.take(READ_LIMIT).read_to_string(&mut token)?;
        ensure!(is_well_formed(&token), "Invalid portal token");
        // Delete before presenting it: the portal may spend the token even on a cancelled restore.
        fs::remove_file(&self.path)?;
        Ok((!token.is_empty()).then_some(token))
    }

    pub(super) fn save(&self, token: &str) -> anyhow::Result<()> {
        ensure!(
            !token.is_empty() && is_well_formed(token),
            "Invalid portal token"
        );
        let directory = self
            .path
            .parent()
            .context("Portal state has no directory")?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(token.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(&self.path)?;
        Ok(())
    }
}

fn is_well_formed(token: &str) -> bool {
    token.len() <= MAX_TOKEN_LEN && !token.contains('\0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_private_rotated_and_consumed_once() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let store = TokenStore::at(&directory.path().join("state"))?;
        store.save("fixture-one")?;
        assert_eq!(store.path.metadata()?.permissions().mode() & 0o777, 0o600);
        store.save("fixture-two")?;
        assert_eq!(store.consume()?.as_deref(), Some("fixture-two"));
        assert!(store.consume()?.is_none());
        store.save("fixture-three")?;
        fs::set_permissions(&store.path, fs::Permissions::from_mode(0o644))?;
        assert!(store.consume().is_err());
        Ok(())
    }
}
