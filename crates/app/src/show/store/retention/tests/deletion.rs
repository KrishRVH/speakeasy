use super::*;

#[test]
fn deleting_a_kept_damaged_show_removes_its_files_and_preserves_other_shows() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let directory = show(root.path(), now)?;
    let other = show(root.path(), now - Duration::from_secs(1))?;
    fs::write(directory.join(store::PIN), b"")?;
    fs::write(directory.join("voice.log"), b"synthetic audio")?;
    let deleted = delete(root.path(), &directory)?;
    assert!(deleted.cleanup.is_none());
    assert!(!directory.exists());
    assert!(
        !root
            .path()
            .join(format!(
                "{}{}",
                store::EXPIRED_PREFIX,
                directory.file_name().unwrap().to_string_lossy()
            ))
            .exists()
    );
    assert!(other.join("summary.md").is_file());
    assert!(delete(root.path(), &directory).is_err());
    assert!(!directory.exists());
    Ok(())
}

#[test]
fn deletion_waits_for_the_retention_owner_and_refuses_unmanaged_paths() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let other = tempfile::tempdir()?;
    let directory = show(root.path(), SystemTime::now())?;
    let moved = show(other.path(), SystemTime::now())?;
    assert!(delete(root.path(), &moved).is_err());
    assert!(moved.is_dir());
    let held = lock(root.path())?;
    let parent = root.path().to_owned();
    let target = directory.clone();
    let (done, finished) = mpsc::channel();
    let (started, starting) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        started.send(()).unwrap();
        done.send(delete(&parent, &target)).unwrap();
    });
    starting.recv_timeout(Duration::from_secs(5))?;
    let early = finished.recv_timeout(Duration::from_millis(100));
    assert!(matches!(early, Err(mpsc::RecvTimeoutError::Timeout)));
    assert!(directory.is_dir());
    drop(held);
    worker.join().unwrap();
    assert!(
        finished
            .recv_timeout(Duration::from_secs(5))??
            .cleanup
            .is_none()
    );
    assert!(!directory.exists());
    Ok(())
}

#[test]
fn deletion_never_replaces_an_existing_cleanup_directory() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = show(root.path(), SystemTime::now())?;
    let retired = root.path().join(format!(
        "{}{}",
        store::EXPIRED_PREFIX,
        directory.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir(&retired)?;
    assert!(delete(root.path(), &directory).is_err());
    assert!(directory.is_dir() && retired.is_dir());
    fs::write(retired.join("summary.md"), b"synthetic earlier Show")?;
    assert!(delete(root.path(), &directory).is_err());
    assert!(directory.is_dir());
    assert_eq!(
        fs::read(retired.join("summary.md"))?,
        b"synthetic earlier Show"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn deletion_refuses_linked_bundles_and_never_follows_links_inside_one() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir()?;
    let other = tempfile::tempdir()?;
    let directory = show(root.path(), SystemTime::now())?;
    let target = show(other.path(), SystemTime::now())?;
    symlink(&target, directory.join("external"))?;
    let linked = root.path().join(format!(
        "{}-2",
        directory.file_name().unwrap().to_string_lossy()
    ));
    symlink(&target, &linked)?;
    assert!(delete(root.path(), &linked).is_err());
    assert!(delete(root.path(), &directory)?.cleanup.is_none());
    assert!(target.join("summary.md").is_file());
    Ok(())
}

#[cfg(unix)]
#[test]
fn incomplete_deletion_is_hidden_and_cleanup_retries_it() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let directory = show(root.path(), now)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o555))?;
    let deleted = delete(root.path(), &directory)?;
    let retired = root.path().join(format!(
        "{}{}",
        store::EXPIRED_PREFIX,
        directory.file_name().unwrap().to_string_lossy()
    ));
    fs::set_permissions(&retired, fs::Permissions::from_mode(0o700))?;
    assert!(deleted.cleanup.is_some());
    assert!(!directory.exists());
    assert_eq!(recall::list(root.path())?.count(), 0);
    store::sweep(root.path(), now)?;
    assert!(!retired.exists());
    Ok(())
}
