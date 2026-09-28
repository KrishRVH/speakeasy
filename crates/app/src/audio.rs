use crate::runtime::Event;
use anyhow::{Context, bail};
use cpal::{
    SampleFormat, SizedSample,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Producer, RingBuffer};
use speakeasy_core::gesture::RECORDING_LIMIT;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub struct Capture {
    // The real-time callback cannot wait on the session owner. This flag only
    // controls this capture; audio and completion return through the ring/channel.
    command: Arc<AtomicU8>,
}

impl Capture {
    pub fn start(
        id: u64,
        microphone: Option<String>,
        tx: async_channel::Sender<Event>,
    ) -> anyhow::Result<Self> {
        let command = Arc::new(AtomicU8::new(0));
        let control = command.clone();
        thread::Builder::new()
            .name("microphone".into())
            .spawn(move || {
                let result = record(id, microphone.as_deref(), &tx, &control);
                let _ = tx.send_blocking(Event::AudioDone(id, result));
            })?;
        Ok(Self { command })
    }
    pub fn finish(&self) {
        let _ = self
            .command
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn cancel(&self) {
        self.command.store(2, Ordering::Release);
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn record(
    id: u64,
    microphone: Option<&str>,
    tx: &async_channel::Sender<Event>,
    command: &Arc<AtomicU8>,
) -> anyhow::Result<Option<Vec<u8>>> {
    let host = cpal::default_host();
    let device = if let Some(id) = microphone {
        host.input_devices()?
            .find(|device| device.id().is_ok_and(|actual| actual.to_string() == id))
            .context(
                "Selected microphone is disconnected. Reconnect it or choose another in Settings.",
            )?
    } else {
        host.default_input_device()
            .context("No microphone found. Connect a microphone and try again.")?
    };
    let format = device
        .default_input_config()
        .context("Cannot open microphone. Check OS microphone permission.")?;
    let rate = format.sample_rate();
    let channels = usize::from(format.channels());
    if rate == 0 || rate > 192_000 || channels == 0 || channels > 32 {
        bail!("Unsupported microphone format");
    }
    let (producer, mut consumer) = RingBuffer::new(rate as usize);
    let failed = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let limit = rate as usize * RECORDING_LIMIT.as_secs() as usize;
    let stream = match format.sample_format() {
        SampleFormat::F32 => stream::<f32>(
            &device,
            format.config(),
            channels,
            producer,
            command.clone(),
            failed.clone(),
            started,
            limit,
        ),
        SampleFormat::I16 => stream::<i16>(
            &device,
            format.config(),
            channels,
            producer,
            command.clone(),
            failed.clone(),
            started,
            limit,
        ),
        SampleFormat::U16 => stream::<u16>(
            &device,
            format.config(),
            channels,
            producer,
            command.clone(),
            failed.clone(),
            started,
            limit,
        ),
        _ => bail!("Microphone sample format is unsupported. Choose a standard PCM microphone."),
    }?;
    // Keep mono PCM at the device's native rate; the local engine resamples it.
    // Reserve for an ordinary utterance; long recordings grow on this consumer
    // thread, never in the real-time callback.
    let mut pcm = Vec::with_capacity(rate as usize * 10 * 2 + 44);
    pcm.resize(44, 0_u8);
    if command.load(Ordering::Acquire) != 0 || tx.is_closed() {
        return Ok(None);
    }
    stream.play().context("Microphone could not start")?;
    let mut stream = Some(stream);
    let mut ready = false;
    let mut energy = 0.0_f64;
    let mut count = 0_u32;
    let mut last_level = Instant::now();
    loop {
        if command.load(Ordering::Acquire) == 2 || tx.is_closed() {
            pcm.fill(0_u8);
            return Ok(None);
        }
        if failed.load(Ordering::Acquire) {
            pcm.fill(0_u8);
            bail!("Microphone disconnected or audio buffer overran. Try recording again.");
        }
        if command.load(Ordering::Acquire) == 1
            || started.elapsed() >= RECORDING_LIMIT
            || (pcm.len() - 44) / 2 >= limit
        {
            let _ = command.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
            drop(stream.take());
        }
        while let Ok(sample) = consumer.pop() {
            if !ready {
                ready = true;
                tx.send_blocking(Event::Ready(id))?;
            }
            if (pcm.len() - 44) / 2 < limit {
                let sample = if sample.is_finite() {
                    sample.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                energy += f64::from(sample * sample);
                count += 1;
                if pcm.len() + 2 > pcm.capacity() {
                    let capacity = (pcm.capacity() * 2).min(limit * 2 + 44);
                    pcm.reserve_exact(capacity - pcm.len());
                }
                pcm.extend_from_slice(&((sample * 32767.0) as i16).to_le_bytes());
            }
        }
        if last_level.elapsed() >= Duration::from_millis(32) {
            let rms = (energy / f64::from(count.max(1))).sqrt() as f32;
            let level = ((20.0 * rms.max(0.000_001).log10() + 60.0) / 54.0).clamp(0.0, 1.0);
            let _ = tx.try_send(Event::Level(id, level));
            energy = 0.0;
            count = 0;
            last_level = Instant::now();
        }
        if stream.is_none() {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    // The callback observes the stop flag even if closing a driver blocks.
    let _ = command.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    drop(stream);
    if command.load(Ordering::Acquire) == 2 {
        pcm.fill(0);
        return Ok(None);
    }
    if (pcm.len() - 44) / 2 < rate as usize / 5 || !trim_quiet_edges(&mut pcm, rate) {
        return Ok(None);
    }
    Ok(Some(wave(pcm, rate)))
}

#[expect(
    clippy::too_many_arguments,
    reason = "The callback captures explicit device and session ownership once at stream construction"
)]
fn stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: usize,
    mut producer: Producer<f32>,
    command: Arc<AtomicU8>,
    failed: Arc<AtomicBool>,
    started: Instant,
    limit: usize,
) -> anyhow::Result<cpal::Stream>
where
    T: SizedSample,
    f32: cpal::FromSample<T>,
{
    let error = failed.clone();
    let mut samples = 0_usize;
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            if command.load(Ordering::Acquire) != 0 || started.elapsed() >= RECORDING_LIMIT {
                return;
            }
            for frame in data.chunks_exact(channels) {
                if samples >= limit {
                    break;
                }
                let mono = frame
                    .iter()
                    .map(|sample| sample.to_sample::<f32>())
                    .sum::<f32>()
                    / channels as f32;
                if producer.push(mono).is_err() {
                    failed.store(true, Ordering::Release);
                    break;
                }
                samples += 1;
            }
        },
        move |_| {
            error.store(true, Ordering::Release);
        },
        None,
    )?)
}

// Use 20 ms energy windows, at least 100 ms audible audio, and 500 ms
// padding when a quiet edge exceeds a second.
// This is conservative edge trimming, not VAD; all interior pauses remain.
fn trim_quiet_edges(pcm: &mut Vec<u8>, rate: u32) -> bool {
    let bytes_per_second = rate as usize * 2;
    let padding = (rate as usize / 2) * 2; // Keep the boundary on a whole PCM sample.
    let window_bytes = (rate as usize / 50).max(1) * 2;
    let audio = &pcm[44..];
    let mut first = None;
    let mut end = 0;
    let mut audible = 0;
    for (index, window) in audio.chunks(window_bytes).enumerate() {
        let energy = window
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let sample = i32::from(i16::from_le_bytes([pair[0], pair[1]]));
                (sample * sample) as u64
            })
            // A 20 ms window at the 192 kHz cap contains at most 3840 samples:
            // its squared sum is below 2^42. Wrapping cannot occur; this form
            // lets the compiler vectorize the exact integer reduction.
            .fold(0_u64, u64::wrapping_add);
        if energy as f64 / (32768.0 * 32768.0) >= 0.003_f64.powi(2) * (window.len() / 2) as f64 {
            first.get_or_insert(index * window_bytes);
            end = index * window_bytes + window.len();
            audible += window.len() / 2;
        }
    }
    let Some(first) = first.filter(|_| audible >= (rate as usize / 10).max(1)) else {
        return false;
    };
    let start = if first >= bytes_per_second {
        first - padding
    } else {
        0
    };
    if audio.len() - end >= bytes_per_second {
        end += padding;
    } else {
        end = audio.len();
    }
    if start != 0 {
        pcm.copy_within(44 + start..44 + end, 44);
    }
    pcm.truncate(44 + end - start);
    true
}

pub(crate) fn wave(mut pcm: Vec<u8>, rate: u32) -> Vec<u8> {
    let bytes = (pcm.len() - 44) as u32;
    let mut wav = Vec::with_capacity(44);
    wav.extend(b"RIFF");
    wav.extend((bytes + 36).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16_u32.to_le_bytes());
    wav.extend(1_u16.to_le_bytes());
    wav.extend(1_u16.to_le_bytes());
    wav.extend(rate.to_le_bytes());
    wav.extend((rate * 2).to_le_bytes());
    wav.extend(2_u16.to_le_bytes());
    wav.extend(16_u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(bytes.to_le_bytes());
    pcm[..44].copy_from_slice(&wav);
    pcm
}

/// Lists input devices without opening a stream. Drivers may block, and CPAL
/// leaves its calling thread in a single-threaded COM apartment, which a shared
/// executor thread must not keep. Enumeration runs on its own short-lived thread.
pub async fn microphones() -> anyhow::Result<Vec<(String, String)>> {
    let (tx, rx) = async_channel::bounded(1);
    thread::Builder::new()
        .name("microphones".into())
        .spawn(move || {
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
    fn quiet_edges_keep_word_padding_and_interior_pauses_but_clicks_are_rejected() {
        let rate = 16_000;
        let mut pcm = vec![0; 44];
        for (seconds, amplitude) in [(2, 0_i16), (1, 3000), (3, 0), (1, 3000), (2, 0)] {
            for _ in 0..rate * seconds {
                pcm.extend_from_slice(&amplitude.to_le_bytes());
            }
        }
        let expected = pcm[44 + rate * 3..44 + rate * 15].to_vec();
        assert!(trim_quiet_edges(&mut pcm, rate as u32));
        assert_eq!(&pcm[44..], expected);

        let mut click = vec![0; 44 + rate * 2];
        for pair in click[44..44 + rate / 10].as_chunks_mut::<2>().0 {
            pair.copy_from_slice(&3000_i16.to_le_bytes());
        }
        assert!(!trim_quiet_edges(&mut click, rate as u32));
    }
}
