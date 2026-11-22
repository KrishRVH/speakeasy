use std::{
    ffi::OsStr,
    fs, io,
    path::{Component, Path, PathBuf},
};

use serde::Serialize;
use speakeasy_show::Manifest;

use crate::show::{
    evidence::{Overview, Problem},
    store::{address::ShowId, recall::Entry},
};

pub(super) const PAGE: usize = 100;

pub(super) const PATH_LIMIT: usize = 4_096;

pub(super) struct Roots {
    roots: Vec<Root>,
    managed: Option<PathBuf>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Listing {
    pub shows: Vec<Overview>,
    pub unavailable_roots: Vec<Problem>,
    pub truncated: bool,
    pub next_cursor: Option<String>,
}

struct Root {
    directory: PathBuf,
    requested: PathBuf,
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
        if !requested.is_dir() {
            return Err("An MCP root must be an existing directory");
        }
        let directory = requested
            .canonicalize()
            .map_err(|_private| "Cannot resolve an MCP root folder")?;
        if requested.to_str().is_none() || directory.to_str().is_none() {
            return Err("An MCP root path must be valid UTF-8");
        }
        Ok(Self {
            directory,
            requested,
        })
    }

    fn translate(&self, path: &Path) -> Result<Option<PathBuf>, &'static str> {
        let Ok(relative) = path
            .strip_prefix(&self.directory)
            .or_else(|_outside| path.strip_prefix(&self.requested))
        else {
            return Ok(None);
        };
        for part in relative.components() {
            let Component::Normal(name) = part else {
                return Err("The Show path contains an invalid component");
            };
            if !visible(name) {
                return Err("The Show path contains a hidden or invalid directory");
            }
        }
        let folder = self.directory.join(relative);
        if !folder.is_dir() {
            return Err("That exact Show is unavailable inside the configured roots");
        }
        Ok(Some(folder))
    }
}

impl Roots {
    pub(super) fn open(roots: &[PathBuf], managed: Option<PathBuf>) -> Result<Self, &'static str> {
        Ok(Self {
            roots: roots
                .iter()
                .map(|root| Root::open(root))
                .collect::<Result<_, _>>()?,
            managed: managed.and_then(|managed| managed.canonicalize().ok()),
        })
    }

    pub(super) fn managed(&self) -> Option<&Path> {
        self.managed.as_deref()
    }

    pub(super) fn resolve(&self, show: &str) -> Result<PathBuf, &'static str> {
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
        let mut found: Option<PathBuf> = None;
        for root in &self.roots {
            let folder = if root.directory.file_name() == Some(name) && holds_show(&root.directory)
            {
                root.directory.clone()
            } else if root.directory.join(name).is_dir() {
                root.directory.join(name)
            } else {
                continue;
            };
            if found.as_ref().is_some_and(|previous| *previous != folder) {
                return Err("That Show name is ambiguous across roots; use its absolute path");
            }
            found = Some(folder);
        }
        found.ok_or("That exact Show is unavailable inside the configured roots")
    }

    fn check(&self, path: &Path) -> Result<PathBuf, &'static str> {
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

    pub(super) fn list(&self, cursor: Option<&str>) -> Result<Listing, &'static str> {
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
        let mut rows = Vec::new();
        let mut unavailable_roots = Vec::new();
        for root in &self.roots {
            if gather(&root.directory, &mut rows, after.as_deref()).is_err() {
                unavailable_roots.push(Problem {
                    file: root.directory.clone(),
                    reason: "Cannot list an MCP root".to_owned(),
                });
            }
        }
        let next_cursor = if rows.len() > PAGE {
            rows.truncate(PAGE);
            rows.last().and_then(|row| row.to_str()).map(str::to_owned)
        } else {
            None
        };
        let shows = rows
            .into_iter()
            .map(|row| match Entry::open(&row, self.managed()) {
                Ok(entry) => Overview::of(&entry),
                Err(error) => Overview::unreadable(row, error.to_string()),
            })
            .collect();
        Ok(Listing {
            shows,
            unavailable_roots,
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }
}

fn gather(root: &Path, rows: &mut Vec<PathBuf>, after: Option<&Path>) -> io::Result<()> {
    if fs::exists(root.join(Manifest::FILE))? {
        retain(rows, root.to_owned(), after);
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let name = entry?.file_name();
        let path = root.join(&name);
        if visible(&name)
            && path.is_dir()
            && (name.to_str().and_then(ShowId::parse).is_some() || holds_show(&path))
        {
            retain(rows, path, after);
        }
    }
    Ok(())
}

fn holds_show(directory: &Path) -> bool {
    directory.join(Manifest::FILE).exists()
}

fn visible(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| !name.starts_with('.'))
}

fn retain(rows: &mut Vec<PathBuf>, row: PathBuf, after: Option<&Path>) {
    if after.is_none_or(|after| row.as_path() > after)
        && row.to_str().is_some()
        && let Err(position) = rows.binary_search(&row)
    {
        rows.insert(position, row);
        rows.truncate(PAGE.saturating_add(1));
    }
}
