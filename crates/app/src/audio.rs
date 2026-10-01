mod control;
use crate::runtime::{Event, SessionId};
use anyhow::{Context, bail};
use control::{Control, Mode};
use cpal::{
    SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};
use speakeasy_core::gesture::RECORDING_LIMIT;
use std::{
    num::NonZeroUsize,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

/// The session owner must retire capture explicitly to await native teardown.
pub(crate) struct Capture {
    control: Arc<Control>,
    finished: async_channel::Receiver<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Capture {
    pub(crate) fn start(
        id: SessionId,
        microphone: Option<String>,
        tx: async_channel::Sender<Event>,
    ) -> anyhow::Result<Self> {
        Self::spawn(id, tx, move |tx, control| {
            record(id, microphone.as_deref(), tx, control)
        })
    }

    fn spawn(
        id: SessionId,
        tx: async_channel::Sender<Event>,
        work: impl FnOnce(
            &async_channel::Sender<Event>,
            &Arc<Control>,
        ) -> anyhow::Result<Option<Vec<u8>>>
        + Send
        + 'static,
    ) -> anyhow::Result<Self> {
        let control = Arc::new(Control::default());
        let worker_control = control.clone();
        let (completed, finished) = async_channel::bounded(1);
        let thread = thread::Builder::new()
            .name("microphone".into())
            .spawn(move || {
                let result = work(&tx, &worker_control);
                if tx.send_blocking(Event::AudioDone(id, result)).is_err() {
                    return; // A gone owner observes this completion lane close.
                }
                // Publish completion after native teardown and the event send.
                // A panic closes this lane, waking retirement as well.
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "The one-shot completion may only be closed by an owner that no longer needs retirement"
                )]
                let _ = completed.try_send(());
            })?;
        Ok(Self {
            control,
            finished,
            thread: Some(thread),
        })
    }
    pub(crate) fn finish(&self) {
        self.control.finish();
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }
    pub(crate) fn cancel(&self) {
        self.control.cancel();
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }

    pub(crate) fn retire(mut self) -> impl std::future::Future<Output = ()> + Send + 'static {
        self.cancel();
        async move {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Either completion or sender closure after a worker panic permits reaping the native thread"
            )]
            let _ = self.finished.recv().await;
            if let Some(thread) = self.thread.take() {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "Joining guarantees teardown even when the worker panicked; retirement has no remaining session to fail"
                )]
                let _ = thread.join();
            }
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        // Emergency cancellation only; explicit retirement owns the join so
        // dropping an owner never waits for a native driver on the input path.
        self.cancel();
    }
}

fn startup_stopped(control: &Control, tx: &async_channel::Sender<Event>) -> bool {
    control.mode() != Mode::Recording || tx.is_closed()
}

fn record(
    id: SessionId,
    microphone: Option<&str>,
    tx: &async_channel::Sender<Event>,
    control: &Arc<Control>,
) -> anyhow::Result<Option<Vec<u8>>> {
    if startup_stopped(control, tx) {
        return Ok(None);
    }
    let host = cpal::default_host();
    if startup_stopped(control, tx) {
        return Ok(None);
    }
    let device = if let Some(id) = microphone {
        let mut selected = None;
        for device in host.input_devices()? {
            if startup_stopped(control, tx) {
                return Ok(None);
            }
            if device.id().is_ok_and(|actual| actual.to_string() == id) {
                selected = Some(device);
                break;
            }
        }
        if startup_stopped(control, tx) {
            return Ok(None);
        }
        selected.context(
            "Selected microphone is disconnected. Reconnect it or choose another in Settings.",
        )?
    } else {
        host.default_input_device()
            .context("No microphone found. Connect a microphone and try again.")?
    };
    if startup_stopped(control, tx) {
        return Ok(None);
    }
    let format = device.default_input_config().map_err(microphone_error)?;
    if startup_stopped(control, tx) {
        return Ok(None);
    }
    let rate = format.sample_rate();
    let channels = NonZeroUsize::new(usize::from(format.channels()))
        .context("Unsupported microphone format: no input channels")?;
    if rate == 0 || rate > 192_000 || channels.get() > 32 {
        bail!("Unsupported microphone format");
    }
    let sample_rate = usize::try_from(rate)?;
    let (producer, mut consumer) = RingBuffer::new(sample_rate);
    let (errors, stream_errors) = async_channel::bounded(1);
    let started = Instant::now();
    let limit = sample_rate
        .checked_mul(usize::try_from(RECORDING_LIMIT.as_secs())?)
        .context("Microphone recording limit is unsupported")?;
    let stream = match format.sample_format() {
        SampleFormat::F32 => stream::<f32>(
            &device,
            format.config(),
            channels,
            producer,
            control.clone(),
            errors,
            started,
            limit,
        ),
        SampleFormat::I16 => stream::<i16>(
            &device,
            format.config(),
            channels,
            producer,
            control.clone(),
            errors,
            started,
            limit,
        ),
        SampleFormat::U16 => stream::<u16>(
            &device,
            format.config(),
            channels,
            producer,
            control.clone(),
            errors,
            started,
            limit,
        ),
        _ => bail!("Microphone sample format is unsupported. Choose a standard PCM microphone."),
    }?;
    if startup_stopped(control, tx) {
        return Ok(None);
    }
    // Keep mono PCM at the device's native rate; the local engine resamples it.
    // Reserve for an ordinary utterance; long recordings grow on this consumer
    // thread, never in the real-time callback.
    let mut pcm = Vec::with_capacity(sample_rate.saturating_mul(20).saturating_add(44));
    pcm.resize(44, 0_u8);
    if startup_stopped(control, tx) {
        return Ok(None);
    }
    stream.play().map_err(microphone_error)?;
    let mut stream = Some(stream);
    let mut ready = false;
    let mut energy = 0.0_f64;
    let mut count = 0_u32;
    let mut last_level = Instant::now();
    loop {
        if control.mode() == Mode::Cancelled || tx.is_closed() {
            pcm.fill(0_u8);
            return Ok(None);
        }
        if let Err(error) = capture_failure(control, &stream_errors) {
            pcm.fill(0_u8);
            return Err(error);
        }
        if control.mode() == Mode::Finishing
            || started.elapsed() >= RECORDING_LIMIT
            || pcm.len().saturating_sub(44) / 2 >= limit
        {
            control.finish();
            drop(stream.take());
        }
        if !ready && !consumer.is_empty() {
            ready = true;
            tx.send_blocking(Event::Ready(id))?;
        }
        let (added_energy, added_count) = consume_pcm(&mut consumer, &mut pcm, limit);
        energy += added_energy;
        count = count.saturating_add(added_count);
        if last_level.elapsed() >= Duration::from_millis(32) {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "The display meter intentionally converts bounded normalized RMS to f32; PCM remains independently encoded"
            )]
            let rms = (energy / f64::from(count.max(1))).sqrt() as f32;
            // Speech meter spans -60 to -6 dBFS; the top is a speech reference,
            // rather than a clipping indicator at 0 dBFS.
            let level = (20.0f32.mul_add(rms.max(0.000_001).log10(), 60.0) / 54.0).clamp(0.0, 1.0);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Meter updates are expendable presentation; a full or closed event lane must not interrupt audio capture"
            )]
            let _ = tx.try_send(Event::Level(id, level));
            energy = 0.0;
            count = 0;
            last_level = Instant::now();
        }
        if stream.is_none() {
            break;
        }
        // Finish/cancel can wake this owned consumer immediately. An unpark
        // arriving before the wait remains pending; audio polling stays bounded.
        thread::park_timeout(Duration::from_millis(5));
    }
    if control.mode() == Mode::Cancelled {
        pcm.fill(0);
        return Ok(None);
    }
    // A callback may have failed between the last poll and stream teardown.
    if let Err(error) = capture_failure(control, &stream_errors) {
        pcm.fill(0);
        return Err(error);
    }
    if pcm.len().saturating_sub(44) / 2 < sample_rate / 5 || !trim_quiet_edges(&mut pcm, rate) {
        pcm.fill(0); // Best effort for this owned buffer, as on cancellation.
        return Ok(None);
    }
    wave(pcm, rate).map(Some)
}

fn consume_pcm(consumer: &mut Consumer<f32>, pcm: &mut Vec<u8>, limit: usize) -> (f64, u32) {
    let mut energy = 0.0;
    let mut count = 0_u32;
    let Ok(chunk) = consumer.read_chunk(consumer.slots()) else {
        return (energy, count);
    };
    let (first, second) = chunk.as_slices();
    for &sample in first.iter().chain(second) {
        if pcm.len().saturating_sub(44) / 2 < limit {
            let sample = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            energy += f64::from(sample * sample);
            count = count.saturating_add(1_u32);
            if pcm.len().saturating_add(2) > pcm.capacity() {
                let capacity = (pcm.capacity().saturating_mul(2))
                    .min(limit.saturating_mul(2).saturating_add(44));
                pcm.reserve_exact(capacity.saturating_sub(pcm.len()));
            }
            #[expect(
                clippy::cast_possible_truncation,
                reason = "A finite sample clamped to [-1, 1] intentionally quantizes to signed PCM16 without overflow"
            )]
            let encoded = (sample * 32_767.0) as i16;
            pcm.extend_from_slice(&encoded.to_le_bytes());
        }
    }
    chunk.commit_all();
    (energy, count)
}

#[expect(
    clippy::too_many_arguments,
    reason = "The callback captures explicit device and session ownership once at stream construction"
)]
fn stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: NonZeroUsize,
    producer: Producer<f32>,
    control: Arc<Control>,
    errors: async_channel::Sender<cpal::Error>,
    started: Instant,
    limit: usize,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let (mut data, error) =
        capture_callbacks::<T>(channels, producer, control, errors, started, limit);
    device
        .build_input_stream(config, move |samples, _| data(samples), error, None)
        .map_err(microphone_error)
}

// The callback pair owns one capture. Keeping it independent of device creation
// lets tests replay native callback ordering without opening a microphone.
fn capture_callbacks<T>(
    channels: NonZeroUsize,
    mut producer: Producer<f32>,
    control: Arc<Control>,
    errors: async_channel::Sender<cpal::Error>,
    started: Instant,
    limit: usize,
) -> (
    impl FnMut(&[T]) + Send + 'static,
    impl FnMut(cpal::Error) + Send + 'static,
)
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let mut samples = 0_usize;
    // This capture-local flag controls whether a discontinuity may discard
    // speech already queued by the data callback. It never publishes UI state.
    let active = control.clone();
    (
        move |data: &[T]| {
            if control.mode() != Mode::Recording || started.elapsed() >= RECORDING_LIMIT {
                return;
            }
            let frames = (data.len() / channels).min(limit.saturating_sub(samples));
            let queued = frames.min(producer.slots());
            let Ok(chunk) = producer.write_chunk_uninit(queued) else {
                control.overrun();
                return;
            };
            // This safe rtrb operation initializes and publishes a whole
            // callback packet once, without scratch buffers or allocation.
            chunk.fill_from_iter(data.chunks_exact(channels.get()).take(queued).map(|frame| {
                frame
                    .iter()
                    .map(|sample| sample.to_sample::<f32>())
                    .sum::<f32>()
                    / channels.get() as f32
            }));
            if queued > 0 && samples == 0 {
                control.start();
            }
            samples = samples.saturating_add(queued);
            if queued < frames {
                control.overrun();
            }
        },
        move |error| handle_stream_error(error, &errors, active.started()),
    )
}

fn handle_stream_error(
    error: cpal::Error,
    errors: &async_channel::Sender<cpal::Error>,
    audio_started: bool,
) {
    // WASAPI can report a discontinuity before its first packet. No previously
    // queued speech can be lost then. Once samples have arrived, stop on Xrun
    // rather than silently transcribing an utterance with potentially lost words.
    if (error.kind() == cpal::ErrorKind::Xrun && !audio_started)
        || matches!(
            error.kind(),
            cpal::ErrorKind::RealtimeDenied | cpal::ErrorKind::DeviceChanged
        )
    {
        return;
    }
    // Keep the first fatal error without blocking or formatting on the callback.
    #[expect(
        clippy::let_underscore_must_use,
        reason = "A full single-slot lane already retains the first fatal error; a closed lane has no recording owner"
    )]
    let _ = errors.try_send(error);
}

fn capture_failure(
    control: &Control,
    errors: &async_channel::Receiver<cpal::Error>,
) -> anyhow::Result<()> {
    if control.overran() {
        bail!(
            "Recording buffer filled because capture could not keep up. Reduce system load and try again."
        );
    }
    if let Ok(error) = errors.try_recv() {
        return Err(microphone_error(error));
    }
    Ok(())
}

fn microphone_error(error: cpal::Error) -> anyhow::Error {
    let guidance = match error.kind() {
        cpal::ErrorKind::DeviceBusy => {
            "Close other apps using the microphone or choose another microphone in Settings."
        },
        cpal::ErrorKind::PermissionDenied => "Check OS microphone permission and try again.",
        cpal::ErrorKind::DeviceNotAvailable => {
            "Reconnect the microphone or choose another in Settings."
        },
        cpal::ErrorKind::Xrun => {
            "Audio was interrupted during recording. Check your audio routing or reduce system load, then try again."
        },
        _ => "Try recording again or choose another microphone in Settings.",
    };
    anyhow::anyhow!("Microphone failed ({:?}): {error} {guidance}", error.kind())
}

// Use 20 ms energy windows, at least 100 ms audible audio, and 500 ms
// padding when a quiet edge exceeds a second.
// This is conservative edge trimming, not VAD; all interior pauses remain.
fn trim_quiet_edges(pcm: &mut Vec<u8>, rate: u32) -> bool {
    let Ok(rate) = usize::try_from(rate) else {
        return false;
    };
    if rate == 0 || rate > 192_000 {
        return false;
    }
    let bytes_per_second = rate.saturating_mul(2);
    let padding = (rate / 2).saturating_mul(2); // Keep a whole PCM sample.
    let window_bytes = (rate / 50).max(1).saturating_mul(2);
    let Some(audio) = pcm.get(44..) else {
        return false;
    };
    let mut first = None;
    let mut end = 0;
    let mut audible = 0_usize;
    let mut offset = 0_usize;
    for window in audio.chunks(window_bytes) {
        let energy = window
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let sample = i16::from_le_bytes(*pair);
                u64::from(sample.unsigned_abs()).pow(2)
            })
            // A 20 ms window at the 192 kHz cap contains at most 3840 samples:
            // its squared sum is below 2^42. Wrapping cannot occur; this form
            // lets the compiler vectorize the exact integer reduction.
            .fold(0_u64, u64::wrapping_add);
        if energy as f64 / (32768.0 * 32768.0) >= 0.003_f64.powi(2) * (window.len() / 2) as f64 {
            first.get_or_insert(offset);
            end = offset.saturating_add(window.len());
            audible = audible.saturating_add(window.len() / 2);
        }
        offset = offset.saturating_add(window.len());
    }
    let Some(first) = first.filter(|_| audible >= (rate / 10).max(1)) else {
        return false;
    };
    let start = if first >= bytes_per_second {
        first.saturating_sub(padding)
    } else {
        0
    };
    if audio.len().saturating_sub(end) >= bytes_per_second {
        end = end.saturating_add(padding);
    } else {
        end = audio.len();
    }
    if start != 0 {
        pcm.copy_within(
            44_usize.saturating_add(start)..44_usize.saturating_add(end),
            44,
        );
    }
    pcm.truncate(44_usize.saturating_add(end.saturating_sub(start)));
    compact_trimmed_pcm(pcm);
    true
}

fn compact_trimmed_pcm(pcm: &mut Vec<u8>) {
    // Keep ordinary utterances allocation-free after trimming. Reclaim only
    // large reservations whose remaining audio occupies at most a quarter.
    if pcm.capacity().saturating_sub(pcm.len()) >= 8 * 1024 * 1024
        && pcm.capacity() / 4 >= pcm.len()
    {
        pcm.shrink_to_fit();
    }
}

pub(crate) fn wave(mut pcm: Vec<u8>, rate: u32) -> anyhow::Result<Vec<u8>> {
    let Some((header, audio)) = pcm.split_at_mut_checked(44) else {
        pcm.fill(0);
        bail!("Recording header is incomplete. Try recording again.");
    };
    let metadata = (|| {
        let bytes = u32::try_from(audio.len()).ok()?;
        if rate == 0 || !bytes.is_multiple_of(2) {
            return None;
        }
        Some((bytes, bytes.checked_add(36)?, rate.checked_mul(2)?))
    })();
    let Some((bytes, chunk_bytes, byte_rate)) = metadata else {
        header.fill(0);
        audio.fill(0);
        bail!(
            "Recording format is unsupported. Choose another microphone in Settings and try again."
        );
    };
    let mut wav = Vec::with_capacity(44);
    wav.extend(b"RIFF");
    wav.extend(chunk_bytes.to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16_u32.to_le_bytes());
    wav.extend(1_u16.to_le_bytes());
    wav.extend(1_u16.to_le_bytes());
    wav.extend(rate.to_le_bytes());
    wav.extend(byte_rate.to_le_bytes());
    wav.extend(2_u16.to_le_bytes());
    wav.extend(16_u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(bytes.to_le_bytes());
    header.copy_from_slice(&wav);
    Ok(pcm)
}

/// Lists input devices without opening a stream. Drivers may block, and CPAL
/// leaves its calling thread in a single-threaded COM apartment, which a shared
/// executor thread must not keep. Enumeration runs on its own short-lived thread.
pub(crate) async fn microphones() -> anyhow::Result<Vec<(String, String)>> {
    let (tx, rx) = async_channel::bounded(1);
    thread::Builder::new()
        .name("microphones".into())
        .spawn(move || {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Closing the one-shot receiver means the settings owner no longer needs the device list"
            )]
            let _ = tx.send_blocking(enumerate_microphones());
        })?;
    rx.recv().await?
}

fn enumerate_microphones() -> anyhow::Result<Vec<(String, String)>> {
    cpal::default_host()
        .input_devices()?
        .map(|device| {
            Ok((
                device.id()?.to_string(),
                device.description()?.name().to_owned(),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_encodes_pcm16_in_place_with_exact_riff_lengths() -> anyhow::Result<()> {
        let mut pcm = vec![0xff; 44];
        let samples = [i16::MIN, 0, i16::MAX];
        for sample in samples {
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
        let allocation = pcm.as_ptr();
        let encoded = wave(pcm, 16_000)?;
        assert_eq!(encoded.as_ptr(), allocation);
        assert_eq!(
            &encoded[..44],
            &[
                b'R', b'I', b'F', b'F', 42, 0, 0, 0, b'W', b'A', b'V', b'E', b'f', b'm', b't',
                b' ', 16, 0, 0, 0, 1, 0, 1, 0, 0x80, 0x3e, 0, 0, 0, 0x7d, 0, 0, 2, 0, 16, 0, b'd',
                b'a', b't', b'a', 6, 0, 0, 0,
            ]
        );
        assert_eq!(&encoded[44..], &[0, 0x80, 0, 0, 0xff, 0x7f]);
        Ok(())
    }

    #[test]
    fn wave_rejects_incomplete_headers_partial_samples_and_invalid_rates() {
        for (length, rate) in [(43, 16_000), (45, 16_000), (46, 0), (46, u32::MAX)] {
            let error = wave(vec![0x5a; length], rate).unwrap_err().to_string();
            assert!(error.contains("Try recording again") || error.contains("try again"));
        }
    }

    #[test]
    fn stopped_startup_returns_without_audio() -> anyhow::Result<()> {
        for mode in [Mode::Finishing, Mode::Cancelled] {
            let control = Arc::new(Control::default());
            if mode == Mode::Finishing {
                control.finish();
            } else {
                control.cancel();
            }
            let (events, _audio) = async_channel::bounded(1);
            assert!(record(SessionId::FIRST, None, &events, &control)?.is_none());
        }
        let (events, _audio) = async_channel::bounded(1);
        events.close();
        assert!(
            record(
                SessionId::FIRST,
                None,
                &events,
                &Arc::new(Control::default())
            )?
            .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn capture_retirement_signals_cancel_and_waits_for_teardown() -> anyhow::Result<()> {
        let (events, _audio) = async_channel::bounded(1);
        let (started, starting) = async_channel::bounded(1);
        let (release, waiting) = async_channel::bounded(1);
        let capture = Capture::spawn(SessionId::FIRST, events, move |_, control| {
            started.send_blocking(control.clone())?;
            waiting.recv_blocking()?;
            Ok(None)
        })?;
        let control = starting.recv().await?;
        let retirement = capture.retire();
        assert_eq!(control.mode(), Mode::Cancelled);
        tokio::pin!(retirement);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), retirement.as_mut())
                .await
                .is_err()
        );
        release.send(()).await?;
        tokio::time::timeout(Duration::from_secs(2), retirement).await?;
        Ok(())
    }

    #[tokio::test]
    async fn finish_and_retirement_wake_the_owned_consumer_even_before_it_parks()
    -> anyhow::Result<()> {
        let mut missed = Vec::new();
        for (cancel, wake_before_park) in
            [(false, false), (false, true), (true, false), (true, true)]
        {
            if !consumer_wakes(cancel, wake_before_park).await? {
                missed.push((cancel, wake_before_park));
            }
        }
        assert!(
            missed.is_empty(),
            "Capture wake missed (cancel, before park): {missed:?}"
        );
        Ok(())
    }

    #[expect(
        clippy::disallowed_types,
        reason = "A test-only condition variable forces native-thread park ordering without microphone access"
    )]
    async fn consumer_wakes(cancel: bool, wake_before_park: bool) -> anyhow::Result<bool> {
        use std::sync::{Condvar, Mutex};

        let (events, audio) = async_channel::bounded(1);
        let (armed, arming) = async_channel::bounded(1);
        let gate = Arc::new((Mutex::new(!wake_before_park), Condvar::new()));
        let waiting = gate.clone();
        let capture = Capture::spawn(SessionId::FIRST, events, move |_, control| {
            // Arm after reading the control, so a missing notification
            // cannot pass by finishing before the worker enters its wait.
            anyhow::ensure!(control.mode() == Mode::Recording);
            armed.send_blocking(thread::current())?;
            let (lock, ready) = &*waiting;
            let guard = lock
                .lock()
                .map_err(|error| anyhow::anyhow!("Capture fixture gate was poisoned: {error}"))?;
            drop(
                ready
                    .wait_while(guard, |released| !*released)
                    .map_err(|error| {
                        anyhow::anyhow!("Capture fixture gate was poisoned: {error}")
                    })?,
            );
            thread::park_timeout(Duration::from_secs(5));
            while control.mode() == Mode::Recording {
                thread::park_timeout(Duration::from_secs(5));
            }
            Ok(None)
        })?;
        let worker = tokio::time::timeout(Duration::from_secs(2), arming.recv()).await??;
        let release = || -> anyhow::Result<()> {
            let (lock, ready) = &*gate;
            *lock
                .lock()
                .map_err(|error| anyhow::anyhow!("Capture fixture gate was poisoned: {error}"))? =
                true;
            ready.notify_one();
            Ok(())
        };
        if cancel {
            let retirement = capture.retire();
            tokio::pin!(retirement);
            release()?;
            let completed =
                tokio::time::timeout(Duration::from_millis(400), retirement.as_mut()).await;
            // Reap even when exercising a broken notification path.
            worker.unpark();
            if completed.is_err() {
                tokio::time::timeout(Duration::from_secs(2), retirement).await?;
            }
            if completed.is_err() {
                return Ok(false);
            }
        } else {
            capture.finish();
            assert_eq!(capture.control.mode(), Mode::Finishing);
            release()?;
            let completed = tokio::time::timeout(Duration::from_millis(400), audio.recv()).await;
            worker.unpark();
            tokio::time::timeout(Duration::from_secs(2), capture.retire()).await?;
            if !matches!(
                completed,
                Ok(Ok(Event::AudioDone(SessionId::FIRST, Ok(None))))
            ) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[tokio::test]
    async fn closing_audio_events_unblocks_owned_capture_retirement() -> anyhow::Result<()> {
        let (events, audio) = async_channel::bounded(1);
        events.try_send(Event::Ready(SessionId::FIRST))?;
        let capture = Capture::spawn(SessionId::FIRST, events, |_, _| Ok(None))?;
        let retirement = capture.retire();
        tokio::pin!(retirement);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), retirement.as_mut())
                .await
                .is_err()
        );
        audio.close();
        tokio::time::timeout(Duration::from_secs(2), retirement).await?;
        Ok(())
    }

    #[tokio::test]
    async fn retirement_wakes_when_a_worker_panics_before_reporting_completion()
    -> anyhow::Result<()> {
        let (events, _) = async_channel::bounded(1);
        let capture = Capture::spawn(SessionId::FIRST, events, |_, _| {
            std::panic::resume_unwind(Box::new(()));
        })?;
        tokio::time::timeout(Duration::from_secs(2), capture.retire()).await?;
        Ok(())
    }

    #[test]
    fn pcm_chunks_wrap_clamp_invalid_samples_and_enforce_the_limit() -> anyhow::Result<()> {
        let (mut producer, mut consumer) = RingBuffer::new(4);
        producer.push_entire_slice(&[0.0, 0.0, 0.0])?;
        consumer.read_chunk(3)?.commit_all();
        producer.push_entire_slice(&[0.5, f32::NAN, 2.0, -2.0])?;
        let mut pcm = vec![0; 44];
        let (energy, count) = consume_pcm(&mut consumer, &mut pcm, 3);
        assert_eq!(count, 3);
        assert_eq!(energy, 1.25);
        let expected = [16383_i16, 0, 32767]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(&pcm[44..], expected);
        assert!(consumer.is_empty());
        assert_eq!(producer.slots(), 4);
        Ok(())
    }

    #[test]
    #[ignore = "opt-in synthetic release profiling; never opens a microphone"]
    fn profile_audio_pipeline() -> anyhow::Result<()> {
        use std::hint::black_box;
        for channels in [1, 2, 8] {
            let mut timings = Vec::new();
            for _ in 0..21 {
                let (producer, mut consumer) = RingBuffer::new(48_000);
                let (errors, receiver) = async_channel::bounded(1);
                let control = Arc::new(Control::default());
                let (mut data, _) = capture_callbacks::<f32>(
                    NonZeroUsize::new(channels).context("Nonzero fixture channels")?,
                    producer,
                    control.clone(),
                    errors,
                    Instant::now(),
                    480 * 3000,
                );
                let samples = vec![0.125; 480 * channels];
                let mut pcm = vec![0; 44];
                pcm.reserve(960);
                let started = Instant::now();
                for _ in 0..3000 {
                    data(black_box(&samples));
                    black_box(consume_pcm(&mut consumer, &mut pcm, 480));
                    black_box(&pcm);
                    pcm.truncate(44);
                }
                timings.push(started.elapsed().as_secs_f64() * 1e6 / 3000.0);
                capture_failure(&control, &receiver)?;
            }
            timings.sort_by(f64::total_cmp);
            eprintln!(
                "audio_pipeline: channels={channels} frames=480 median_us={:.3} p95_us={:.3} runs=21 batches_per_run=3000",
                timings[10], timings[19]
            );
        }
        let mut fixture = vec![0; 44 + 48_000 * 300 * 2];
        for pair in fixture[44 + 96_000..44 + 96_000 * 299]
            .as_chunks_mut::<2>()
            .0
        {
            pair.copy_from_slice(&3000_i16.to_le_bytes());
        }
        let mut timings = Vec::new();
        for _ in 0..21 {
            let mut pcm = fixture.clone();
            let started = Instant::now();
            assert!(trim_quiet_edges(black_box(&mut pcm), 48_000));
            timings.push(started.elapsed().as_secs_f64() * 1000.0);
            black_box(pcm);
        }
        timings.sort_by(f64::total_cmp);
        eprintln!(
            "audio_trim: seconds=300 median_ms={:.3} p95_ms={:.3} runs=21",
            timings[10], timings[19]
        );
        Ok(())
    }

    #[test]
    fn startup_xrun_is_allowed_but_an_interruption_after_samples_is_fatal() -> anyhow::Result<()> {
        let (producer, mut consumer) = RingBuffer::new(4);
        let (errors, receiver) = async_channel::bounded(1);
        let control = Arc::new(Control::default());
        let (mut data, mut error) = capture_callbacks::<f32>(
            NonZeroUsize::new(2).context("Stereo fixture")?,
            producer,
            control.clone(),
            errors,
            Instant::now(),
            4,
        );
        error(cpal::ErrorKind::Xrun.into());
        assert!(capture_failure(&control, &receiver).is_ok());
        data(&[0.25, 0.75]);
        assert_eq!(consumer.pop()?, 0.5);
        error(cpal::ErrorKind::Xrun.into());
        let failure = capture_failure(&control, &receiver)
            .err()
            .context("An interruption after samples must not silently discard speech")?;
        assert!(failure.to_string().contains("Xrun"));
        Ok(())
    }

    #[test]
    fn callback_overflow_is_fatal_but_recording_limit_and_cancellation_do_not_overflow()
    -> anyhow::Result<()> {
        for limit in [2, 3] {
            let (producer, mut consumer) = RingBuffer::new(2);
            let (errors, receiver) = async_channel::bounded(1);
            let control = Arc::new(Control::default());
            let (mut data, _) = capture_callbacks::<f32>(
                NonZeroUsize::MIN,
                producer,
                control.clone(),
                errors,
                Instant::now(),
                limit,
            );
            data(&[0.25, 0.5, 0.75]);
            assert_eq!(capture_failure(&control, &receiver).is_err(), limit == 3);
            assert_eq!(consumer.pop()?, 0.25);
            assert_eq!(consumer.pop()?, 0.5);
            control.cancel();
            data(&[1.0]);
            assert!(consumer.pop().is_err());
        }
        Ok(())
    }

    #[test]
    fn recoverable_stream_notifications_do_not_abort_recording() {
        let (errors, receiver) = async_channel::bounded(1);
        for kind in [
            cpal::ErrorKind::RealtimeDenied,
            cpal::ErrorKind::DeviceChanged,
        ] {
            for active in [false, true] {
                handle_stream_error(kind.into(), &errors, active);
                assert!(receiver.try_recv().is_err(), "{kind} aborted capture");
                assert!(capture_failure(&Control::default(), &receiver).is_ok());
            }
        }
    }

    #[test]
    fn fatal_stream_errors_keep_the_first_cause_without_blocking() -> anyhow::Result<()> {
        let (errors, receiver) = async_channel::bounded(1);
        handle_stream_error(
            cpal::Error::with_message(cpal::ErrorKind::DeviceBusy, "fixture device is busy"),
            &errors,
            false,
        );
        for _ in 0..10 {
            handle_stream_error(cpal::ErrorKind::Xrun.into(), &errors, false);
            handle_stream_error(cpal::ErrorKind::DeviceNotAvailable.into(), &errors, false);
        }
        let error = capture_failure(&Control::default(), &receiver)
            .err()
            .context("fatal driver error must abort recording")?
            .to_string();
        assert!(error.contains("DeviceBusy"));
        assert!(error.contains("fixture device is busy"));
        assert!(error.contains("Close other apps"));
        assert!(!error.contains("buffer"));
        Ok(())
    }

    #[test]
    fn device_loss_and_backend_failures_abort_recording() -> anyhow::Result<()> {
        for kind in [
            cpal::ErrorKind::DeviceNotAvailable,
            cpal::ErrorKind::StreamInvalidated,
            cpal::ErrorKind::PermissionDenied,
            cpal::ErrorKind::BackendError,
        ] {
            let (errors, receiver) = async_channel::bounded(1);
            handle_stream_error(
                cpal::Error::with_message(kind, "fixture driver failure"),
                &errors,
                false,
            );
            let error = capture_failure(&Control::default(), &receiver)
                .err()
                .context("fatal driver error must abort recording")?
                .to_string();
            assert!(error.contains(&format!("{kind:?}")));
            assert!(error.contains("fixture driver failure"));
        }
        Ok(())
    }

    #[test]
    fn recording_buffer_overflow_has_its_own_actionable_error() -> anyhow::Result<()> {
        let (_errors, receiver) = async_channel::bounded(1);
        let control = Control::default();
        control.overrun();
        let error = capture_failure(&control, &receiver)
            .err()
            .context("application ring overflow must abort recording")?
            .to_string();
        assert!(error.contains("Recording buffer filled"));
        assert!(error.contains("Reduce system load"));
        Ok(())
    }

    #[test]
    fn quiet_edges_keep_word_padding_and_interior_pauses_but_clicks_are_rejected() {
        let rate = 16_000;
        let mut pcm = vec![0; 44];
        for (seconds, amplitude) in [(2, 0_i16), (1, 3000), (3, 0), (1, 3000), (2, 0)] {
            for _ in 0..rate * seconds {
                pcm.extend_from_slice(&amplitude.to_le_bytes());
            }
        }
        let expected = pcm[44 + rate * 3..44 + rate * 15].to_vec();
        assert!(trim_quiet_edges(&mut pcm, u32::try_from(rate).unwrap()));
        assert_eq!(&pcm[44..], expected);

        let mut click = vec![0; 44 + rate * 2];
        for pair in click[44..44 + rate / 10].as_chunks_mut::<2>().0 {
            pair.copy_from_slice(&3000_i16.to_le_bytes());
        }
        assert!(!trim_quiet_edges(&mut click, u32::try_from(rate).unwrap()));
    }

    #[test]
    fn sparse_long_recording_releases_capacity_without_changing_padded_audio() {
        let rate = 48_000;
        let mut pcm = vec![0; 44 + rate * 300 * 2];
        for pair in pcm[44 + rate * 40 * 2..44 + rate * 50 * 2]
            .as_chunks_mut::<2>()
            .0
        {
            pair.copy_from_slice(&3000_i16.to_le_bytes());
        }
        let expected = pcm[44 + rate * 79..44 + rate * 101].to_vec();
        assert!(trim_quiet_edges(&mut pcm, u32::try_from(rate).unwrap()));
        assert_eq!(&pcm[44..], expected);
        assert_eq!(pcm.capacity(), pcm.len());
    }

    #[test]
    fn trimmed_capacity_is_kept_unless_both_slack_thresholds_are_met() {
        for (capacity, length) in [
            (1024 * 1024, 44),
            (8 * 1024 * 1024, 1024 * 1024),
            (12 * 1024 * 1024, 4 * 1024 * 1024),
        ] {
            let mut pcm = Vec::with_capacity(capacity);
            pcm.resize(length, 0x5a);
            let allocation = pcm.as_ptr();
            compact_trimmed_pcm(&mut pcm);
            assert_eq!(pcm.capacity(), capacity);
            assert_eq!(pcm.as_ptr(), allocation);
            assert_eq!(pcm.len(), length);
            assert!(pcm.iter().all(|&byte| byte == 0x5a));
        }
    }
}
