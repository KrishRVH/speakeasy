use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::Serialize;
use speakeasy_show::Manifest;

use crate::show::{
    evidence::{Overview, Problem},
    store::{address::ShowId, recall::Entry},
};

pub(super) const PAGE: usize = 100;

pub(super) struct Roots {
    roots: Vec<PathBuf>,
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

impl Roots {
    pub(super) fn open(roots: &[PathBuf], managed: Option<PathBuf>) -> Result<Self, &'static str> {
        Ok(Self {
            roots: roots
                .iter()
                .map(|root| {
                    root.canonicalize()
                        .ok()
                        .filter(|root| root.is_dir())
                        .ok_or("An MCP root must be an existing directory")
                })
                .collect::<Result<_, _>>()?,
            managed: managed.and_then(|managed| managed.canonicalize().ok()),
        })
    }

    pub(super) fn managed(&self) -> Option<&Path> {
        self.managed.as_deref()
    }

    pub(super) fn resolve(&self, show: &str) -> Result<PathBuf, &'static str> {
        let mut found: Option<PathBuf> = None;
        for folder in self.roots.iter().filter_map(|root| inside(root, show)) {
            if found.as_ref().is_some_and(|previous| *previous != folder) {
                return Err("That Show name is ambiguous across roots; use its absolute path");
            }
            found = Some(folder);
        }
        found.ok_or("That exact Show is unavailable inside the configured roots")
    }

    pub(super) fn list(&self, cursor: Option<&str>) -> Listing {
        let after = cursor.map(Path::new);
        let mut rows = Vec::new();
        let mut unavailable_roots = Vec::new();
        for root in &self.roots {
            if gather(root, &mut rows, after).is_err() {
                unavailable_roots.push(Problem {
                    file: root.clone(),
                    reason: "Cannot list an MCP root".to_owned(),
                });
            }
        }
        let next_cursor = if rows.len() > PAGE {
            rows.truncate(PAGE);
            rows.last().map(|row| row.to_string_lossy().into_owned())
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
        Listing {
            shows,
            unavailable_roots,
            truncated: next_cursor.is_some(),
            next_cursor,
        }
    }
}

fn inside(root: &Path, show: &str) -> Option<PathBuf> {
    let folder = if root.file_name().is_some_and(|name| name == show) && holds_show(root) {
        root.to_owned()
    } else {
        root.join(show).canonicalize().ok()?
    };
    (folder.starts_with(root) && folder.is_dir()).then_some(folder)
}

fn gather(root: &Path, rows: &mut Vec<PathBuf>, after: Option<&Path>) -> io::Result<()> {
    if fs::exists(root.join(Manifest::FILE))? {
        retain(rows, root.to_owned(), after);
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let name = entry?.file_name();
        let path = root.join(&name);
        if path.is_dir() && (name.to_str().and_then(ShowId::parse).is_some() || holds_show(&path)) {
            retain(rows, path, after);
        }
    }
    Ok(())
}

fn holds_show(directory: &Path) -> bool {
    directory.join(Manifest::FILE).exists()
}

fn retain(rows: &mut Vec<PathBuf>, row: PathBuf, after: Option<&Path>) {
    if after.is_none_or(|after| row.as_path() > after)
        && let Err(position) = rows.binary_search(&row)
    {
        rows.insert(position, row);
        rows.truncate(PAGE.saturating_add(1));
    }
}
