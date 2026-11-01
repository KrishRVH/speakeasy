use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use speakeasy_show::Manifest;

use crate::show::{
    evidence::Overview,
    store::{address::ShowId, recall::Entry},
};

pub(super) struct Roots(Vec<Root>);

struct Root {
    directory: PathBuf,
    requested: PathBuf,
}

impl Roots {
    pub(super) fn open(roots: Vec<PathBuf>) -> Result<Self, &'static str> {
        let mut canonical = Vec::new();
        for root in roots {
            let metadata =
                fs::symlink_metadata(&root).map_err(|_private| "Cannot read an MCP root folder")?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("An MCP root must be an existing directory, not a symlink");
            }
            let requested = std::path::absolute(&root)
                .map_err(|_private| "Cannot resolve an MCP root folder")?;
            let directory = root
                .canonicalize()
                .map_err(|_private| "Cannot resolve an MCP root folder")?;
            canonical.push(Root {
                directory,
                requested,
            });
        }
        if canonical.is_empty() {
            return Err("MCP needs at least one explicit root");
        }
        Ok(Self(canonical))
    }

    pub(super) fn resolve(&self, show: &str) -> Result<PathBuf, &'static str> {
        let path = Path::new(show);
        if show.is_empty()
            || show.len() > 4_096
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err("Use an exact Show path inside a configured root, without '..'");
        }
        if path.is_absolute() {
            return self.check(path);
        }
        if path.components().count() != 1
            || !matches!(path.components().next(), Some(Component::Normal(_)))
        {
            return Err("Use an absolute Show path or an exact folder name");
        }
        let mut found = None;
        for configured in &self.0 {
            let root = &configured.directory;
            let candidate =
                if root.file_name() == path.file_name() && root.join(Manifest::FILE).exists() {
                    root.clone()
                } else {
                    root.join(path)
                };
            if let Ok(directory) = self.check(&candidate) {
                if found
                    .as_ref()
                    .is_some_and(|previous| previous != &directory)
                {
                    return Err("That Show name is ambiguous across roots; use its absolute path");
                }
                found = Some(directory);
            }
        }
        found.ok_or("That exact Show is unavailable inside the configured roots")
    }

    fn check(&self, path: &Path) -> Result<PathBuf, &'static str> {
        for root in &self.0 {
            let Ok(relative) = path
                .strip_prefix(&root.directory)
                .or_else(|_outside| path.strip_prefix(&root.requested))
            else {
                continue;
            };
            let mut inspected = root.directory.clone();
            let parts = std::iter::once(None).chain(relative.components().map(Some));
            for part in parts {
                if let Some(part) = part {
                    if !matches!(part, Component::Normal(_) | Component::CurDir) {
                        return Err("The Show path contains an invalid component");
                    }
                    inspected.push(part.as_os_str());
                }
                let metadata = fs::symlink_metadata(&inspected)
                    .map_err(|_private| "That exact Show is unavailable")?;
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err("Show paths must stay in directories, without symlinks");
                }
            }
            let canonical = inspected
                .canonicalize()
                .map_err(|_private| "Cannot resolve that Show")?;
            if canonical.starts_with(&root.directory) {
                return Ok(canonical);
            }
        }
        Err("That Show is outside the configured roots")
    }

    pub(super) fn list(
        &self,
        cursor: Option<&str>,
    ) -> Result<(Vec<Overview>, Option<String>), &'static str> {
        let after = cursor.map(PathBuf::from);
        if let Some(after) = &after
            && (!after.is_absolute()
                || !self.0.iter().any(|root| after.starts_with(&root.directory))
                || after
                    .components()
                    .any(|part| matches!(part, Component::ParentDir)))
        {
            return Err("Invalid Show list cursor");
        }
        let mut paths = Vec::new();
        for configured in &self.0 {
            let root = &configured.directory;
            self.check(root)?;
            if root.join(Manifest::FILE).symlink_metadata().is_ok() {
                retain(&mut paths, root.clone(), after.as_deref());
                continue;
            }
            for entry in fs::read_dir(root).map_err(|_private| "Cannot list an MCP root")? {
                let entry = entry.map_err(|_private| "Cannot list an MCP root")?;
                let kind = entry
                    .file_type()
                    .map_err(|_private| "Cannot inspect a Show folder")?;
                if !kind.is_dir() || kind.is_symlink() {
                    continue;
                }
                let name = entry.file_name();
                if name.to_str().is_none_or(|name| name.starts_with('.')) {
                    continue;
                }
                let path = entry.path();
                if name.to_str().and_then(ShowId::parse).is_some()
                    || path.join(Manifest::FILE).symlink_metadata().is_ok()
                {
                    retain(&mut paths, path, after.as_deref());
                }
            }
        }
        let truncated = paths.len() > 100;
        paths.truncate(100);
        let next = if truncated {
            paths
                .last()
                .and_then(|path| path.to_str())
                .map(str::to_owned)
        } else {
            None
        };
        let rows = paths
            .into_iter()
            .map(|path| {
                let checked = self.check(&path)?;
                let entry =
                    Entry::open(&checked).map_err(|_private| "Cannot inspect a listed Show")?;
                Ok(Overview::of(&entry, checked.parent()))
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        Ok((rows, next))
    }
}

fn retain(paths: &mut Vec<PathBuf>, path: PathBuf, after: Option<&Path>) {
    if after.is_none_or(|after| path.as_path() > after)
        && path.to_str().is_some()
        && let Err(position) = paths.binary_search(&path)
    {
        paths.insert(position, path);
        paths.truncate(101);
    }
}
