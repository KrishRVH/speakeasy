//! Reading a saved Show back, for Speakeasy's menu to copy its summary or open its folder. Linux
//! records no Shows and has no such menu, so builds this only for tests.

use std::{
    fs,
    io::{self, ErrorKind, Read as _},
    path::Path,
};

use speakeasy_show::Bundle;

use super::SavedShow;

/// The most of a summary read back; a real one, naming at most 36 keyframes and the words said
/// over them, is a few kilobytes.
pub(super) const SUMMARY_LIMIT: u64 = 1 << 20;

impl SavedShow {
    /// The Show's folder while it still exists, or `None` once it has expired or been deleted.
    pub(crate) fn folder(&self) -> anyhow::Result<Option<&Path>> {
        match fs::metadata(&self.0) {
            Ok(metadata) => Ok(metadata.is_dir().then_some(&*self.0)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(cannot_read(&self.0)(error)),
        }
    }

    /// The summary as the Show's folder holds it now, so the user's edits survive, or `None` once
    /// it has expired or been deleted. At most [`SUMMARY_LIMIT`] bytes are read, whatever has taken
    /// the file's place.
    pub(crate) fn summary(&self) -> anyhow::Result<Option<String>> {
        let path = self.0.join(Bundle::SUMMARY);
        let file = match fs::File::open(&path) {
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            file => file.map_err(cannot_read(&path))?,
        };
        let mut summary = String::new();
        file.take(SUMMARY_LIMIT.saturating_add(1))
            .read_to_string(&mut summary)
            .map_err(cannot_read(&path))?;
        anyhow::ensure!(
            u64::try_from(summary.len()).is_ok_and(|length| length <= SUMMARY_LIMIT),
            "{} is too large to copy; a Show's summary is a few kilobytes.",
            path.display()
        );
        Ok(Some(summary))
    }
}

/// Explains a failed read of a saved Show's `path`, with the system's reason.
fn cannot_read(path: &Path) -> impl FnOnce(io::Error) -> anyhow::Error {
    move |error| anyhow::anyhow!("Cannot read {}: {error}.", path.display())
}
