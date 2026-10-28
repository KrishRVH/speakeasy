//! Parses both engines' replies and pins their arguments, drives startup and requests against
//! fake engine scripts and programs that take the engine's port, and recognizes a public
//! recording with a real engine in an opt-in run.

use std::{path::PathBuf, time::Instant};

use super::*;

fn narration(body: &str) -> anyhow::Result<Narration> {
    reply::<Timed>(body.as_bytes())?.into_narration()
}

fn timed(words: &[(u64, u64, &str)]) -> Narration {
    Narration::Words(
        words
            .iter()
            .map(|&(start, end, text)| Word {
                span: Span::new(Moment::from_millis(start), Moment::from_millis(end)),
                text: text.to_owned(),
            })
            .collect(),
    )
}

#[test]
fn parakeet_words_keep_their_times_and_punctuation() -> anyhow::Result<()> {
    // Trimmed from a NeMo-Speech.cpp reply to whisper.cpp's public `jfk.wav`.
    let reply = r#"{"duration":11,"language":"","task":"transcribe","text":"And so, my",
        "words":[{"confidence":1,"end":0.56000000000000005,"start":0.23999999999999999,"word":"And"},
                 {"confidence":1,"end":1.04,"start":0.56000000000000005,"word":"so,"},
                 {"confidence":1,"end":1.28,"start":1.04,"word":"my"}]}"#;
    assert_eq!(
        narration(reply)?,
        timed(&[(240, 560, "And"), (560, 1_040, "so,"), (1_040, 1_280, "my")])
    );
    Ok(())
}

#[test]
fn whisper_tokens_join_into_words_without_control_tokens() -> anyhow::Result<()> {
    let reply = r#"{"text":" Ask yourself.","segments":[{"words":[
        {"word":"[_BEG_]","start":0.0,"end":0.0},
        {"word":" Ask","start":0.10,"end":0.30},
        {"word":" your","start":0.30,"end":0.40},
        {"word":"self.","start":0.40,"end":0.62}]}]}"#;
    assert_eq!(
        narration(reply)?,
        timed(&[(100, 300, "Ask"), (300, 620, "yourself.")])
    );
    Ok(())
}

#[test]
fn replies_without_timings_keep_their_text_and_bad_timings_fail() -> anyhow::Result<()> {
    assert_eq!(
        narration(r#"{"text":" Untimed. "}"#)?,
        Narration::Text("Untimed.".into())
    );
    assert_eq!(
        narration(r#"{"text":"","words":[]}"#)?,
        Narration::Words(Vec::new())
    );
    for bad in [
        r#"{"words":[{"word":"x","start":2.0,"end":1.0}]}"#,
        r#"{"words":[{"word":"x","start":-1.0,"end":1.0}]}"#,
    ] {
        assert!(narration(bad).is_err(), "{bad}");
    }
    Ok(())
}

#[test]
fn malformed_replies_never_quote_engine_text() {
    let error = reply::<Timed>(br#"{"words":"private transcript"}"#)
        .err()
        .map(|error| format!("{error:#}"));
    assert!(
        error.is_some_and(|error| !error.contains("private")),
        "A malformed reply was accepted or quoted"
    );
}

#[test]
fn exit_diagnostics_identify_actions_without_engine_output() {
    for (code, signal, remedy) in [
        (Some(2), None, "server arguments"),
        (None, Some(4), "CPU instructions"),
        (Some(0xC000_001D), None, "CPU instructions"),
        (Some(0xC000_0135), None, "native libraries"),
        (Some(0xC000_0139), None, "native libraries"),
        (Some(0xC000_007B), None, "native libraries"),
        (Some(1), None, "selected model"),
    ] {
        assert!(
            exit_remedy(code.map(u32::cast_signed), signal).contains(remedy),
            "Exit code {code:X?}, signal {signal:?}"
        );
    }
}

#[test]
fn server_arguments_match_each_engine() {
    let arguments = |config: &Config| {
        server_command(config, 8080, "secret", Path::new("served"))
            .as_std()
            .get_args()
            .filter_map(|argument| argument.to_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let whisper = Config {
        model: "model.bin".into(),
        use_gpu: false,
        ..Config::default()
    };
    assert_eq!(
        arguments(&whisper),
        [
            "--model",
            "model.bin",
            "--host",
            "127.0.0.1",
            "--port",
            "8080",
            "--request-path",
            "/secret",
            "--public",
            "served",
            "--threads",
            "4",
            "--language",
            "en",
            "--no-gpu",
        ],
        "Whisper keeps default timestamp decoding and ends with its CPU switch"
    );
    let whisper_on_gpu = Config {
        use_gpu: true,
        ..whisper
    };
    assert_eq!(
        arguments(&whisper_on_gpu).last().map(String::as_str),
        Some("en")
    );
    let parakeet = Config {
        engine: Engine::Parakeet,
        model: "model.nemo".into(),
        ..Config::default()
    };
    assert_eq!(
        arguments(&parakeet),
        [
            "serve",
            "--asr-model",
            "model.nemo",
            "--device",
            "auto",
            "--host",
            "127.0.0.1",
            "--port",
            "8080",
            "--no-ui",
            "--threads",
            "1",
            "--asr.batching.enabled=false",
        ]
    );
}

#[test]
fn engine_secrets_are_unrelated_128_bit_tokens() -> anyhow::Result<()> {
    let (first, second) = (random_token()?, random_token()?);
    assert_eq!(first.len(), 32);
    assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(first, second);
    Ok(())
}

#[tokio::test]
async fn a_truncated_reply_reports_only_a_stopped_engine() -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let url = format!(
        "http://{}/{}/inference",
        listener.local_addr()?,
        random_token()?
    );
    let engine = tokio::spawn(async move {
        let mut stream = BufReader::new(listener.accept().await?.0);
        let mut line = String::new();
        while stream.read_line(&mut line).await? > 0 && !line.trim().is_empty() {
            line.clear();
        }
        // Promises more of the reply than it sends, then hangs up.
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 64\r\n\r\n{\"text\":")
            .await?;
        anyhow::Ok(())
    });
    let response = engine_client()?.get(&url).send().await?;
    engine.await??;
    let error = read_bounded(response)
        .await
        .err()
        .context("A truncated reply was accepted")?;
    assert_eq!(format!("{error:#}"), STOPPED);
    Ok(())
}

#[cfg(target_os = "linux")]
fn fake_engine(script: &str) -> anyhow::Result<(tempfile::TempDir, Config)> {
    use std::os::unix::fs::PermissionsExt as _;
    let directory = tempfile::tempdir()?;
    let executable = directory.path().join("worker");
    std::fs::write(&executable, script)?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
    let config = Config {
        engine_executable: executable,
        use_gpu: false,
        ..Config::default()
    };
    Ok((directory, config))
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn argument_failure_reports_exit_status_and_keeps_stderr_private() -> anyhow::Result<()> {
    let (_directory, config) =
        fake_engine("#!/bin/sh\nprintf 'private transcript or credential' >&2\nexit 2\n")?;
    let (_cancel, cancelled) = watch::channel(false);
    let result = LocalSpeech::start(config, cancelled).await;
    let error = format!(
        "{:#}",
        result.err().context("Engine unexpectedly became ready")?
    );
    assert!(error.contains("exit status: 2"));
    assert!(error.contains("server arguments"));
    assert!(!error.contains("private transcript"));
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn startup_cancellation_reaps_the_owned_process() -> anyhow::Result<()> {
    let (directory, config) = fake_engine(
        "#!/bin/sh\nprintf '%s' \"$$\" > \"$(dirname \"$0\")/worker.pid\"\nexec sleep 60\n",
    )?;
    let (cancel, cancelled) = watch::channel(false);
    let task = tokio::spawn(LocalSpeech::start(config, cancelled));
    let pid: u32 = timeout(
        Duration::from_secs(5),
        fixture_number(&directory.path().join("worker.pid")),
    )
    .await?;
    cancel.send_replace(true);
    assert!(
        timeout(Duration::from_secs(5), task)
            .await?
            .is_ok_and(|result| result.is_err())
    );
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "Cancelled startup left a live or unreaped worker"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_program_that_takes_the_released_port_never_becomes_the_engine() -> anyhow::Result<()> {
    let (directory, config) = fake_engine(concat!(
        "#!/bin/sh\n",
        "while [ \"$#\" -gt 0 ]; do\n",
        "  [ \"$1\" = --port ] && printf '%s' \"$2\" > \"$(dirname \"$0\")/worker.port\"\n",
        "  shift\n",
        "done\n",
        "exec sleep 60\n",
    ))?;
    let (_cancel, cancelled) = watch::channel(false);
    let start = tokio::spawn(LocalSpeech::start(config, cancelled));
    let port: u16 = timeout(
        Duration::from_secs(5),
        fixture_number(&directory.path().join("worker.port")),
    )
    .await?;
    let squatter = tokio::net::TcpListener::bind((LOOPBACK, port)).await?;
    let started = tokio::select! {
        started = start => started?,
        failed = impersonate_engine(&squatter) => {
            failed?;
            bail!("The impersonated engine stopped answering");
        },
    };
    let error = started
        .err()
        .context("The program on the engine's port became the engine")?;
    assert_eq!(
        error.to_string(),
        "Another program took the local speech port. Try again."
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn audio_never_reaches_a_program_that_took_a_ready_engines_port() -> anyhow::Result<()> {
    let squatter = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let mut command = hidden_command("sleep");
    command.arg("60").process_group(0);
    let mut child = command.spawn()?;
    // A ready engine that has since lost its port to the squatter.
    let mut speech = LocalSpeech {
        engine: Engine::Whisper,
        group: contain(&mut child).await?,
        child,
        port: squatter.local_addr()?.port(),
        secret: random_token()?,
        client: engine_client()?,
        _directory: tempfile::tempdir()?,
    };
    let wav = audio::silent_wav(16_000, Duration::from_secs(1))?;
    let transcribed = tokio::select! {
        transcribed = speech.transcribe(wav, "en") => transcribed,
        failed = impersonate_engine(&squatter) => {
            failed?;
            bail!("The impersonated engine stopped answering");
        },
    };
    speech.stop().await;
    let error = transcribed
        .err()
        .context("The program on the engine's port transcribed")?;
    assert_eq!(error.to_string(), STOPPED);
    Ok(())
}

/// Answers every request on `listener` with an empty transcript, as a program that took the
/// engine's port could; returns only on failure.
#[cfg(target_os = "linux")]
async fn impersonate_engine(listener: &tokio::net::TcpListener) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    loop {
        let mut stream = BufReader::new(listener.accept().await?.0);
        let mut length = 0;
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line).await? == 0 || line.trim().is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse()?;
            }
        }
        stream.read_exact(&mut vec![0; length]).await?;
        stream
            .write_all(
                concat!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n",
                    "Content-Length: 11\r\nConnection: close\r\n\r\n{\"text\":\"\"}",
                )
                .as_bytes(),
            )
            .await?;
    }
}

#[cfg(target_os = "linux")]
async fn fixture_number<T: std::str::FromStr>(path: &Path) -> T {
    loop {
        if let Ok(text) = tokio::fs::read_to_string(path).await
            && let Ok(number) = text.parse()
        {
            return number;
        }
        sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
#[ignore = "Requires SPEAKEASY_FIXTURE_CONFIG and SPEAKEASY_FIXTURE_WAV (whisper.cpp samples/jfk.wav)"]
async fn local_worker_recognizes_fixture_and_stops() -> anyhow::Result<()> {
    let path = PathBuf::from(std::env::var("SPEAKEASY_FIXTURE_CONFIG")?);
    let config = Config::read(&path)?.validated(&path)?.into_inner();
    let wav = std::fs::read(std::env::var("SPEAKEASY_FIXTURE_WAV")?)?;
    let language = config.language.clone();
    let started = Instant::now();
    let (_cancel, cancelled) = watch::channel(false);
    let mut worker = LocalSpeech::start(config, cancelled).await?;
    let startup = started.elapsed();
    let started = Instant::now();
    let result = worker.transcribe(wav, &language).await?;
    eprintln!(
        "Engine startup: {startup:?}; inference: {:?}",
        started.elapsed()
    );
    // Failure output must never include the recognized text.
    assert!(
        result.to_lowercase().contains("ask not what your country"),
        "Fixture recognition did not contain the expected phrase"
    );
    let stopped = Instant::now();
    worker.stop().await;
    eprintln!("Engine shutdown: {:?}", stopped.elapsed());
    assert!(
        worker.child.try_wait()?.is_some(),
        "Owned worker did not terminate"
    );
    Ok(())
}
