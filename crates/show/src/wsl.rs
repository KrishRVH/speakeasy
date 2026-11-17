use std::path::Path;

const DEFAULT_ROOT: &str = "/mnt/";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wsl {
    drives: Option<String>,
    interop: bool,
}

impl Wsl {
    #[must_use]
    pub fn configured(conf: &str) -> Self {
        let mut settings = Settings::default();
        let mut section = None;
        for line in conf.lines() {
            match Line::parse(line) {
                Line::Blank | Line::Invalid => {},
                Line::Section(name) => section = name,
                Line::Entry(key, value) => {
                    if let Some(section) = section {
                        settings.offer(section, key, value);
                    }
                },
            }
        }
        let drives = if settings.automount.as_deref().is_none_or(enabled) {
            settings
                .root
                .map_or_else(|| Some(DEFAULT_ROOT.to_owned()), root)
        } else {
            None
        };
        Self {
            drives,
            interop: settings.interop.as_deref().is_none_or(enabled),
        }
    }

    #[must_use]
    pub fn path(&self, windows: &Path) -> Option<String> {
        let root = self.drives.as_deref()?;
        let text = windows.to_str()?;
        let text = text.strip_prefix(r"\\?\").unwrap_or(text);
        let mut characters = text.chars();
        let drive = characters
            .next()
            .filter(char::is_ascii_alphabetic)?
            .to_ascii_lowercase();
        let rest = characters.as_str().strip_prefix(':')?;
        let rest = match rest.strip_prefix(['\\', '/']) {
            Some(rest) => rest,
            None if rest.is_empty() => rest,
            None => return None,
        };
        let mut path = format!("{root}{drive}");
        for part in rest.split(['\\', '/']).filter(|part| !part.is_empty()) {
            path.push('/');
            path.push_str(part);
        }
        Some(path)
    }

    #[must_use]
    pub fn command(&self, windows: &Path) -> Option<String> {
        if self.interop {
            self.path(windows)
        } else {
            None
        }
    }
}

#[derive(Default)]
struct Settings {
    automount: Option<String>,
    root: Option<String>,
    interop: Option<String>,
}

impl Settings {
    fn offer(&mut self, section: &str, key: &str, value: String) {
        let is = |expected_section: &str, expected_key: &str| {
            section.eq_ignore_ascii_case(expected_section) && key.eq_ignore_ascii_case(expected_key)
        };
        let slot = if is("automount", "enabled") {
            &mut self.automount
        } else if is("automount", "root") {
            &mut self.root
        } else if is("interop", "enabled") {
            &mut self.interop
        } else {
            return;
        };
        slot.get_or_insert(value);
    }
}

enum Line<'a> {
    Blank,
    Invalid,
    Section(Option<&'a str>),
    Entry(&'a str, String),
}

impl<'a> Line<'a> {
    fn parse(line: &'a str) -> Self {
        let line = line.trim_start_matches(SPACE);
        if line.is_empty() || line.starts_with('#') {
            return Self::Blank;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let (name, rest) = name(rest);
            let closed = rest
                .strip_prefix(']')
                .map(|rest| rest.trim_start_matches(SPACE))
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('#'));
            return Self::Section((closed && !name.is_empty()).then_some(name));
        }
        let (key, rest) = name(line);
        let value = rest
            .trim_start_matches(SPACE)
            .strip_prefix('=')
            .filter(|_| !key.is_empty())
            .and_then(|value| unquote(value.trim_start_matches(SPACE)));
        value.map_or(Self::Invalid, |value| Self::Entry(key, value))
    }
}

const SPACE: [char; 2] = [' ', '\t'];

fn name(text: &str) -> (&str, &str) {
    let end = text
        .char_indices()
        .find(|&(index, character)| {
            !(character.is_ascii_alphabetic() || (index > 0 && character.is_ascii_digit()))
        })
        .map_or(text.len(), |(index, _)| index);
    text.split_at_checked(end).unwrap_or((text, ""))
}

fn unquote(raw: &str) -> Option<String> {
    let mut value = String::new();
    let mut kept = 0;
    let mut quoted = false;
    let mut characters = raw.chars();
    while let Some(character) = characters.next() {
        match character {
            '"' => quoted = !quoted,
            '\\' => value.push(match characters.next()? {
                escaped @ ('\\' | '"') => escaped,
                'b' => '\u{8}',
                'n' => '\n',
                't' => '\t',
                _ => return None,
            }),
            '#' if !quoted => break,
            other => value.push(other),
        }
        if !SPACE.contains(&character) {
            kept = value.len();
        }
    }
    value.truncate(kept);
    (!quoted).then_some(value)
}

fn enabled(value: &str) -> bool {
    !(value == "0" || value.eq_ignore_ascii_case("false"))
}

fn root(mut root: String) -> Option<String> {
    if !root.ends_with('/') {
        root.push('/');
    }
    (root.starts_with('/') && !root.chars().any(char::is_control)).then_some(root)
}

#[cfg(test)]
mod tests;
