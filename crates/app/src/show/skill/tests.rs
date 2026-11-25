use std::fs;

use super::*;

#[test]
fn installation_replaces_any_copy_already_there() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let record = root.path().join("record.json");
    let agent = root.path().join("skills");
    let path = install_recorded(&record, &agent)?;
    assert_eq!(fs::read_to_string(&path)?, SKILL);
    fs::write(&path, "An earlier or edited guide.\n")?;
    assert_eq!(install_recorded(&record, &agent)?, path);
    assert_eq!(
        fs::read_to_string(&path)?,
        SKILL,
        "An installed copy kept its old text"
    );
    Ok(())
}

#[test]
fn an_update_rewrites_each_installed_copy_once_and_leaves_removed_ones_removed()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let record = root.path().join("record.json");
    let kept = install_recorded(&record, &root.path().join("claude"))?;
    let removed = install_recorded(&record, &root.path().join("codex"))?;
    fs::write(&kept, "The previous version's guide.\n")?;
    fs::remove_dir_all(removed.parent().context("A copy names no folder")?)?;

    follow_update_recorded(&record, "9.9.9")?;
    assert_eq!(
        fs::read_to_string(&kept)?,
        SKILL,
        "An update left an installed copy on the old guide"
    );
    assert!(!removed.exists(), "An update reinstalled a removed copy");

    fs::write(&kept, "Edited after the update.\n")?;
    follow_update_recorded(&record, "9.9.9")?;
    assert_eq!(
        fs::read_to_string(&kept)?,
        "Edited after the update.\n",
        "A launch without an update rewrote an agent's copy"
    );
    Ok(())
}

#[test]
fn a_copy_that_cant_be_rewritten_is_reported_without_holding_back_the_rest() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let record = root.path().join("record.json");
    let stuck = install_recorded(&record, &root.path().join("stuck"))?;
    let kept = install_recorded(&record, &root.path().join("kept"))?;
    // A folder where the copy was can be neither read nor replaced.
    fs::remove_file(&stuck)?;
    fs::create_dir(&stuck)?;
    fs::write(&kept, "The previous version's guide.\n")?;

    let error = follow_update_recorded(&record, "9.9.9")
        .err()
        .context("A copy Speakeasy couldn't rewrite went unreported")?;
    assert!(error.to_string().contains(&stuck.display().to_string()));
    assert_eq!(
        fs::read_to_string(&kept)?,
        SKILL,
        "A copy Speakeasy couldn't rewrite held back the others"
    );
    Ok(())
}
