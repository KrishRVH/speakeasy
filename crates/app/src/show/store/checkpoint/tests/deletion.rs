use super::*;
use crate::show::store::{self, checkpoint::audio as recorded, retention};

fn saved(parent: &Path, checkpoint: &Checkpoint) -> anyhow::Result<PathBuf> {
    let directory = store::fresh_directory(parent, SystemTime::now())?;
    fs::create_dir(&directory)?;
    fs::write(directory.join(PROVENANCE), checkpoint.id()?)?;
    fs::write(
        checkpoint.directory.join(PUBLISHED),
        directory.file_name().unwrap().to_string_lossy().as_bytes(),
    )?;
    fs::write(
        checkpoint.directory.join(recorded::FILE),
        b"synthetic audio",
    )?;
    Ok(directory)
}

#[tokio::test]
async fn deleting_a_saved_show_discards_its_pending_audio_without_resurrecting_recovery()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = capture(root.path(), SystemTime::now()).await?;
    let directory = saved(root.path(), &checkpoint)?;
    let path = checkpoint.directory.clone();
    assert!(retention::delete(root.path(), &directory).is_err());
    assert!(directory.is_dir() && path.join(recorded::FILE).is_file());
    drop(checkpoint);
    let deleted = retention::delete(root.path(), &directory)?;
    assert_eq!(deleted.checkpoint.as_deref(), Some(path.as_path()));
    assert!(deleted.cleanup.is_none());
    assert!(!directory.exists() && !path.exists());
    assert_eq!(list(root.path(), SystemTime::now()).shows, []);
    Ok(())
}

#[tokio::test]
async fn a_copied_or_forged_provenance_marker_cannot_discard_another_shows_checkpoint()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let checkpoint = capture(root.path(), SystemTime::now()).await?;
    let original = saved(root.path(), &checkpoint)?;
    let copy = store::fresh_directory(root.path(), SystemTime::now())?;
    fs::create_dir(&copy)?;
    fs::write(copy.join(PROVENANCE), checkpoint.id()?)?;
    let path = checkpoint.directory.clone();
    drop(checkpoint);
    assert!(retention::delete(root.path(), &copy)?.checkpoint.is_none());
    assert!(original.is_dir() && path.join(recorded::FILE).is_file());
    assert_eq!(list(root.path(), SystemTime::now()).shows.len(), 1);
    Ok(())
}

#[tokio::test]
async fn damaged_recovery_links_refuse_deletion_without_removing_private_audio()
-> anyhow::Result<()> {
    for missing in [LOCK, PUBLISHED] {
        let root = tempfile::tempdir()?;
        let checkpoint = capture(root.path(), SystemTime::now()).await?;
        let directory = saved(root.path(), &checkpoint)?;
        let path = checkpoint.directory.clone();
        drop(checkpoint);
        fs::remove_file(path.join(missing))?;
        assert!(retention::delete(root.path(), &directory).is_err());
        assert!(directory.is_dir() && path.join(recorded::FILE).is_file());
    }
    let root = tempfile::tempdir()?;
    let directory = root.path().join(store::utc_stamp(SystemTime::now()));
    fs::create_dir(&directory)?;
    fs::write(directory.join(PROVENANCE), b"../unowned")?;
    assert!(retention::delete(root.path(), &directory).is_err());
    assert!(directory.is_dir());
    Ok(())
}
