use speakeasy_show::{Image, Journal, Moment, Recording, Typeface, compose, select};

use super::*;

fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .unwrap()
}

fn names(directory: &Path) -> io::Result<Vec<String>> {
    let mut names = fs::read_dir(directory)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}

#[test]
fn a_week_old_show_expires_unless_pinned() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let parent = root.path();
    let now = SystemTime::now();
    let days_ago = |days: u64| utc_stamp(now - Duration::from_hours(days * 24));

    let written = || -> anyhow::Result<String> {
        let show = fresh_directory(parent, now - Duration::from_hours(8 * 24))?;
        fs::create_dir(&show)?;
        Ok(show
            .file_name()
            .context("A Show has a name")?
            .to_string_lossy()
            .into_owned())
    };
    written()?;
    written()?;
    let pinned = written()?;
    for name in [days_ago(6), "notes".into()] {
        fs::create_dir(parent.join(name))?;
    }
    fs::write(parent.join(&pinned).join(PIN), b"")?;
    fs::write(parent.join(days_ago(9)), b"not a Show")?;

    sweep(parent, now)?;
    let mut kept = vec![days_ago(9), days_ago(6), pinned, "notes".into()];
    kept.sort();
    assert_eq!(names(parent)?, kept);

    sweep(&parent.join("missing"), now)
}

#[test]
fn only_names_speakeasy_writes_are_stamps() {
    assert_eq!(stamp("20261019T143603Z"), Some("20261019T143603Z"));
    assert_eq!(stamp("20261019T143603Z-12"), Some("20261019T143603Z"));
    for name in [
        "20261019T143603",
        "20261019T143603Z-",
        "20261019T143603Z-a",
        "20261019T143603Zcopy",
        "2026-10-19T14360Z",
        "notes",
    ] {
        assert_eq!(stamp(name), None, "{name}");
    }
}

#[test]
fn stamps_name_utc_moments_in_order() {
    assert_eq!(utc_stamp(at(0)), "19700101T000000Z");
    assert_eq!(utc_stamp(at(951_782_400)), "20000229T000000Z");
    assert_eq!(utc_stamp(at(1_792_420_563)), "20261019T143603Z");
    assert_eq!(utc_stamp(at(4_102_444_799)), "20991231T235959Z");
}

fn bundle() -> anyhow::Result<Bundle> {
    let mut journal = Journal::default();
    let frame = Image::from_bgra(32, 20, vec![255; 2_560])?;
    journal
        .admit()
        .context("An empty journal admits a frame")?
        .record(Moment::START, frame);
    let recording = Recording::default();
    let selection = select(&recording, journal.samples()).context("A frame to choose")?;
    let typeface = Typeface::parse(crate::BRAND_FONT_FILE)?;
    compose(
        recording,
        selection,
        |at| journal.frame_at(at).context("A recorded frame"),
        &typeface,
    )
}

#[test]
fn a_bundle_appears_whole_under_a_fresh_name() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let parent = root.path().join("shows");
    let first = fresh_directory(&parent, at(1_792_420_563))?;
    let (saved, summary) = stage(managed(&first, None)?, bundle()?, None)?.persist()?;
    assert_eq!(
        saved.folder()?,
        Some(first.as_path()),
        "The saved Show is the one persisted"
    );
    assert_eq!(
        fs::read_to_string(first.join("summary.md"))?,
        summary,
        "The summary returned is the one written"
    );
    assert!(summary.contains(&first.display().to_string()));
    assert!(first.join("frames/frame-01.png").is_file());
    assert!(
        stage(managed(&first, None)?, bundle()?, None)?
            .persist()
            .is_err(),
        "An existing bundle is never replaced"
    );
    let second = fresh_directory(&parent, at(1_792_420_563))?;
    assert_eq!(second.file_name(), Some("20261019T143603Z-2".as_ref()));
    drop(stage(managed(&second, None)?, bundle()?, None)?);
    assert_eq!(
        names(&parent)?,
        ["20261019T143603Z"],
        "a bundle dropped before it persists, as a Show cancelled at commit drops it, leaves \
         nothing behind"
    );
    Ok(())
}

#[test]
fn a_saved_show_reads_back_as_its_folder_holds_it_until_it_is_gone() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let parent = root.path().join("shows");
    let folder = fresh_directory(&parent, at(1_792_420_563))?;
    let (show, summary) = stage(managed(&folder, None)?, bundle()?, None)?.persist()?;
    let (newer, _) = stage(
        managed(&fresh_directory(&parent, at(1_792_420_564))?, None)?,
        bundle()?,
        None,
    )?
    .persist()?;
    let file = folder.join(Bundle::SUMMARY);
    assert_eq!(show.folder()?, Some(folder.as_path()));
    assert_eq!(show.summary()?, Some(summary.clone()));

    let edited = format!("{summary}\nThe agent should start with the save button.\n");
    fs::write(&file, &edited)?;
    assert_eq!(show.summary()?, Some(edited), "The user's edit was lost");

    fs::remove_dir_all(&folder)?;
    assert_eq!(show.folder()?, None, "A deleted Show's folder was found");
    assert_eq!(show.summary()?, None, "A deleted Show's summary was read");
    assert!(newer.summary()?.is_some(), "Only the deleted Show has gone");
    Ok(())
}

#[test]
fn a_show_that_cannot_be_saved_names_the_shows_folder_and_a_remedy() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let blocked = root.path().join("shows");
    fs::write(&blocked, b"a file where the folder belongs")?;
    let message = stage(
        managed(&blocked.join("20261019T143603Z"), None)?,
        bundle()?,
        None,
    )
    .err()
    .context("A Show was staged beneath a file")?
    .to_string();
    assert!(
        message.starts_with(&format!("Cannot save the Show in {}: ", blocked.display()))
            && message.ends_with(", then try again."),
        "{message}"
    );
    Ok(())
}
