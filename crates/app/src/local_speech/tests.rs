//! Parses both engines' replies and pins their arguments, drives startup and requests against
//! fake engine scripts and programs that take the engine's port, and recognizes a public
//! recording with a real engine in an opt-in run.

use std::{path::PathBuf, time::Instant};

use super::*;

fn narration(body: &str) -> anyhow::Result<Narration> {
    reply::<Timed>(body.as_bytes()).map(Timed::into_narration)
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
fn replies_without_usable_timings_keep_only_their_text() -> anyhow::Result<()> {
    assert_eq!(
        narration(r#"{"text":" Untimed. "}"#)?,
        Narration::Text("Untimed.".into())
    );
    assert_eq!(
        narration(r#"{"text":"","words":[]}"#)?,
        Narration::Words(Vec::new())
    );
    // One unusable time, beside good ones, leaves the whole reply untimed: its text, or the words'
    // own text when it has none.
    for timings in [
        r#""words":[{"word":"Kept","start":0.1,"end":0.3},{"word":"text.","start":2.0,"end":1.0}]"#,
        r#""words":[{"word":"Kept","start":-1.0,"end":0.3},{"word":"text.","start":0.3,"end":0.6}]"#,
        r#""words":[{"word":"Kept","start":null,"end":0.3},{"word":"text.","start":0.3,"end":0.6}]"#,
        r#""words":[{"word":"Kept","start":0.1,"end":0.3},{"word":"text.","start":0.3}]"#,
        r#""segments":[{"words":[{"word":" Kept","start":0.1,"end":0.3},{"word":" text.","start":0.6,"end":0.3}]}]"#,
        r#""segments":[{"words":[{"word":" Kept","start":0.1,"end":0.3},{"word":" te","start":0.3,"end":0.4},{"word":"xt.","start":null,"end":0.6}]}]"#,
    ] {
        assert_eq!(
            narration(&format!(r#"{{"text":" Its text. ",{timings}}}"#))?,
            Narration::Text("Its text.".into()),
            "{timings}"
        );
        assert_eq!(
            narration(&format!("{{{timings}}}"))?,
            Narration::Text("Kept text.".into()),
            "{timings}"
        );
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
fn exits_name_the_remedies_their_status_proves_and_only_startup_blames_arguments() {
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt as _;

    // Each status's remedy whenever the engine exits, then before it is ready. Raw Unix statuses
    // carry an exit code in their second byte, or the ending signal alone.
    #[cfg(unix)]
    let exits = [
        (SIGILL, Some("CPU instructions"), "CPU instructions"),
        (USAGE_ERROR << 8, None, "server arguments"),
        (1 << 8, None, "selected model"),
        (9, None, "selected model"),
    ];
    #[cfg(windows)]
    let exits = [
        (
            STATUS_ILLEGAL_INSTRUCTION,
            Some("CPU instructions"),
            "CPU instructions",
        ),
        (
            STATUS_DLL_NOT_FOUND,
            Some("native libraries"),
            "native libraries",
        ),
        (
            STATUS_ENTRYPOINT_NOT_FOUND,
            Some("native libraries"),
            "native libraries",
        ),
        (
            STATUS_INVALID_IMAGE_FORMAT,
            Some("native libraries"),
            "native libraries",
        ),
        (USAGE_ERROR.cast_unsigned(), None, "server arguments"),
        (1, None, "selected model"),
    ];
    for (raw, whenever, before_ready) in exits {
        let status = ExitStatus::from_raw(raw);
        match (exit_remedy(status), whenever) {
            (Some(remedy), Some(cause)) => assert!(remedy.contains(cause), "{status}"),
            (remedy, cause) => assert_eq!(remedy, cause, "{status}"),
        }
        assert!(startup_exit(status).contains(before_ready), "{status}");
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
async fn a_truncated_reply_is_a_broken_connection() -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let url = format!("http://{}/inference", listener.local_addr()?);
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
    let exchanged = exchange(engine_client()?.get(&url)).await;
    engine.await??;
    assert!(
        matches!(exchanged, Err(Unanswered::Disconnected)),
        "A truncated reply was not taken for a broken connection"
    );
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
        "Another program took the speech engine's port. Try again."
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn audio_never_reaches_a_program_that_took_a_ready_engines_port() -> anyhow::Result<()> {
    let squatter = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let mut speech = sleeping_engine(squatter.local_addr()?.port()).await?;
    let transcribed = transcribe_beside(&mut speech, &squatter).await?;
    speech.stop().await;
    let error = transcribed
        .err()
        .context("The program on the engine's port transcribed")?;
    assert_eq!(error.to_string(), STOPPED);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn audio_never_reaches_a_program_that_took_an_exited_engines_port() -> anyhow::Result<()> {
    let squatter = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let mut speech = sleeping_engine(squatter.local_addr()?.port()).await?;
    speech.stop().await;
    let error = transcribe_beside(&mut speech, &squatter)
        .await?
        .err()
        .context("The program on the exited engine's port transcribed")?;
    assert_eq!(error.to_string(), STOPPED);
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_engine_that_died_while_idle_names_the_remedy_its_exit_proves() -> anyhow::Result<()> {
    let listener = std::net::TcpListener::bind((LOOPBACK, 0))?;
    let (_directory, mut speech) = shell_engine("kill -s ILL $$", listener).await?;
    speech.child.wait().await?;
    let wav = audio::silent_wav(16_000, Duration::from_secs(1))?;
    let transcribed = speech
        .transcribe(wav, "en", &crate::recognition_terms::Terms::default())
        .await;
    speech.stop().await;
    let error = transcribed.err().context("The exited engine transcribed")?;
    assert!(error.to_string().contains("CPU instructions"), "{error}");
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_request_the_engine_crashes_during_names_its_remedy_and_ends_its_group()
-> anyhow::Result<()> {
    use crate::testing::PATIENCE;

    let (directory, mut speech, engine) =
        crashing_engine("sleep 60 & printf '%s' \"$!\" > member.pid; ").await?;
    let wav = audio::silent_wav(16_000, Duration::from_secs(1))?;
    let (transcribed, crashed) = timeout(PATIENCE, async {
        let terms = Terms::default();
        tokio::join!(speech.transcribe(wav, "en", &terms), engine)
    })
    .await?;
    crashed?;
    let member: u32 = timeout(
        PATIENCE,
        fixture_number(&directory.path().join("member.pid")),
    )
    .await?;
    let ended = timeout(
        PATIENCE,
        until(|| process_state(member).is_none_or(|state| state == 'Z')),
    )
    .await
    .is_ok();
    speech.stop().await;
    let error = transcribed
        .err()
        .context("The crashed engine transcribed")?;
    assert!(error.to_string().contains("CPU instructions"), "{error}");
    assert!(
        ended,
        "A process of the crashed engine's group kept running"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_warmup_the_engine_crashes_during_names_its_remedy() -> anyhow::Result<()> {
    use crate::testing::PATIENCE;

    let (_directory, mut speech, engine) = crashing_engine("").await?;
    let config = Config {
        use_gpu: false,
        ..Config::default()
    };
    let (_cancel, mut cancelled) = watch::channel(false);
    let (ready, crashed) = timeout(PATIENCE, async {
        tokio::join!(speech.become_ready(&config, &mut cancelled), engine)
    })
    .await?;
    crashed?;
    speech.stop().await;
    let error = ready.err().context("The crashed engine became ready")?;
    assert!(error.to_string().contains("CPU instructions"), "{error}");
    Ok(())
}

/// A `shell_engine` that runs `script`, then crashes by an illegal instruction during its first
/// transcription, and a future that plays its network side. That future answers health checks,
/// then crashes the engine once a transcription arrives, and hangs up only once the crash has
/// fixed the engine's exit status, as a real crash's connections close.
#[cfg(target_os = "linux")]
async fn crashing_engine(
    script: &str,
) -> anyhow::Result<(
    tempfile::TempDir,
    LocalSpeech,
    impl Future<Output = anyhow::Result<()>> + use<>,
)> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    let held = std::net::TcpListener::bind((LOOPBACK, 0))?;
    let listener = held.try_clone()?;
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let (directory, mut speech) =
        shell_engine(&format!("{script}read -r _ && kill -s ILL $$"), held).await?;
    let leader = speech.child.id().context("The engine exited at once")?;
    let mut crash = speech
        .child
        .stdin
        .take()
        .context("The engine has no input")?;
    let network = async move {
        loop {
            let mut stream = BufReader::new(listener.accept().await?.0);
            let mut request = String::new();
            stream.read_line(&mut request).await?;
            if !request.starts_with("GET ") {
                crash.write_all(b"\n").await?;
                until(|| process_state(leader) == Some('Z')).await;
                drop(stream);
                return Ok(());
            }
            let mut header = String::new();
            while stream.read_line(&mut header).await? > 0 && !header.trim().is_empty() {
                header.clear();
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await?;
        }
    };
    Ok((directory, speech, network))
}

/// A ready engine running `script` in `sh`, without core dumps, in a fresh working directory. It
/// reads standard input and holds `listener` as its standard output.
#[cfg(target_os = "linux")]
async fn shell_engine(
    script: &str,
    listener: std::net::TcpListener,
) -> anyhow::Result<(tempfile::TempDir, LocalSpeech)> {
    let directory = tempfile::tempdir()?;
    let port = listener.local_addr()?.port();
    let mut command = hidden_command("sh");
    command
        .arg("-c")
        .arg(format!("ulimit -c 0; {script}"))
        .current_dir(directory.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::os::fd::OwnedFd::from(listener));
    Ok((directory, engine_from(command, port).await?))
}

/// The state letter `/proc` reports for `pid`, such as `Z` for exited but unreaped; `None` once no
/// such process exists.
#[cfg(target_os = "linux")]
fn process_state(pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?.1.trim_start().chars().next()
}

#[cfg(target_os = "linux")]
async fn until(mut condition: impl FnMut() -> bool) {
    while !condition() {
        sleep(Duration::from_millis(5)).await;
    }
}

/// A ready engine, idle in `sleep`, that has since lost `port` to another program.
#[cfg(unix)]
async fn sleeping_engine(port: u16) -> anyhow::Result<LocalSpeech> {
    let mut command = hidden_command("sleep");
    command.arg("60");
    engine_from(command, port).await
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn streaming_verifies_the_listener_before_sending_credentials_or_audio() -> anyhow::Result<()>
{
    let listener = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let mut speech = sleeping_engine(listener.local_addr()?.port()).await?;
    speech.engine = Engine::Parakeet;
    speech.nemotron = true;
    let (_frames, received) = tokio::sync::mpsc::channel(1);
    let result = timeout(
        crate::testing::PATIENCE,
        speech.transcribe_stream(
            crate::runtime::ports::LiveAudio {
                rate: 16_000,
                frames: received,
                tail: tokio::sync::oneshot::channel().1,
            },
            "en-US",
            &Terms::default(),
        ),
    )
    .await;
    speech.stop().await;
    assert!(result?.is_err());
    assert!(
        timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err(),
        "Connected to a program that does not own the engine"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn streaming_authenticates_and_sends_per_request_language_and_hints() -> anyhow::Result<()> {
    use futures_util::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::{
        accept_hdr_async,
        tungstenite::{
            Message,
            handshake::server::{Request, Response},
        },
    };

    use crate::runtime::ports::LiveAudio;

    let held = std::net::TcpListener::bind((LOOPBACK, 0))?;
    let listener = held.try_clone()?;
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let (_directory, mut speech) = shell_engine("read -r _", held).await?;
    speech.engine = Engine::Parakeet;
    speech.nemotron = true;
    let authorization = format!("Bearer {}", speech.secret);
    let mut terms = Terms::default();
    terms.add("Example Name")?;
    let (frames, received) = tokio::sync::mpsc::channel(2);
    frames.send(zeroize::Zeroizing::new(vec![0; 3200])).await?;
    drop(frames);
    let (ending, tail) = tokio::sync::oneshot::channel();
    assert!(ending.send(zeroize::Zeroizing::new(Vec::new())).is_ok());
    let server = async {
        #[expect(
            clippy::result_large_err,
            reason = "The WebSocket handshake callback returns the library-owned HTTP response type"
        )]
        let mut socket = accept_hdr_async(
            listener.accept().await?.0,
            |request: &Request, response: Response| {
                assert!(
                    request
                        .headers()
                        .get("Authorization")
                        .is_some_and(|value| value == authorization.as_str())
                );
                assert_eq!(request.uri().path(), "/v1/audio/transcriptions/realtime");
                Ok(response)
            },
        )
        .await?;
        socket
            .send(Message::Text(r#"{"type":"session.created"}"#.into()))
            .await?;
        let Some(Ok(Message::Text(update))) = socket.next().await else {
            bail!("Missing session update");
        };
        let update: serde_json::Value = serde_json::from_str(&update)?;
        assert_eq!(update["session"]["language"], "auto");
        assert_eq!(update["session"]["speech_contexts"], terms.contexts());
        socket
            .send(Message::Text(r#"{"type":"session.updated"}"#.into()))
            .await?;
        assert!(matches!(socket.next().await, Some(Ok(Message::Binary(_)))));
        assert!(matches!(socket.next().await, Some(Ok(Message::Text(_)))));
        for event in [
            r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"Fixture."}"#,
            r#"{"type":"input_audio_buffer.committed"}"#,
        ] {
            socket.send(Message::Text(event.into())).await?;
        }
        anyhow::Ok(())
    };
    let outcome = timeout(crate::testing::PATIENCE, async {
        tokio::try_join!(
            speech.transcribe_stream(
                LiveAudio {
                    rate: 16_000,
                    frames: received,
                    tail,
                },
                "de",
                &terms
            ),
            server
        )
    })
    .await;
    speech.stop().await;
    let (text, ()) = outcome??;
    assert_eq!(text.as_deref(), Some("Fixture."));
    Ok(())
}

/// A ready engine serving `port` from the process `command` spawns.
#[cfg(unix)]
async fn engine_from(mut command: Command, port: u16) -> anyhow::Result<LocalSpeech> {
    let mut child = command.process_group(0).spawn()?;
    Ok(LocalSpeech {
        nemotron: false,
        engine: Engine::Whisper,
        group: contain(&mut child).await?,
        child,
        port,
        secret: random_token()?,
        client: engine_client()?,
        _directory: tempfile::tempdir()?,
    })
}

/// Asks `speech` to transcribe while `squatter` impersonates the engine on its port.
#[cfg(unix)]
async fn transcribe_beside(
    speech: &mut LocalSpeech,
    squatter: &tokio::net::TcpListener,
) -> anyhow::Result<anyhow::Result<String>> {
    let wav = audio::silent_wav(16_000, Duration::from_secs(1))?;
    let terms = Terms::default();
    tokio::select! {
        transcribed = speech.transcribe(wav, "en", &terms) => Ok(transcribed),
        failed = impersonate_engine(squatter) => {
            failed?;
            bail!("The impersonated engine stopped answering");
        },
    }
}

/// Answers every request on `listener` with an empty transcript, as a program that took the
/// engine's port could; returns only on failure.
#[cfg(unix)]
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
    let terms = config.recognition_terms.clone();
    let mut worker = LocalSpeech::start(config, cancelled).await?;
    let startup = started.elapsed();
    let started = Instant::now();
    let result = worker.transcribe(wav, &language, &terms).await?;
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
