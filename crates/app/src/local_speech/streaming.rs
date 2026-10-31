//! Authenticated continuous PCM recognition on the owned engine. Only committed finals return;
//! transport, protocol, or backpressure failures leave whole-recording fallback to the owner.

use std::{mem, time::Duration};

use anyhow::{anyhow, ensure};
use futures_util::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use tokio::{net::TcpStream, time::timeout};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config,
    tungstenite::{Message, client::IntoClientRequest as _, protocol::WebSocketConfig},
};
use zeroize::Zeroizing;

use super::{CONNECT_TIMEOUT, INFERENCE_TIMEOUT, LOOPBACK, LocalSpeech, MAX_RESPONSE_BYTES};
use crate::runtime::ports::LiveAudio;

const FAILED: &str =
    "Continuous recognition stopped. Retrying this recording with the selected model.";
const FINAL_TIMEOUT: Duration = Duration::from_secs(10);
type Socket = WebSocketStream<TcpStream>;

pub(super) async fn transcribe(
    speech: &mut LocalSpeech,
    audio: LiveAudio,
    language: &str,
    terms: &crate::recognition_terms::Terms,
) -> anyhow::Result<Option<String>> {
    ensure!(
        speech.nemotron && (8_000..=96_000).contains(&audio.rate),
        FAILED
    );
    speech.verify_listener().await?;
    let address = (LOOPBACK, speech.port);
    let connection = timeout(CONNECT_TIMEOUT, TcpStream::connect(address))
        .await
        .map_err(|_private| anyhow!(FAILED))?
        .map_err(|_private| anyhow!(FAILED))?;
    connection
        .set_nodelay(true)
        .map_err(|_private| anyhow!(FAILED))?;
    speech.verify_listener().await?;
    let mut request = format!(
        "ws://{LOOPBACK}:{}/v1/audio/transcriptions/realtime",
        speech.port
    )
    .into_client_request()
    .map_err(|_private| anyhow!(FAILED))?;
    let mut authorization = tokio_tungstenite::tungstenite::http::HeaderValue::from_str(&format!(
        "Bearer {}",
        speech.secret
    ))
    .map_err(|_private| anyhow!(FAILED))?;
    authorization.set_sensitive(true);
    request.headers_mut().insert("Authorization", authorization);
    let limits = WebSocketConfig::default()
        .max_message_size(Some(MAX_RESPONSE_BYTES))
        .max_frame_size(Some(MAX_RESPONSE_BYTES));
    let (mut socket, _) = timeout(
        CONNECT_TIMEOUT,
        client_async_with_config(request, connection, Some(limits)),
    )
    .await
    .map_err(|_private| anyhow!(FAILED))?
    .map_err(|_private| anyhow!(FAILED))?;
    timeout(CONNECT_TIMEOUT, async {
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
    .await
    .map_err(|_private| anyhow!(FAILED))??;
    timeout(INFERENCE_TIMEOUT, recognize(&mut socket, audio))
        .await
        .map_err(|_private| anyhow!(FAILED))?
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
            () = &mut deadline, if committed => return Err(anyhow!(FAILED)),
            frame = audio.frames.recv(), if !committed => {
                if let Some(mut bytes) = frame {
                    send(socket, Message::Binary(mem::take(&mut *bytes).into())).await?;
                } else {
                    let Ok(tail) = (&mut audio.tail).await else {
                        return Ok(None);
                    };
                    // Capture never waits on inference. Its final tail can include a backlog;
                    // preserve every sample while keeping each network write small and bounded.
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
                        .ok_or_else(|| anyhow!(FAILED))?;
                    deadline.as_mut().reset(end);
                }
            },
            message = socket.next() => {
                let message = message
                    .ok_or_else(|| anyhow!(FAILED))?
                    .map_err(|_private| anyhow!(FAILED))?;
                match message {
                    Message::Text(bytes) => {
                        let event = parse(&bytes)?;
                        match event.kind.as_str() {
                            "conversation.item.input_audio_transcription.completed" => {
                                let words = event.transcript.as_deref()
                                    .ok_or_else(|| anyhow!(FAILED))?.trim();
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
                                // NeMo Speech 0.2.0 emits finish()'s finals before this acknowledgement.
                                ensure!(committed && final_seen, FAILED);
                                return Ok(Some(mem::take(&mut *text)));
                            },
                            "error" => return Err(anyhow!(FAILED)),
                            _ => {},
                        }
                    },
                    Message::Ping(_) => {
                        timeout(CONNECT_TIMEOUT, socket.flush()).await
                            .map_err(|_private| anyhow!(FAILED))?
                            .map_err(|_private| anyhow!(FAILED))?;
                    },
                    Message::Pong(_) => {},
                    _ => return Err(anyhow!(FAILED)),
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
    serde_json::from_str(text).map_err(|_private| anyhow!(FAILED))
}

async fn expect(socket: &mut Socket, kind: &str) -> anyhow::Result<()> {
    let message = socket
        .next()
        .await
        .ok_or_else(|| anyhow!(FAILED))?
        .map_err(|_private| anyhow!(FAILED))?;
    let Message::Text(text) = message else {
        return Err(anyhow!(FAILED));
    };
    ensure!(parse(&text)?.kind == kind, FAILED);
    Ok(())
}

async fn send(socket: &mut Socket, message: Message) -> anyhow::Result<()> {
    timeout(CONNECT_TIMEOUT, socket.send(message))
        .await
        .map_err(|_private| anyhow!(FAILED))?
        .map_err(|_private| anyhow!(FAILED))
}

#[cfg(test)]
mod tests;
