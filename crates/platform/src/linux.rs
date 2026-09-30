//! Linux desktop resources live on an owned thread. Rendering may use Xwayland;
//! input selection follows the host session rather than the presence of DISPLAY.
mod ei;
mod portal;
mod token;
mod tray;
mod x11;
use super::*;
use anyhow::Context;
use std::{thread, time::Duration};
pub use tray::{NativeTray, TrayAction, TrayPresentation};
pub const APPLICATION_ID: &str = "io.github.krvh.speakeasy";

pub struct InputMonitor {
    stop: async_channel::Sender<()>,
    finished: async_channel::Receiver<()>,
    thread: Option<thread::JoinHandle<()>>,
}
#[derive(Clone)]
pub struct Inserter {
    requests: async_channel::Sender<Insertion>,
}
struct Insertion {
    text: String,
    permit: InsertPermit,
    preserve: bool,
    reply: async_channel::Sender<anyhow::Result<Inserted>>,
}
impl Inserter {
    pub async fn insert(
        &self,
        text: String,
        permit: InsertPermit,
        preserve: bool,
    ) -> anyhow::Result<Inserted> {
        if !permit.active() {
            return Ok(Inserted::Cancelled);
        }
        let (reply, result) = async_channel::bounded(1);
        self.requests
            .send(Insertion {
                text,
                permit,
                preserve,
                reply,
            })
            .await
            .context("Desktop input is unavailable")?;
        result.recv().await.context("Desktop insertion stopped")?
    }
}
pub fn prepare(
    input: InputSender,
    options: DesktopOptions,
) -> anyhow::Result<(InputMonitor, Inserter)> {
    let (requests, incoming) = async_channel::bounded(1);
    let (stop, stopping) = async_channel::bounded(1);
    let (complete, finished) = async_channel::bounded(1);
    let thread = thread::Builder::new()
        .name("linux-desktop".into())
        .spawn(move || {
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                let wayland = wayland_session(
                    std::env::var("XDG_SESSION_TYPE").ok().as_deref(),
                    std::env::var_os("WAYLAND_DISPLAY").is_some(),
                );
                tokio::task::LocalSet::new().block_on(&runtime, async {
                    if wayland {
                        portal::run(&input, &options, incoming, &stopping).await
                    } else {
                        x11::run(&input, &options, incoming, &stopping).await
                    }
                })
            })();
            if let Err(error) = result
                && !input.is_closed()
            {
                deliver(
                    &input,
                    Input::Unavailable(format!(
                        "Linux desktop access stopped: {error}. Check desktop permissions or choose Desktop bindings/manual paste in Settings, then resume."
                    )),
                );
            }
            input.close();
            let _ = complete.try_send(());
        })?;
    Ok((
        InputMonitor {
            stop,
            finished,
            thread: Some(thread),
        },
        Inserter { requests },
    ))
}
impl InputMonitor {
    pub fn request_stop(&self) {
        let _ = self.stop.try_send(());
    }
    pub fn stopped(&self) -> impl std::future::Future<Output = ()> + use<> {
        let finished = self.finished.clone();
        async move {
            let _ = finished.recv().await;
        }
    }
}
impl Drop for InputMonitor {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn wayland_session(kind: Option<&str>, wayland_display: bool) -> bool {
    match kind {
        Some("wayland") => true,
        Some("x11") => false,
        _ => wayland_display,
    }
}
pub fn configure_pill(handle: RawWindowHandle) -> anyhow::Result<()> {
    x11::configure_pill(handle)
}
pub fn set_pill_visible(handle: RawWindowHandle, visible: bool) {
    let _ = x11::visible(handle, visible);
}
pub fn set_settings_visible(handle: RawWindowHandle, visible: bool) {
    let _ = x11::visible(handle, visible);
}
pub fn reduced_motion() -> bool {
    false
}
pub fn show_error(message: &str) {
    eprintln!("Speakeasy: {message}");
}

async fn respond(request: Insertion, result: anyhow::Result<Inserted>) {
    let _ = request.reply.send(result).await;
}
const MODIFIER_WAIT: Duration = Duration::from_millis(800);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_session_is_independent_of_xwayland_rendering() {
        assert!(wayland_session(Some("wayland"), false));
        assert!(wayland_session(None, true));
        assert!(!wayland_session(Some("x11"), true));
        assert!(!wayland_session(None, false));
    }
}
