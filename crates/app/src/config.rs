//! Typed settings: startup classification, path resolution, validation, and saves that patch the
//! file.
//!
//! The JSON file is a user-edited contract that outlives releases. Missing keys read as
//! [`Config::default`], so a new field needs no attribute, and unknown top-level keys are ignored,
//! so a file from another release still loads. A renamed key keeps its old name with
//! `#[serde(alias = "old")]`, on the field or on the enum variant that replaces a retired Engine or
//! Theme, and a retyped key goes through a private wire struct that migrates the old shape.
//! [`LinuxSettings`] stays strict because its shortcut and cancel keys exist only as hand edits, so a
//! misspelled key fails instead of silently keeping a default; retiring one of its keys must keep
//! accepting that key. A save writes only the keys Settings changed, so unknown keys, hand edits,
//! path spellings and unchanged Linux defaults stay as the file has them, though a number past
//! 64-bit precision comes back rounded.

use std::{
    fs,
    io::{self, ErrorKind, Write},
    ops::Deref,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use speakeasy_platform::{Delivery, DesktopOptions, LinuxPaste};

use crate::{theme::Theme, transcript::Fillers};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Engine {
    #[default]
    Whisper,
    Parakeet,
}

impl Engine {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Whisper => "Whisper",
            Self::Parakeet => "Parakeet",
        }
    }

    /// The next engine Settings offers, wrapping around.
    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Whisper => Self::Parakeet,
            Self::Parakeet => Self::Whisper,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each flag persists an independent user preference that can be toggled separately"
)]
pub(crate) struct Config {
    #[serde(skip_serializing_if = "LinuxSettings::is_default")]
    pub linux: LinuxSettings,
    pub engine: Engine,
    pub microphone: Option<String>,
    pub engine_executable: PathBuf,
    pub model: PathBuf,
    pub language: String,
    pub remove_fillers: bool,
    pub threads: u16,
    pub use_gpu: bool,
    pub reduced_motion: bool,
    pub preserve_clipboard: bool,
    pub play_sounds: bool,
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            linux: LinuxSettings::default(),
            engine: Engine::default(),
            microphone: None,
            engine_executable: PathBuf::new(),
            model: PathBuf::new(),
            language: "en".into(),
            remove_fillers: true,
            threads: 4,
            use_gpu: true,
            reduced_motion: false,
            preserve_clipboard: false,
            play_sounds: true,
            theme: Theme::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LinuxSettings {
    pub shortcut: String,
    pub cancel: String,
    pub terminal_paste: bool,
    pub manual_paste: bool,
    pub external_shortcut: bool,
}

impl LinuxSettings {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for LinuxSettings {
    fn default() -> Self {
        DesktopOptions::default().into()
    }
}

impl From<DesktopOptions> for LinuxSettings {
    fn from(
        DesktopOptions {
            shortcut,
            cancel,
            paste,
            external_shortcut,
        }: DesktopOptions,
    ) -> Self {
        Self {
            shortcut,
            cancel,
            terminal_paste: paste == LinuxPaste::Terminal,
            manual_paste: paste == LinuxPaste::Manual,
            external_shortcut,
        }
    }
}

impl From<LinuxSettings> for DesktopOptions {
    fn from(
        LinuxSettings {
            shortcut,
            cancel,
            terminal_paste,
            manual_paste,
            external_shortcut,
        }: LinuxSettings,
    ) -> Self {
        let paste = if manual_paste {
            LinuxPaste::Manual
        } else if terminal_paste {
            LinuxPaste::Terminal
        } else {
            LinuxPaste::Standard
        };
        Self {
            shortcut,
            cancel,
            paste,
            external_shortcut,
        }
    }
}

/// How dictated text reaches the focused app. The file keeps it as `preserve_clipboard` and the
/// Linux `manual_paste` and `terminal_paste` flags, which every release reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextDelivery {
    /// Copies the text and sends the paste shortcut: Command+V on macOS, Ctrl+V elsewhere.
    Paste,
    /// Copies the text and sends Ctrl+Shift+V, which terminals expect.
    #[cfg(target_os = "linux")]
    TerminalPaste,
    /// Types the text as key input, leaving the clipboard untouched.
    Type,
    /// Copies the text for the user to paste, so Speakeasy needs no keyboard control.
    #[cfg(target_os = "linux")]
    Copy,
}

impl TextDelivery {
    /// The next choice this platform offers, wrapping around.
    pub(crate) const fn next(self) -> Self {
        #[cfg(target_os = "linux")]
        match self {
            Self::Paste => Self::TerminalPaste,
            Self::TerminalPaste => Self::Type,
            Self::Type => Self::Copy,
            Self::Copy => Self::Paste,
        }
        #[cfg(not(target_os = "linux"))]
        match self {
            Self::Paste => Self::Type,
            Self::Type => Self::Paste,
        }
    }
}

impl Config {
    /// How dictated text reaches the focused app, the one reading of the stored flags. On Linux,
    /// manual paste wins over typing, because typing needs the keyboard control that manual paste
    /// declines.
    pub(crate) const fn text_delivery(&self) -> TextDelivery {
        #[cfg(target_os = "linux")]
        if self.linux.manual_paste {
            return TextDelivery::Copy;
        }
        if self.preserve_clipboard {
            return TextDelivery::Type;
        }
        #[cfg(target_os = "linux")]
        if self.linux.terminal_paste {
            return TextDelivery::TerminalPaste;
        }
        TextDelivery::Paste
    }

    /// Stores `delivery` as the flags that read back as it. Typing and copying keep the chosen
    /// paste keys for a later return to paste.
    pub(crate) fn set_text_delivery(&mut self, delivery: TextDelivery) {
        self.preserve_clipboard = delivery == TextDelivery::Type;
        #[cfg(target_os = "linux")]
        {
            self.linux.manual_paste = delivery == TextDelivery::Copy;
            match delivery {
                TextDelivery::Paste => self.linux.terminal_paste = false,
                TextDelivery::TerminalPaste => self.linux.terminal_paste = true,
                TextDelivery::Type | TextDelivery::Copy => {},
            }
        }
    }

    /// Whether a request removes English hesitations: the user's switch, where
    /// [`Config::fillers_apply`] allows it.
    pub(crate) fn fillers(&self) -> Fillers {
        if self.remove_fillers && self.fillers_apply() {
            Fillers::Remove
        } else {
            Fillers::Preserve
        }
    }

    /// Whether the engine and language let the switch remove English hesitations. Parakeet detects
    /// the language without reporting it, so the user's switch decides. Whisper removes them only
    /// for an explicit English request, because "um" is a word in languages such as German and
    /// Portuguese, and `auto` names no language to check.
    pub(crate) fn fillers_apply(&self) -> bool {
        self.engine == Engine::Parakeet || self.language.eq_ignore_ascii_case("en")
    }

    pub(crate) fn delivery(&self) -> Delivery {
        match self.text_delivery() {
            TextDelivery::Type => Delivery::Direct,
            TextDelivery::Paste => Delivery::Paste,
            #[cfg(target_os = "linux")]
            TextDelivery::TerminalPaste | TextDelivery::Copy => Delivery::Paste,
        }
    }

    /// Whether the user or the OS asks for reduced motion.
    pub(crate) fn prefers_reduced_motion(&self) -> bool {
        self.reduced_motion || speakeasy_platform::reduced_motion()
    }

    pub(crate) fn desktop_options(&self) -> DesktopOptions {
        self.linux.clone().into()
    }

    pub(crate) fn read(path: &Path) -> anyhow::Result<Self> {
        let contents = fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?;
        Ok(parse(&contents, path)?.0)
    }

    /// Reads the settings at `path` for launch: the draft Settings edits, and how it stands. A file
    /// that cannot be read yields defaults for Settings to edit, and [`Config::save_changes`] still
    /// refuses to replace that file until it parses.
    pub(crate) fn load(path: &Path) -> (Self, Startup) {
        if !path.exists() {
            return (Self::first_launch(), Startup::Unconfigured);
        }
        let config = match Self::read(path) {
            Ok(config) => config,
            Err(error) => return (Self::default(), Startup::Invalid(format!("{error:#}"))),
        };
        if config.needs_engine() {
            return (config, Startup::Unconfigured);
        }
        match config.validated(path) {
            Ok(valid) => (Self::clone(&valid), Startup::Configured(valid)),
            Err(error) => (config, Startup::Invalid(format!("{error:#}"))),
        }
    }

    /// What a first launch, with no settings file, starts from. Automatic setup installs Parakeet,
    /// so the draft names it before setup runs; a file without an engine key still reads as
    /// [`Config::default`]'s Whisper, which such files were written for.
    pub(crate) fn first_launch() -> Self {
        Self {
            engine: Engine::Parakeet,
            ..Self::default()
        }
    }

    /// What a save treats the file at `path` as holding when Settings opens on `draft`: the draft
    /// for a file that exists, and otherwise the defaults a missing key reads as, so a save writes
    /// every choice Settings made, a first launch's Parakeet included.
    pub(crate) fn held_by(path: &Path, draft: &Self) -> Self {
        if path.exists() {
            draft.clone()
        } else {
            Self::default()
        }
    }

    /// Whether the user still has to choose a speech engine.
    pub(crate) fn needs_engine(&self) -> bool {
        self.engine_executable.as_os_str().is_empty()
    }

    /// How far moving to `next` reaches: the highest tier among the fields that differ. Every field
    /// is named, so a new one cannot compile until it is classified.
    pub(crate) fn change_to(&self, next: &Self) -> Change {
        let Self {
            linux,
            engine,
            microphone,
            engine_executable,
            model,
            language,
            remove_fillers,
            threads,
            use_gpu,
            reduced_motion,
            preserve_clipboard,
            play_sounds,
            theme,
        } = next;
        [
            (
                cfg!(target_os = "linux") && self.linux != *linux,
                Change::Desktop,
            ),
            (
                self.engine != *engine
                    || self.engine_executable != *engine_executable
                    || self.model != *model
                    || self.threads != *threads
                    || self.use_gpu != *use_gpu,
                Change::Engine,
            ),
            // Whisper takes the language with each request, so its launch language is only a
            // default; Parakeet takes none.
            (self.language != *language, Change::Session),
            (
                self.microphone != *microphone
                    || self.remove_fillers != *remove_fillers
                    || self.preserve_clipboard != *preserve_clipboard,
                Change::Session,
            ),
            // The owner sounds the cues, tells the microphone of each, and holds their output.
            (self.play_sounds != *play_sounds, Change::Session),
            (
                self.theme != *theme || self.reduced_motion != *reduced_motion,
                Change::Presentation,
            ),
        ]
        .into_iter()
        .filter_map(|(changed, tier)| changed.then_some(tier))
        .max()
        .unwrap_or(Change::Unchanged)
    }

    /// Resolves the engine and model of a copy against the settings directory and canonicalizes
    /// them, then checks that they exist and that the remaining values are in range. The draft
    /// stays as the user wrote it.
    pub(crate) fn validated(&self, path: &Path) -> anyhow::Result<ValidConfig> {
        let mut config = self.clone();
        let directory = path.parent().unwrap_or_else(|| Path::new("."));
        for file in [&mut config.engine_executable, &mut config.model] {
            ensure!(
                !file.as_os_str().is_empty(),
                "Choose the local speech executable and a matching model."
            );
            if file.is_relative() {
                *file = directory.join(&file);
            }
            *file = file
                .canonicalize()
                .map_err(|error| cannot_open(file, &error))?;
            ensure!(
                file.is_file(),
                "Not a file: {}. Choose the engine executable and model in Settings.",
                file.display()
            );
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            let engine = &config.engine_executable;
            let metadata = engine
                .metadata()
                .map_err(|error| cannot_open(engine, &error))?;
            ensure!(
                metadata.permissions().mode() & 0o111 != 0,
                "Speech executable is not executable. Choose an installed engine or enable its executable permission."
            );
        }
        ensure!(
            (1..=256).contains(&config.threads),
            "Set threads in the settings file to a number from 1 to 256."
        );
        ensure!(
            (1..=16).contains(&config.language.len()),
            "Set language in the settings file to a language code, such as en, or to auto."
        );
        Ok(ValidConfig(config))
    }

    /// Saves what Settings changed in `draft`, writing each changed key's value from `validated`.
    /// `held` is the settings the file last held, in each spelling a draft may still have, such as
    /// a submitted draft and the paths validation resolved from it; a key that matches any of them
    /// is unchanged. Every other key stays as the file has it, so edits made while Speakeasy runs
    /// survive; a key changed both ways takes the Settings value. A new file gets every key but a
    /// default Linux block, and a file the user still has to repair is never replaced.
    pub(crate) fn save_changes(
        held: &[&Self],
        draft: &Self,
        validated: &ValidConfig,
        path: &Path,
    ) -> anyhow::Result<()> {
        let cannot_save = |error: io::Error| {
            anyhow!(
                "Cannot save settings to {}: {error}. {}",
                path.display(),
                crate::paths::remedy(&error)
            )
        };
        let target = link_target(path).map_err(cannot_save)?;
        let contents = match fs::read(&target) {
            Ok(contents) => {
                let (_, mut file) = parse(&contents, path)?;
                patch(&mut file, held, draft, validated)
                    .with_context(|| format!("Cannot save settings to {}", path.display()))?;
                serde_json::to_vec_pretty(&file)?
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {
                serde_json::to_vec_pretty(&**validated)?
            },
            Err(error) => return Err(cannot_save(error)),
        };
        replace(&target, &contents).map_err(cannot_save)
    }
}

/// How far a settings change reaches, ordered by disruption; each tier's action covers the tiers
/// below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Change {
    Unchanged,
    /// Only the shell, the pill and the tray read these, so a session continues.
    Presentation,
    /// The owner reads these for each session or request, so a session in progress ends.
    Session,
    /// These start the engine process, so the worker reloads.
    Engine,
    /// Linux binds its desktop options when the owners start, so they restart.
    Desktop,
}

/// How saved settings stand at launch.
pub(crate) enum Startup {
    /// A first launch, or a file without an engine: Settings offers setup.
    Unconfigured,
    Configured(ValidConfig),
    /// The file cannot be read or validated; the message says why.
    Invalid(String),
}

/// Settings whose engine and model resolved to canonical absolute files and whose values were in
/// range when checked. Only [`Config::validated`] builds one; the files can still disappear later,
/// and engine startup reports that.
#[derive(Clone)]
pub(crate) struct ValidConfig(Config);

impl ValidConfig {
    pub(crate) fn into_inner(self) -> Config {
        self.0
    }

    /// Trusts `config` without checking it, for tests whose fake engines have no files.
    #[cfg(test)]
    pub(crate) const fn assume(config: Config) -> Self {
        Self(config)
    }
}

impl Deref for ValidConfig {
    type Target = Config;

    fn deref(&self) -> &Config {
        &self.0
    }
}

/// Parses `contents` as settings and as the map a save patches, so a file that reads is always one a
/// save can patch: skipping an unknown key accepts values that holding it does not, such as `1e400`
/// or a lone surrogate, and serde reads `linux` from an array that a save cannot patch key by key.
/// Settings parse first, so a repair notice names the setting to fix.
fn parse(contents: &[u8], path: &Path) -> anyhow::Result<(Config, Map<String, Value>)> {
    let invalid = || format!("Invalid settings in {}", path.display());
    let config: Config = serde_json::from_slice(contents).with_context(invalid)?;
    let file: Map<String, Value> = serde_json::from_slice(contents).with_context(invalid)?;
    if file.get("linux").is_some_and(|linux| !linux.is_object()) {
        return Err(anyhow!("The linux settings must be an object").context(invalid()));
    }
    Ok((config, file))
}

/// Linux's own limit on following a chain of links, which also ends a loop of them.
const LINK_DEPTH: usize = 40;

/// The file a save writes: where a link at `path` points, through any chain of links, so a settings
/// file that a dotfile manager links in stays linked, even before the file it names exists.
fn link_target(path: &Path) -> io::Result<PathBuf> {
    let mut target = path.to_owned();
    for _ in 0..LINK_DEPTH {
        if !target.is_symlink() {
            return Ok(target);
        }
        let link = fs::read_link(&target)?;
        target = match target.parent() {
            Some(folder) => folder.join(link),
            None => link,
        };
    }
    Err(io::Error::other("Too many levels of symbolic links"))
}

fn cannot_open(file: &Path, error: &io::Error) -> anyhow::Error {
    if error.kind() == ErrorKind::NotFound {
        anyhow!(
            "File not found: {}. Choose it again in Settings.",
            file.display()
        )
    } else {
        anyhow!(
            "Cannot open {} ({error}). Choose it again in Settings.",
            file.display()
        )
    }
}

/// Sets each key whose value in `draft` matches none of `held` to its value in `validated`. Linux
/// keys are compared one by one, because a default Linux block is not serialized at all.
fn patch(
    file: &mut Map<String, Value>,
    held: &[&Config],
    draft: &Config,
    validated: &Config,
) -> anyhow::Result<()> {
    for (key, value) in changes(held.iter().copied(), draft, validated)? {
        if key != "linux" {
            file.insert(key, value);
        }
    }
    let linux = changes(
        held.iter().map(|config| &config.linux),
        &draft.linux,
        &validated.linux,
    )?;
    if !linux.is_empty() {
        file.entry("linux")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .context("parse admits only a linux object")?
            .extend(linux);
    }
    Ok(())
}

/// The keys of `validated` whose values in `draft` match none of `held`, with their values.
fn changes<'a, T: Serialize + 'a>(
    held: impl IntoIterator<Item = &'a T>,
    draft: &T,
    validated: &T,
) -> serde_json::Result<Vec<(String, Value)>> {
    let held = held
        .into_iter()
        .map(serde_json::to_value)
        .collect::<serde_json::Result<Vec<_>>>()?;
    let draft = serde_json::to_value(draft)?;
    let Value::Object(validated) = serde_json::to_value(validated)? else {
        return Ok(Vec::new());
    };
    Ok(validated
        .into_iter()
        .filter(|(key, _)| held.iter().all(|held| held.get(key) != draft.get(key)))
        .collect())
}

/// Replaces `target` with `contents` all at once from a temporary file beside it, so a failure
/// never leaves half a file and the rename stays on one filesystem.
fn replace(target: &Path, contents: &[u8]) -> io::Result<()> {
    let directory = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(contents)?;
    file.as_file().sync_all()?;
    file.persist(target)?;
    Ok(())
}

/// Settings that pass validation: a fake `engine` and `model.gguf` written into `directory`, which
/// a settings file there may also name relatively.
#[cfg(test)]
pub(crate) fn valid_config(directory: &Path) -> anyhow::Result<Config> {
    let engine_executable = directory.join("engine");
    let model = directory.join("model.gguf");
    fs::write(&engine_executable, b"fake engine")?;
    fs::write(&model, b"fake model")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&engine_executable, fs::Permissions::from_mode(0o700))?;
    }
    Ok(Config {
        engine_executable,
        model,
        ..Config::default()
    })
}

#[cfg(test)]
mod tests;
