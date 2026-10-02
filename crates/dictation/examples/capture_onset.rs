//! Times each stage of opening the default microphone, cold and from a stream built in advance, to
//! show where capture onset goes.
//!
//! Opt-in native acceptance: it opens the real microphone, so macOS asks the terminal for
//! Microphone access, and the menu bar shows the microphone indicator while each run captures. It
//! keeps no audio and prints one JSON line of timings per run.
//!
//! ```sh
//! cargo run --release -p speakeasy-dictation --example capture_onset -- --runs 20
//! ```

use std::{
    env,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use cpal::{
    SampleFormat, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

const FIRST_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5);
/// Lets the device settle between runs, as between separate dictations.
const BETWEEN_RUNS: Duration = Duration::from_millis(750);

fn main() -> anyhow::Result<()> {
    let mut arguments = env::args().skip(1);
    let runs = match (arguments.next().as_deref(), arguments.next()) {
        (None, _) => 10,
        (Some("--runs"), Some(runs)) => runs.parse().context("--runs needs a count")?,
        _ => bail!("Usage: capture_onset [--runs COUNT]"),
    };
    for run in 0..runs {
        cold(run)?;
        thread::sleep(BETWEEN_RUNS);
        prepared(run)?;
        thread::sleep(BETWEEN_RUNS);
    }
    Ok(())
}

/// Every stage a dictation runs after the shortcut, timed from its start.
fn cold(run: u32) -> anyhow::Result<()> {
    let started = Instant::now();
    let host = cpal::default_host();
    let host_ready = started.elapsed();
    let device = host.default_input_device().context("No microphone found")?;
    let device_ready = started.elapsed();
    let config = device.default_input_config()?;
    let configured = started.elapsed();
    let (stream, first_callback) = build(&device, &config)?;
    let built = started.elapsed();
    stream.play()?;
    let playing = started.elapsed();
    let (arrived, frames) = first_callback.recv_timeout(FIRST_CALLBACK_TIMEOUT)?;
    let teardown = Instant::now();
    drop(stream);
    println!(
        r#"{{"mode":"cold","run":{run},"host_ms":{},"device_ms":{},"config_ms":{},"build_ms":{},"play_ms":{},"first_callback_ms":{},"teardown_ms":{},"rate":{},"channels":{},"first_samples":{frames}}}"#,
        milliseconds(host_ready),
        milliseconds(device_ready),
        milliseconds(configured),
        milliseconds(built),
        milliseconds(playing),
        milliseconds(arrived.saturating_duration_since(started)),
        milliseconds(teardown.elapsed()),
        config.sample_rate(),
        config.channels(),
    );
    Ok(())
}

/// Only starting a stream that was built before the shortcut, as a prepared device would.
fn prepared(run: u32) -> anyhow::Result<()> {
    let device = cpal::default_host()
        .default_input_device()
        .context("No microphone found")?;
    let config = device.default_input_config()?;
    let (stream, first_callback) = build(&device, &config)?;
    let started = Instant::now();
    stream.play()?;
    let playing = started.elapsed();
    let (arrived, frames) = first_callback.recv_timeout(FIRST_CALLBACK_TIMEOUT)?;
    println!(
        r#"{{"mode":"prepared","run":{run},"play_ms":{},"first_callback_ms":{},"first_samples":{frames}}}"#,
        milliseconds(playing),
        milliseconds(arrived.saturating_duration_since(started)),
    );
    Ok(())
}

/// A stream whose first callback reports when it ran and how many samples it carried.
fn build(
    device: &cpal::Device,
    config: &SupportedStreamConfig,
) -> anyhow::Result<(cpal::Stream, mpsc::Receiver<(Instant, usize)>)> {
    if config.sample_format() != SampleFormat::F32 {
        bail!(
            "This probe reads float input; the device offers {}",
            config.sample_format()
        );
    }
    let (first, arrived) = mpsc::sync_channel(1);
    let mut reported = false;
    let data = move |samples: &[f32], _: &cpal::InputCallbackInfo| {
        if !reported {
            reported = true;
            // The one-slot channel is preallocated, so the callback never allocates or blocks.
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Only the first callback reports; a probe that already gave up has no receiver"
            )]
            let _ = first.try_send((Instant::now(), samples.len()));
        }
    };
    let stream = device.build_input_stream(config.config(), data, |_| {}, None)?;
    Ok((stream, arrived))
}

fn milliseconds(duration: Duration) -> String {
    format!("{:.3}", duration.as_secs_f64() * 1_000.0)
}
