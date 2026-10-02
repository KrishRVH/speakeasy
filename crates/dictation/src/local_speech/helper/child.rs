//! The helper process: this executable started with [`HELPER_FLAG`].
//!
//! The main thread reads requests from standard input while the engine thread owns the model and
//! answers on a private copy of standard output. The parent closing standard input ends the process
//! at once, even while the engine thread is inside native inference.

use std::{
    ffi::{OsStr, OsString},
    io::{self, BufWriter, Read, Write},
    path::PathBuf,
    sync::mpsc,
    thread,
};

use speakeasy_platform::{
    exit_now, private_stdout,
    speech::{Accelerator, Failure, Recognizer},
};

use super::frame::{self, Request};

/// The first argument that turns this executable into the speech helper.
pub const HELPER_FLAG: &str = "--speech-helper";
/// The pipe protocol's version, passed after [`HELPER_FLAG`]. The parent re-runs its own executable,
/// which an update may have replaced with one that speaks another version.
pub(super) const PROTOCOL: &str = "1";
/// The helper's exit status when the parent speaks another protocol version.
pub(super) const PROTOCOL_MISMATCH: i32 = 65;

/// Malformed arguments or a thread that cannot start: `EX_USAGE`.
const USAGE: i32 = 64;
/// The model failed to load; its reason was reported on the protocol first.
const LOAD_FAILED: i32 = 1;

/// The engine operation the helper serves, separable for tests.
pub(super) trait Recognize {
    fn recognize(&mut self, samples: &[f32], rate: u32) -> Result<String, Failure>;
}

impl Recognize for Recognizer {
    fn recognize(&mut self, samples: &[f32], rate: u32) -> Result<String, Failure> {
        Self::recognize(self, samples, rate)
    }
}

/// One request's audio: PCM16 bytes at `rate` Hz.
pub(super) struct Job {
    rate: u32,
    pcm: Vec<u8>,
}

/// Loads `library` and `model` from `arguments`, answers requests until the parent closes standard
/// input, then ends the process.
pub fn run_helper(mut arguments: impl Iterator<Item = OsString>) -> ! {
    if arguments.next().as_deref().and_then(OsStr::to_str) != Some(PROTOCOL) {
        exit_now(PROTOCOL_MISMATCH);
    }
    let (Some(library), Some(model), Some(accelerator), None) = (
        arguments.next(),
        arguments.next(),
        arguments.next(),
        arguments.next(),
    ) else {
        exit_now(USAGE)
    };
    let accelerator = match accelerator.to_str() {
        Some("gpu") => Accelerator::Gpu,
        Some("cpu") => Accelerator::Cpu,
        _ => exit_now(USAGE),
    };
    let Ok(replies) = private_stdout(&io::stdout()) else {
        exit_now(USAGE)
    };
    let (jobs, received) = mpsc::channel();
    let engine = thread::Builder::new()
        .name("speech-engine".into())
        .spawn(move || {
            let loaded =
                Recognizer::load(&PathBuf::from(library), &PathBuf::from(model), accelerator);
            let code = if loaded.is_ok() { 0 } else { LOAD_FAILED };
            // A failed write means the parent is gone, which ends the process just the same.
            drop(serve(loaded, &received, BufWriter::new(replies)));
            exit_now(code)
        });
    if engine.is_err() {
        exit_now(USAGE);
    }
    // End of input and read errors both mean the parent is gone.
    drop(read_jobs(io::stdin().lock(), &jobs));
    exit_now(0)
}

/// Forwards framed requests to the engine thread until the input ends or the engine thread exits.
pub(super) fn read_jobs(mut input: impl Read, jobs: &mpsc::Sender<Job>) -> io::Result<()> {
    loop {
        let mut header = [0; Request::BYTES];
        if let Err(error) = input.read_exact(&mut header) {
            return match error.kind() {
                io::ErrorKind::UnexpectedEof => Ok(()),
                _ => Err(error),
            };
        }
        let request = Request::decode(header)?;
        let mut pcm = vec![0; request.pcm_bytes()];
        input.read_exact(&mut pcm)?;
        let job = Job {
            rate: request.rate,
            pcm,
        };
        if jobs.send(job).is_err() {
            return Ok(());
        }
    }
}

/// Reports the load, then answers each job in order. Audio is erased once recognized, best effort.
pub(super) fn serve(
    loaded: Result<impl Recognize, Failure>,
    jobs: &mpsc::Receiver<Job>,
    mut replies: impl Write,
) -> io::Result<()> {
    replies.write_all(&[loaded
        .as_ref()
        .err()
        .map_or(frame::READY, |&failure| failure as u8)])?;
    replies.flush()?;
    let Ok(mut engine) = loaded else {
        return Ok(());
    };
    let mut samples = Vec::new();
    for Job { rate, mut pcm } in jobs {
        // The engine's own WAV reader scales PCM16 by 1/32768, so both routes see identical input.
        samples.extend(
            pcm.as_chunks::<2>()
                .0
                .iter()
                .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32_768.0),
        );
        pcm.fill(0);
        let recognized = engine.recognize(&samples, rate);
        samples.fill(0.0);
        samples.clear();
        match recognized {
            Ok(text) => {
                let length = u32::try_from(text.len()).unwrap_or(u32::MAX);
                replies.write_all(&[frame::READY])?;
                replies.write_all(&length.to_le_bytes())?;
                replies.write_all(text.as_bytes())?;
            },
            Err(failure) => replies.write_all(&[failure as u8])?,
        }
        replies.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// Reports each request's rate and its first sample, and fails empty audio like the engine.
    struct Echo;

    impl Recognize for Echo {
        fn recognize(&mut self, samples: &[f32], rate: u32) -> Result<String, Failure> {
            let first = samples.first().ok_or(Failure::Audio)?;
            Ok(format!("{rate} {first}"))
        }
    }

    fn request(rate: u32, samples: &[i16]) -> Vec<u8> {
        let pcm: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let mut bytes = Request::for_pcm(rate, &pcm).unwrap().encode().to_vec();
        bytes.extend(pcm);
        bytes
    }

    fn reply(text: &str) -> Vec<u8> {
        let mut bytes = vec![frame::READY];
        bytes.extend(u32::try_from(text.len()).unwrap().to_le_bytes());
        bytes.extend(text.as_bytes());
        bytes
    }

    #[test]
    fn requests_are_answered_in_order_with_the_engine_scale() -> anyhow::Result<()> {
        let mut input = request(48_000, &[-16_384, 7]);
        input.extend(request(16_000, &[]));
        input.extend(request(44_100, &[32_767]));
        let (jobs, received) = mpsc::channel();
        read_jobs(Cursor::new(input), &jobs)?;
        drop(jobs);
        let mut replies = Vec::new();
        serve(Ok(Echo), &received, &mut replies)?;
        let expected = [
            vec![frame::READY],
            reply("48000 -0.5"),
            vec![Failure::Audio as u8],
            reply("44100 0.9999695"),
        ]
        .concat();
        assert_eq!(replies, expected);
        Ok(())
    }

    #[test]
    fn a_failed_load_is_reported_before_any_request() -> anyhow::Result<()> {
        let (jobs, received) = mpsc::channel();
        jobs.send(Job {
            rate: 16_000,
            pcm: vec![0; 2],
        })?;
        let mut replies = Vec::new();
        serve(Err::<Echo, _>(Failure::Library), &received, &mut replies)?;
        assert_eq!(replies, [Failure::Library as u8]);
        Ok(())
    }

    #[test]
    fn a_truncated_request_is_an_error_but_closed_input_is_not() {
        let (jobs, _received) = mpsc::channel();
        assert!(read_jobs(Cursor::new(Vec::new()), &jobs).is_ok());
        let mut truncated = request(16_000, &[1, 2]);
        truncated.pop();
        assert!(read_jobs(Cursor::new(truncated), &jobs).is_err());
    }
}
