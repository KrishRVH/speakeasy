use std::{mem, time::Duration};

use anyhow::{Context as _, bail, ensure};
use futures_util::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{WebSocketStream, client_async, tungstenite::Message};

use super::{CONNECT_TIMEOUT, INFERENCE_TIMEOUT, LOOPBACK, LocalSpeech, PARAKEET_RATES, Protocol};
use crate::{recognition_terms::Terms, runtime::ports::LiveAudio};

const FAILED: &str = "Continuous recognition stopped.";
const FINAL_TIMEOUT: Duration = Duration::from_secs(10);
type Socket = WebSocketStream<TcpStream>;

pub(super) async fn transcribe(
    speech: &LocalSpeech,
    audio: LiveAudio,
    language: &str,
    terms: &Terms,
) -> anyhow::Result<Option<String>> {
    ensure!(
        speech.protocol == Protocol::Nemotron && PARAKEET_RATES.contains(&audio.rate),
        FAILED
    );
    let connection = within(CONNECT_TIMEOUT, TcpStream::connect((LOOPBACK, speech.port))).await?;
    connection.set_nodelay(true).ok().context(FAILED)?;
    let url = format!(
        "ws://{LOOPBACK}:{}/v1/audio/transcriptions/realtime",
        speech.port
    );
    let (mut socket, _) = within(CONNECT_TIMEOUT, client_async(url, connection)).await?;
    within(CONNECT_TIMEOUT, async {
        expect(&mut socket, Kind::Created).await?;
        let session = serde_json::json!({
            "type": "session.update",
            "session": {
                "sample_rate": audio.rate,
                "language": crate::models::nemotron_language(language),
                "speech_contexts": terms.contexts()
            }
        });
        send(&mut socket, Message::Text(session.to_string().into())).await?;
        expect(&mut socket, Kind::Updated).await
    })
    .await?;
    within(INFERENCE_TIMEOUT, recognize(&mut socket, audio)).await
}

async fn recognize(socket: &mut Socket, mut audio: LiveAudio) -> anyhow::Result<Option<String>> {
    let mut committed = false;
    let mut final_seen = false;
    let mut text = String::new();
    let chunk_size = usize::try_from(audio.rate / 10)?.saturating_mul(2);
    ensure!(chunk_size > 0, FAILED);
    let deadline = tokio::time::sleep(FINAL_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline, if committed => bail!(FAILED),
            frame = audio.frames.recv(), if !committed => {
                if let Some(mut bytes) = frame {
                    send(socket, Message::Binary(mem::take(&mut *bytes).into())).await?;
                } else {
                    let Ok(tail) = (&mut audio.tail).await else {
                        return Ok(None);
                    };

                    for chunk in tail.chunks(chunk_size) {
                        send(socket, Message::Binary(chunk.to_vec().into())).await?;
                    }
                    send(
                        socket,
                        Message::Text(r#"{"type":"input_audio_buffer.commit"}"#.into()),
                    ).await?;
                    committed = true;
                    let end = tokio::time::Instant::now()
                        .checked_add(FINAL_TIMEOUT)
                        .context(FAILED)?;
                    deadline.as_mut().reset(end);
                }
            },
            message = socket.next() => {
                let Some(Ok(message)) = message else {
                    bail!(FAILED);
                };
                match message {
                    Message::Text(bytes) => {
                        let Event { kind, transcript } = parse(&bytes)?;
                        match kind {
                            Kind::Completed => {
                                let words = transcript.as_deref().context(FAILED)?.trim();
                                if !text.is_empty() && !words.is_empty() {
                                    text.push(' ');
                                }
                                text.push_str(words);
                                final_seen = true;
                            },
                            Kind::Committed => {
                                ensure!(committed && final_seen, FAILED);
                                return Ok(Some(text));
                            },
                            Kind::Error => bail!(FAILED),
                            Kind::Created | Kind::Updated | Kind::Ignored => {},
                        }
                    },
                    Message::Ping(_) => within(CONNECT_TIMEOUT, socket.flush()).await?,
                    Message::Pong(_) => {},
                    _ => bail!(FAILED),
                }
            },
        }
    }
}

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: Kind,
    transcript: Option<String>,
}

#[derive(Deserialize, PartialEq, Eq)]
enum Kind {
    #[serde(rename = "session.created")]
    Created,
    #[serde(rename = "session.updated")]
    Updated,
    #[serde(rename = "conversation.item.input_audio_transcription.completed")]
    Completed,
    #[serde(rename = "input_audio_buffer.committed")]
    Committed,
    #[serde(rename = "error")]
    Error,
    #[serde(other)]
    Ignored,
}

fn parse(text: &str) -> anyhow::Result<Event> {
    serde_json::from_str(text).ok().context(FAILED)
}

async fn expect(socket: &mut Socket, expected: Kind) -> anyhow::Result<()> {
    let Some(Ok(Message::Text(text))) = socket.next().await else {
        bail!(FAILED);
    };
    ensure!(parse(&text)?.kind == expected, FAILED);
    Ok(())
}

async fn send(socket: &mut Socket, message: Message) -> anyhow::Result<()> {
    within(CONNECT_TIMEOUT, socket.send(message)).await
}

async fn within<T, E>(
    limit: Duration,
    work: impl Future<Output = Result<T, E>>,
) -> anyhow::Result<T> {
    timeout(limit, work)
        .await
        .ok()
        .and_then(Result::ok)
        .context(FAILED)
}

#[cfg(test)]
mod tests;
