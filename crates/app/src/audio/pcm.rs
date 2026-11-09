use std::{mem, ops::Range, time::Duration};

use anyhow::{anyhow, bail};
use zeroize::{Zeroize, Zeroizing};

use super::speech::Audibility;
use crate::runtime::ports::Sealed;

const WAV_HEADER_BYTES: usize = 44;
const SAMPLE_RATE_OFFSET: usize = 24;
pub(super) const SAMPLE_BYTES: usize = 2;
const MINIMUM_RECORDING: Duration = Duration::from_millis(200);
const RECLAIM_SLACK_BYTES: usize = 8 * 1024 * 1024;
const INCOMPLETE_HEADER: &str = "Recording header is incomplete. Try recording again.";

pub(super) struct Pcm16 {
    bytes: Vec<u8>,
    limit: usize,
}

impl Pcm16 {
    pub(super) fn from_kept(audio: Zeroizing<Vec<u8>>) -> Self {
        let samples = audio.len() / SAMPLE_BYTES;
        let mut bytes = Vec::with_capacity(audio.len().saturating_add(WAV_HEADER_BYTES));
        bytes.resize(WAV_HEADER_BYTES, 0);
        bytes.extend_from_slice(&audio);
        Self {
            bytes,
            limit: samples,
        }
    }

    pub(super) fn with_reservation(reserved: usize, limit: usize) -> Self {
        let mut bytes = Vec::with_capacity(WAV_HEADER_BYTES.saturating_add(bytes_in(reserved)));
        bytes.resize(WAV_HEADER_BYTES, 0);
        Self { bytes, limit }
    }

    fn silence(samples: usize) -> Self {
        Self {
            bytes: vec![0; WAV_HEADER_BYTES.saturating_add(bytes_in(samples))],
            limit: samples,
        }
    }

    pub(super) fn audio(&self) -> &[u8] {
        self.bytes.get(WAV_HEADER_BYTES..).unwrap_or_default()
    }

    pub(super) fn samples(&self) -> usize {
        self.audio().len() / SAMPLE_BYTES
    }

    pub(super) fn room(&self) -> usize {
        self.limit.saturating_sub(self.samples())
    }

    pub(super) fn long_enough(&self, rate: u32) -> bool {
        self.samples() >= samples_in(MINIMUM_RECORDING, rate)
    }

    pub(super) fn push(&mut self, sample: i16) {
        let required = self.bytes.len().saturating_add(SAMPLE_BYTES);
        if required > self.bytes.capacity() {
            let full = WAV_HEADER_BYTES.saturating_add(bytes_in(self.limit));
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .min(full)
                .max(required);
            self.reallocate(capacity);
        }
        self.bytes.extend_from_slice(&sample.to_le_bytes());
    }

    pub(super) fn seal(
        mut self,
        mut speech: Audibility,
        rate: u32,
    ) -> anyhow::Result<Option<Sealed>> {
        if !self.long_enough(rate) {
            return Ok(None);
        }
        speech.complete(self.audio());
        let Some(range) = speech.retained(self.audio().len()) else {
            return Ok(None);
        };
        let trimmed = duration_of(range.start, rate);
        self.retain_audio(range);
        self.release_excess_capacity();
        let wav = self.into_wav(rate)?;
        Ok(Some(Sealed {
            wav,
            trimmed,
            partition: None,
        }))
    }

    pub(super) fn excerpt(&self, range: Range<usize>, rate: u32) -> anyhow::Result<Sealed> {
        let audio = self
            .audio()
            .get(range.clone())
            .ok_or_else(|| anyhow!(INCOMPLETE_HEADER))?;
        Ok(Sealed {
            wav: encode(audio, rate)?,
            trimmed: duration_of(range.start, rate),
            partition: None,
        })
    }

    #[cfg(test)]
    pub(super) fn clear(&mut self) {
        self.bytes.truncate(WAV_HEADER_BYTES);
    }

    fn retain_audio(&mut self, audio: Range<usize>) {
        self.bytes
            .truncate(WAV_HEADER_BYTES.saturating_add(audio.end));
        self.bytes
            .drain(WAV_HEADER_BYTES..WAV_HEADER_BYTES.saturating_add(audio.start));
    }

    fn release_excess_capacity(&mut self) {
        let capacity = self.bytes.capacity();
        if capacity.saturating_sub(self.bytes.len()) >= RECLAIM_SLACK_BYTES
            && capacity / 4 >= self.bytes.len()
        {
            self.reallocate(self.bytes.len());
        }
    }

    fn reallocate(&mut self, capacity: usize) {
        let mut replacement = Vec::with_capacity(capacity);
        replacement.extend_from_slice(&self.bytes);
        self.bytes.zeroize();
        self.bytes = replacement;
    }

    fn into_wav(mut self, rate: u32) -> anyhow::Result<Vec<u8>> {
        let Some((header, audio)) = self.bytes.split_first_chunk_mut::<WAV_HEADER_BYTES>() else {
            bail!(INCOMPLETE_HEADER);
        };
        let Some(encoded) = wav_header(audio.len(), rate) else {
            bail!(
                "Recording format is unsupported. Choose another microphone in Settings, then try again."
            );
        };
        *header = encoded;
        Ok(mem::take(&mut self.bytes))
    }
}

impl Drop for Pcm16 {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

fn encode(audio: &[u8], rate: u32) -> anyhow::Result<Vec<u8>> {
    let header = wav_header(audio.len(), rate).ok_or_else(|| anyhow!(INCOMPLETE_HEADER))?;
    let mut wav = Vec::with_capacity(WAV_HEADER_BYTES.saturating_add(audio.len()));
    wav.extend_from_slice(&header);
    wav.extend_from_slice(audio);
    Ok(wav)
}

pub(crate) fn wav_tail(wav: &[u8], range: Range<usize>) -> anyhow::Result<Vec<u8>> {
    let audio = wav
        .get(WAV_HEADER_BYTES..)
        .filter(|audio| range.end == audio.len())
        .and_then(|audio| audio.get(range))
        .ok_or_else(|| anyhow!(INCOMPLETE_HEADER))?;
    encode(audio, wav_sample_rate(wav)?)
}

fn wav_header(data_bytes: usize, rate: u32) -> Option<[u8; WAV_HEADER_BYTES]> {
    const CHUNK_HEADER_BYTES: usize = 8;
    const FORMAT_CHUNK_BYTES: u32 = 16;
    const PCM: u16 = 1;
    const MONO: u16 = 1;
    const BITS_PER_SAMPLE: u16 = 16;
    const BLOCK_ALIGN: u16 = MONO * BITS_PER_SAMPLE / 8;
    if rate == 0 || !data_bytes.is_multiple_of(SAMPLE_BYTES) {
        return None;
    }
    let riff_bytes = data_bytes.checked_add(WAV_HEADER_BYTES - CHUNK_HEADER_BYTES)?;
    let byte_rate = rate.checked_mul(u32::from(BLOCK_ALIGN))?;
    let fields: [&[u8]; 13] = [
        b"RIFF",
        &u32::try_from(riff_bytes).ok()?.to_le_bytes(),
        b"WAVE",
        b"fmt ",
        &FORMAT_CHUNK_BYTES.to_le_bytes(),
        &PCM.to_le_bytes(),
        &MONO.to_le_bytes(),
        &rate.to_le_bytes(),
        &byte_rate.to_le_bytes(),
        &BLOCK_ALIGN.to_le_bytes(),
        &BITS_PER_SAMPLE.to_le_bytes(),
        b"data",
        &u32::try_from(data_bytes).ok()?.to_le_bytes(),
    ];
    let mut header = [0; WAV_HEADER_BYTES];
    for (byte, field) in header.iter_mut().zip(fields.into_iter().flatten()) {
        *byte = *field;
    }
    Some(header)
}

pub(crate) fn wav_sample_rate(wav: &[u8]) -> anyhow::Result<u32> {
    let Some(field) = wav.get(SAMPLE_RATE_OFFSET..).and_then(<[u8]>::first_chunk) else {
        bail!(INCOMPLETE_HEADER);
    };
    Ok(u32::from_le_bytes(*field))
}

#[cfg(test)]
pub(crate) fn wav_duration(wav: &[u8]) -> anyhow::Result<Duration> {
    let rate = wav_sample_rate(wav)?;
    let audio = wav
        .get(WAV_HEADER_BYTES..)
        .ok_or_else(|| anyhow!(INCOMPLETE_HEADER))?;
    anyhow::ensure!(
        rate > 0 && audio.len().is_multiple_of(SAMPLE_BYTES),
        "The fixture needs a positive rate and complete PCM16 samples"
    );
    Ok(duration_of(audio.len(), rate))
}

pub(crate) fn silent_wav(rate: u32, duration: Duration) -> anyhow::Result<Vec<u8>> {
    Pcm16::silence(samples_in(duration, rate)).into_wav(rate)
}

pub(super) fn samples_in(duration: Duration, rate: u32) -> usize {
    let samples = u128::from(rate).saturating_mul(duration.as_millis()) / 1000;
    usize::try_from(samples).unwrap_or(usize::MAX)
}

pub(super) fn bytes_in(samples: usize) -> usize {
    samples.saturating_mul(SAMPLE_BYTES)
}

fn duration_of(bytes: usize, rate: u32) -> Duration {
    let samples = bytes / SAMPLE_BYTES;
    Duration::from_secs_f64(samples as f64 / f64::from(rate.max(1)))
}

#[cfg(test)]
mod tests {
    use anyhow::Context as _;

    use super::*;
    use crate::audio::{fixture::square_wave, stream::MAX_SAMPLE_RATE};

    fn pcm_from(segments: &[(usize, i16)]) -> Pcm16 {
        let mut pcm = Pcm16::with_reservation(0, usize::MAX);
        for &(samples, amplitude) in segments {
            for sample in square_wave(amplitude).take(samples) {
                pcm.push(sample);
            }
        }
        pcm
    }

    #[test]
    fn wav_encoding_rewrites_the_header_in_place_with_exact_riff_lengths() -> anyhow::Result<()> {
        let mut pcm = Pcm16 {
            bytes: vec![0xFF; WAV_HEADER_BYTES],
            limit: usize::MAX,
        };
        for sample in [i16::MIN, 0, i16::MAX] {
            pcm.push(sample);
        }
        let allocation = pcm.bytes.as_ptr();
        let encoded = pcm.into_wav(16_000)?;
        assert_eq!(encoded.as_ptr(), allocation);
        assert_eq!(
            &encoded[..WAV_HEADER_BYTES],
            &[
                b'R', b'I', b'F', b'F', 42, 0, 0, 0, b'W', b'A', b'V', b'E', b'f', b'm', b't',
                b' ', 16, 0, 0, 0, 1, 0, 1, 0, 0x80, 0x3E, 0, 0, 0, 0x7D, 0, 0, 2, 0, 16, 0, b'd',
                b'a', b't', b'a', 6, 0, 0, 0,
            ]
        );
        assert_eq!(&encoded[WAV_HEADER_BYTES..], &[0, 0x80, 0, 0, 0xFF, 0x7F]);
        Ok(())
    }

    #[test]
    fn wav_encoding_rejects_incomplete_headers_partial_samples_and_invalid_rates() {
        let unsupported = "Recording format is unsupported";
        for (length, rate, expected) in [
            (WAV_HEADER_BYTES - 1, 16_000, INCOMPLETE_HEADER),
            (WAV_HEADER_BYTES + 1, 16_000, unsupported),
            (WAV_HEADER_BYTES + 2, 0, unsupported),
            (WAV_HEADER_BYTES + 2, u32::MAX, unsupported),
        ] {
            let pcm = Pcm16 {
                bytes: vec![0x5A; length],
                limit: usize::MAX,
            };
            let error = pcm.into_wav(rate).unwrap_err().to_string();
            assert!(
                error.starts_with(expected),
                "{length} bytes at {rate} Hz: {error}"
            );
        }
    }

    #[test]
    fn silence_round_trips_its_sample_rate() -> anyhow::Result<()> {
        let silence = silent_wav(16_000, Duration::from_secs(1))?;
        assert_eq!(wav_sample_rate(&silence)?, 16_000);
        assert_eq!(silence.len(), WAV_HEADER_BYTES + 32_000);
        assert_eq!(
            wav_sample_rate(&silence[..27]).map_err(|error| error.to_string()),
            Err(INCOMPLETE_HEADER.to_owned())
        );
        Ok(())
    }

    #[test]
    fn profiling_duration_rejects_incomplete_audio_and_zero_rate() -> anyhow::Result<()> {
        let duration = Duration::from_millis(500);
        let wav = silent_wav(16_000, duration)?;
        assert_eq!(wav_duration(&wav)?, duration);
        let mut zero_rate = wav.clone();
        zero_rate[SAMPLE_RATE_OFFSET..SAMPLE_RATE_OFFSET + 4].fill(0);
        for invalid in [
            &wav[..27],
            &wav[..WAV_HEADER_BYTES - 1],
            &wav[..wav.len() - 1],
            &zero_rate,
        ] {
            assert!(wav_duration(invalid).is_err());
        }
        Ok(())
    }

    #[test]
    fn quiet_edges_keep_word_padding_and_interior_pauses_but_clicks_are_rejected() {
        let rate = 16_000;
        let pcm = pcm_from(&[
            (rate * 2, 0),
            (rate, 3000),
            (rate * 3, 0),
            (rate, 3000),
            (rate * 2, 0),
        ]);
        let padding = rate / 2;
        let kept = bytes_in(rate * 2 - padding)..bytes_in(rate * 7 + padding);
        let expected = pcm.audio()[kept].to_vec();
        let click = pcm_from(&[(rate / 20, 3000), (rate * 2 - rate / 20, 0)]);
        let rate = u32::try_from(rate).unwrap();
        let sealed = pcm.seal(Audibility::new(rate), rate).unwrap().unwrap();
        assert_eq!(&sealed.wav[WAV_HEADER_BYTES..], expected);
        assert_eq!(wav_sample_rate(&sealed.wav).unwrap(), rate);

        assert_eq!(sealed.trimmed, Duration::from_millis(1_500));

        assert!(click.seal(Audibility::new(rate), rate).unwrap().is_none());
    }

    #[test]
    fn sealing_requires_two_hundred_milliseconds_of_recorded_audio() {
        for rate in [8_000, 44_101, MAX_SAMPLE_RATE] {
            for millis in [199, 200] {
                let samples = samples_in(Duration::from_millis(millis), rate);
                let pcm = pcm_from(&[(samples, 3000)]);
                let mut speech = Audibility::new(rate);
                speech.extend(pcm.audio());
                let sealed = pcm.seal(speech, rate).unwrap();
                assert_eq!(sealed.is_some(), millis == 200);
                if let Some(sealed) = sealed {
                    assert_eq!(sealed.wav.len(), WAV_HEADER_BYTES + bytes_in(samples));
                    assert_eq!(sealed.trimmed, Duration::ZERO);
                }
            }
        }
    }

    #[test]
    fn sparse_long_recording_releases_capacity_without_changing_padded_audio() {
        let rate = 48_000;
        let mut pcm = Pcm16 {
            bytes: vec![0; WAV_HEADER_BYTES + bytes_in(rate * 300)],
            limit: usize::MAX,
        };
        let speech = WAV_HEADER_BYTES + bytes_in(rate * 40)..WAV_HEADER_BYTES + bytes_in(rate * 50);
        for (pair, sample) in pcm.bytes[speech]
            .as_chunks_mut::<SAMPLE_BYTES>()
            .0
            .iter_mut()
            .zip(square_wave(3000))
        {
            *pair = sample.to_le_bytes();
        }
        let padding = rate / 2;
        let kept = bytes_in(rate * 40 - padding)..bytes_in(rate * 50 + padding);
        let expected = pcm.audio()[kept].to_vec();
        let rate = u32::try_from(rate).unwrap();
        let sealed = pcm.seal(Audibility::new(rate), rate).unwrap().unwrap();
        assert_eq!(&sealed.wav[WAV_HEADER_BYTES..], expected);
        assert_eq!(sealed.wav.capacity(), sealed.wav.len());
        assert_eq!(sealed.trimmed, Duration::from_millis(39_500));
    }

    #[test]
    fn trimmed_capacity_is_kept_unless_both_slack_thresholds_are_met() {
        for (capacity, length) in [
            (1024 * 1024, WAV_HEADER_BYTES),
            (8 * 1024 * 1024, 1024 * 1024),
            (12 * 1024 * 1024, 4 * 1024 * 1024),
        ] {
            let mut bytes = Vec::with_capacity(capacity);
            bytes.resize(length, 0x5A);
            let allocation = bytes.as_ptr();
            let mut pcm = Pcm16 {
                bytes,
                limit: usize::MAX,
            };
            pcm.release_excess_capacity();
            assert_eq!(pcm.bytes.capacity(), capacity);
            assert_eq!(pcm.bytes.as_ptr(), allocation);
            assert_eq!(pcm.bytes.len(), length);
            assert!(pcm.bytes.iter().all(|&byte| byte == 0x5A));
        }
    }

    #[test]
    #[ignore = "opt-in synthetic release profiling; never opens a microphone"]
    fn profile_audio_seal() -> anyhow::Result<()> {
        use std::{hint::black_box, time::Instant};

        let rate = 48_000;
        let fixture = pcm_from(&[(rate, 0), (rate * 297, 3000), (rate * 2, 0)]);
        let rate = u32::try_from(rate)?;
        let mut timings = Vec::new();
        for _ in 0..21 {
            let pcm = Pcm16 {
                bytes: fixture.bytes.clone(),
                limit: usize::MAX,
            };
            let mut speech = Audibility::new(rate);

            for length in (480 * SAMPLE_BYTES..pcm.audio().len()).step_by(480 * SAMPLE_BYTES) {
                speech.extend(black_box(&pcm.audio()[..length]));
            }
            let started = Instant::now();
            let wav = black_box(pcm)
                .seal(speech, rate)?
                .context("Audible fixture")?;
            timings.push(started.elapsed().as_secs_f64() * 1000.0);
            black_box(&wav);
        }
        timings.sort_by(f64::total_cmp);
        eprintln!(
            "audio_seal: seconds=300 median_ms={:.3} p95_ms={:.3} runs=21",
            timings[10], timings[19]
        );
        Ok(())
    }
}
