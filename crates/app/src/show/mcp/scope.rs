use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use speakeasy_platform::ReadDirectory;
use speakeasy_show::Manifest;

use crate::show::{
    evidence::Overview,
    store::{self, address::ShowId, recall::Entry},
};

pub(super) const PAGE: usize = 100;

pub(super) const PATH_LIMIT: usize = 4_096;

pub(super) struct Roots {
    roots: Vec<Root>,
    managed: Option<PathBuf>,
}

pub(super) struct Folder {
    pub path: PathBuf,
    pub files: ReadDirectory,
}

struct Root {
    directory: PathBuf,
    requested: PathBuf,
    files: ReadDirectory,
}

impl Root {
    fn open(root: &Path) -> Result<Self, &'static str> {
        let requested: PathBuf = std::path::absolute(root)
            .map_err(|_private| "Cannot resolve an MCP root folder")?
            .components()
            .collect();
        if requested.components().next_back() == Some(Component::ParentDir) {
            return Err("An MCP root cannot end with '..'");
        }
        let files = ReadDirectory::open(&requested)
            .map_err(|_private| "An MCP root must be an existing directory, not a symlink")?;
        let directory = requested
            .canonicalize()
            .map_err(|_private| "Cannot resolve an MCP root folder")?;
        if !files.is_at(&directory).unwrap_or(false) {
            return Err("The MCP root changed while it was opening");
        }
        if requested.to_str().is_none() || directory.to_str().is_none() {
            return Err("An MCP root path must be valid UTF-8");
        }
        Ok(Self {
            directory,
            requested,
            files,
        })
    }

    fn translate(&self, path: &Path) -> Result<Option<Folder>, &'static str> {
        let Ok(relative) = path
            .strip_prefix(&self.directory)
            .or_else(|_outside| path.strip_prefix(&self.requested))
        else {
            return Ok(None);
        };
        if !self.files.is_at(&self.directory).unwrap_or(false) {
            return Err("The MCP root has moved or is unavailable");
        }
        let mut path = self.directory.clone();
        let mut files: Option<ReadDirectory> = None;
        for part in relative.components() {
            let Component::Normal(name) = part else {
                return Err("The Show path contains an invalid component");
            };
            if !visible(name) {
                return Err("The Show path contains a hidden or invalid directory");
            }
            files =
                Some(
                    files.as_ref().unwrap_or(&self.files).child(name).map_err(
                        |_private| "Show paths must stay in directories, without symlinks",
                    )?,
                );
            path.push(name);
        }
        let files = match files {
            Some(files) => files,
            None => self
                .files
                .duplicate()
                .map_err(|_private| "Cannot open that Show")?,
        };
        Ok(Some(Folder { path, files }))
    }
}

impl Roots {
    pub(super) fn open(roots: &[PathBuf]) -> Result<Self, &'static str> {
        Ok(Self {
            roots: roots
                .iter()
                .map(|root| Root::open(root))
                .collect::<Result<_, _>>()?,
            managed: store::directory().ok(),
        })
    }

    pub(super) fn managed(&self) -> Option<&Path> {
        self.managed.as_deref()
    }

    pub(super) fn resolve(&self, show: &str) -> Result<Folder, &'static str> {
        let path = Path::new(show);
        if show.is_empty()
            || show.len() > PATH_LIMIT
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err("Use an exact Show path inside a configured root, without '..'");
        }
        if path.is_absolute() {
            return self.check(path);
        }
        let mut parts = path.components();
        let (Some(Component::Normal(name)), None) = (parts.next(), parts.next()) else {
            return Err("Use an absolute Show path or an exact folder name");
        };
        if !visible(name) {
            return Err("The Show path contains a hidden or invalid directory");
        }
        let mut found: Option<Folder> = None;
        for configured in &self.roots {
            let root = &configured.directory;
            let candidate =
                if root.file_name() == path.file_name() && has_manifest(&configured.files) {
                    root.clone()
                } else {
                    root.join(path)
                };
            if let Ok(Some(directory)) = configured.translate(&candidate) {
                if found
                    .as_ref()
                    .is_some_and(|previous| previous.path != directory.path)
                {
                    return Err("That Show name is ambiguous across roots; use its absolute path");
                }
                found = Some(directory);
            }
        }
        found.ok_or("That exact Show is unavailable inside the configured roots")
    }

    fn check(&self, path: &Path) -> Result<Folder, &'static str> {
        let mut reason = "That Show is outside the configured roots";
        for root in &self.roots {
            match root.translate(path) {
                Ok(Some(folder)) => return Ok(folder),
                Ok(None) => {},
                Err(error) => reason = error,
            }
        }
        Err(reason)
    }

    pub(super) fn list(
        &self,
        cursor: Option<&str>,
    ) -> Result<(Vec<Overview>, Option<String>), &'static str> {
        let after = cursor.map(PathBuf::from);
        if let Some(after) = &after
            && (after.as_os_str().len() > PATH_LIMIT
                || !after.is_absolute()
                || !self
                    .roots
                    .iter()
                    .any(|root| after.starts_with(&root.directory))
                || after
                    .components()
                    .any(|part| matches!(part, Component::ParentDir)))
        {
            return Err("Invalid Show list cursor");
        }
        let mut paths = Vec::new();
        for configured in &self.roots {
            let root = &configured.directory;
            self.check(root)?;
            if has_manifest(&configured.files) {
                retain(&mut paths, root.clone(), after.as_deref());
                continue;
            }
            configured
                .files
                .visit(|name| {
                    if !visible(&name) {
                        return;
                    }
                    let Ok(child) = configured.files.child(&name) else {
                        return;
                    };
                    let path = root.join(&name);
                    if name.to_str().and_then(ShowId::parse).is_some() || has_manifest(&child) {
                        retain(&mut paths, path, after.as_deref());
                    }
                })
                .map_err(|_private| "Cannot list an MCP root")?;
        }
        let next = if paths.len() > PAGE {
            paths.truncate(PAGE);
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
                let entry = Entry::read_from(checked.path, &checked.files);
                Ok(Overview::of(&entry, self.managed()))
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        Ok((rows, next))
    }
}

fn has_manifest(directory: &ReadDirectory) -> bool {
    directory.file(Path::new(Manifest::FILE)).is_ok()
}

fn visible(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| !name.starts_with('.'))
}

fn retain(paths: &mut Vec<PathBuf>, path: PathBuf, after: Option<&Path>) {
    if after.is_none_or(|after| path.as_path() > after)
        && path.to_str().is_some()
        && let Err(position) = paths.binary_search(&path)
    {
        paths.insert(position, path);
        paths.truncate(PAGE.saturating_add(1));
    }
}
