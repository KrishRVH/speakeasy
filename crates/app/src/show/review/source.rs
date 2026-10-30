//! Real discovery is read-only. Previews own a private temporary library of synthetic Shows.

use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use speakeasy_show::{Demo, Typeface};

use crate::show::store;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Saved,
    Demo,
}

pub(super) struct Root {
    pub directory: PathBuf,
    pub fixture: Option<tempfile::TempDir>,
}

impl Source {
    pub(super) fn load(self) -> anyhow::Result<Root> {
        match self {
            Self::Saved => Ok(Root {
                directory: store::directory()?,
                fixture: None,
            }),
            Self::Demo => fixture(),
        }
    }
}

fn fixture() -> anyhow::Result<Root> {
    let fixture = tempfile::tempdir()?;
    let directory = fixture.path().to_owned();
    let now = SystemTime::now();
    let bundle = Demo::new(Typeface::parse(crate::BRAND_FONT_FILE)?).bundle((1_440, 900))?;
    for days in [0_u64, 1, 6] {
        let at = now
            .checked_sub(Duration::from_secs(days.saturating_mul(86_400)))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let destination = store::fresh_directory(&directory, at)?;
        store::stage(&destination, bundle.clone())?.persist()?;
        match days {
            0 => fs::write(destination.join(".pinned"), b"")?,
            1 => {
                let path = destination.join("show.json");
                let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
                manifest
                    .as_object_mut()
                    .context("Cannot create preview Show")?
                    .extend([
                        ("ending".to_owned(), "windowClosed".into()),
                        ("narrationLost".to_owned(), "notTranscribed".into()),
                    ]);
                fs::write(path, serde_json::to_vec_pretty(&manifest)?)?;
                fs::write(
                    destination.join("summary.md"),
                    "Screen recording (0:20, 4 keyframes).\nThe recorded window closed before you finished.\nYour speech couldn't be transcribed; these frames are all that remains.\n",
                )?;
            },
            _ => fs::remove_file(destination.join("frames/frame-03.png"))?,
        }
    }
    Ok(Root {
        directory,
        fixture: Some(fixture),
    })
}
