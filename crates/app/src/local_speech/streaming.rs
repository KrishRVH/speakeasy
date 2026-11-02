use std::{mem, time::Duration};

use anyhow::{Context as _, bail, ensure};
use futures_util::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{
        Message, client::IntoClientRequest as _, http::HeaderValue, protocol::WebSocketConfig,
    },
};
use zeroize::Zeroizing;

use super::{
    CONNECT_TIMEOUT, INFERENCE_TIMEOUT, LOOPBACK, LocalSpeech, MAX_RESPONSE_BYTES, PARAKEET_RATES,
};
use crate::{recognition_terms::Terms, runtime::ports::LiveAudio};

const FAILED: &str =
    "Continuous recognition stopped. Retrying this recording with the selected model.";
const FINAL_TIMEOUT: Duration = Duration::from_secs(10);
type Socket = WebSocketStream<TcpStream>;

pub(super) async fn transcribe(
    speech: &mut LocalSpeech,
    audio: LiveAudio,
    language: &str,
    terms: &Terms,
) -> anyhow::Result<Option<String>> {
    ensure!(
        speech.nemotron && PARAKEET_RATES.contains(&audio.rate),
        FAILED
    );
    speech.verify_listener().await?;
    let connection = within(CONNECT_TIMEOUT, TcpStream::connect((LOOPBACK, speech.port))).await?;
    connection.set_nodelay(true).ok().context(FAILED)?;
    speech.verify_listener().await?;
    let mut request = format!(
        "ws://{LOOPBACK}:{}/v1/audio/transcriptions/realtime",
        speech.port
    )
    .into_client_request()
    .ok()
    .context(FAILED)?;
    let mut authorization = HeaderValue::from_str(&format!("Bearer {}", speech.secret))
        .ok()
        .context(FAILED)?;
    authorization.set_sensitive(true);
    request.headers_mut().insert("Authorization", authorization);
    let limits = WebSocketConfig::default()
        .max_message_size(Some(MAX_RESPONSE_BYTES))
        .max_frame_size(Some(MAX_RESPONSE_BYTES));
    let (mut socket, _) = within(
        CONNECT_TIMEOUT,
        client_async_with_config(request, connection, Some(limits)),
    )
    .await?;
    within(CONNECT_TIMEOUT, async {
        expect(&mut socket, "session.created").await?;
        let session = serde_json::json!({
            "type": "session.update",
            "session": {
                "sample_rate": audio.rate,
                "language": crate::models::nemotron_language(language),
                "speech_contexts": terms.contexts()
            }
        });
        send(&mut socket, Message::Text(session.to_string().into())).await?;
        expect(&mut socket, "session.updated").await
    })
    .await?;
    within(INFERENCE_TIMEOUT, recognize(&mut socket, audio)).await
}

async fn recognize(socket: &mut Socket, mut audio: LiveAudio) -> anyhow::Result<Option<String>> {
    let mut committed = false;
    let mut final_seen = false;
    let mut text = Zeroizing::new(String::new());
    let chunk_size = usize::try_from(audio.rate / 10)?.saturating_mul(2);
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
                        Message::Text("{\"type\":\"input_audio_buffer.commit\"}".into()),
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
                        let event = parse(&bytes)?;
                        match event.kind.as_str() {
                            "conversation.item.input_audio_transcription.completed" => {
                                let words = event.transcript.as_deref().context(FAILED)?.trim();
                                ensure!(
                                    words.len().saturating_add(1)
                                        <= MAX_RESPONSE_BYTES.saturating_sub(text.len()),
                                    FAILED
                                );
                                if !text.is_empty() && !words.is_empty() {
                                    text.push(' ');
                                }
                                text.push_str(words);
                                final_seen = true;
                            },
                            "input_audio_buffer.committed" => {
                                ensure!(committed && final_seen, FAILED);
                                return Ok(Some(mem::take(&mut *text)));
                            },
                            "error" => bail!(FAILED),
                            _ => {},
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
    kind: String,
    transcript: Option<String>,
}

fn parse(text: &str) -> anyhow::Result<Event> {
    ensure!(text.len() <= MAX_RESPONSE_BYTES, FAILED);
    serde_json::from_str(text).ok().context(FAILED)
}

async fn expect(socket: &mut Socket, kind: &str) -> anyhow::Result<()> {
    let Some(Ok(Message::Text(text))) = socket.next().await else {
        bail!(FAILED);
    };
    ensure!(parse(&text)?.kind == kind, FAILED);
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
