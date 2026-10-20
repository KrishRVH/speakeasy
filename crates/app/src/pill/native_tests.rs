//! Opt-in native checks on a private Xvfb display: the idle pill's window hints, input shape, and
//! native visibility, and GPUI's queued-event patch presenting a window opened after launch while
//! the pill is open.

use std::collections::HashSet;

use anyhow::{anyhow, bail, ensure};
use gpui::{AsyncApp, div, rgb};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use x11rb::{
    protocol::{
        shape::{self, ConnectionExt as _},
        xproto::{AtomEnum, ConnectionExt as _, ImageFormat, MapState},
    },
    xcb_ffi::XCBConnection,
};

use super::*;
use crate::gpui_ext::AppUpdate;

#[test]
#[ignore = "Opt-in native windows on a private Xvfb display, never microphone, input, or clipboard"]
fn idle_native_pill_starts_unmapped_and_can_show_and_hide() -> anyhow::Result<()> {
    run_native(async |cx: &AsyncApp| cx.update(check_idle_pill).flatten())
}

#[test]
#[ignore = "Opt-in native windows on a private Xvfb display, never microphone, input, or clipboard"]
fn idle_native_pill_can_render_a_window_opened_after_launch() -> anyhow::Result<()> {
    run_native(check_late_window)
}

fn run_native(
    check: impl AsyncFnOnce(&AsyncApp) -> anyhow::Result<()> + 'static,
) -> anyhow::Result<()> {
    let (completed, outcome) = std::sync::mpsc::channel();
    gpui::Application::new().run(move |cx| {
        cx.spawn(async move |cx| {
            completed.send(check(cx).await).unwrap();
            cx.update_if_running(|cx| cx.quit());
        })
        .detach();
    });
    outcome
        .try_recv()
        .context("Native check did not complete")?
}

fn check_idle_pill(cx: &mut App) -> anyhow::Result<()> {
    let (_publisher, updates) = watch::channel(Snapshot::default());
    let (_meter, levels) = watch::channel(Meter::default());
    let pill = open(updates, levels, true, Theme::default(), cx)?;
    let (native, raw, display) = pill.update(cx, |_, window, _| {
        Ok::<_, anyhow::Error>((
            native_window(window, "pill")?,
            HasWindowHandle::window_handle(window)
                .map_err(|error| anyhow!("Window handle: {error}"))?
                .as_raw(),
            HasDisplayHandle::display_handle(window)
                .map_err(|error| anyhow!("Display handle: {error}"))?
                .as_raw(),
        ))
    })??;
    let id = match (raw, display) {
        (RawWindowHandle::Xcb(window), RawDisplayHandle::Xcb(display)) => {
            ensure!(
                display.connection.is_some(),
                "Missing live XCB display handle"
            );
            window.window.get()
        },
        (RawWindowHandle::Xlib(window), RawDisplayHandle::Xlib(display)) => {
            ensure!(
                display.display.is_some(),
                "Missing live Xlib display handle"
            );
            u32::try_from(window.window)?
        },
        _ => bail!("Expected matching X11 window and display handles"),
    };
    let (connection, _) = XCBConnection::connect(None)?;
    let attributes = connection.get_window_attributes(id)?.reply()?;
    ensure!(
        attributes.map_state == MapState::UNMAPPED,
        "Idle pill is natively mapped after open and its initial draw"
    );
    ensure!(
        attributes.override_redirect,
        "Pill is managed as an ordinary window"
    );
    let atom = |name: &[u8]| -> anyhow::Result<u32> {
        Ok(connection.intern_atom(false, name)?.reply()?.atom)
    };
    let window_types: Vec<_> = connection
        .get_property(
            false,
            id,
            atom(b"_NET_WM_WINDOW_TYPE")?,
            AtomEnum::ATOM,
            0,
            8,
        )?
        .reply()?
        .value32()
        .context("Invalid _NET_WM_WINDOW_TYPE format")?
        .collect();
    ensure!(
        window_types == [atom(b"_NET_WM_WINDOW_TYPE_NOTIFICATION")?],
        "Pill is not a notification window"
    );
    let hints = connection
        .get_property(false, id, AtomEnum::WM_HINTS, AtomEnum::WM_HINTS, 0, 9)?
        .reply()?;
    let hints: Vec<_> = hints
        .value32()
        .context("Invalid WM_HINTS format")?
        .collect();
    ensure!(
        hints.len() == 9 && hints[0] & 1 != 0 && hints[1] == 0,
        "Pill accepts input focus"
    );
    ensure!(
        connection
            .shape_get_rectangles(id, shape::SK::INPUT)?
            .reply()?
            .rectangles
            .is_empty(),
        "Pill input region intercepts the pointer"
    );

    native.set_pill_visible(true);
    ensure!(
        connection.get_window_attributes(id)?.reply()?.map_state == MapState::VIEWABLE,
        "Pill did not become viewable"
    );
    native.set_pill_visible(false);
    ensure!(
        connection.get_window_attributes(id)?.reply()?.map_state == MapState::UNMAPPED,
        "Pill did not become unmapped"
    );
    Ok(())
}

struct LateColors;

impl Render for LateColors {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x11_33_55))
            .child(div().w(px(32.0)).h_full().bg(rgb(0x55_77_99)))
    }
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance creates and observes GPUI windows on their owning UI thread"
)]
async fn check_late_window(cx: &AsyncApp) -> anyhow::Result<()> {
    // Opening during launch would let startup event draining present the window.
    Timer::after(Duration::from_millis(100)).await;
    let id = cx.update(|cx| {
        let (_publisher, updates) = watch::channel(Snapshot::default());
        let (_meter, levels) = watch::channel(Meter::default());
        let _pill = open(updates, levels, true, Theme::default(), cx)?;
        let colors = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.0), px(0.0)),
                    size(px(64.0), px(64.0)),
                ))),
                titlebar: None,
                ..Default::default()
            },
            |_, cx| cx.new(|_| LateColors),
        )?;
        colors.update(cx, |_, window, _| {
            match HasWindowHandle::window_handle(window)
                .map_err(|error| anyhow!("Window handle: {error}"))?
                .as_raw()
            {
                RawWindowHandle::Xcb(handle) => Ok(handle.window.get()),
                _ => bail!("Expected XCB window"),
            }
        })?
    })??;
    let (connection, _) = XCBConnection::connect(None)?;
    let geometry = connection.get_geometry(id)?.reply()?;
    let started = Instant::now();
    let deadline = started
        .checked_add(Duration::from_secs(3))
        .unwrap_or(started);
    loop {
        Timer::after(Duration::from_millis(50)).await;
        let image = connection
            .get_image(
                ImageFormat::Z_PIXMAP,
                id,
                0,
                0,
                geometry.width,
                geometry.height,
                u32::MAX,
            )?
            .reply()?;
        let colors: HashSet<_> = image
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| u32::from_ne_bytes(*pixel) & 0x00FF_FFFF)
            .collect();
        if colors.contains(&0x11_33_55) && colors.contains(&0x55_77_99) {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Window opened after launch never presented its two colors ({}×{} pixels)",
            geometry.width,
            geometry.height
        );
    }
}
