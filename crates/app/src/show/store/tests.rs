//! Naming, writing, reading back, and expiring Shows in temporary folders.

use speakeasy_show::{Image, Journal, Moment, Recording, Typeface, compose, select};

use super::{recall::SUMMARY_LIMIT, *};

fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .unwrap()
}

/// The names in `directory`, sorted.
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
    // Three Shows the writer named in one second. The bare `<stamp>` and `<stamp>-2` expire, so
    // the sweep must read both name forms; the pinned `<stamp>-3` stays.
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
    let staging = format!("{STAGING_PREFIX}1");
    for name in [days_ago(6), "notes".into(), staging.clone()] {
        fs::create_dir(parent.join(name))?;
    }
    fs::write(parent.join(&pinned).join(PIN), b"")?;
    fs::write(parent.join(days_ago(9)), b"not a Show")?;
    #[cfg(unix)]
    let elsewhere = {
        let elsewhere = tempfile::tempdir()?;
        std::os::unix::fs::symlink(elsewhere.path(), parent.join(days_ago(10)))?;
        elsewhere
    };

    sweep(parent, now)?;
    let mut kept = vec![
        ".retention.lock".into(),
        staging.clone(),
        days_ago(9),
        days_ago(6),
        pinned,
        "notes".into(),
    ];
    #[cfg(unix)]
    {
        kept.push(days_ago(10));
        assert!(
            elsewhere.path().exists(),
            "What a link named like an expired Show points to is left alone"
        );
    }
    kept.sort();
    assert_eq!(names(parent)?, kept);

    sweep(parent, now + 2 * ABANDONED_AFTER)?;
    kept.retain(|name| *name != staging);
    assert_eq!(names(parent)?, kept);

    sweep(&parent.join("missing"), now)
}

#[cfg(unix)]
#[test]
fn a_show_that_cannot_be_inspected_is_kept_and_fails_the_sweep() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir()?;
    let parent = root.path();
    let now = SystemTime::now();
    let sealed = parent.join(utc_stamp(now - Duration::from_hours(8 * 24)));
    let expired = parent.join(utc_stamp(now - Duration::from_hours(9 * 24)));
    fs::create_dir(&sealed)?;
    fs::create_dir(&expired)?;
    // A folder that can be listed but not searched hides whether it holds a pin.
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o600))?;
    let probe = fs::symlink_metadata(sealed.join(PIN)).map_err(|error| error.kind());

    let swept = sweep(parent, now);
    fs::set_permissions(&sealed, fs::Permissions::from_mode(0o700))?;
    assert_eq!(probe.err(), Some(ErrorKind::PermissionDenied));
    let message = format!("{:#}", swept.err().context("The sweep reports the Show")?);
    assert!(message.contains("Cannot check every Show"), "{message}");
    assert!(sealed.exists(), "A Show that may be pinned is kept");
    assert!(!expired.exists(), "The rest of the folder still expires");
    Ok(())
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

/// A bundle of one blank frame.
fn bundle() -> anyhow::Result<Bundle> {
    let mut journal = Journal::new();
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
    let (saved, summary) = stage(&first, bundle()?)?.persist()?;
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
        stage(&first, bundle()?).is_err(),
        "An existing bundle is never replaced"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(fs::metadata(&first)?.permissions().mode() & 0o777, 0o700);
    }
    let second = fresh_directory(&parent, at(1_792_420_563))?;
    assert_eq!(second.file_name(), Some("20261019T143603Z-2".as_ref()));
    drop(stage(&second, bundle()?)?);
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
    let (show, summary) = stage(&folder, bundle()?)?.persist()?;
    let (newer, _) = stage(&fresh_directory(&parent, at(1_792_420_564))?, bundle()?)?.persist()?;
    let file = folder.join(Bundle::SUMMARY);
    assert_eq!(show.folder()?, Some(folder.as_path()));
    assert_eq!(show.summary()?, Some(summary.clone()));

    let edited = format!("{summary}\nThe agent should start with the save button.\n");
    fs::write(&file, &edited)?;
    assert_eq!(show.summary()?, Some(edited), "The user's edit was lost");

    let limit = usize::try_from(SUMMARY_LIMIT)?;
    fs::write(&file, "a".repeat(limit))?;
    assert_eq!(show.summary()?.map(|summary| summary.len()), Some(limit));
    fs::write(&file, "a".repeat(limit.saturating_add(1)))?;
    let message = show
        .summary()
        .err()
        .context("An oversized summary was read")?
        .to_string();
    assert!(message.contains("too large to copy"), "{message}");

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
    let message = stage(&blocked.join("20261019T143603Z"), bundle()?)
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

#[cfg(unix)]
#[test]
fn the_shows_folder_is_made_private_and_a_link_is_refused() -> anyhow::Result<()> {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let mode = |path: &Path| -> anyhow::Result<u32> {
        Ok(fs::metadata(path)?.permissions().mode() & 0o777)
    };
    let root = tempfile::tempdir()?;
    let created = root.path().join("speakeasy/shows");
    make_private(&created)?;
    assert_eq!(mode(&created)?, 0o700);

    let existing = root.path().join("existing");
    fs::create_dir(&existing)?;
    fs::set_permissions(&existing, fs::Permissions::from_mode(0o755))?;
    let link = root.path().join("link");
    symlink(&existing, &link)?;
    assert!(make_private(&link).is_err(), "A link is never followed");
    assert_eq!(mode(&existing)?, 0o755, "What a link names is left alone");
    make_private(&existing)?;
    assert_eq!(mode(&existing)?, 0o700, "An existing folder is tightened");
    Ok(())
}
