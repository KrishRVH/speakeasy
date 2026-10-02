//! The helper's pipe protocol; every integer is little-endian.
//!
//! Once its model loads, the helper writes one status byte: [`READY`], or a [`Failure`]. Each request
//! is then a [`Request`] header followed by that many PCM16 samples, and each reply a status byte
//! followed, on success, by a `u32` length and that many bytes of UTF-8 text.

use std::io;

use speakeasy_platform::speech::Failure;

/// The status of a loaded model and of a successful recognition.
pub(super) const READY: u8 = 0;
/// Five minutes at the engine's highest supported rate.
const MAX_SAMPLES: u32 = 300 * 96_000;
/// Bytes of PCM16 per sample.
const SAMPLE_BYTES: usize = 2;

/// One recognition request's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Request {
    pub rate: u32,
    pub samples: u32,
}

impl Request {
    pub(super) const BYTES: usize = 8;

    /// A header for `pcm`, PCM16 bytes at `rate`, if the protocol can carry it.
    pub(super) fn for_pcm(rate: u32, pcm: &[u8]) -> Option<Self> {
        let samples = u32::try_from(pcm.len() / SAMPLE_BYTES).ok()?;
        (pcm.len().is_multiple_of(SAMPLE_BYTES) && samples <= MAX_SAMPLES)
            .then_some(Self { rate, samples })
    }

    pub(super) fn encode(self) -> [u8; Self::BYTES] {
        let [r0, r1, r2, r3] = self.rate.to_le_bytes();
        let [s0, s1, s2, s3] = self.samples.to_le_bytes();
        [r0, r1, r2, r3, s0, s1, s2, s3]
    }

    /// Rejects a length the parent could never send, before anything is allocated for it.
    pub(super) fn decode(bytes: [u8; Self::BYTES]) -> io::Result<Self> {
        let [r0, r1, r2, r3, s0, s1, s2, s3] = bytes;
        let request = Self {
            rate: u32::from_le_bytes([r0, r1, r2, r3]),
            samples: u32::from_le_bytes([s0, s1, s2, s3]),
        };
        if request.samples > MAX_SAMPLES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Recording exceeds five minutes",
            ));
        }
        Ok(request)
    }

    /// The PCM16 bytes that follow the header.
    pub(super) fn pcm_bytes(self) -> usize {
        usize::try_from(self.samples)
            .unwrap_or(usize::MAX)
            .saturating_mul(SAMPLE_BYTES)
    }
}

/// The outcome a status byte reports; an unknown byte means the stream is corrupt.
pub(super) fn outcome(status: u8) -> io::Result<Result<(), Failure>> {
    if status == READY {
        return Ok(Ok(()));
    }
    Failure::from_byte(status)
        .map(Err)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Unknown helper status"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_and_oversized_lengths_are_refused_before_allocation() {
        let request = Request::for_pcm(48_000, &[0; 6]).unwrap();
        assert_eq!(
            request,
            Request {
                rate: 48_000,
                samples: 3
            }
        );
        assert_eq!(Request::decode(request.encode()).unwrap(), request);
        assert_eq!(request.pcm_bytes(), 6);
        assert_eq!(Request::for_pcm(48_000, &[0; 3]), None, "Half a sample");
        let oversized = Request {
            rate: 96_000,
            samples: MAX_SAMPLES + 1,
        };
        assert!(Request::decode(oversized.encode()).is_err());
    }

    #[test]
    fn every_failure_survives_its_status_byte() {
        for failure in [
            Failure::Library,
            Failure::Unsupported,
            Failure::OutOfMemory,
            Failure::Engine,
            Failure::Audio,
        ] {
            assert_eq!(outcome(failure as u8).unwrap(), Err(failure));
        }
        assert_eq!(outcome(READY).unwrap(), Ok(()));
        assert!(outcome(0xFF).is_err());
    }
}
