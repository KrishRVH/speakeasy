use std::{fs, io, path::Path, time::Instant};

use anyhow::bail;
use speakeasy_platform::{InputSender, InsertPermit};
use speakeasy_show::{
    Image, Journal, Manifest, Moment, Narration, Point, Rect, Size, Span, Tracker, Word,
};

use super::*;

fn permit() -> (InputSender, InsertPermit) {
    let (sender, _inputs) = async_channel::bounded(1);
    let input = InputSender::new(sender);
    let permit = input.begin().unwrap();
    (input, permit)
}

fn desktop(width: u32, height: u32, scroll: u32, typed: u32, dialog: bool) -> Image {
    let pixels = desktop_pixels(width, height, scroll, typed, dialog);
    Image::from_bgra(width, height, pixels).unwrap()
}

fn video(width: u32, height: u32, seed: u32) -> Image {
    const PLAYER: (u32, u32) = (1_280, 720);
    let mut pixels = desktop_pixels(width, height, 0, 0, false);
    let left = width.saturating_sub(PLAYER.0) / 2;
    let top = height.saturating_sub(PLAYER.1) / 2;
    let mut state = seed.wrapping_mul(2_654_435_761) | 1;
    for row in pixels
        .as_chunks_mut::<4>()
        .0
        .chunks_exact_mut(usize::try_from(width).expect("Fixture row width fits usize"))
        .skip(usize::try_from(top).expect("Fixture top fits usize"))
        .take(usize::try_from(PLAYER.1).expect("Player height fits usize"))
    {
        for pixel in row
            .iter_mut()
            .skip(usize::try_from(left).expect("Fixture left fits usize"))
            .take(usize::try_from(PLAYER.0).expect("Player width fits usize"))
        {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            pixel[..3].copy_from_slice(&state.to_le_bytes()[..3]);
        }
    }
    Image::from_bgra(width, height, pixels).unwrap()
}

fn desktop_pixels(width: u32, height: u32, scroll: u32, typed: u32, dialog: bool) -> Vec<u8> {
    const CELL_WIDTH: u32 = 9;
    const CELL_HEIGHT: u32 = 18;
    let capacity = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .expect("Synthetic desktop pixel bytes fit u32");
    let panel_x = width / 3
        ..width
            .checked_mul(2)
            .expect("Synthetic desktop width fits doubled")
            / 3;
    let panel_y = height / 3
        ..height
            .checked_mul(2)
            .expect("Synthetic desktop height fits doubled")
            / 3;
    let mut pixels = Vec::with_capacity(
        usize::try_from(capacity).expect("Synthetic desktop pixel bytes fit usize"),
    );
    for y in 0..height {
        let scrolled = y.checked_add(scroll).expect("Synthetic scroll fits u32");
        let line = scrolled / CELL_HEIGHT;
        for x in 0..width {
            let column = x / CELL_WIDTH;
            let panel = dialog && panel_x.contains(&x) && panel_y.contains(&y);
            let glyphs = if line == 0 {
                typed
                    .checked_add(60)
                    .expect("Synthetic glyph count fits u32")
            } else {
                60
            };
            let written =
                column < glyphs && line.wrapping_mul(2_654_435_761).wrapping_add(column) % 7 != 0;
            let (inside_x, inside_y) = (x % CELL_WIDTH, scrolled % CELL_HEIGHT);
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

fn words(spoken: &[(u64, &str)]) -> Narration {
    Narration::Words(
        spoken
            .iter()
            .map(|&(at, text)| Word {
                span: Span::new(
                    Moment::from_millis(at),
                    Moment::from_millis(at.checked_add(300).expect("Synthetic word end fits u64")),
                ),
                text: text.into(),
            })
            .collect(),
    )
}

struct Rehearsal {
    journal: Journal,
    tracker: Tracker,
}

impl Rehearsal {
    fn new(width: u32, height: u32) -> Self {
        Self {
            journal: Journal::new(),
            tracker: Tracker::new(Size {
                width: width as f32,
                height: height as f32,
            }),
        }
    }

    fn frame(&mut self, at: u64, image: Image) {
        if let Some(admission) = self.journal.admit() {
            admission.record(Moment::from_millis(at), image);
        }
    }

    fn pointer(&mut self, at: u64, x: f32, y: f32, drawing: bool, pressed: bool) {
        self.tracker
            .observe(Moment::from_millis(at), Point::new(x, y), drawing, pressed);
    }

    fn finish(self, ended: u64) -> Footage {
        Footage {
            recording: self.tracker.finish(Moment::from_millis(ended)),
            journal: self.journal,
            framing: Framing::Automatic,
        }
    }
}

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

fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(bytes.get(..8), Some(&b"\x89PNG\r\n\x1a\n"[..]));
    let field = |at: usize| u32::from_be_bytes(*bytes[at..].first_chunk().unwrap());
    (field(16), field(20))
}

#[tokio::test]
async fn unknown_checkpoint_state_precedes_composition_and_respects_cancellation()
-> anyhow::Result<()> {
    for cancelled in [false, true] {
        let crate::show::Kept {
            keeper,
            frames,
            audio,
        } = crate::show::Keeper::start(
            || panic!("Synthetic keeper interruption"),
            SystemTime::now(),
        )?;
        drop(frames);
        drop(audio);
        let (input, permit) = permit();
        if cancelled {
            input.cancel();
        }
        let result = publish(
            Publication {
                footage: Rehearsal::new(80, 60).finish(500),
                checkpoint: Some(crate::show::Checkpointing::Writing(keeper)),
                origin: Origin::Live,
            },
            permit,
        )
        .await;
        if cancelled {
            assert!(matches!(result, Ok(Published::Cancelled)));
        } else {
            let error = result.err().context("Checkpoint failure was ignored")?;
            assert_eq!(
                error.to_string(),
                "The Show could not be saved because its checkpoint failed. Check Shows for recoverable footage."
            );
            assert!(
                error.chain().any(|cause| cause.to_string()
                    == "Show checkpoint stopped before returning its files")
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn unavailable_checkpoint_preserves_in_memory_publication_and_cancellation()
-> anyhow::Result<()> {
    for (fail_parent, cancelled) in [(false, false), (false, true), (true, false), (true, true)] {
        let root = tempfile::tempdir()?;
        let blocked = root.path().join("blocked");
        fs::write(&blocked, b"fixture")?;
        let crate::show::Kept {
            keeper,
            frames,
            audio,
        } = crate::show::Keeper::start(
            move || {
                if fail_parent {
                    bail!("Synthetic checkpoint parent failure");
                }
                Ok(blocked)
            },
            SystemTime::now(),
        )?;
        drop(frames);
        drop(audio);
        let checkpoint = crate::show::Checkpointing::Writing(keeper).close().await?;
        assert!(checkpoint.is_none());
        let (input, permit) = permit();
        if cancelled {
            input.cancel();
        }
        let (footage, narration) = rehearse(80, 60);
        let mut copied = false;
        let result = publish_with(
            None,
            narrated(footage, narration.clone(), None),
            checkpoint,
            Origin::Live,
            &permit,
            || Ok(root.path().to_owned()),
            |_| {
                copied = true;
                Ok(())
            },
        )?;
        if cancelled {
            assert!(matches!(result, Published::Cancelled));
            assert!(!copied);
            assert_eq!(fs::read_dir(root.path())?.count(), 1);
        } else {
            let Published::Saved {
                show, copied: copy, ..
            } = result
            else {
                bail!("In-memory footage was not published");
            };
            assert!(copied && copy.is_ok());
            let manifest = Manifest::parse(&fs::read(show.directory().join(Manifest::FILE))?)?;
            assert_eq!(manifest.recording().narration, narration);
            assert!(store::source::inspect(show.directory())?.moments().len() >= 2);
        }
        assert_eq!(
            store::checkpoint::list(root.path(), SystemTime::now()).shows,
            []
        );
    }
    Ok(())
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
    let mut clipboard = None;
    let published = publish_with(
        None,
        narrated(footage, narration.clone(), None),
        None,
        Origin::Live,
        &permit,
        || Ok(shows.clone()),
        |summary| {
            clipboard = Some(summary.to_owned());
            Ok(())
        },
    )?;
    let Published::Saved {
        show,
        copied: Ok(()),
        ..
    } = published
    else {
        bail!("An active permit saves and copies the Show");
    };
    let summary = clipboard.context("The summary was copied")?;
    assert!(!permit.active(), "Publishing spends the permit");
    let mut entries = fs::read_dir(&shows)?;
    let directory = entries
        .next()
        .context("The bundle is in the Shows folder")??
        .path();
    assert!(
        entries.next().is_none(),
        "The Shows folder holds only the bundle"
    );
    assert_eq!(
        show.folder()?,
        Some(directory.as_path()),
        "The Show saved is the bundle"
    );

    assert_eq!(
        fs::read_to_string(directory.join("summary.md"))?,
        summary,
        "The summary copied is the one written"
    );
    let manifest: Manifest = Manifest::parse(&fs::read(directory.join("show.json"))?)?;
    let source = store::source::inspect(&directory)?;
    assert_eq!(source.moments(), moments);
    assert_eq!(source.duration(), manifest.recording().duration);
    let held = store::source::frame(&directory, &source, Moment::from_millis(3_150))?;
    assert_eq!(held.at.as_millis(), 3_000);
    assert_eq!(held.image, desktop(width, height, 0, 0, false));
    assert_eq!(manifest.recording().narration, narration);
    assert_eq!(manifest.recording().strokes.len(), 1, "The circle");
    assert_eq!(manifest.recording().actions.len(), 1, "The click");
    assert_ne!(manifest.keyframes().len(), 0);
    for keyframe in manifest.keyframes() {
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
    assert!(summary.contains("dialog"), "Narration reaches the agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&directory)?.permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    Ok(())
}

#[test]
fn only_a_screen_show_crops_its_frames_to_the_activity() -> anyhow::Result<()> {
    let (width, height) = (1_600, 1_000);
    for framing in [Framing::Automatic, Framing::Chosen] {
        let mut show = Rehearsal::new(width, height);
        for at in (0..4_000).step_by(200) {
            show.frame(at, desktop(width, height, 0, 0, at >= 1_200));
        }
        show.pointer(1_000, 0.15, 0.2, false, true);
        show.pointer(1_080, 0.15, 0.2, false, false);
        let root = tempfile::tempdir()?;
        let (_input, permit) = permit();
        let Published::Saved { show, .. } = publish_with(
            None,
            Footage {
                framing,
                ..show.finish(4_000)
            },
            None,
            Origin::Live,
            &permit,
            || Ok(root.path().to_owned()),
            |_| Ok(()),
        )?
        else {
            bail!("An active permit saves the Show");
        };
        let directory = show.folder()?.context("The Show was saved")?;
        let manifest = Manifest::parse(&fs::read(directory.join("show.json"))?)?;
        let mut cropped = 0;
        for keyframe in manifest.keyframes() {
            let file = keyframe.file();
            let crop = keyframe
                .crop
                .unwrap_or_else(|| Rect::from([0.0, 0.0, 1.0, 1.0]));
            let (wide, tall) = png_size(&fs::read(directory.join(&file))?);
            assert!(
                (wide as f32 - (crop.right - crop.left) * width as f32).abs() < 0.5
                    && (tall as f32 - (crop.bottom - crop.top) * height as f32).abs() < 0.5,
                "{file} is {wide}×{tall} for {crop:?}"
            );
            cropped += usize::from(keyframe.crop.is_some());
        }
        let automatic = framing == Framing::Automatic;
        assert_eq!(
            cropped > 0,
            automatic,
            "{framing:?} cropped {cropped} frames"
        );
        let summary = show.summary()?.context("The Show kept its summary")?;
        assert_eq!(summary.contains("cropped to the area"), automatic);
    }
    Ok(())
}

#[test]
fn a_show_that_cannot_be_copied_stays_saved() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let shows = root.path().join("shows");
    let (_input, permit) = permit();
    let (footage, narration) = rehearse(32, 20);
    let published = publish_with(
        None,
        narrated(footage, narration, None),
        None,
        Origin::Live,
        &permit,
        || Ok(shows.clone()),
        |_| bail!("Another app holds the clipboard"),
    )?;
    let Published::Saved {
        show,
        copied: Err(error),
        ..
    } = published
    else {
        bail!("A Show saved but not copied was reported otherwise");
    };
    assert_eq!(error.to_string(), "Another app holds the clipboard");
    let folder = show.folder()?.context("The Show stayed saved")?;
    assert_eq!(folder.parent(), Some(shows.as_path()));
    let summary = show.summary()?.context("The Show kept its summary")?;
    assert!(summary.contains(&folder.display().to_string()));
    Ok(())
}

fn uncopied(_: &str) -> anyhow::Result<()> {
    bail!("A cancelled Show is never copied")
}

#[test]
fn a_cancelled_show_never_reaches_the_disk() -> anyhow::Result<()> {
    let (input, permit) = permit();
    input.cancel();
    let unprepared = || bail!("A cancelled Show never prepares its folder");
    let (footage, narration) = rehearse(32, 20);
    let published = publish_with(
        None,
        narrated(footage, narration, None),
        None,
        Origin::Live,
        &permit,
        unprepared,
        uncopied,
    )?;
    assert!(matches!(published, Published::Cancelled));
    Ok(())
}

#[test]
fn a_show_cancelled_before_staging_never_writes_its_files() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;

    let blocked = directory.path().join("shows");
    fs::write(&blocked, b"")?;
    let (input, permit) = permit();

    let cancel_once_composed = || {
        input.cancel();
        Ok(blocked.clone())
    };
    let (footage, narration) = rehearse(32, 20);
    let published = publish_with(
        None,
        narrated(footage, narration, None),
        None,
        Origin::Live,
        &permit,
        cancel_once_composed,
        uncopied,
    )?;
    assert!(matches!(published, Published::Cancelled));
    Ok(())
}

#[test]
fn a_show_that_lost_its_narration_says_why_in_its_files() -> anyhow::Result<()> {
    let shows = tempfile::tempdir()?;
    let (_input, permit) = permit();
    let (footage, _) = rehearse(32, 20);
    let lost = NarrationLoss::AudioStopped;
    let mut clipboard = None;
    let published = publish_with(
        None,
        narrated(footage, Narration::default(), Some(lost)),
        None,
        Origin::Live,
        &permit,
        || Ok(shows.path().to_owned()),
        |summary| {
            clipboard = Some(summary.to_owned());
            Ok(())
        },
    )?;
    assert!(matches!(
        published,
        Published::Saved {
            preservation: Preservation {
                narration_lost: Some(_),
                ..
            },
            ..
        }
    ));
    let summary = clipboard.context("The summary was copied")?;
    assert!(summary.contains("Audio capture stopped at 0:10 and the Show ended there"));
    let directory = fs::read_dir(shows.path())?
        .next()
        .context("The bundle is in the Shows folder")??
        .path();
    let manifest: Manifest = Manifest::parse(&fs::read(directory.join("show.json"))?)?;
    assert_eq!(manifest.recording().narration_lost, Some(lost));
    Ok(())
}

fn bytes_under(directory: &Path) -> io::Result<u64> {
    fs::read_dir(directory)?.try_fold(0_u64, |bytes, entry| {
        let entry = entry?;
        let size = if entry.file_type()?.is_dir() {
            bytes_under(&entry.path())?
        } else {
            entry.metadata()?.len()
        };
        Ok(bytes.saturating_add(size))
    })
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

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
        let root = tempfile::tempdir()?;
        let surface = Size {
            width: width as f32,
            height: height as f32,
        };
        let (checkpoint, mut writer) = crate::show::Keeper::fixture(
            root.path(),
            SystemTime::now(),
            speakeasy_show::Log::MAX_BUDGET,
            surface,
            Framing::Automatic,
        )?;
        let mut take =
            speakeasy_show::Take::new(speakeasy_show::Log::MAX_BUDGET, surface, Framing::Automatic);
        let mut timings = Vec::new();
        for (index, image) in images.into_iter().enumerate() {
            let at = Moment::from_millis(u64::try_from(index)? * 200);
            let started = Instant::now();
            let event = take.arrive(at, || Some(image))?;
            assert!(
                writer.send(event),
                "Synthetic checkpoint writer fell behind"
            );
            timings.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let started = Instant::now();
        assert!(runtime.block_on(writer.close()));
        let checkpoint = runtime
            .block_on(checkpoint.close())?
            .context("Fixture checkpoint is missing")?;
        let flushed_ms = started.elapsed().as_secs_f64() * 1000.0;
        let started = Instant::now();
        let replayed = checkpoint.replay()?;
        let replay_ms = started.elapsed().as_secs_f64() * 1000.0;
        let (_, journal) = take.finish();
        assert_eq!(journal.samples().len(), replayed.journal().samples().len());
        let checkpoint_mb = bytes_under(root.path())? as f64 / 1e6;
        eprintln!(
            "show_checkpoint: scene={scene} flush_ms={flushed_ms:.2} replay_ms={replay_ms:.2} mb={checkpoint_mb:.1}"
        );
        black_box(journal.samples());
        let journal_mb = journal.bytes() as f64 / 1e6;
        eprintln!(
            "show_record: scene={scene} size={width}x{height} median_ms={:.2} journal_mb={journal_mb:.1} mb_per_frame={:.2} frames={frames}",
            median_ms(timings),
            journal_mb / frames as f64,
        );
    }

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
    let Footage {
        mut recording,
        journal,
        ..
    } = footage;
    recording.narration = narration;
    let selection = select(&recording, journal.samples()).context("Selected")?;
    let selected = started.elapsed();
    let typeface = Typeface::parse(crate::BRAND_FONT_FILE)?;
    let bundle = compose(
        recording,
        selection,
        |at| journal.frame_at(at).context("Recorded"),
        &typeface,
    )?;
    let composed = started.elapsed();
    let root = tempfile::tempdir()?;
    let directory = root.path().join("show");
    let staged = store::stage(
        &directory,
        bundle,
        Some(store::Retained {
            journal: &journal,
            duration: Moment::from_millis(60_000),
            command: None,
        }),
    )?;
    assert!(!staged.source_lost);
    staged.persist()?;
    let written = started.elapsed();
    let bytes = bytes_under(&directory)?;
    let keyframes = fs::read_dir(directory.join("frames"))?.count();
    eprintln!(
        "show_publish: seconds=60 keyframes={keyframes} select_ms={:.1} compose_ms={:.1} write_ms={:.1} total_ms={:.1} bundle_mb={:.1}",
        selected.as_secs_f64() * 1000.0,
        composed.saturating_sub(selected).as_secs_f64() * 1000.0,
        written.saturating_sub(composed).as_secs_f64() * 1000.0,
        written.as_secs_f64() * 1000.0,
        bytes as f64 / 1e6,
    );
    Ok(())
}

mod recovery;

#[test]
fn retrieval_is_advertised_only_when_both_source_and_command_are_available() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let command = root.path().join("Apps ü & `tools`").join("speakeasy-cli");
    for (available, source_ok) in [(true, true), (false, true), (true, false)] {
        let (mut footage, narration) = rehearse(80, 60);
        if !source_ok {
            footage.recording.duration = Moment::from_millis(360_001);
        }
        let (_input, permit) = permit();
        let mut copied = String::new();
        let published = publish_with(
            available.then_some(command.as_path()),
            narrated(footage, narration, None),
            None,
            Origin::Live,
            &permit,
            || Ok(root.path().join("shows")),
            |summary| {
                summary.clone_into(&mut copied);
                Ok(())
            },
        )?;
        let Published::Saved {
            show,
            preservation: Preservation { source_lost, .. },
            ..
        } = published
        else {
            bail!("Expected a saved Show");
        };
        assert_eq!(source_lost, !source_ok);
        assert_eq!(
            copied,
            fs::read_to_string(show.directory().join("summary.md"))?
        );
        assert_eq!(copied.contains("Local command:"), available && source_ok);
        if available && source_ok {
            let id = show.directory().file_name().unwrap().to_str().unwrap();
            assert!(copied.contains(&format!("show frames {id} --json")));
            assert!(copied.contains(&format!("``{}``", command.display())));
        }
        assert_eq!(store::source::size(show.directory())?.is_some(), source_ok);
    }
    Ok(())
}

fn narrated(mut footage: Footage, narration: Narration, lost: Option<NarrationLoss>) -> Footage {
    footage.recording.narration = narration;
    footage.recording.narration_lost = lost;
    footage
}
