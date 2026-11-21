use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::{accept_async, client_async};

use super::*;
use crate::testing::PATIENCE;

async fn sockets() -> anyhow::Result<(Socket, Socket)> {
    let listener = TcpListener::bind((LOOPBACK, 0)).await?;
    let address = listener.local_addr()?;
    let client = async {
        let stream = TcpStream::connect(address).await?;
        client_async(format!("ws://{address}/"), stream)
            .await
            .map(|(socket, _)| socket)
            .map_err(anyhow::Error::from)
    };
    let server = async {
        accept_async(listener.accept().await?.0)
            .await
            .map_err(anyhow::Error::from)
    };
    timeout(PATIENCE, async { tokio::try_join!(client, server) }).await?
}

fn ended(frames: &[&[u8]]) -> LiveAudio {
    let (sender, received) = mpsc::channel(frames.len().max(1));
    for frame in frames {
        assert!(sender.try_send(Zeroizing::new(frame.to_vec())).is_ok());
    }
    let (ending, tail) = oneshot::channel();
    assert!(ending.send(Zeroizing::new(Vec::new())).is_ok());
    LiveAudio {
        rate: 16_000,
        frames: received,
        tail,
    }
}

#[tokio::test]
async fn partials_never_replace_finals_and_multiple_utterances_join_after_commit()
-> anyhow::Result<()> {
    let (mut client, mut server) = sockets().await?;
    let exchange = async {
        assert!(matches!(server.next().await.unwrap()?, Message::Binary(_)));
        for event in [
            r#"{"type":"input_audio_buffer.speech_started"}"#,
            r#"{"type":"conversation.item.input_audio_transcription.delta","delta":"wrong partial"}"#,
            r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"First."}"#,
            r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":" Second."}"#,
        ] {
            send(&mut server, Message::Text(event.into())).await?;
        }
        let Some(Ok(Message::Text(commit))) = server.next().await else {
            anyhow::bail!("Missing commit");
        };
        assert!(commit.contains("input_audio_buffer.commit"));
        send(
            &mut server,
            Message::Text(r#"{"type":"input_audio_buffer.committed"}"#.into()),
        )
        .await
    };
    let (text, ()) = timeout(PATIENCE, async {
        tokio::try_join!(recognize(&mut client, ended(&[&[0, 0]])), exchange)
    })
    .await??;
    assert_eq!(text.as_deref(), Some("First. Second."));
    Ok(())
}

#[tokio::test]
async fn closed_feed_and_premature_or_malformed_final_never_return_text() -> anyhow::Result<()> {
    for event in [
        None,
        Some(r#"{"type":"input_audio_buffer.committed"}"#),
        Some(
            r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":123}"#,
        ),
        Some(r#"{"type":"error","message":"private content"}"#),
    ] {
        let (mut client, mut server) = sockets().await?;
        let (frames, received) = mpsc::channel(1);
        let (ending, tail) = oneshot::channel();
        drop(ending);
        if let Some(event) = event {
            send(&mut server, Message::Text(event.into())).await?;
        } else {
            drop(frames);
        }
        let result = timeout(
            PATIENCE,
            recognize(
                &mut client,
                LiveAudio {
                    rate: 16_000,
                    frames: received,
                    tail,
                },
            ),
        )
        .await?;
        if event.is_none() {
            assert!(result?.is_none());
        } else {
            assert_eq!(result.unwrap_err().to_string(), FAILED);
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_transcript_that_is_not_text_fails_even_on_events_recognition_ignores()
-> anyhow::Result<()> {
    const FINAL: &str =
        r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"First."}"#;
    for (events, acknowledgement) in [
        (
            &[
                r#"{"type":"conversation.item.input_audio_transcription.delta","transcript":123}"#,
                FINAL,
            ][..],
            r#"{"type":"input_audio_buffer.committed"}"#,
        ),
        (
            &[FINAL][..],
            r#"{"type":"input_audio_buffer.committed","transcript":123}"#,
        ),
    ] {
        let (mut client, mut server) = sockets().await?;
        let exchange = async {
            for &event in events {
                send(&mut server, Message::Text(event.into())).await?;
            }
            let Some(Ok(Message::Text(_))) = server.next().await else {
                anyhow::bail!("Missing commit");
            };
            send(&mut server, Message::Text(acknowledgement.into())).await
        };
        let result = timeout(PATIENCE, async {
            tokio::try_join!(recognize(&mut client, ended(&[])), exchange)
        })
        .await?;
        assert_eq!(result.unwrap_err().to_string(), FAILED);
    }
    Ok(())
}

#[tokio::test]
async fn a_committed_stream_cannot_wait_forever_for_its_final() -> anyhow::Result<()> {
    let (mut client, mut server) = sockets().await?;
    let recognizer = recognize(&mut client, ended(&[]));
    tokio::pin!(recognizer);
    tokio::select! {
        result = &mut recognizer => panic!("Returned before committing: {}", result.is_ok()),
        message = server.next() => assert!(matches!(message, Some(Ok(Message::Text(_))))),
    }
    tokio::time::pause();
    tokio::time::advance(FINAL_TIMEOUT.saturating_add(Duration::from_secs(1))).await;
    let error = timeout(Duration::from_secs(1), recognizer)
        .await?
        .unwrap_err();
    assert_eq!(error.to_string(), FAILED);
    Ok(())
}
