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

    for timings in [
        r#""words":[{"word":"Kept","start":0.1,"end":0.3},{"word":"text.","start":2.0,"end":1.0}]"#,
        r#""words":[{"word":"Kept","start":0.1,"end":0.3},{"word":"text.","start":0.3}]"#,
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
fn exits_name_the_remedies_their_status_proves_and_only_startup_blames_arguments() {
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt as _;

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
        server_command(config, 8080)
            .as_std()
            .get_args()
            .filter_map(|argument| argument.to_str().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let whisper = Config {
        engine: Engine::Whisper,
        language: "de-DE".into(),
        model: "model.bin".into(),
        use_gpu: false,
        threads: 3,
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
            "--threads",
            "3",
            "--language",
            "de",
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
        Some("de")
    );
    let nemo = Config {
        engine: Engine::Nemo,
        model: "model.nemo".into(),
        ..Config::default()
    };
    assert!(
        server_command(&nemo, 8080)
            .as_std()
            .get_envs()
            .any(|(name, value)| name == "NEMO_SPEECH_HTTP_API_KEY" && value.is_none()),
        "NeMo inherited an API key the requests do not send"
    );
    assert_eq!(
        arguments(&nemo),
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

#[cfg(target_os = "macos")]
#[test]
fn engines_on_apple_gpus_skip_ggml_metal_tensor_kernels() {
    for engine in [Engine::Nemo, Engine::Whisper] {
        let config = Config {
            engine,
            ..Config::default()
        };
        let command = server_command(&config, 8080);
        assert!(
            command.as_std().get_envs().any(|(name, value)| {
                name == "GGML_METAL_TENSOR_DISABLE" && value == Some("1".as_ref())
            }),
            "{engine:?}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn startup_probes_each_engine_at_its_own_health_route() -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    for (protocol, name) in [
        (Protocol::Whisper, "Whisper"),
        (Protocol::Nemo, "NeMo"),
        (Protocol::Nemotron, "Nemotron"),
    ] {
        let (_directory, mut speech, listener) = shell_engine("read -r _").await?;
        speech.protocol = protocol;
        let expected = match protocol {
            Protocol::Whisper => "GET /health HTTP/1.1\r\n",
            Protocol::Nemo | Protocol::Nemotron => "GET /ready HTTP/1.1\r\n",
        };
        let engine = async {
            let mut stream = BufReader::new(listener.accept().await?.0);
            let mut request_line = String::new();
            stream.read_line(&mut request_line).await?;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await?;
            anyhow::Ok(request_line)
        };
        let (served, request_line) = timeout(crate::testing::PATIENCE, async {
            tokio::join!(speech.wait_until_serving(), engine)
        })
        .await?;
        speech.stop().await;
        served?;
        let request_line = request_line?;
        assert!(
            request_line == expected,
            "Startup probed the {name} engine at another route"
        );
    }
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
        engine: Engine::Whisper,
        engine_executable: executable,
        use_gpu: false,
        ..Config::default()
    };
    Ok((directory, config))
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_engine_that_rejects_its_arguments_names_its_exit_and_remedy() -> anyhow::Result<()> {
    let (_directory, config) = fake_engine("#!/bin/sh\nexit 2\n")?;
    let (_cancel, cancelled) = watch::channel(false);
    let result = LocalSpeech::start(config, cancelled).await;
    let error = format!(
        "{:#}",
        result.err().context("Engine unexpectedly became ready")?
    );
    assert!(error.contains("exit status: 2"));
    assert!(error.contains("server arguments"));
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
        !std::path::Path::new(&format!("/proc/{pid}")).exists(),
        "Cancelled startup left a live or unreaped worker"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_engine_that_died_while_idle_names_the_remedy_its_exit_proves() -> anyhow::Result<()> {
    use crate::testing::PATIENCE;

    let (directory, mut speech, _listener) =
        shell_engine("sleep 60 & printf '%s' \"$!\" > member.pid; kill -s ILL $$").await?;
    let leader = speech.child.id().context("Missing engine process")?;
    timeout(PATIENCE, until(|| process_state(leader) == Some('Z'))).await?;
    let member: u32 = timeout(
        PATIENCE,
        fixture_number(&directory.path().join("member.pid")),
    )
    .await?;
    let wav = audio::silent_wav(16_000, Duration::from_secs(1))?;
    let transcribed = speech.transcribe(wav, "en", &Terms::default()).await;
    let ended = timeout(
        PATIENCE,
        until(|| process_state(member).is_none_or(|state| state == 'Z')),
    )
    .await
    .is_ok();
    speech.stop().await;
    let error = transcribed.err().context("The exited engine transcribed")?;
    assert!(error.to_string().contains("CPU instructions"), "{error}");
    assert!(ended, "A process of the exited engine's group kept running");
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

#[cfg(target_os = "linux")]
async fn crashing_engine(
    script: &str,
) -> anyhow::Result<(
    tempfile::TempDir,
    LocalSpeech,
    impl Future<Output = anyhow::Result<()>> + use<>,
)> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    let (directory, mut speech, listener) =
        shell_engine(&format!("{script}read -r _ && kill -s ILL $$")).await?;
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

#[cfg(unix)]
async fn shell_engine(
    script: &str,
) -> anyhow::Result<(tempfile::TempDir, LocalSpeech, tokio::net::TcpListener)> {
    let listener = tokio::net::TcpListener::bind((LOOPBACK, 0)).await?;
    let directory = tempfile::tempdir()?;
    let mut child = hidden_command("sh")
        .arg("-c")
        .arg(format!("ulimit -c 0; {script}"))
        .current_dir(directory.path())
        .stdin(std::process::Stdio::piped())
        .process_group(0)
        .spawn()?;
    let speech = LocalSpeech {
        protocol: Protocol::Whisper,
        group: contain(&mut child).await?,
        child,
        port: listener.local_addr()?.port(),
        client: engine_client()?,
    };
    Ok((directory, speech, listener))
}

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

#[cfg(target_os = "linux")]
#[tokio::test]
async fn streaming_sends_per_request_language_and_hints() -> anyhow::Result<()> {
    use futures_util::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    let (_directory, mut speech, listener) = shell_engine("read -r _").await?;
    speech.protocol = Protocol::Nemotron;
    let mut terms = Terms::default();
    terms.add("Example Name")?;
    let (frames, received) = tokio::sync::mpsc::channel(2);
    frames.send(vec![0; 3200]).await?;
    drop(frames);
    let (ending, tail) = tokio::sync::oneshot::channel();
    assert!(ending.send(Vec::new()).is_ok());
    let server = async {
        let mut socket = accept_async(listener.accept().await?.0).await?;
        socket
            .send(Message::Text(r#"{"type":"session.created"}"#.into()))
            .await?;
        let Some(Ok(Message::Text(update))) = socket.next().await else {
            bail!("Missing session update");
        };
        let update: serde_json::Value = serde_json::from_str(&update)?;
        assert_eq!(update["session"]["language"], "de-DE");
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

#[cfg(target_os = "linux")]
#[tokio::test]
async fn whisper_requests_carry_hints_as_the_prompt_only_when_listed() -> anyhow::Result<()> {
    let (_directory, mut speech, listener) = shell_engine("read -r _").await?;
    let mut terms = Terms::default();
    terms.add("Example Name")?;
    terms.add("Kubernetes")?;
    let mut prompts = Vec::new();
    for terms in [terms, Terms::default()] {
        let wav = audio::silent_wav(16_000, Duration::from_secs(1))?;
        let (transcribed, body) = timeout(crate::testing::PATIENCE, async {
            tokio::join!(speech.transcribe(wav, "en", &terms), async {
                answer_silence(listener.accept().await?.0).await
            })
        })
        .await?;
        transcribed?;
        prompts.push(multipart_field(&body?, "prompt"));
    }
    speech.stop().await;
    assert_eq!(prompts, [Some("Example Name, Kubernetes".to_owned()), None]);
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn only_whisper_word_timing_requests_wrap_segments_between_words() -> anyhow::Result<()> {
    let (_directory, mut speech, listener) = shell_engine("read -r _").await?;
    let terms = Terms::default();
    let mut splits = Vec::new();
    for words in [true, false] {
        let wav = audio::silent_wav(16_000, Duration::from_secs(1))?;
        let (transcribed, body) = timeout(crate::testing::PATIENCE, async {
            tokio::join!(
                async {
                    if words {
                        speech.transcribe_words(wav, "en", &terms).await.map(drop)
                    } else {
                        speech.transcribe(wav, "en", &terms).await.map(drop)
                    }
                },
                async { answer_silence(listener.accept().await?.0).await }
            )
        })
        .await?;
        transcribed?;
        splits.push(multipart_field(&body?, "split_on_word"));
    }
    speech.stop().await;
    assert_eq!(splits, [Some("true".to_owned()), None]);
    Ok(())
}

#[cfg(target_os = "linux")]
fn multipart_field(body: &[u8], name: &str) -> Option<String> {
    let body = String::from_utf8_lossy(body);
    let (_, after) = body.split_once(&format!("name=\"{name}\"\r\n\r\n"))?;
    let (value, _) = after.split_once("\r\n--")?;
    Some(value.to_owned())
}

#[cfg(target_os = "linux")]
async fn answer_silence(stream: tokio::net::TcpStream) -> anyhow::Result<Vec<u8>> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let mut stream = BufReader::new(stream);
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
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await?;
    stream
        .write_all(
            concat!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n",
                "Content-Length: 11\r\nConnection: close\r\n\r\n{\"text\":\"\"}",
            )
            .as_bytes(),
        )
        .await?;
    Ok(body)
}

#[cfg(target_os = "linux")]
async fn fixture_number<T: std::str::FromStr>(path: &std::path::Path) -> T {
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

#[cfg(target_os = "linux")]
#[tokio::test]
async fn abandoned_and_failed_streams_probe_the_owned_engine_before_reuse() -> anyhow::Result<()> {
    use futures_util::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    for failed in [false, true] {
        let (_directory, mut speech, listener) = shell_engine("read -r _").await?;
        speech.protocol = Protocol::Nemotron;
        let (frames, received) = tokio::sync::mpsc::channel(1);
        let _frames = failed.then_some(frames);
        let (ending, tail) = tokio::sync::oneshot::channel();
        drop(ending);
        let server = async {
            let mut socket = accept_async(listener.accept().await?.0).await?;
            socket
                .send(Message::Text(r#"{"type":"session.created"}"#.into()))
                .await?;
            assert!(matches!(socket.next().await, Some(Ok(Message::Text(_)))));
            socket
                .send(Message::Text(r#"{"type":"session.updated"}"#.into()))
                .await?;
            if failed {
                socket
                    .send(Message::Text(r#"{"type":"error"}"#.into()))
                    .await?;
            }
            assert!(matches!(
                socket.next().await,
                None | Some(Err(_) | Ok(Message::Close(_)))
            ));

            answer_silence(listener.accept().await?.0).await.map(drop)
        };
        let terms = Terms::default();
        let result = timeout(crate::testing::PATIENCE, async {
            tokio::try_join!(
                speech.transcribe_stream(
                    LiveAudio {
                        rate: 16_000,
                        frames: received,
                        tail
                    },
                    "en-US",
                    &terms
                ),
                server
            )
        })
        .await;
        speech.stop().await;
        assert!(result??.0.is_none());
    }
    Ok(())
}

#[test]
fn whisper_tokens_preserve_whitespace_and_segment_boundaries() -> anyhow::Result<()> {
    let body = r#"{"text":"Hello world. Next part.","segments":[
        {"words":[{"word":"Hello","start":0,"end":0.1},
                  {"word":" ","start":0.1,"end":0.2},
                  {"word":"wor","start":0.2,"end":0.3},
                  {"word":"ld. ","start":0.3,"end":0.4},
                  {"word":"Next","start":0.4,"end":0.5}]},
        {"words":[{"word":"part.","start":0.5,"end":0.6}]}]}"#;
    assert_eq!(
        narration(body)?,
        timed(&[
            (0, 100, "Hello"),
            (200, 400, "world."),
            (400, 500, "Next"),
            (500, 600, "part.")
        ])
    );
    Ok(())
}
