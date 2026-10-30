//! Sidecar trust boundaries and explicit exports use synthetic frames and temporary directories.

use speakeasy_show::Image;

use super::*;

#[test]
fn optional_source_roundtrips_without_modifying_earlier_exports() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    assert_eq!(size(root.path())?, None);
    let mut journal = Journal::new();
    for (at, color) in [(200, 30), (900, 90)] {
        journal.admit().unwrap().record(
            Moment::from_millis(at),
            Image::from_bgra(70, 65, [color, color, color, 255].repeat(70 * 65))?,
        );
    }
    write(root.path(), &journal, Moment::from_millis(1_500))?;
    let index = inspect(root.path())?;
    assert_eq!(longest_gap(&index), 700);
    let served = frame(root.path(), &index, Moment::from_millis(800))?;
    assert_eq!(served.at.as_millis(), 200);
    assert_eq!(served.image, journal.frame_at(served.at).unwrap());
    let destination = root.path().join("frame.png");
    let png = served.image.encode_png()?;
    export(&destination, &png)?;
    assert!(export(&destination, b"replacement").is_err());
    assert_eq!(fs::read(destination)?, png);
    fs::write(root.path().join(FILE), b"broken")?;
    assert!(frame(root.path(), &index, Moment::START).is_err());
    assert!(inspect(root.path()).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn linked_sources_and_destinations_never_cross_the_bundle_boundary() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir()?;
    let other = tempfile::NamedTempFile::new()?;
    fs::write(other.path(), b"PRIVATE_PIXELS")?;
    symlink(other.path(), root.path().join(FILE))?;
    assert!(size(root.path()).is_err());
    assert!(inspect(root.path()).is_err());
    assert!(export(&root.path().join(FILE), b"replacement").is_err());
    assert_eq!(fs::read(other.path())?, b"PRIVATE_PIXELS");
    Ok(())
}
