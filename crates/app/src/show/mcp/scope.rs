use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Component, Path, PathBuf},
};

use serde::Serialize;
use speakeasy_platform::ReadDirectory;
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

pub(super) struct Folder {
    pub path: PathBuf,
    pub files: ReadDirectory,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Listing {
    pub shows: Vec<Overview>,
    pub unavailable_roots: Vec<Problem>,
    pub truncated: bool,
    pub next_cursor: Option<String>,
}

struct Row {
    path: PathBuf,
    root: usize,
    name: Option<OsString>,
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
    pub(super) fn open(roots: &[PathBuf], managed: Option<PathBuf>) -> Result<Self, &'static str> {
        Ok(Self {
            roots: roots
                .iter()
                .map(|root| Root::open(root))
                .collect::<Result<_, _>>()?,
            managed,
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
            let Ok(root) = self.check(&configured.directory) else {
                continue;
            };
            let folder = if root.path.file_name() == Some(name)
                && matches!(holds_show(&root.files), Ok(true))
            {
                root
            } else if let Ok(files) = root.files.child(name) {
                Folder {
                    path: root.path.join(name),
                    files,
                }
            } else {
                continue;
            };
            if found
                .as_ref()
                .is_some_and(|previous| previous.path != folder.path)
            {
                return Err("That Show name is ambiguous across roots; use its absolute path");
            }
            found = Some(folder);
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

    pub(super) fn list(&self, cursor: Option<&str>) -> Result<Listing, &'static str> {
        self.list_with(cursor, &|directory, each| directory.visit(each))
    }

    pub(super) fn list_with(
        &self,
        cursor: Option<&str>,
        visit: Visit<'_>,
    ) -> Result<Listing, &'static str> {
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
        let mut reached = Vec::new();
        let mut rows = Vec::new();
        let mut unavailable_roots = Vec::new();
        for configured in &self.roots {
            let listed = self.check(&configured.directory).and_then(|root| {
                for row in gather(&root, reached.len(), after.as_deref(), visit)? {
                    retain(&mut rows, row, after.as_deref());
                }
                reached.push(root);
                Ok(())
            });
            if let Err(reason) = listed {
                unavailable_roots.push(Problem {
                    file: configured.directory.clone(),
                    reason: reason.to_owned(),
                });
            }
        }
        let next_cursor = if rows.len() > PAGE {
            rows.truncate(PAGE);
            rows.last()
                .and_then(|row| row.path.to_str())
                .map(str::to_owned)
        } else {
            None
        };
        let shows = rows
            .into_iter()
            .filter_map(|row| self.overview(row, &reached))
            .collect();
        Ok(Listing {
            shows,
            unavailable_roots,
            truncated: next_cursor.is_some(),
            next_cursor,
        })
    }

    fn overview(&self, row: Row, reached: &[Folder]) -> Option<Overview> {
        let root = reached.get(row.root)?;
        let child;
        let files = match &row.name {
            None => &root.files,
            Some(name) => match root.files.child(name) {
                Ok(opened) => {
                    child = opened;
                    &child
                },
                Err(_private) => {
                    return Some(Overview::damaged(
                        row.path,
                        "Cannot open this folder".to_owned(),
                    ));
                },
            },
        };
        let entry = Entry::read_from(row.path, files, self.managed());
        Some(Overview::of(&entry))
    }
}

pub(super) type Visit<'a> = &'a dyn Fn(&ReadDirectory, &mut dyn FnMut(OsString)) -> io::Result<()>;

fn gather(
    root: &Folder,
    index: usize,
    after: Option<&Path>,
    visit: Visit<'_>,
) -> Result<Vec<Row>, &'static str> {
    const UNLISTED: &str = "Cannot list an MCP root";
    let mut rows = Vec::new();
    let mut keep = |path, name| {
        let row = Row {
            path,
            root: index,
            name,
        };
        retain(&mut rows, row, after);
    };
    if holds_show(&root.files).map_err(|_private| UNLISTED)? {
        keep(root.path.clone(), None);
        return Ok(rows);
    }
    visit(&root.files, &mut |name| {
        if !visible(&name) {
            return;
        }
        let listed = match root.files.child(&name) {
            Ok(files) => {
                name.to_str().and_then(ShowId::parse).is_some()
                    || holds_show(&files).unwrap_or(true)
            },
            Err(error) => !matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ),
        };
        if listed {
            keep(root.path.join(&name), Some(name));
        }
    })
    .map_err(|_private| UNLISTED)?;
    Ok(rows)
}

fn holds_show(directory: &ReadDirectory) -> io::Result<bool> {
    directory.contains(OsStr::new(Manifest::FILE))
}

fn visible(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| !name.starts_with('.'))
}

fn retain(rows: &mut Vec<Row>, row: Row, after: Option<&Path>) {
    if after.is_none_or(|after| row.path.as_path() > after)
        && row.path.to_str().is_some()
        && let Err(position) = rows.binary_search_by(|kept| kept.path.cmp(&row.path))
    {
        rows.insert(position, row);
        rows.truncate(PAGE.saturating_add(1));
    }
}
