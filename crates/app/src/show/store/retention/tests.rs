//! Retention races use temporary files and threads, never the user's library or native devices.

use std::sync::mpsc;

use super::*;
use crate::show::store::{self, address, recall};

fn show(parent: &Path, at: SystemTime) -> io::Result<std::path::PathBuf> {
    let directory = parent.join(store::utc_stamp(at));
    fs::create_dir(&directory)?;
    fs::write(directory.join("summary.md"), "PRIVATE_EDIT")?;
    fs::write(directory.join("show.json"), "PRIVATE_MANIFEST")?;
    Ok(directory)
}

#[test]
fn keeping_preserves_edits_and_allowing_expiry_uses_the_original_date() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let directory = show(root.path(), now - Duration::from_hours(8 * 24))?;
    assert_eq!(set_kept(root.path(), &directory, true)?, Retention::Kept);
    assert_eq!(set_kept(root.path(), &directory, true)?, Retention::Kept);
    store::sweep(root.path(), now)?;
    assert_eq!(
        fs::read_to_string(directory.join("summary.md"))?,
        "PRIVATE_EDIT"
    );
    assert_eq!(
        fs::read_to_string(directory.join("show.json"))?,
        "PRIVATE_MANIFEST"
    );
    let retention = set_kept(root.path(), &directory, false)?;
    assert_eq!(retention.label(now), "Expires at the next cleanup");
    assert_eq!(set_kept(root.path(), &directory, false)?, retention);
    assert!(
        retention
            .expires_at()
            .is_some_and(|stamp| stamp.ends_with('Z'))
    );
    store::sweep(root.path(), now)?;
    assert!(!directory.exists());
    assert_eq!(
        set_kept(root.path(), &directory, true).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert!(!directory.exists(), "Keep recreated an expired Show");
    Ok(())
}

#[test]
fn cleanup_checks_the_pin_only_after_acquiring_the_retention_lock() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let directory = show(root.path(), now - Duration::from_hours(8 * 24))?;
    let held = lock(root.path())?;
    let parent = root.path().to_owned();
    let (started, starting) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        started.send(()).unwrap();
        done.send(store::sweep(&parent, now)).unwrap();
    });
    starting.recv_timeout(Duration::from_secs(5))?;
    let early = finished.recv_timeout(Duration::from_millis(100));
    let pin = fs::write(directory.join(store::PIN), b"");
    drop(held);
    worker.join().unwrap();
    pin?;
    assert!(
        matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
        "Cleanup ran while retention was locked"
    );
    finished.recv_timeout(Duration::from_secs(5))??;
    assert!(
        directory.exists(),
        "Cleanup used a pin check made before locking"
    );
    Ok(())
}

#[test]
fn interrupted_deletion_stays_hidden_and_cleanup_retries_it() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let directory = show(root.path(), now - Duration::from_hours(8 * 24))?;
    let entry = fs::read_dir(root.path())?.next().unwrap()?;
    let store::Cleanup::Remove(retired) =
        store::retire_expired(root.path(), &entry, &store::utc_stamp(now), now)?
    else {
        panic!("The expired Show was not retired");
    };
    assert!(!directory.exists());
    fs::remove_file(retired.join("summary.md"))?;
    assert_eq!(recall::list(root.path())?.count(), 0);
    store::sweep(root.path(), now)?;
    assert!(!retired.exists());
    Ok(())
}

#[test]
fn identity_and_mutation_do_not_follow_moved_missing_or_non_show_folders() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let other = tempfile::tempdir()?;
    let now = SystemTime::now();
    let directory = show(root.path(), now)?;
    let moved = show(other.path(), now)?;
    assert!(address::managed_id(root.path(), &directory)?.is_some());
    assert!(address::managed_id(root.path(), &moved)?.is_none());
    assert!(set_kept(root.path(), &moved, true).is_err());
    let notes = root.path().join("notes");
    fs::create_dir(&notes)?;
    assert!(set_kept(root.path(), &notes, true).is_err());
    let absent = root.path().join("absent");
    assert!(set_kept(&absent, &directory, true).is_err());
    assert!(!absent.exists());
    assert!(!notes.join(store::PIN).exists() && !moved.join(store::PIN).exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn linked_folders_are_refused_and_linked_pins_never_change_their_target() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir()?;
    let other = tempfile::tempdir()?;
    let now = SystemTime::now();
    let directory = show(root.path(), now)?;
    let linked = root.path().join(format!("{}-2", store::utc_stamp(now)));
    symlink(other.path(), &linked)?;
    assert!(set_kept(root.path(), &linked, true).is_err());
    let with_separator = std::path::PathBuf::from(format!("{}/", linked.display()));
    assert!(set_kept(root.path(), &with_separator, true).is_err());
    assert!(address::managed_id(root.path(), &with_separator)?.is_none());
    assert!(!other.path().join(store::PIN).exists());
    let target = other.path().join("private");
    fs::write(&target, b"PRIVATE_TARGET")?;
    symlink(&target, directory.join(store::PIN))?;
    assert_eq!(set_kept(root.path(), &directory, true)?, Retention::Kept);
    set_kept(root.path(), &directory, false)?;
    assert_eq!(fs::read(&target)?, b"PRIVATE_TARGET");
    assert!(!directory.join(store::PIN).exists());
    Ok(())
}

#[test]
fn exact_ids_never_fall_back_to_relative_paths() -> anyhow::Result<()> {
    let parent = Path::new("/managed");
    let id = "20261030T101010Z-2";
    assert_eq!(
        address::resolve(Path::new(id), || Ok(parent.into()))?,
        parent.join(id)
    );
    let explicit = format!("./{id}");
    assert_eq!(
        address::resolve(Path::new(&explicit), || panic!(
            "explicit path consulted storage"
        ))?,
        std::path::absolute(&explicit)?
    );
    for invalid in [
        "latest",
        "../20261030T101010Z",
        "20261030T101010Z/",
        "20261030T101010Z-a",
    ] {
        assert!(address::ShowId::parse(invalid).is_none());
    }
    Ok(())
}

#[cfg(target_os = "windows")]
#[test]
fn an_open_windows_file_reports_blocked_cleanup_without_damaging_the_show() -> anyhow::Result<()> {
    use std::os::windows::fs::OpenOptionsExt as _;
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let directory = show(root.path(), now - Duration::from_hours(8 * 24))?;
    let held = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(directory.join("summary.md"))?;
    let result = store::sweep(root.path(), now);
    drop(held);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .starts_with("Cannot delete every expired Show")
    );
    assert_eq!(
        fs::read_to_string(directory.join("summary.md"))?,
        "PRIVATE_EDIT"
    );
    assert_eq!(
        fs::read_to_string(directory.join("show.json"))?,
        "PRIVATE_MANIFEST"
    );
    store::sweep(root.path(), now)?;
    assert!(!directory.exists());
    Ok(())
}
