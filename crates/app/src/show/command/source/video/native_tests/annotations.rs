//! Synthetic native exports preserve annotation timing, crop order, and honest fallback metadata.

use super::*;

#[test]
#[ignore = "Opt in: native video decoder; set SPEAKEASY_MOVIE_GEOMETRY_DIRECTORY to synthetic geometry fixtures"]
fn native_annotated_video_exports_match_full_canvas_crops_and_refuse_uncovered_samples()
-> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let show = root.path().join("show");
    fs::create_dir(&show)?;
    movie::tests::native_geometry_fixture(&show)?;
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(show.join("show.json"))?)?;
    manifest["pins"] = serde_json::json!([{"at":750,"point":[0.5,0.5]}]);
    manifest["spaces"] = serde_json::json!({"strokes":[],"pins":[{"generation":1,"part":[0,0,0.5,1],"observed":750}]});
    fs::write(show.join("show.json"), serde_json::to_vec(&manifest)?)?;
    let run = |name: &str, options: &[&str]| -> anyhow::Result<serde_json::Value> {
        let args = [
            show.as_os_str().to_owned(),
            "--video".into(),
            "--annotated".into(),
            "--out".into(),
            root.path().join(name).into_os_string(),
            "--json".into(),
        ]
        .into_iter()
        .chain(options.iter().map(OsString::from));
        let mut report = Vec::new();
        Command::parse(true, args)?.write(&mut report)?;
        assert!(!String::from_utf8_lossy(&report).contains("PRIVATE"));
        Ok(serde_json::from_slice(&report)?)
    };
    let full = run("full.png", &["--mark", "1"])?;
    assert_eq!(
        full["video"]["rendering"],
        serde_json::json!({"state":"painted","drawn":1,"omitted":0})
    );
    let cropped = run("crop.png", &["--mark", "1", "--crop"])?;
    assert_eq!(
        cropped["cells"][0]["cropPx"],
        serde_json::json!({"left":144,"top":96,"width":32,"height":48})
    );
    let full = image::load_from_memory(&fs::read(root.path().join("full.png"))?)?.into_rgba8();
    let crop = image::load_from_memory(&fs::read(root.path().join("crop.png"))?)?.into_rgba8();
    assert_eq!(
        crop,
        image::imageops::crop_imm(&full, 144, 96, 32, 48).to_image()
    );
    let raw = movie::Reading::open(&show, Moment::from_millis(1000))?
        .frame(Moment::from_millis(750), None, None, || false)?
        .unwrap();
    let raw_png = raw.image.encode_png()?;
    assert_ne!(raw_png, fs::read(root.path().join("full.png"))?);
    assert!(run("full.png", &["--mark", "1"]).is_err());
    for at in ["1.1", "1.2"] {
        assert!(run("tail.png", &["--at", at]).is_err());
        assert!(!root.path().join("tail.png").exists());
    }
    manifest["spaces"]["pins"][0]["observed"] = serde_json::json!(751);
    fs::write(show.join("show.json"), serde_json::to_vec(&manifest)?)?;
    let delayed = run("delayed.png", &["--at", "0.75"])?;
    assert_eq!(
        delayed["video"]["rendering"],
        serde_json::json!({"state":"painted","drawn":0,"omitted":0})
    );
    assert_eq!(raw_png, fs::read(root.path().join("delayed.png"))?);
    manifest["spaces"]["pins"][0]
        .as_object_mut()
        .unwrap()
        .remove("observed");
    fs::write(show.join("show.json"), serde_json::to_vec(&manifest)?)?;
    let legacy = run("legacy.png", &["--mark", "1"])?;
    assert_eq!(
        legacy["video"]["rendering"],
        serde_json::json!({"state":"painted","drawn":0,"omitted":1})
    );
    assert_eq!(raw_png, fs::read(root.path().join("legacy.png"))?);
    let mut header: serde_json::Value =
        serde_json::from_slice(&fs::read(show.join(movie::HEADER))?)?;
    header.as_object_mut().unwrap().remove("geometry");
    fs::write(show.join(movie::HEADER), serde_json::to_vec(&header)?)?;
    assert!(run("unknown.png", &["--mark", "1"]).is_err());
    assert!(!root.path().join("unknown.png").exists());
    Ok(())
}
