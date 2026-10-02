//! Times warm recognition of public WAV fixtures, in process through the engine's C ABI or through
//! the `nemo-speech serve` HTTP route that the 0.3.2 app uses.
//!
//! ```sh
//! cargo run --release -p speakeasy-platform --example engine_profile -- \
//!   --library nemo-speech/lib/libnemo_speech_asr_c.1.dylib --model parakeet.gguf \
//!   --repetitions 10 jfk.wav long.wav
//! NEMO_SPEECH_HTTP_API_KEY=key nemo-speech serve --asr-model parakeet.gguf --port 8178 &
//! cargo run --release -p speakeasy-platform --example engine_profile -- \
//!   --http 127.0.0.1:8178 --key key jfk.wav long.wav
//! ```
//!
//! Each fixture prints one JSON line of timings, never its transcript: the word count and an
//! FNV-1a hash of the normalized words let paths be compared for identical output.
//! `--warmup SECONDS` sets the silent warmup's length (the app uses one second), and
//! `--idle SECONDS` waits before every request, to expose GPU residency or power-state costs
//! that back-to-back requests hide.

use std::{
    env,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail, ensure};
use speakeasy_platform::speech::{Accelerator, Recognizer};

struct Arguments {
    engine: Engine,
    repetitions: usize,
    warmup: u32,
    idle: Duration,
    fixtures: Vec<PathBuf>,
}

enum Engine {
    InProcess {
        library: PathBuf,
        model: PathBuf,
        accelerator: Accelerator,
    },
    Http {
        address: String,
        key: String,
    },
}

impl Arguments {
    fn parse() -> anyhow::Result<Self> {
        let mut library = None;
        let mut model = None;
        let mut accelerator = Accelerator::Gpu;
        let mut address = None;
        let mut key = String::new();
        let mut repetitions = 5;
        let mut warmup = 1;
        let mut idle = Duration::ZERO;
        let mut fixtures = Vec::new();
        let mut arguments = env::args_os().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--library") => library = arguments.next().map(PathBuf::from),
                Some("--model") => model = arguments.next().map(PathBuf::from),
                Some("--cpu") => accelerator = Accelerator::Cpu,
                Some("--http") => address = arguments.next().and_then(|a| a.into_string().ok()),
                Some("--key") => {
                    key = arguments
                        .next()
                        .and_then(|key| key.into_string().ok())
                        .context("--key needs a value")?;
                },
                Some("--repetitions") => {
                    repetitions = arguments
                        .next()
                        .and_then(|count| count.to_str()?.parse().ok())
                        .context("--repetitions needs a count")?;
                },
                Some("--warmup") => {
                    warmup = arguments
                        .next()
                        .and_then(|seconds| seconds.to_str()?.parse().ok())
                        .context("--warmup needs whole seconds")?;
                },
                Some("--idle") => {
                    idle = arguments
                        .next()
                        .and_then(|seconds| seconds.to_str()?.parse().ok())
                        .map(Duration::from_secs_f64)
                        .context("--idle needs seconds")?;
                },
                Some(flag) if flag.starts_with("--") => bail!("Unknown option {flag}"),
                _ => fixtures.push(PathBuf::from(argument)),
            }
        }
        let engine = match address {
            Some(address) => Engine::Http { address, key },
            None => Engine::InProcess {
                library: library.context("--library or --http is required")?,
                model: model.context("--model is required")?,
                accelerator,
            },
        };
        Ok(Self {
            engine,
            repetitions,
            warmup,
            idle,
            fixtures,
        })
    }
}

/// Mono PCM16 decoded with the engine's own 1/32768 scale, so both paths see identical samples.
struct Fixture {
    wav: Vec<u8>,
    samples: Vec<f32>,
    rate: u32,
}

impl Fixture {
    fn read(path: &PathBuf) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        ensure!(
            bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WAVE"),
            "{} is not a WAV file",
            path.display()
        );
        let mut rate = None;
        let mut chunks = bytes.get(12..).unwrap_or_default();
        while let Some((header, rest)) = chunks.split_first_chunk::<8>() {
            let [id @ .., s0, s1, s2, s3] = *header;
            let size = usize::try_from(u32::from_le_bytes([s0, s1, s2, s3]))?;
            let body = rest.get(..size).context("Truncated WAV chunk")?;
            match &id {
                b"fmt " => {
                    let format: [u8; 16] = body.try_into().context("Unsupported fmt chunk")?;
                    let [_, _, c0, c1, r0, r1, r2, r3, .., b0, b1] = format;
                    ensure!(
                        u16::from_le_bytes([c0, c1]) == 1 && u16::from_le_bytes([b0, b1]) == 16,
                        "{} must be mono PCM16",
                        path.display()
                    );
                    rate = Some(u32::from_le_bytes([r0, r1, r2, r3]));
                },
                b"data" => {
                    let samples = body
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32_768.0)
                        .collect();
                    let rate = rate.context("WAV data precedes its format")?;
                    return Ok(Self {
                        wav: bytes,
                        samples,
                        rate,
                    });
                },
                _ => {},
            }
            chunks = rest.get(size.next_multiple_of(2)..).unwrap_or_default();
        }
        bail!("{} has no audio", path.display())
    }

    fn seconds(&self) -> f64 {
        self.samples.len() as f64 / f64::from(self.rate)
    }
}

/// A warm recognition path.
enum Path {
    InProcess(Recognizer),
    Http(Client),
}

impl Path {
    fn open(engine: Engine) -> anyhow::Result<Self> {
        Ok(match engine {
            Engine::InProcess {
                library,
                model,
                accelerator,
            } => Self::InProcess(Recognizer::load(&library, &model, accelerator)?),
            Engine::Http { address, key } => Self::Http(Client::connect(address, key)?),
        })
    }

    fn transcribe(&mut self, fixture: &Fixture) -> anyhow::Result<String> {
        match self {
            Self::InProcess(recognizer) => {
                Ok(recognizer.recognize(&fixture.samples, fixture.rate)?)
            },
            Self::Http(client) => client.transcribe(&fixture.wav),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse()?;
    let started = Instant::now();
    let mut path = Path::open(arguments.engine)?;
    let load = started.elapsed();
    let samples = 16_000_usize.saturating_mul(usize::try_from(arguments.warmup)?);
    let silence = Fixture {
        wav: silent_wav(u32::try_from(samples)?)?,
        samples: vec![0.0; samples],
        rate: 16_000,
    };
    let started = Instant::now();
    path.transcribe(&silence)?;
    println!(
        r#"{{"event":"ready","load_ms":{:.3},"warmup_ms":{:.3}}}"#,
        milliseconds(load),
        milliseconds(started.elapsed())
    );
    for fixture_path in &arguments.fixtures {
        let fixture = Fixture::read(fixture_path)?;
        let mut runs = Vec::with_capacity(arguments.repetitions);
        let mut output = None;
        for _ in 0..arguments.repetitions {
            thread::sleep(arguments.idle);
            let started = Instant::now();
            let text = path.transcribe(&fixture)?;
            runs.push(milliseconds(started.elapsed()));
            output.get_or_insert_with(|| Normalized::of(&text));
        }
        let Normalized { words, hash } = output.context("--repetitions must be positive")?;
        let runs = runs
            .iter()
            .map(|run| format!("{run:.3}"))
            .collect::<Vec<_>>();
        println!(
            r#"{{"event":"fixture","name":"{}","seconds":{:.3},"rate":{},"words":{words},"hash":"{hash:016x}","runs_ms":[{}]}}"#,
            fixture_path.file_name().unwrap_or_default().display(),
            fixture.seconds(),
            fixture.rate,
            runs.join(",")
        );
    }
    Ok(())
}

/// One keep-alive connection to the engine's OpenAI-compatible transcription route, sending the
/// same multipart WAV upload as the 0.3.2 app.
struct Client {
    stream: BufReader<TcpStream>,
    address: String,
    key: String,
}

impl Client {
    const BOUNDARY: &str = "speakeasy-profile-boundary";

    fn connect(address: String, key: String) -> anyhow::Result<Self> {
        let stream = TcpStream::connect(&address)?;
        stream.set_nodelay(true)?;
        Ok(Self {
            stream: BufReader::new(stream),
            address,
            key,
        })
    }

    fn transcribe(&mut self, wav: &[u8]) -> anyhow::Result<String> {
        let boundary = Self::BOUNDARY;
        let head = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"dictation.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        );
        let tail = format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n--{boundary}--\r\n"
        );
        let length = [head.len(), wav.len(), tail.len()]
            .into_iter()
            .try_fold(0_usize, usize::checked_add)
            .context("Fixture is too large to upload")?;
        let mut request = format!(
            "POST /v1/audio/transcriptions HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: multipart/form-data; boundary={boundary}\r\nContent-Length: {length}\r\n\r\n{head}",
            self.address, self.key,
        )
        .into_bytes();
        request.extend_from_slice(wav);
        request.extend_from_slice(tail.as_bytes());
        self.stream.get_mut().write_all(&request)?;
        let mut status = String::new();
        self.stream.read_line(&mut status)?;
        ensure!(
            status.contains(" 200 "),
            "Engine returned {}",
            status.trim_end()
        );
        let mut length = None;
        loop {
            let mut header = String::new();
            self.stream.read_line(&mut header)?;
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = Some(value.trim().parse::<usize>()?);
            }
        }
        let mut body = vec![0; length.context("Engine reply has no length")?];
        self.stream.read_exact(&mut body)?;
        let reply: serde_json::Value = serde_json::from_slice(&body)?;
        Ok(reply
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned())
    }
}

/// `samples` of 16 kHz silence as a PCM16 WAV; one second is the app's warmup request.
fn silent_wav(samples: u32) -> anyhow::Result<Vec<u8>> {
    let data_bytes = samples.checked_mul(2).context("Warmup is too long")?;
    let mut wav = Vec::new();
    for field in [
        &b"RIFF"[..],
        &data_bytes
            .checked_add(36)
            .context("Warmup is too long")?
            .to_le_bytes(),
        b"WAVEfmt ",
        &16_u32.to_le_bytes(),
        &1_u16.to_le_bytes(),
        &1_u16.to_le_bytes(),
        &16_000_u32.to_le_bytes(),
        &32_000_u32.to_le_bytes(),
        &2_u16.to_le_bytes(),
        &16_u16.to_le_bytes(),
        b"data",
        &data_bytes.to_le_bytes(),
    ] {
        wav.extend_from_slice(field);
    }
    wav.resize(wav.len().saturating_add(usize::try_from(data_bytes)?), 0);
    Ok(wav)
}

/// Lowercase alphanumeric words, so harmless spacing differences between paths compare equal.
struct Normalized {
    words: usize,
    hash: u64,
}

impl Normalized {
    fn of(text: &str) -> Self {
        let words = text
            .split_whitespace()
            .map(|word| {
                word.chars()
                    .filter(|character| character.is_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>()
            })
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>();
        // FNV-1a over the space-joined words: stable across runs, toolchains, and languages.
        let hash = words
            .join(" ")
            .bytes()
            .fold(0xCBF2_9CE4_8422_2325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01B3)
            });
        Self {
            words: words.len(),
            hash,
        }
    }
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
