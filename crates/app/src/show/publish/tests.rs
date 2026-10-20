//! Publishes a rehearsed Show into a temporary folder and checks every file and the cancel path;
//! the opt-in `profile_show_pipeline` times recording and publishing at 2560 × 1440.

use std::{fs, time::Instant};

use speakeasy_platform::{InputSender, InsertPermit};
use speakeasy_show::{Manifest, Moment, Point, Size, Span, Tracker, Word};

use super::*;

fn permit() -> (InputSender, InsertPermit) {
    let (sender, _inputs) = async_channel::bounded(1);
    let input = InputSender::new(sender);
    let permit = input.begin().unwrap();
    (input, permit)
}

/// A desktop of text-like glyphs `width × height` pixels, its lines scrolled up by `scroll`
/// pixels, with `typed` extra glyphs at the end of the first line and, when `dialog`, a
/// panel over its middle.
fn desktop(width: u32, height: u32, scroll: u32, typed: u32, dialog: bool) -> Image {
    let pixels = desktop_pixels(width, height, scroll, typed, dialog);
    Image::from_bgra(width, height, pixels).unwrap()
}

/// A still desktop whose middle 1280 × 720 pixels play noise drawn from `seed`, as a video
/// does: every frame changes every tile it covers, and none compresses.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "Pixel geometry of frames up to 2560 × 1440; debug overflow checks fail the test"
)]
fn video(width: u32, height: u32, seed: u32) -> Image {
    const PLAYER: (u32, u32) = (1_280, 720);
    let mut pixels = desktop_pixels(width, height, 0, 0, false);
    let left = width.saturating_sub(PLAYER.0) / 2;
    let top = height.saturating_sub(PLAYER.1) / 2;
    let mut state = seed.wrapping_mul(2_654_435_761) | 1;
    for y in top..(top + PLAYER.1).min(height) {
        for x in left..(left + PLAYER.0).min(width) {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let at = usize::try_from((y * width + x) * 4).unwrap();
            pixels[at..at + 3].copy_from_slice(&state.to_le_bytes()[..3]);
        }
    }
    Image::from_bgra(width, height, pixels).unwrap()
}

/// The BGRA pixels of [`desktop`].
#[expect(
    clippy::arithmetic_side_effects,
    reason = "Pixel geometry of frames up to 2560 × 1440; debug overflow checks fail the test"
)]
fn desktop_pixels(width: u32, height: u32, scroll: u32, typed: u32, dialog: bool) -> Vec<u8> {
    const CELL_WIDTH: u32 = 9;
    const CELL_HEIGHT: u32 = 18;
    let mut pixels = Vec::with_capacity(usize::try_from(width * height * 4).unwrap());
    for y in 0..height {
        let line = (y + scroll) / CELL_HEIGHT;
        for x in 0..width {
            let column = x / CELL_WIDTH;
            let panel = dialog
                && (width / 3..width * 2 / 3).contains(&x)
                && (height / 3..height * 2 / 3).contains(&y);
            let glyphs = if line == 0 { 60 + typed } else { 60 };
            let written =
                column < glyphs && line.wrapping_mul(2_654_435_761).wrapping_add(column) % 7 != 0;
            let (inside_x, inside_y) = (x % CELL_WIDTH, (y + scroll) % CELL_HEIGHT);
            let ink = written
                && (1..CELL_WIDTH - 2).contains(&inside_x)
                && (4..CELL_HEIGHT - 4).contains(&inside_y)
                && (inside_x ^ inside_y ^ column) & 1 == 0;
            pixels.extend(match (panel, ink) {
                (true, _) => [52, 120, 246, 255],
                (false, true) => [30, 30, 36, 255],
                (false, false) => [250, 250, 248, 255],
            });
        }
    }
    pixels
}

#[expect(
    clippy::arithmetic_side_effects,
    reason = "Word ends 300 ms after literal starts; debug overflow checks fail the test"
)]
fn words(spoken: &[(u64, &str)]) -> Narration {
    Narration::Words(
        spoken
            .iter()
            .map(|&(at, text)| Word {
                span: Span::new(Moment::from_millis(at), Moment::from_millis(at + 300)),
                text: text.into(),
            })
            .collect(),
    )
}

/// Records frames every 200 ms through the journal and the tracker as the recorder does.
struct Rehearsal {
    journal: Journal,
    tracker: Tracker,
    surface: Size,
}

impl Rehearsal {
    fn new(width: u32, height: u32) -> Self {
        let surface = Size {
            width: width as f32,
            height: height as f32,
        };
        Self {
            journal: Journal::new(),
            tracker: Tracker::new(surface),
            surface,
        }
    }

    fn frame(&mut self, at: u64, image: Image) {
        self.journal.record(Moment::from_millis(at), image);
    }

    fn pointer(&mut self, at: u64, x: f32, y: f32, drawing: bool, pressed: bool) {
        self.tracker
            .observe(Moment::from_millis(at), Point::new(x, y), drawing, pressed);
    }

    fn finish(self, ended: u64) -> Footage {
        let ended = Moment::from_millis(ended);
        let (strokes, actions) = self.tracker.finish(ended);
        Footage {
            surface: self.surface,
            ended,
            journal: self.journal,
            strokes,
            actions,
        }
    }
}

/// Ten seconds: a click opens a dialog, then a circle of ink is drawn around it while it is
/// named.
fn rehearse(width: u32, height: u32) -> (Footage, Narration) {
    let mut show = Rehearsal::new(width, height);
    for at in (0..10_000).step_by(200) {
        show.frame(at, desktop(width, height, 0, 0, at >= 3_200));
    }
    show.pointer(3_000, 0.5, 0.5, false, true);
    show.pointer(3_080, 0.5, 0.5, false, false);
    for (step, at) in (0..=40_u16).zip((5_000..).step_by(25)) {
        let turn = f32::from(step) / 40.0 * std::f32::consts::TAU;
        show.pointer(
            at,
            0.2_f32.mul_add(turn.cos(), 0.5),
            0.2_f32.mul_add(turn.sin(), 0.5),
            true,
            false,
        );
    }
    show.pointer(6_100, 0.7, 0.5, false, false);
    let narration = words(&[
        (2_800, "click"),
        (3_100, "here"),
        (5_200, "this"),
        (5_500, "dialog"),
        (5_800, "is"),
        (6_000, "wrong."),
    ]);
    (show.finish(10_000), narration)
}

/// The width and height a PNG's header declares.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(bytes.get(..8), Some(&b"\x89PNG\r\n\x1a\n"[..]));
    let field = |at: usize| u32::from_be_bytes(*bytes[at..].first_chunk().unwrap());
    (field(16), field(20))
}

#[test]
fn a_show_publishes_a_complete_private_bundle() -> anyhow::Result<()> {
    let (width, height) = (320, 200);
    let (footage, narration) = rehearse(width, height);
    let moments: Vec<Moment> = footage
        .journal
        .samples()
        .iter()
        .map(|sample| sample.at)
        .collect();
    let root = tempfile::tempdir()?;
    let shows = root.path().join("shows");
    let (_input, permit) = permit();
    let summary = write_bundle(footage, narration.clone(), &permit, || Ok(shows.clone()))?
        .context("an active permit publishes")?;
    assert!(!permit.active(), "Publishing spends the permit");
    let mut entries = fs::read_dir(&shows)?;
    let directory = entries
        .next()
        .context("the bundle is in the Shows folder")??
        .path();
    assert!(
        entries.next().is_none(),
        "the Shows folder holds only the bundle"
    );

    assert_eq!(
        fs::read_to_string(directory.join("summary.md"))?,
        summary,
        "the summary copied is the one written"
    );
    let manifest: Manifest = serde_json::from_slice(&fs::read(directory.join("show.json"))?)?;
    assert_eq!(manifest.recording.narration, narration);
    assert_eq!(manifest.recording.strokes.len(), 1, "the circle");
    assert_eq!(manifest.recording.actions.len(), 1, "the click");
    assert_ne!(manifest.keyframes.len(), 0);
    for keyframe in &manifest.keyframes {
        let index = keyframe.index;
        assert!(
            moments.contains(&keyframe.at),
            "#{index} shows a recorded frame"
        );
        let file = keyframe.file();
        let size = png_size(&fs::read(directory.join(&file))?);
        assert_eq!(size, (width, height), "{file} is full size");
        assert!(summary.contains(&format!("#{index} (")), "#{index}");
    }
    let sheet = png_size(&fs::read(directory.join("contact-sheet.png"))?);
    assert!(sheet.0 > 0 && sheet.1 > 0);
    assert!(summary.contains(&directory.display().to_string()));
    assert!(summary.contains("dialog"), "narration reaches the agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&directory)?.permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    Ok(())
}

#[test]
fn a_cancelled_show_never_reaches_the_disk() -> anyhow::Result<()> {
    let (input, permit) = permit();
    input.cancel();
    let unprepared = || anyhow::bail!("a cancelled Show never prepares its folder");
    let (footage, narration) = rehearse(32, 20);
    assert!(write_bundle(footage, narration, &permit, unprepared)?.is_none());
    Ok(())
}

#[test]
fn a_show_cancelled_while_its_images_encode_never_writes_them() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    // A file where the Shows folder belongs, so any attempt to write the bundle fails the call.
    let blocked = directory.path().join("shows");
    fs::write(&blocked, b"")?;
    let (input, permit) = permit();
    // The folder is prepared just before encoding begins, after the last check that precedes it.
    let cancel_while_encoding = || {
        input.cancel();
        Ok(blocked.clone())
    };
    let (footage, narration) = rehearse(32, 20);
    assert!(write_bundle(footage, narration, &permit, cancel_while_encoding)?.is_none());
    Ok(())
}

fn median_ms(mut timings: Vec<f64>) -> f64 {
    timings.sort_by(f64::total_cmp);
    timings[timings.len() / 2]
}

#[test]
#[ignore = "opt-in synthetic release profiling; never captures the screen"]
fn profile_show_pipeline() -> anyhow::Result<()> {
    use std::hint::black_box;
    let (width, height) = (2_560, 1_440);
    // What the recorder does with each kept frame, for screens that change in different ways.
    let scenes: [(&str, &dyn Fn(u32) -> Image); 5] = [
        ("still", &|_| desktop(width, height, 0, 0, false)),
        ("typing", &|frame| desktop(width, height, 0, frame, false)),
        ("scrolling", &|frame| {
            desktop(width, height, frame * 40, 0, false)
        }),
        ("dialog", &|frame| {
            desktop(width, height, 0, 0, frame % 2 == 1)
        }),
        ("video", &|frame| video(width, height, frame)),
    ];
    for (scene, paint) in scenes {
        let images: Vec<Image> = (0..30).map(paint).collect();
        let frames = images.len();
        let mut journal = Journal::new();
        let mut timings = Vec::new();
        for (index, image) in images.into_iter().enumerate() {
            let at = Moment::from_millis(u64::try_from(index)? * 200);
            let started = Instant::now();
            journal.record(at, image);
            timings.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        black_box(journal.samples());
        let journal_mb = journal.bytes() as f64 / 1e6;
        eprintln!(
            "show_record: scene={scene} size={width}x{height} median_ms={:.2} journal_mb={journal_mb:.1} mb_per_frame={:.2} frames={frames}",
            median_ms(timings),
            journal_mb / frames as f64,
        );
    }
    // Publishing a one-minute Show of that size.
    let mut show = Rehearsal::new(width, height);
    for at in (0..60_000_u32).step_by(200) {
        let scroll = at / 10_000 * 300;
        show.frame(
            u64::from(at),
            desktop(width, height, scroll, at / 1_000 % 10, at % 10_000 > 6_000),
        );
    }
    for click in 0..6_u64 {
        let at = click * 10_000 + 6_000;
        show.pointer(at - 100, 0.4, 0.6, false, true);
        show.pointer(at - 20, 0.4, 0.6, false, false);
    }
    let footage = show.finish(60_000);
    let narration = words(
        &(0..60)
            .map(|second| (second * 1_000, "look"))
            .collect::<Vec<_>>(),
    );
    let started = Instant::now();
    let (recording, journal) = narrated(footage, narration);
    let selection = select(&recording, journal.samples()).context("selected")?;
    let selected = started.elapsed();
    let frames = journal
        .frames_at(&selection.moments())
        .into_iter()
        .map(|frame| frame.context("recorded"))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let decoded = started.elapsed();
    let typeface = Typeface::parse(crate::BRAND_FONT_FILE)?;
    let keyframes = selection.keyframes().len();
    let bundle = compose(recording, selection, frames, &typeface)?;
    let composed = started.elapsed();
    let root = tempfile::tempdir()?;
    let files = bundle.files(root.path())?;
    let encoded = started.elapsed();
    store::stage(&root.path().join("show"), &files)?.persist()?;
    let written = started.elapsed();
    let bytes: usize = files.iter().map(|file| file.contents.len()).sum();
    eprintln!(
        "show_publish: seconds=60 keyframes={keyframes} select_ms={:.1} decode_ms={:.1} compose_ms={:.1} encode_ms={:.1} write_ms={:.1} total_ms={:.1} bundle_mb={:.1}",
        selected.as_secs_f64() * 1000.0,
        decoded.saturating_sub(selected).as_secs_f64() * 1000.0,
        composed.saturating_sub(decoded).as_secs_f64() * 1000.0,
        encoded.saturating_sub(composed).as_secs_f64() * 1000.0,
        written.saturating_sub(encoded).as_secs_f64() * 1000.0,
        written.as_secs_f64() * 1000.0,
        bytes as f64 / 1e6,
    );
    Ok(())
}
