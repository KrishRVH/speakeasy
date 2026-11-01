use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::Context as _;
use speakeasy_show::{Annotations, Image, Typeface};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    process::{Child, ChildStdin, ChildStdout},
};

use super::{seconds, timeline::Timing};
use crate::{
    child::{hidden_command, kill_and_reap},
    show::store::movie::{self, Inspected},
};

pub(super) struct Stats {
    pub unpainted: u64,
    pub omitted: usize,
}

fn ffmpeg() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let on_path = std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|path| path.join("ffmpeg").is_file())
        });
        if !on_path
            && let Some(path) = [
                "/opt/homebrew/bin/ffmpeg",
                "/usr/local/bin/ffmpeg",
                "/opt/local/bin/ffmpeg",
            ]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
        {
            return path;
        }
    }
    PathBuf::from("ffmpeg")
}

pub(super) struct Inputs<'a> {
    pub work: &'a Path,
    pub output: &'a Path,
    pub movie: &'a Inspected,
    pub scene: &'a Annotations,
    pub timing: &'a Timing,
    pub rate: Option<u32>,
    pub captions: bool,
}

pub(super) async fn run(
    inputs: Inputs<'_>,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> anyhow::Result<Stats> {
    let Inputs {
        work,
        output,
        movie,
        scene,
        timing,
        rate,
        captions,
    } = inputs;
    let [width, height] = movie.metadata.size;
    let program = ffmpeg();
    let mut decode = hidden_command(&program);
    decode
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(work.join("source.mp4"))
        .args([
            "-map",
            "0:v:0",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "bgra",
            "pipe:1",
        ])
        .stdout(Stdio::piped());
    let mut decoder = decode.spawn().context("Cannot start FFmpeg. Install FFmpeg 5.1 or newer with H.264 and AAC support, make ffmpeg available on PATH, then restart Speakeasy")?;
    let mut command = hidden_command(&program);
    command
        .args([
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "rawvideo",
            "-pixel_format",
            "bgra",
            "-video_size",
        ])
        .arg(format!("{width}x{height}"))
        .arg("-framerate")
        .arg(super::timeline::FPS.to_string())
        .args(["-i", "pipe:0"]);
    if let Some(rate) = rate {
        command
            .args(["-f", "s16le", "-ar"])
            .arg(rate.to_string())
            .args(["-ac", "1", "-i"])
            .arg(work.join("voice.pcm"));
    }
    if captions {
        command
            .args(["-c:s", "text", "-keep_ass_markup", "1", "-i"])
            .arg(work.join("captions.srt"));
    }
    command.args(["-map", "0:v:0"]);
    if rate.is_some() {
        command.args(["-map", "1:a:0", "-c:a", "aac", "-b:a", "128k"]);
    }
    if captions {
        command
            .arg("-map")
            .arg(if rate.is_some() { "2:s:0" } else { "1:s:0" })
            .args(["-c:s", "mov_text", "-disposition:s:0", "default"]);
    }
    command
        .args([
            "-c:v", "libx264", "-threads", "2", "-preset", "fast", "-crf", "18", "-pix_fmt",
            "yuv420p", "-t",
        ])
        .arg(seconds(timing.duration()))
        .args(["-movflags", "+faststart", "-f", "mp4"])
        .arg(output)
        .stdin(Stdio::piped());
    let mut encoder = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            kill_and_reap(&mut decoder).await;
            return Err(error).context("Cannot start the FFmpeg video encoder");
        },
    };
    let result = tokio::select! {
        result = process(&mut decoder, &mut encoder, movie, scene, timing, cancelled) => result,
        () = cancellation(cancelled) => Err(anyhow::anyhow!("Video export cancelled")),
        () = tokio::time::sleep(Duration::from_mins(30)) => Err(anyhow::anyhow!("Video export exceeded 30 minutes; try a shorter interval or free system resources")),
    };
    kill_and_reap(&mut decoder).await;
    kill_and_reap(&mut encoder).await;
    result
}

async fn cancellation(cancelled: &(dyn Fn() -> bool + Sync)) {
    while !cancelled() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn process(
    decoder: &mut Child,
    encoder: &mut Child,
    movie: &Inspected,
    scene: &Annotations,
    timing: &Timing,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> anyhow::Result<Stats> {
    let stdout = decoder
        .stdout
        .take()
        .context("FFmpeg has no video output")?;
    let stdin = encoder
        .stdin
        .take()
        .context("FFmpeg has no encoder input")?;
    let stats = frames(stdout, stdin, movie, scene, timing, cancelled).await?;
    anyhow::ensure!(
        decoder.wait().await?.success(),
        "FFmpeg could not decode the saved video; the original Show is unchanged"
    );
    anyhow::ensure!(
        encoder.wait().await?.success(),
        "FFmpeg could not encode the export; check free space and FFmpeg H.264/AAC support"
    );
    Ok(stats)
}

async fn frames(
    mut input: ChildStdout,
    mut output: ChildStdin,
    movie: &Inspected,
    scene: &Annotations,
    timing: &Timing,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> anyhow::Result<Stats> {
    let [width, height] = movie.metadata.size;
    let length = usize::try_from(
        u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(4),
    )?;
    let mut pixels = vec![0; length];
    let mut samples = movie.prefix.samples().iter().peekable();
    let mut current = None;
    let typeface = Typeface::parse(crate::BRAND_FONT_FILE)?;
    let origin = Duration::from_millis(movie.metadata.origin_ms);
    let through = Duration::from_millis(
        movie
            .metadata
            .coverage
            .as_ref()
            .context("Movie has no saved coverage")?
            .annotations_through_ms,
    );
    let mut stats = Stats {
        unpainted: 0,
        omitted: 0,
    };
    for index in 0..timing.frames {
        anyhow::ensure!(!cancelled(), "Video export cancelled");
        let at = timing.tick(index);
        let source_at = at.saturating_sub(origin);
        while samples
            .peek()
            .is_some_and(|sample| current.is_none() || sample.at <= source_at)
        {
            let sample = samples.next().context("Video sample index changed")?;
            input.read_exact(&mut pixels).await.context(
                "FFmpeg decoded fewer frames than the saved video; check that FFmpeg is version 5.1 or newer. The original Show is unchanged",
            )?;
            current = Some(sample.at);
        }
        let sample = current.context("The saved video has no frames")?;
        let frame = if let Some(viewport) =
            movie::geometry::viewport(movie, movie::decoder_time(sample)).filter(|_| at <= through)
        {
            let mut image = Image::from_bgra(width, height, pixels.clone())?;
            let painted = scene
                .paint(&mut image, at, viewport, &typeface, cancelled)?
                .context("Video export cancelled")?;
            stats.omitted = stats.omitted.max(painted.omitted);
            Cow::Owned(image.into_bgra())
        } else {
            stats.unpainted = stats.unpainted.saturating_add(1);
            Cow::Borrowed(pixels.as_slice())
        };
        output
            .write_all(&frame)
            .await
            .context("FFmpeg stopped accepting video; check free space and H.264 support")?;
    }
    output.shutdown().await?;
    drop(output);
    for _sample in samples {
        input
            .read_exact(&mut pixels)
            .await
            .context("FFmpeg decoded fewer frames than the saved video")?;
    }
    anyhow::ensure!(
        input.read(&mut [0]).await? == 0,
        "FFmpeg decoded more frames than the saved video; export stopped to preserve timing"
    );
    Ok(stats)
}
