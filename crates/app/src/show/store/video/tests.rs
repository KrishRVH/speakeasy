use std::{
    fs,
    process::{Command, Stdio},
    time::Instant,
};

use serde_json::json;
use speakeasy_show::{Moment, Narration, Recording, Word};
use zeroize::Zeroizing;

use super::*;
use crate::show::store::checkpoint::KeptAudio;

fn audio(start: u64) -> KeptAudio {
    KeptAudio {
        rate: 30,
        start: Duration::from_millis(start),
        language: "en".into(),
        pcm: Zeroizing::new([1000_i16.to_le_bytes(); 60].concat()),
        cues: Vec::new(),
        complete: true,
    }
}

#[test]
fn voice_alignment_preserves_leading_silence_and_trims_at_clip_origin() -> anyhow::Result<()> {
    let timing = timeline::Timing {
        start_ms: 0,
        end_ms: 3000,
        frames: 90,
    };
    let mut pcm = Vec::new();
    timeline::audio(&audio(1000), &timing, &mut pcm)?;
    assert_eq!(&pcm[..60], &[0; 60]);
    assert_eq!(&pcm[60..], &*audio(0).pcm);
    let timing = timeline::Timing {
        start_ms: 1500,
        end_ms: 2500,
        frames: 30,
    };
    pcm.clear();
    let mut voice = audio(1000);
    voice.pcm[..30].fill(0);
    timeline::audio(&voice, &timing, &mut pcm)?;
    assert_eq!(pcm, [1000_i16.to_le_bytes(); 30].concat());
    assert_eq!(timing.tick(15), Duration::from_secs(2));
    let timing = timeline::Timing {
        start_ms: 1500,
        end_ms: 2501,
        frames: 31,
    };
    pcm.clear();
    timeline::audio(&voice, &timing, &mut pcm)?;
    assert_eq!(&pcm[..60], [1000_i16.to_le_bytes(); 30].concat());
    assert_eq!(
        &pcm[60..],
        &[0, 0],
        "Rounding video to a frame must not include voice outside the selected interval"
    );
    Ok(())
}

#[test]
fn captions_clip_and_rebase_word_times_without_exposing_markup() -> anyhow::Result<()> {
    let recording = Recording {
        narration: Narration::Words(vec![Word {
            span: Span::new(Moment::from_millis(100), Moment::from_millis(800)),
            text: "<private>".into(),
        }]),
        ..Recording::default()
    };
    let timing = timeline::Timing {
        start_ms: 300,
        end_ms: 700,
        frames: 12,
    };
    let mut output = Vec::new();
    assert_eq!(timeline::captions(&recording, &timing, &mut output)?, 1);
    assert_eq!(
        String::from_utf8(output)?,
        "1\n00:00:00,000 --> 00:00:00,400\n&lt;private&gt;\n\n"
    );
    let recording = Recording {
        narration: Narration::Words(vec![
            Word {
                span: Span::new(Moment::from_millis(100), Moment::from_millis(200)),
                text: "secret before".into(),
            },
            Word {
                span: Span::new(Moment::from_millis(400), Moment::from_millis(500)),
                text: "safe".into(),
            },
            Word {
                span: Span::new(Moment::from_millis(800), Moment::from_millis(900)),
                text: "secret after".into(),
            },
        ]),
        ..Recording::default()
    };
    let mut output = Vec::new();
    timeline::captions(&recording, &timing, &mut output)?;
    assert_eq!(
        String::from_utf8(output)?,
        "1\n00:00:00,100 --> 00:00:00,200\nsafe\n\n"
    );
    Ok(())
}

#[test]
fn refusal_and_cancellation_never_replace_an_existing_file() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let show = root.path().join("show");
    fs::create_dir(&show)?;
    let destination = root.path().join("kept.mp4");
    fs::write(&destination, b"existing user edit")?;
    assert!(write(&show, &destination, Options::default(), || false).is_err());
    assert_eq!(fs::read(&destination)?, b"existing user edit");
    let destination = root.path().join("cancelled.mp4");
    assert!(write(&show, &destination, Options::default(), || true).is_err());
    assert!(!destination.exists());
    Ok(())
}

fn ffmpeg(args: &[&str], path: &Path) -> anyhow::Result<Vec<u8>> {
    let result = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin"])
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()?;
    anyhow::ensure!(result.status.success(), "Synthetic FFmpeg command failed");
    Ok(result.stdout)
}

fn fixture(root: &Path) -> anyhow::Result<PathBuf> {
    let checkpoint = tempfile::tempdir_in(root)?;
    let directory = root.join("show");
    fs::create_dir(&directory)?;
    let started = Instant::now();
    let (mut metadata, mut geometry) = movie::Metadata::begin(
        checkpoint.path(),
        started,
        started.checked_add(Duration::from_millis(250)).unwrap(),
        [160, 100],
    )?;
    geometry.record(0, [0, 0, 160, 100], [0.0, 0.0, 1.0, 1.0])?;
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=160x100:r=2:d=2",
            "-an",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-bf",
            "0",
            "-g",
            "1",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+frag_keyframe+empty_moov+default_base_moof",
        ],
        &checkpoint.path().join(movie::FILE),
    )?;
    metadata.finish(
        checkpoint.path(),
        started,
        started.checked_add(Duration::from_millis(1750)).unwrap(),
        started.checked_add(Duration::from_millis(2250)).unwrap(),
        4,
        true,
    )?;
    let movie::CheckpointMovie::Present(reading) = movie::checkpoint(checkpoint.path())? else {
        anyhow::bail!("Synthetic movie was not retained");
    };
    movie::retain(&directory, *reading, Moment::from_millis(2250))?;
    fs::write(
        directory.join(Manifest::FILE),
        serde_json::to_vec(&json!({
            "version": 2, "duration": 2250, "surface": {"width": 160, "height": 100},
        "strokes": [[[0.125, 0.5, 600], [0.875, 0.5, 900]]], "pins": [], "actions": [],
            "spaces": {"strokes": [{"generation": 1, "part": [0.0, 0.0, 1.0, 1.0]}], "pins": []},
            "narration": {"words": [{"span": [750, 1500], "text": "Synthetic voice."}]},
            "marks": [], "keyframes": [{"index": 1, "at": 500, "reason": "middle", "event": "synthetic", "marks": [0, 0]}]
        }))?,
    )?;
    let mut log = b"SPKAUD01".to_vec();
    let mut begin = vec![1];
    begin.extend(8000_u32.to_le_bytes());
    begin.extend(750_u64.to_le_bytes());
    begin.extend(b"en");
    let mut samples = vec![2];
    for index in 0..8000 {
        samples.extend(if index % 20 < 10 { 5000_i16 } else { -5000_i16 }.to_le_bytes());
    }
    for payload in [begin, samples, vec![4]] {
        log.extend(u32::try_from(payload.len())?.to_le_bytes());
        log.extend(&payload);
        log.extend(crc32fast::hash(&payload).to_le_bytes());
    }
    fs::write(directory.join("voice.log"), log)?;
    fs::write(
        directory.join("voice.expires"),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs()
            .checked_add(600)
            .unwrap()
            .to_le_bytes(),
    )?;
    Ok(directory)
}

#[test]
#[ignore = "opt-in synthetic FFmpeg export; no microphone, screen, input or clipboard access"]
fn synthetic_ffmpeg_export_preserves_voice_ink_captions_and_clip_timing() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let directory = fixture(root.path())?;
    let original = fs::read(directory.join(movie::FILE))?;
    let destination = root.path().join("voiced.mp4");
    let report = write(
        &directory,
        &destination,
        Options {
            captions: true,
            ..Options::default()
        },
        || false,
    )?;
    assert_eq!(
        (
            report.start_ms,
            report.end_ms,
            report.frames,
            report.captions
        ),
        (250, 2250, 60, 1)
    );
    assert_eq!(report.frames_without_annotations, 0);
    assert!(matches!(report.voice, voice::Status::Available));
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(&destination)
        .output()?;
    assert!(probe.status.success());
    let probe: serde_json::Value = serde_json::from_slice(&probe.stdout)?;
    let streams = probe["streams"].as_array().unwrap();
    assert_eq!(
        streams
            .iter()
            .map(|stream| stream["codec_name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["h264", "aac", "mov_text"]
    );
    let pixels = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&destination)
        .args([
            "-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", "bgra", "pipe:1",
        ])
        .output()?;
    assert!(pixels.status.success());
    let frames = pixels.stdout.as_chunks::<{ 160 * 100 * 4 }>().0;
    assert_eq!(frames.len(), 60);
    let bright = |frame: &[u8]| {
        frame
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[..3].iter().any(|&v| v > 100))
            .count()
    };
    assert_eq!(bright(&frames[0]), 0);
    assert!(bright(&frames[12]) > 0);
    assert!(bright(&frames[24]) > bright(&frames[12]));
    let sound = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&destination)
        .args(["-map", "0:a:0", "-f", "s16le", "-ar", "8000", "pipe:1"])
        .output()?;
    assert!(sound.status.success());
    let samples: Vec<_> = sound
        .stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|sample| i16::from_le_bytes(*sample).unsigned_abs())
        .collect();
    assert!(samples[..3000].iter().all(|&sample| sample < 100));
    assert!(samples[5000..7000].iter().any(|&sample| sample > 2000));
    let clipped = root.path().join("clip.mp4");
    let report = write(
        &directory,
        &clipped,
        Options {
            span: Some(Span::new(
                Moment::from_millis(1000),
                Moment::from_millis(2000),
            )),
            captions: true,
        },
        || false,
    )?;
    assert_eq!(
        (
            report.start_ms,
            report.end_ms,
            report.frames,
            report.captions
        ),
        (1000, 2000, 30, 1)
    );
    assert_eq!(fs::read(directory.join(movie::FILE))?, original);
    let movie = movie::Reading::open(&directory, Moment::from_millis(2250))?;
    let prelude = timeline::Timing::new(&movie.movie, None, Some(&audio(0)))?;
    assert_eq!(
        (prelude.start_ms, prelude.end_ms, prelude.frames),
        (0, 2250, 68)
    );
    let (calls, checks) = async_channel::bounded(2);
    calls.send_blocking(())?;
    calls.send_blocking(())?;
    let cancelled = root.path().join("cancelled.mp4");
    assert!(
        write(&directory, &cancelled, Options::default(), || {
            checks.try_recv().is_err()
        })
        .is_err()
    );
    assert!(!cancelled.exists());
    assert!(!fs::read_dir(root.path())?.any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".speakeasy-video-")
    }));
    Ok(())
}
