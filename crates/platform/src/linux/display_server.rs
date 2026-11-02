use std::{env, ffi::OsString, os::unix::net::UnixStream, path::PathBuf};

use anyhow::{Context as _, anyhow};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::wl_registry::{self, WlRegistry},
};

const LAYER_SHELL: &str = "zwlr_layer_shell_v1";

const NO_OVERLAY: &str = "This Wayland desktop cannot keep the pill above other windows, and it runs no Xwayland. Enable Xwayland, or use a compositor with the wlr-layer-shell protocol, then start Speakeasy again.";

const NO_SESSION: &str = "No desktop session found: WAYLAND_DISPLAY and DISPLAY are unset. Start Speakeasy from a Wayland or X11 desktop session.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayServer {
    Wayland,
    X11,
}

#[derive(Debug)]
enum Compositor {
    Unnamed,
    Unreachable(anyhow::Error),
    Reachable { layer_shell: bool },
}

pub fn display_server() -> anyhow::Result<DisplayServer> {
    let named = |variable| env::var_os(variable).filter(|value| !value.is_empty());
    let compositor = match named("WAYLAND_DISPLAY") {
        None => Compositor::Unnamed,
        Some(display) => match offers_layer_shell(display, named("XDG_RUNTIME_DIR")) {
            Ok(layer_shell) => Compositor::Reachable { layer_shell },
            Err(error) => Compositor::Unreachable(error),
        },
    };
    choose(compositor, named("DISPLAY").is_some())
}

fn choose(compositor: Compositor, x11: bool) -> anyhow::Result<DisplayServer> {
    match compositor {
        Compositor::Reachable { layer_shell: true } => Ok(DisplayServer::Wayland),
        _ if x11 => Ok(DisplayServer::X11),
        Compositor::Reachable { layer_shell: false } => Err(anyhow!(NO_OVERLAY)),
        Compositor::Unreachable(error) => Err(error),
        Compositor::Unnamed => Err(anyhow!(NO_SESSION)),
    }
}

fn offers_layer_shell(display: OsString, runtime: Option<OsString>) -> anyhow::Result<bool> {
    let display = PathBuf::from(display);
    let socket = if display.is_absolute() {
        display
    } else {
        PathBuf::from(runtime.context(
            "Cannot find the Wayland display because XDG_RUNTIME_DIR is unset. Start Speakeasy from your desktop session.",
        )?)
        .join(display)
    };
    let unreachable = || {
        format!(
            "Cannot connect to the Wayland display at {}. Start Speakeasy from your desktop session",
            socket.display()
        )
    };
    let connection =
        Connection::from_socket(UnixStream::connect(&socket).with_context(unreachable)?)
            .with_context(unreachable)?;
    let (globals, _queue) =
        registry_queue_init::<Registry>(&connection).with_context(unreachable)?;
    Ok(globals
        .contents()
        .with_list(|globals| globals.iter().any(|global| global.interface == LAYER_SHELL)))
}

struct Registry;

impl Dispatch<WlRegistry, GlobalListContents> for Registry {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chosen(compositor: Compositor, x11: bool) -> Result<DisplayServer, String> {
        choose(compositor, x11).map_err(|error| format!("{error:#}"))
    }

    #[test]
    fn only_a_layer_shell_compositor_presents_natively_on_wayland() {
        let layer_shell = || Compositor::Reachable { layer_shell: true };
        let without_layer_shell = || Compositor::Reachable { layer_shell: false };
        let unreachable = || Compositor::Unreachable(anyhow!("Refused"));
        for x11 in [false, true] {
            assert_eq!(chosen(layer_shell(), x11), Ok(DisplayServer::Wayland));
        }
        for compositor in [without_layer_shell(), unreachable(), Compositor::Unnamed] {
            assert_eq!(
                chosen(compositor, true),
                Ok(DisplayServer::X11),
                "Xwayland or X11 did not present for a compositor without layer shell"
            );
        }
        assert_eq!(chosen(without_layer_shell(), false), Err(NO_OVERLAY.into()));
        assert_eq!(chosen(unreachable(), false), Err("Refused".into()));
        assert_eq!(chosen(Compositor::Unnamed, false), Err(NO_SESSION.into()));
    }
}
