use std::os::unix::fs::PermissionsExt as _;

use super::*;

#[test]
fn a_show_folder_that_cannot_be_opened_is_a_damaged_row_beside_intact_rows() -> anyhow::Result<()> {
    let root = Collection::Demo.load()?;
    let (rows, _) = list(&root.directory)?;
    let sealed = &rows[1].directory;
    let access = fs::metadata(sealed)?.permissions();
    fs::set_permissions(sealed, fs::Permissions::from_mode(0o000))?;
    let unrestricted = fs::read_dir(sealed).is_ok();
    let listed = list(&root.directory);
    fs::set_permissions(sealed, access)?;
    if unrestricted {
        return Ok(());
    }
    let (listed, _) = listed?;
    let summary = |rows: &[Row]| -> Vec<_> {
        rows.iter()
            .map(|row| (row.directory.clone(), row.health, row.frames, row.pinned))
            .collect()
    };
    let mut expected = summary(&rows);
    expected[1] = (sealed.clone(), Health::Damaged, 0, None);
    assert_eq!(summary(&listed), expected);
    Ok(())
}
