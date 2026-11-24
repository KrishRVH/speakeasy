use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read as _,
};

use serde_json::json;
use speakeasy_show::{Bundle, Image, Journal, Moment};
use zip::ZipArchive;

use super::{super::recall, *};

fn fixture(parent: &Path) -> anyhow::Result<PathBuf> {
    let directory = parent.join("original");
    fs::create_dir_all(directory.join("frames"))?;
    let manifest = serde_json::to_vec(&json!({
        "version": 3, "duration": 1000, "surface": {"width": 70, "height": 65},
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
    let mut journal = Journal::default();
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
    for name in ["voice.log", ".pinned", "unrelated.txt"] {
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
    assert!(recall::Entry::open(&extracted, None)?.show.is_ok());
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
    let unplaced = write(&directory, &root.path().join("missing/export.zip"))
        .err()
        .context("An export was written into a missing folder")?;
    assert_eq!(
        unplaced.to_string(),
        "Can't open the export folder; choose an existing folder"
    );
    assert!(!root.path().join("missing").exists());
    Ok(())
}
