//! The Show skill: the bundled reading guide and the copies `show skill --install` puts in agents'
//! skill folders. Each copy is recorded, and the first Show command after an update rewrites it, so
//! an agent reads the guide of the Speakeasy it calls. Commands run as the agent, with its access to
//! the folders; the app touching them at sign-in would raise macOS privacy prompts.

use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

pub(super) const SKILL: &str = include_str!("../../../../skills/speakeasy-show/SKILL.md");

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub(super) fn reading_guide() -> &'static str {
    SKILL
        .strip_prefix("---\n")
        .and_then(|front| front.split_once("\n---\n"))
        .map_or(SKILL, |(_, guide)| guide.trim_start())
}

/// The installed copies, kept in Speakeasy's data folder.
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Installed {
    /// The version that last rewrote the copies, so each update rewrites them once.
    current: Option<String>,
    copies: Vec<PathBuf>,
}

/// Installs the guide in place of any copy already there, edited or not, and records the copy so
/// updates rewrite it.
pub(super) fn install(directory: &Path) -> anyhow::Result<PathBuf> {
    install_recorded(&record()?, directory)
}

/// Brings the recorded copies to this version's guide before a Show command runs. Agents run
/// these commands, so the first one after an update keeps their skills current; a copy left
/// behind is a warning, and the command runs either way.
pub(crate) fn follow_update() {
    let Ok(record) = record() else {
        return;
    };
    if let Err(error) = follow_update_recorded(&record, VERSION) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "stderr is the only channel for a warning beside the command's output"
        )]
        let _ = writeln!(io::stderr().lock(), "speakeasy: {error:#}");
    }
}

fn record() -> anyhow::Result<PathBuf> {
    Ok(crate::paths::data_root()?.join("agent-skills.json"))
}

fn install_recorded(record: &Path, directory: &Path) -> anyhow::Result<PathBuf> {
    let path = std::path::absolute(directory)?
        .join("speakeasy-show")
        .join("SKILL.md");
    let mut installed = read(record)?.unwrap_or_default();
    if !installed.copies.contains(&path) {
        installed.copies.push(path.clone());
        save(record, &installed)?;
    }
    crate::paths::replace(&path, SKILL.as_bytes())
        .with_context(|| format!("Can't install the Show skill at {}", path.display()))?;
    Ok(path)
}

/// Rewrites each recorded copy once per version, so other commands touch no agent folder. A copy
/// someone removed, or one on a folder that isn't reachable now, is left alone; one Speakeasy
/// can't rewrite is reported once and waits for the next update or install.
fn follow_update_recorded(record: &Path, version: &str) -> anyhow::Result<()> {
    let Some(mut installed) = read(record)? else {
        return Ok(());
    };
    if installed.current.as_deref() == Some(version) {
        return Ok(());
    }
    let mut stuck = None;
    for copy in &installed.copies {
        let rewritten = match fs::read(copy) {
            Ok(guide) if guide == SKILL.as_bytes() => Ok(()),
            Ok(_) => crate::paths::replace(copy, SKILL.as_bytes()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
        if let Err(error) = rewritten
            && stuck.is_none()
        {
            stuck = Some(anyhow::anyhow!(
                "Can't update the Show skill at {}, so agents there read an older guide ({error}). \
                 Install it again with speakeasy show skill --install.",
                copy.display()
            ));
        }
    }
    installed.current = Some(version.to_owned());
    save(record, &installed)?;
    stuck.map_or(Ok(()), Err)
}

fn read(record: &Path) -> anyhow::Result<Option<Installed>> {
    let text = match fs::read(record) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Can't read {}", record.display()));
        },
    };
    serde_json::from_slice(&text).map(Some).with_context(|| {
        format!(
            "{} is damaged. Delete it, then install the Show skill again.",
            record.display()
        )
    })
}

fn save(record: &Path, installed: &Installed) -> anyhow::Result<()> {
    crate::paths::replace(record, &serde_json::to_vec_pretty(installed)?)
        .with_context(|| format!("Can't record installed Show skills in {}", record.display()))
}

#[cfg(test)]
mod tests;
