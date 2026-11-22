use std::{
    fs, io,
    path::{Path, PathBuf},
};

use speakeasy_show::Wsl;

const LXSS: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

const SHARES: [&str; 2] = [r"\\wsl.localhost", r"\\wsl$"];

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
    let id = registry(LXSS, "DefaultDistribution")?;
    let name = registry(&format!(r"{LXSS}\{id}"), "DistributionName")?;
    SHARES.into_iter().find_map(|share| {
        match conf(&PathBuf::from(format!(r"{share}\{name}\etc"))) {
            Conf::Unreachable => None,
            Conf::Missing => Some(Wsl::configured("")),
            Conf::Text(text) => Some(Wsl::configured(&text)),
        }
    })
}

fn read(etc: &Path) -> Conf {
    if !etc.is_dir() {
        return Conf::Unreachable;
    }
    match fs::read_to_string(etc.join("wsl.conf")) {
        Ok(text) => Conf::Text(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Conf::Missing,
        Err(_unreadable) => Conf::Unreachable,
    }
}

#[cfg(test)]
mod tests;
