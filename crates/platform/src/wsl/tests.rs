use std::{fs, path::Path};

use super::*;

const ID: &str = "{0f6a1d3e-2b8c-4c5d-9e7f-a1b2c3d4e5f6}";

const SHOW: &str = r"C:\Users\ada\AppData\Local\speakeasy\shows\20261117T093000Z";

fn installed(name: &str) -> impl Fn(&str, &str) -> Option<String> {
    let name = name.to_owned();
    move |key, value| match value {
        "DefaultDistribution" if key == LXSS => Some(ID.to_owned()),
        "DistributionName" if key == format!(r"{LXSS}\{ID}") => Some(name.clone()),
        _ => None,
    }
}

fn lookup(
    registry: impl Fn(&str, &str) -> Option<String>,
    answers: &[Conf],
) -> (Option<String>, Vec<String>) {
    let mut asked = Vec::new();
    let mut answers = answers.iter().cloned();
    let wsl = default_distribution(registry, |etc| {
        asked.push(etc.to_str().unwrap().to_owned());
        answers.next().unwrap_or(Conf::Unreachable)
    });
    (wsl.and_then(|wsl| wsl.path(Path::new(SHOW))), asked)
}

#[test]
fn the_default_distribution_names_where_its_settings_live() {
    let moved = Conf::Text("[automount]\nroot = /windir\n".to_owned());
    assert_eq!(
        lookup(installed("Ubuntu-24.04"), &[moved]),
        (
            Some("/windir/c/Users/ada/AppData/Local/speakeasy/shows/20261117T093000Z".to_owned()),
            vec![r"\\wsl.localhost\Ubuntu-24.04\etc".to_owned()]
        )
    );
    assert_eq!(
        lookup(installed("Ubuntu"), &[Conf::Unreachable, Conf::Missing]),
        (
            Some("/mnt/c/Users/ada/AppData/Local/speakeasy/shows/20261117T093000Z".to_owned()),
            vec![
                r"\\wsl.localhost\Ubuntu\etc".to_owned(),
                r"\\wsl$\Ubuntu\etc".to_owned()
            ]
        ),
        "\\\\wsl$ answers when \\\\wsl.localhost can't"
    );
    assert_eq!(
        lookup(installed("Ubuntu"), &[]).0,
        None,
        "Unreadable settings aren't guessed"
    );
}

#[test]
fn no_distribution_reads_no_files() {
    assert_eq!(lookup(|_, _| None, &[]), (None, Vec::new()));
}

#[test]
fn settings_are_read_when_the_distribution_is_reachable() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let etc = root.path().join("etc");
    assert_eq!(read(&etc), Conf::Unreachable);
    fs::create_dir(&etc)?;
    assert_eq!(read(&etc), Conf::Missing);
    fs::write(etc.join("wsl.conf"), "[automount]\nroot = /windir\n")?;
    assert_eq!(
        read(&etc),
        Conf::Text("[automount]\nroot = /windir\n".to_owned())
    );
    Ok(())
}
