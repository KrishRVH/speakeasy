use std::{
    fs,
    io::{self, Read as _},
    path::{Path, PathBuf},
};

use speakeasy_show::Wsl;

const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

const SHARES: [&str; 2] = [r"\\wsl.localhost", r"\\wsl$"];

const CONF_LIMIT: usize = 64 << 10;

#[must_use]
pub fn wsl() -> Option<Wsl> {
    default_distribution(registry, read)
}

#[cfg(target_os = "windows")]
fn registry(key: &str, value: &str) -> Option<String> {
    crate::windows::registry::text(key, value)
        .ok()??
        .into_string()
        .ok()
}

#[cfg(not(target_os = "windows"))]
const fn registry(_: &str, _: &str) -> Option<String> {
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Conf {
    Unreachable,
    Missing,
    Text(String),
}

fn default_distribution(
    registry: impl Fn(&str, &str) -> Option<String>,
    mut conf: impl FnMut(&Path) -> Conf,
) -> Option<Wsl> {
    let id = registry(LXSS, "DefaultDistribution").filter(|id| {
        !id.is_empty()
            && id
                .chars()
                .all(|character| character.is_ascii_hexdigit() || "{-}".contains(character))
    })?;
    let name = registry(&format!(r"{LXSS}\{id}"), "DistributionName").filter(|name| {
        !matches!(name.as_str(), "" | "." | "..")
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    })?;
    SHARES.into_iter().find_map(|share| {
        match conf(&PathBuf::from(format!(r"{share}\{name}\etc"))) {
            Conf::Unreachable => None,
            Conf::Missing => Some(Wsl::configured("")),
            Conf::Text(text) => Some(Wsl::configured(&text)),
        }
    })
}

fn read(etc: &Path) -> Conf {
    if !fs::metadata(etc).is_ok_and(|metadata| metadata.is_dir()) {
        return Conf::Unreachable;
    }
    let file = match fs::File::open(etc.join("wsl.conf")) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Conf::Missing,
        Err(_unreadable) => return Conf::Unreachable,
    };
    let mut text = String::new();
    let length = u64::try_from(CONF_LIMIT)
        .ok()
        .and_then(|limit| file.take(limit).read_to_string(&mut text).ok());
    match length {
        Some(length) if length < CONF_LIMIT => Conf::Text(text),
        _ => Conf::Unreachable,
    }
}

#[cfg(test)]
mod tests;
