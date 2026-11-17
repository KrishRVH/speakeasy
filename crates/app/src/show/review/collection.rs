use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use speakeasy_show::{Bundle, Demo, Ending, NarrationLoss, Typeface};

use crate::show::store;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Collection {
    Saved,
    Demo,
}

#[derive(Clone)]
pub(super) struct Root {
    pub directory: PathBuf,
    pub fixture: Option<Arc<tempfile::TempDir>>,
}

impl Collection {
    pub(super) fn load(self) -> anyhow::Result<Root> {
        match self {
            Self::Saved => Ok(Root {
                directory: store::directory()?,
                fixture: None,
            }),
            Self::Demo => demo(),
        }
    }
}

fn demo() -> anyhow::Result<Root> {
    let fixture = tempfile::tempdir()?;
    let directory = fixture.path().to_owned();
    let now = SystemTime::now();
    let bundle = Demo::new(Typeface::parse(crate::BRAND_FONT_FILE)?).bundle((1_440, 900))?;
    let save = |days_ago: u64, bundle: Bundle| -> anyhow::Result<PathBuf> {
        let at = now
            .checked_sub(Duration::from_secs(days_ago.saturating_mul(86_400)))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let destination = store::fresh_directory(&directory, at)?;
        store::stage(store::managed(&destination, None)?, bundle, None)?.persist()?;
        Ok(destination)
    };
    store::retention::set_kept(&directory, &save(0, bundle.clone())?, true)?;
    let ended_early = save(
        1,
        bundle
            .clone()
            .ended(Ending::WindowClosed, Some(NarrationLoss::NotTranscribed)),
    )?;
    fs::write(
        ended_early.join(Bundle::SUMMARY),
        "Screen recording (0:20, 5 keyframes).\nThe recorded window closed before you finished.\nYour speech couldn't be transcribed; these frames are all that remains.\n",
    )?;
    fs::remove_file(save(6, bundle)?.join("frames/frame-03.png"))?;
    Ok(Root {
        directory,
        fixture: Some(Arc::new(fixture)),
    })
}
