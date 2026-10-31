//! Export trust and ownership checks use synthetic pixels, private text sentinels, and temp files.

use std::{collections::BTreeSet, fs};

use serde_json::json;
use speakeasy_show::{Image, Journal, Moment};
use zip::ZipArchive;

use super::*;

fn fixture(parent: &Path) -> anyhow::Result<PathBuf> {
    let directory = parent.join("original");
    fs::create_dir_all(directory.join("frames"))?;
    let manifest = serde_json::to_vec(&json!({
        "version": 2, "duration": 1000, "surface": {"width": 70, "height": 65},
        "strokes": [], "pins": [], "actions": [], "narration": {"text": "PRIVATE_NARRATION"},
        "marks": [], "keyframes": [{"index": 1, "at": 500, "reason": "middle", "event": "PRIVATE_EVENT", "marks": [0, 0]}]
    }))?;
    let image = Image::from_bgra(70, 65, [30, 30, 30, 255].repeat(70 * 65))?;
    fs::write(directory.join(Manifest::FILE), &manifest)?;
    fs::write(
        directory.join(Bundle::SUMMARY),
        "User edit: PRIVATE_SUMMARY\n旧 /old/machine/frame.png\n",
    )?;
    for name in Manifest::parse(&manifest)?
        .files()
        .filter(|name| name != Manifest::FILE && name != Bundle::SUMMARY)
    {
        fs::write(directory.join(name), image.encode_png()?)?;
    }
    let mut journal = Journal::new();
    journal
        .admit()
        .unwrap()
        .record(Moment::from_millis(200), image);
    source::write(&directory, &journal, Moment::from_millis(1000))?;
    Ok(directory)
}

#[test]
fn archive_moves_with_edited_summary_images_and_retained_samples_but_no_extra_files()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path())?;
    for name in ["audio.wav", ".pinned", ".recovery", "unrelated.txt"] {
        fs::write(directory.join(name), b"PRIVATE_NOT_EXPORTED")?;
    }
    let destination = root.path().join("share.zip");
    let exported = write(&directory, &destination)?;
    assert!(exported.retained_frames);
    assert_eq!(exported.bytes, fs::metadata(&destination)?.len());
    let mut archive = ZipArchive::new(File::open(&destination)?)?;
    let names: BTreeSet<_> = archive
        .file_names()
        .map(|name| name.map(std::borrow::Cow::into_owned))
        .collect::<zip::result::ZipResult<_>>()?;
    assert_eq!(
        names,
        [
            "README.md",
            "show.json",
            "summary.md",
            "contact-sheet.png",
            "frames/frame-01.png",
            "source.frames"
        ]
        .map(str::to_owned)
        .into_iter()
        .collect()
    );
    assert_eq!(exported.files, names.len());
    let extracted = root.path().join("another machine");
    fs::create_dir(&extracted)?;
    for name in names {
        let mut bytes = Vec::new();
        archive.by_name(&name)?.read_to_end(&mut bytes)?;
        let path = extracted.join(&name);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, &bytes)?;
        if name != GUIDE {
            assert_eq!(bytes, fs::read(directory.join(&name))?);
        }
    }
    fs::remove_dir_all(&directory)?;
    assert_eq!(
        recall::Entry::open(&extracted)?.health(),
        recall::Health::Ok
    );
    let retained = source::inspect(&extracted)?;
    assert_eq!(
        source::frame(&extracted, &retained, Moment::from_millis(500))?
            .at
            .as_millis(),
        200
    );
    let guide = fs::read_to_string(extracted.join(GUIDE))?;
    assert!(guide.contains("[Frame 1 at 500 ms](frames/frame-01.png)"));
    assert!(guide.contains("speakeasy show frames . --json"));
    assert!(
        !guide.contains("PRIVATE_") && !guide.contains(&directory.to_string_lossy().to_string())
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(destination)?.permissions().mode() & 0o777,
            0o600
        );
    }
    Ok(())
}

#[test]
fn movie_exports_preserve_video_timing_and_relative_playback_links() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path())?;
    let checkpoint = tempfile::tempdir()?;
    movie::tests::checkpoint(checkpoint.path(), true)?;
    movie::retain(
        &directory,
        movie::open(checkpoint.path())?.unwrap(),
        Moment::from_millis(1000),
    )?;
    let destination = root.path().join("video.zip");
    write(&directory, &destination)?;
    let mut archive = ZipArchive::new(File::open(destination)?)?;
    for name in [movie::FILE, movie::HEADER, Bundle::SUMMARY] {
        let mut bytes = Vec::new();
        archive.by_name(name)?.read_to_end(&mut bytes)?;
        assert_eq!(bytes, fs::read(directory.join(name))?);
    }
    let mut guide = String::new();
    archive.by_name(GUIDE)?.read_to_string(&mut guide)?;
    assert!(guide.contains("[recording.mp4](recording.mp4)"));
    assert!(guide.contains("250–1250 ms") && guide.contains("through 1000 ms"));
    assert!(!guide.contains(&directory.to_string_lossy().to_string()));
    fs::remove_file(directory.join(movie::FILE))?;
    let failed = root.path().join("missing-video.zip");
    assert!(write(&directory, &failed).is_err());
    assert!(!failed.exists());
    Ok(())
}

#[test]
fn failed_or_repeated_exports_leave_destinations_and_originals_unchanged() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path())?;
    let destination = root.path().join("export.zip");
    fs::write(&destination, b"existing export")?;
    assert!(write(&directory, &destination).is_err());
    assert_eq!(fs::read(&destination)?, b"existing export");
    fs::remove_file(&destination)?;
    fs::remove_file(directory.join("frames/frame-01.png"))?;
    assert!(write(&directory, &destination).is_err());
    assert!(!destination.exists());
    assert_eq!(
        fs::read_dir(root.path())?.count(),
        1,
        "Failed export left staging files"
    );
    assert!(write(&directory, &directory.join("nested.zip")).is_err());
    assert!(!directory.join("nested.zip").exists());
    Ok(())
}

#[test]
fn source_damage_and_oversized_images_fail_without_publishing_partial_archives()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path())?;
    let destination = root.path().join("export.zip");
    fs::write(directory.join(source::FILE), b"damaged retained frames")?;
    assert!(write(&directory, &destination).is_err());
    assert!(!destination.exists());
    fs::remove_file(directory.join(source::FILE))?;
    let mut other = Journal::new();
    other.admit().unwrap().record(
        Moment::from_millis(200),
        Image::from_bgra(2, 2, vec![20; 16])?,
    );
    source::write(&directory, &other, Moment::from_millis(1500))?;
    assert!(
        write(&directory, &destination).is_err(),
        "Export accepted retained frames whose duration disagrees with the Show"
    );
    assert!(!destination.exists());
    fs::remove_file(directory.join(source::FILE))?;
    File::options()
        .write(true)
        .open(directory.join("contact-sheet.png"))?
        .set_len(recall::IMAGE_LIMIT + 1)?;
    assert!(write(&directory, &destination).is_err());
    assert_eq!(fs::read_dir(root.path())?.count(), 1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn linked_inventory_and_destinations_are_refused_even_when_targets_exist() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path())?;
    let outside = root.path().join("private.txt");
    fs::write(&outside, b"PRIVATE_OUTSIDE_SHOW")?;
    fs::remove_file(directory.join("frames/frame-01.png"))?;
    symlink(&outside, directory.join("frames/frame-01.png"))?;
    let destination = root.path().join("export.zip");
    assert!(write(&directory, &destination).is_err());
    assert!(!destination.exists());
    symlink(&outside, &destination)?;
    assert!(write(&directory, &destination).is_err());
    assert_eq!(fs::read(outside)?, b"PRIVATE_OUTSIDE_SHOW");
    Ok(())
}
