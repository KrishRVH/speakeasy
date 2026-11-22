use super::*;
use crate::show::store::{self, address};

fn show(parent: &Path, at: SystemTime) -> io::Result<PathBuf> {
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
fn keep_and_delete_touch_only_shows_in_the_managed_folder() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let other = tempfile::tempdir()?;
    let now = SystemTime::now();
    let directory = show(root.path(), now)?;
    let moved = show(other.path(), now)?;
    assert!(address::managed_id(root.path(), &directory).is_some());
    assert!(address::managed_id(root.path(), &moved).is_none());
    assert!(set_kept(root.path(), &moved, true).is_err());
    assert!(delete(root.path(), &moved).is_err());
    let notes = root.path().join("notes");
    fs::create_dir(&notes)?;
    assert!(set_kept(root.path(), &notes, true).is_err());
    let absent = root.path().join("absent");
    assert!(set_kept(&absent, &directory, true).is_err());
    assert!(!absent.exists());
    assert!(moved.is_dir() && !notes.join(store::PIN).exists() && !moved.join(store::PIN).exists());
    Ok(())
}

#[test]
fn deleting_a_kept_show_removes_its_files_and_preserves_other_shows() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let directory = show(root.path(), now)?;
    let other = show(root.path(), now - Duration::from_secs(1))?;
    fs::write(directory.join(store::PIN), b"")?;
    fs::write(directory.join("voice.log"), b"synthetic audio")?;
    assert!(delete(root.path(), &directory)?.cleanup.is_none());
    assert!(!directory.exists());
    assert!(other.join("summary.md").is_file());
    assert!(delete(root.path(), &directory).is_err());
    Ok(())
}

#[cfg(target_os = "windows")]
#[test]
fn an_open_windows_file_reports_blocked_cleanup_and_the_next_sweep_finishes() -> anyhow::Result<()>
{
    use std::os::windows::fs::OpenOptionsExt as _;
    let root = tempfile::tempdir()?;
    let now = SystemTime::now();
    let directory = show(root.path(), now - Duration::from_hours(8 * 24))?;
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(directory.join("summary.md"))?;
    let result = store::sweep(root.path(), now);
    drop(held);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .starts_with("Cannot delete the expired Show")
    );
    store::sweep(root.path(), now)?;
    assert!(!directory.exists());
    Ok(())
}
