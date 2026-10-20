//! Publishing a finished Show: choosing and composing its keyframes, writing its bundle, and
//! copying its summary for the agent.

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context as _, anyhow};
use speakeasy_platform::InsertPermit;
use speakeasy_show::{Image, Moment, Typeface, budget, compose, select};

use crate::ports::{PUBLISH_STOPPED, Published, Recorded};

/// Composes and writes `recorded` off the caller's thread. The bundle appears and its summary is
/// copied only once `permit` commits; a publication cancelled before then stops at its next check
/// and deletes anything it staged.
pub(crate) async fn publish(recorded: Recorded, permit: InsertPermit) -> anyhow::Result<Published> {
    tokio::task::spawn_blocking(move || {
        let Some(written) = write_bundle(recorded, &permit, &super::directory())? else {
            return Ok(Published::Cancelled);
        };
        speakeasy_platform::copy_text(&written.summary).map_err(|error| {
            anyhow!("The Show was saved, but its summary could not be copied: {error}")
        })?;
        Ok(Published::Copied(written.directory))
    })
    .await
    // A failed join reports a fixed message, since a panic payload could carry private text.
    .unwrap_or_else(|_| Err(anyhow!(PUBLISH_STOPPED)))
}

/// A bundle in place, and the summary that names its files.
struct Written {
    directory: PathBuf,
    summary: String,
}

/// Writes `recorded` as a new bundle under `parent`, or nothing once `permit` is revoked.
fn write_bundle(
    recorded: Recorded,
    permit: &InsertPermit,
    parent: &Path,
) -> anyhow::Result<Option<Written>> {
    let cancelled = || !permit.active();
    if cancelled() {
        return Ok(None);
    }
    let Recorded {
        recording,
        samples,
        journal,
    } = recorded;
    let selection = select(
        &recording,
        &samples,
        budget(recording.duration.since(Moment::START)),
    );
    let moments: Vec<Moment> = selection
        .keyframes()
        .iter()
        .map(|keyframe| keyframe.at)
        .collect();
    let frames = journal
        .frames_at(&moments)
        .into_iter()
        .map(|frame| frame.context("A keyframe was not recorded"))
        .collect::<anyhow::Result<Vec<Image>>>()?;
    drop(journal);
    if cancelled() {
        return Ok(None);
    }
    let typeface = Typeface::parse(crate::BRAND_FONT_FILE)?;
    let bundle = compose(recording, selection, frames, &typeface)?;
    if cancelled() {
        return Ok(None);
    }
    let directory = super::fresh_directory(parent, SystemTime::now())?;
    let staged = super::stage(&directory, &bundle.files(&directory)?)?;
    if !permit.commit() {
        return Ok(None);
    }
    staged.persist()?;
    Ok(Some(Written {
        summary: bundle.summary(&directory),
        directory,
    }))
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Instant};

    use speakeasy_platform::{InputSender, InsertPermit};
    use speakeasy_show::{
        Journal, Manifest, Narration, Point, Recording, Sample, Signature, Size, Span, Tracker,
        Word,
    };

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
        const CELL_WIDTH: u32 = 9;
        const CELL_HEIGHT: u32 = 18;
        let mut pixels = Vec::with_capacity(
            usize::try_from(width.wrapping_mul(height).wrapping_mul(4)).unwrap(),
        );
        for y in 0..height {
            let line = y.wrapping_add(scroll) / CELL_HEIGHT;
            for x in 0..width {
                let column = x / CELL_WIDTH;
                let panel = dialog
                    && (width / 3..width.wrapping_mul(2) / 3).contains(&x)
                    && (height / 3..height.wrapping_mul(2) / 3).contains(&y);
                let written = column < 60_u32.wrapping_add(if line == 0 { typed } else { 0 })
                    && line.wrapping_mul(2_654_435_761).wrapping_add(column) % 7 != 0;
                let (inside_x, inside_y) = (x % CELL_WIDTH, y.wrapping_add(scroll) % CELL_HEIGHT);
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
        Image::from_bgra(width, height, pixels).unwrap()
    }

    fn words(spoken: &[(u64, &str)]) -> Narration {
        Narration::Words(
            spoken
                .iter()
                .map(|&(at, text)| Word {
                    span: Span::new(
                        Moment::from_millis(at),
                        Moment::from_millis(at.wrapping_add(300)),
                    ),
                    text: text.into(),
                })
                .collect(),
        )
    }

    /// Records frames every 200 ms through the journal and the tracker as the recorder does.
    struct Rehearsal {
        journal: Journal,
        samples: Vec<Sample>,
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
                samples: Vec::new(),
                tracker: Tracker::new(surface),
                surface,
            }
        }

        fn frame(&mut self, at: u64, image: Image) {
            let at = Moment::from_millis(at);
            let signature = Signature::of(&image);
            self.journal.record(at, image);
            self.samples.push(Sample { at, signature });
        }

        fn pointer(&mut self, at: u64, x: f32, y: f32, drawing: bool, pressed: bool) {
            self.tracker
                .observe(Moment::from_millis(at), Point::new(x, y), drawing, pressed);
        }

        fn finish(self, ended: u64, narration: Narration) -> Recorded {
            let duration = Moment::from_millis(ended);
            let (strokes, actions) = self.tracker.finish(duration);
            Recorded {
                recording: Recording {
                    duration,
                    surface: self.surface,
                    strokes,
                    actions,
                    narration,
                },
                samples: self.samples,
                journal: self.journal,
            }
        }
    }

    /// Ten seconds: a click opens a dialog, then a circle of ink is drawn around it while it is
    /// named.
    fn rehearse(width: u32, height: u32) -> Recorded {
        let mut show = Rehearsal::new(width, height);
        for at in (0..10_000).step_by(200) {
            show.frame(at, desktop(width, height, 0, 0, at >= 3_200));
        }
        show.pointer(3_000, 0.5, 0.5, false, true);
        show.pointer(3_080, 0.5, 0.5, false, false);
        for step in 0..=40_u16 {
            let turn = f32::from(step) / 40.0 * std::f32::consts::TAU;
            let at = u64::from(step).wrapping_mul(25).wrapping_add(5_000);
            show.pointer(
                at,
                0.2_f32.mul_add(turn.cos(), 0.5),
                0.2_f32.mul_add(turn.sin(), 0.5),
                true,
                false,
            );
        }
        show.pointer(6_100, 0.7, 0.5, false, false);
        show.finish(
            10_000,
            words(&[
                (2_800, "click"),
                (3_100, "here"),
                (5_200, "this"),
                (5_500, "dialog"),
                (5_800, "is"),
                (6_000, "wrong."),
            ]),
        )
    }

    /// The width and height a PNG's header declares.
    fn png_size(bytes: &[u8]) -> (u32, u32) {
        assert_eq!(bytes.get(..8), Some(&b"\x89PNG\r\n\x1a\n"[..]));
        let field =
            |at: usize| u32::from_be_bytes(bytes[at..at.wrapping_add(4)].try_into().unwrap());
        (field(16), field(20))
    }

    #[test]
    fn a_show_publishes_a_complete_private_bundle() -> anyhow::Result<()> {
        let (width, height) = (320, 200);
        let recorded = rehearse(width, height);
        let narration = recorded.recording.narration.clone();
        let moments = recorded.journal.moments().to_vec();
        let root = tempfile::tempdir()?;
        let (_input, permit) = permit();
        let written = write_bundle(recorded, &permit, &root.path().join("shows"))?
            .context("an active permit publishes")?;
        assert!(!permit.active(), "Publishing spends the permit");

        assert_eq!(
            fs::read_to_string(written.directory.join("summary.md"))?,
            written.summary,
            "the summary copied is the one written"
        );
        let manifest: Manifest =
            serde_json::from_slice(&fs::read(written.directory.join("show.json"))?)?;
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
            let size = png_size(&fs::read(written.directory.join(&file))?);
            assert_eq!(size, (width, height), "{file} is full size");
            assert!(written.summary.contains(&format!("#{index} (")), "#{index}");
        }
        let sheet = png_size(&fs::read(written.directory.join("contact-sheet.png"))?);
        assert!(sheet.0 > 0 && sheet.1 > 0);
        assert!(
            written
                .summary
                .contains(&written.directory.display().to_string())
        );
        assert!(
            written.summary.contains("dialog"),
            "narration reaches the agent"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(&written.directory)?.permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        Ok(())
    }

    #[test]
    fn a_cancelled_show_never_reaches_the_disk() -> anyhow::Result<()> {
        let (input, permit) = permit();
        input.cancel();
        let root = tempfile::tempdir()?;
        let parent = root.path().join("shows");
        assert!(write_bundle(rehearse(32, 20), &permit, &parent)?.is_none());
        assert!(!parent.exists(), "Nothing was staged");
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
        for (scene, frames) in [
            ("still", (0..30).map(|_| (0, 0, false)).collect::<Vec<_>>()),
            ("typing", (0..30).map(|frame| (0, frame, false)).collect()),
            (
                "scrolling",
                (0..30).map(|frame| (frame * 40, 0, false)).collect(),
            ),
            (
                "dialog",
                (0..30).map(|frame| (0, 0, frame % 2 == 1)).collect(),
            ),
        ] {
            let images: Vec<Image> = frames
                .into_iter()
                .map(|(scroll, typed, dialog)| desktop(width, height, scroll, typed, dialog))
                .collect();
            let mut journal = Journal::new();
            let mut timings = Vec::new();
            for (index, image) in images.into_iter().enumerate() {
                let started = Instant::now();
                let signature = black_box(Signature::of(&image));
                journal.record(Moment::from_millis(u64::try_from(index)? * 200), image);
                timings.push(started.elapsed().as_secs_f64() * 1000.0);
                black_box(signature);
            }
            eprintln!(
                "show_record: scene={scene} size={width}x{height} median_ms={:.2} journal_mb={:.1} frames=30",
                median_ms(timings),
                journal.bytes() as f64 / 1e6
            );
        }
        // Publishing a one-minute Show of that size.
        let mut show = Rehearsal::new(width, height);
        for at in (0..60_000_u32).step_by(200) {
            let scroll = (at / 10_000).wrapping_mul(300);
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
        let recorded = show.finish(
            60_000,
            words(
                &(0..60)
                    .map(|second| (second * 1_000, "look"))
                    .collect::<Vec<_>>(),
            ),
        );
        let started = Instant::now();
        let selection = select(
            &recorded.recording,
            &recorded.samples,
            budget(recorded.recording.duration.since(Moment::START)),
        );
        let selected = started.elapsed();
        let moments: Vec<Moment> = selection
            .keyframes()
            .iter()
            .map(|keyframe| keyframe.at)
            .collect();
        let frames = recorded
            .journal
            .frames_at(&moments)
            .into_iter()
            .map(|frame| frame.context("recorded"))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let decoded = started.elapsed();
        let typeface = Typeface::parse(crate::BRAND_FONT_FILE)?;
        let keyframes = selection.keyframes().len();
        let bundle = compose(recorded.recording, selection, frames, &typeface)?;
        let composed = started.elapsed();
        let root = tempfile::tempdir()?;
        let files = bundle.files(root.path())?;
        let encoded = started.elapsed();
        super::super::stage(&root.path().join("show"), &files)?.persist()?;
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
}
