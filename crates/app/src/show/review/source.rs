use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use speakeasy_show::{Bundle, Demo, Location, Manifest, Typeface};

use crate::show::store;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Saved,
    Demo,
}

#[derive(Clone)]
pub(super) struct Root {
    pub directory: PathBuf,
    pub fixture: Option<Arc<tempfile::TempDir>>,
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
    let save = |days_ago: u64| -> anyhow::Result<PathBuf> {
        let at = now
            .checked_sub(Duration::from_secs(days_ago.saturating_mul(86_400)))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let destination = store::fresh_directory(&directory, at)?;
        store::stage(
            Location::managed(&destination, None).context(store::UNNAMED)?,
            bundle.clone(),
            None,
        )?
        .persist()?;
        Ok(destination)
    };
    fs::write(save(0)?.join(".pinned"), b"")?;
    let ended_early = save(1)?;
    let path = ended_early.join(Manifest::FILE);
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
        ended_early.join(Bundle::SUMMARY),
        "Screen recording (0:20, 5 keyframes).\nThe recorded window closed before you finished.\nYour speech couldn't be transcribed; these frames are all that remains.\n",
    )?;
    fs::remove_file(save(6)?.join("frames/frame-03.png"))?;
    Ok(Root {
        directory,
        fixture: Some(Arc::new(fixture)),
    })
}
