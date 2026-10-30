//! Interrupted Shows use private synthetic logs; locking, torn tails, expiry, and discard are real.

use speakeasy_show::{Image, Moment, TakeEvent};

use super::*;

fn surface() -> Size {
    Size {
        width: 800.0,
        height: 600.0,
    }
}

async fn capture(parent: &Path, at: SystemTime) -> anyhow::Result<Checkpoint> {
    let (checkpoint, mut writer) = Checkpoint::begin(parent, at, 1_000, surface())?;
    let mut take = Take::new(1_000, surface());
    let event = take.arrive(Moment::from_millis(100), || {
        Some(Image::from_bgra(2, 2, [3, 2, 1, 255].repeat(4)).unwrap())
    })?;
    assert!(writer.send(event));
    assert!(writer.send(TakeEvent::End {
        at: Moment::from_millis(500),
        ending: speakeasy_show::Ending::Finished
    }));
    assert!(writer.close().await);
    Ok(checkpoint)
}

#[tokio::test]
async fn only_unlocked_interrupted_shows_are_listed_and_discard_cannot_recover()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let checkpoint = capture(root.path(), now).await?;
    assert_eq!(list(root.path(), now).shows, []);
    let path = checkpoint.directory.clone();
    assert!(checkpoint.replay()?.ended());
    drop(checkpoint);
    let rows = list(root.path(), now).shows;
    assert_eq!(rows.len(), 1);
    let held = rows[0].lock()?;
    assert!(rows[0].lock().is_err());
    held.discard()?;
    assert!(!path.exists());
    assert_eq!(list(root.path(), now).shows, []);
    Ok(())
}

#[tokio::test]
async fn a_torn_end_recovers_a_prefix_and_expiry_uses_the_original_stamp() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let checkpoint = capture(root.path(), now).await?;
    let file = OpenOptions::new()
        .write(true)
        .open(checkpoint.directory.join(LOG))?;
    file.set_len(file.metadata()?.len().saturating_sub(1))?;
    let take = checkpoint.replay()?;
    assert!(!take.ended());
    assert_eq!(take.finish().0.duration.as_millis(), 100);
    let path = checkpoint.directory.clone();
    drop(file);
    drop(checkpoint);
    let later = now + std::time::Duration::from_hours(192);
    assert_eq!(list(root.path(), later).shows, []);
    assert!(path.exists(), "Listing must not perform deletion");
    sweep(root.path(), later)?;
    assert!(!path.exists());
    Ok(())
}

#[tokio::test]
async fn a_published_marker_cannot_delete_evidence_for_an_unrelated_folder() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let checkpoint = capture(root.path(), now).await?;
    let path = checkpoint.directory.clone();
    let destination = super::super::fresh_directory(root.path(), now)?;
    fs::create_dir(&destination)?;
    fs::write(
        path.join(PUBLISHED),
        destination.file_name().unwrap().as_encoded_bytes(),
    )?;
    drop(checkpoint);
    sweep(root.path(), now)?;
    assert_eq!(list(root.path(), now).shows.len(), 1);
    fs::write(
        destination.join(PROVENANCE),
        path.file_name().unwrap().as_encoded_bytes(),
    )?;
    sweep(root.path(), now)?;
    assert!(!path.exists());
    assert!(destination.exists());
    Ok(())
}

#[tokio::test]
async fn discard_ids_never_alias_saved_shows_and_stale_selection_cannot_recover()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let checkpoint = capture(root.path(), now).await?;
    let path = checkpoint.directory.clone();
    drop(checkpoint);
    let interrupted = list(root.path(), now).shows.remove(0);
    assert!(resolve(root.path(), Path::new(&interrupted.id())).is_ok());
    assert!(resolve(root.path(), &path).is_ok());
    assert!(resolve(root.path(), Path::new(path.file_name().unwrap())).is_err());
    fs::write(path.join(DISCARDED), b"")?;
    assert!(interrupted.lock().is_err());
    assert!(resolve(root.path(), &path).is_err());
    sweep(root.path(), now)?;
    assert!(!path.exists());
    Ok(())
}

#[tokio::test]
async fn malformed_checkpoints_do_not_block_sibling_discovery_or_saved_show_expiry()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let checkpoint = capture(root.path(), now).await?;
    let bad = checkpoint.directory.clone();
    drop(checkpoint);
    fs::write(bad.join(PUBLISHED), b"invalid marker")?;
    drop(capture(root.path(), now).await?);
    let listing = list(root.path(), now);
    assert_eq!(listing.shows.len(), 1);
    assert_eq!(listing.problems.len(), 1);
    let expired =
        super::super::fresh_directory(root.path(), now - std::time::Duration::from_hours(192))?;
    fs::create_dir(&expired)?;
    assert!(super::super::sweep(root.path(), now).is_err());
    assert!(
        !expired.exists(),
        "A damaged checkpoint blocked expiry of an unrelated saved Show"
    );
    assert!(bad.exists(), "Uninspectable evidence must not be deleted");
    Ok(())
}

#[tokio::test]
async fn failed_log_deletion_preserves_the_lock_and_discard_marker_for_retry() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let checkpoint = capture(root.path(), now).await?;
    let path = checkpoint.directory.clone();
    let held = root.path().join("held-log");
    fs::rename(path.join(LOG), &held)?;
    // A non-file at the log path makes deletion fail on every host, without changing permissions.
    fs::create_dir(path.join(LOG))?;
    assert!(checkpoint.discard().is_err());
    assert!(path.join(LOCK).is_file());
    assert!(path.join(DISCARDED).is_file());
    assert_eq!(list(root.path(), now).shows, []);
    fs::remove_dir(path.join(LOG))?;
    fs::rename(held, path.join(LOG))?;
    sweep(root.path(), now)?;
    assert!(!path.exists());
    Ok(())
}

#[cfg(target_os = "windows")]
#[tokio::test]
async fn a_windows_reader_that_denies_delete_sharing_cannot_orphan_private_frames()
-> anyhow::Result<()> {
    use std::os::windows::fs::OpenOptionsExt as _;

    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let checkpoint = capture(root.path(), now).await?;
    let path = checkpoint.directory.clone();
    let reader = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path.join(LOG))?;
    assert!(checkpoint.discard().is_err());
    assert!(path.join(LOCK).is_file());
    assert!(path.join(DISCARDED).is_file());
    drop(reader);
    sweep(root.path(), now)?;
    assert!(!path.exists());
    Ok(())
}
