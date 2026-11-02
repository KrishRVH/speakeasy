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

#[test]
#[ignore = "Opt-in native windows on the X11 display, never microphone, input, or clipboard"]
fn idle_native_pill_starts_unmapped_and_can_show_and_hide() -> anyhow::Result<()> {
    run_native(async |cx: &AsyncApp| cx.update(check_idle_pill))
}

#[test]
#[ignore = "Opt-in native windows on the X11 display, never microphone, input, or clipboard"]
fn idle_native_pill_can_render_a_window_opened_after_launch() -> anyhow::Result<()> {
    run_native(check_late_window)
}

#[test]
#[ignore = "Opt-in native windows on the X11 display, never microphone, input, or clipboard"]
fn reopened_native_pill_shows_an_active_session_but_never_replays_feedback() -> anyhow::Result<()> {
    run_native(check_reopened_pill)
}

fn run_native(
    check: impl AsyncFnOnce(&AsyncApp) -> anyhow::Result<()> + 'static,
) -> anyhow::Result<()> {
    let (completed, outcome) = std::sync::mpsc::channel();
    let (application, presentation) = crate::application::new().unwrap();
    application.run(move |cx| {
        cx.set_global(presentation);
        cx.spawn(async move |cx| {
            completed.send(check(cx).await).unwrap();
            cx.update(|cx| cx.quit());
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
    let pill = open(
        updates,
        levels,
        true,
        Theme::default(),
        Opening::Startup,
        cx,
    )?;
    let id = pill.update(cx, |_, window, _| x11_window(window))??;
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
    ensure!(
        connection
            .shape_get_rectangles(id, shape::SK::INPUT)?
            .reply()?
            .rectangles
            .is_empty(),
        "Pill input region intercepts the pointer"
    );

    pill.update(cx, |_, window, _| window.set_window_visible(true))?;
    ensure!(
        connection.get_window_attributes(id)?.reply()?.map_state == MapState::VIEWABLE,
        "Pill did not become viewable"
    );
    pill.update(cx, |_, window, _| window.set_window_visible(false))?;
    ensure!(
        connection.get_window_attributes(id)?.reply()?.map_state == MapState::UNMAPPED,
        "Pill did not become unmapped"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance creates and observes GPUI windows on their owning UI thread"
)]
async fn check_reopened_pill(cx: &AsyncApp) -> anyhow::Result<()> {
    let (connection, _) = XCBConnection::connect(None)?;
    let failed = Snapshot {
        phase: Phase::Error,
        message: "Simulated failure".into(),
        ..Snapshot::default()
    };
    let recording = Snapshot {
        phase: Phase::Recording,
        ..Snapshot::default()
    };
    for (opening, snapshot, shown) in [
        (Opening::Startup, failed.clone(), true),
        (Opening::Reopened, failed, false),
        (Opening::Reopened, recording, true),
    ] {
        let phase = snapshot.phase;
        let (_publisher, updates) = watch::channel(snapshot);
        let (_meter, levels) = watch::channel(Meter::default());
        let id = cx.update(|cx| {
            let pill = open(updates, levels, true, Theme::default(), opening, cx)?;
            pill.update(cx, |_, window, _| x11_window(window))?
        })?;
        cx.background_executor()
            .timer(Duration::from_millis(300))
            .await;
        let viewable =
            connection.get_window_attributes(id)?.reply()?.map_state == MapState::VIEWABLE;
        ensure!(
            viewable == shown,
            "A pill opened at {opening:?} during {phase:?} is viewable: {viewable}"
        );
    }
    Ok(())
}

fn x11_window(window: &Window) -> anyhow::Result<u32> {
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow!("Window handle: {error}"))?
        .as_raw();
    let display = HasDisplayHandle::display_handle(window)
        .map_err(|error| anyhow!("Display handle: {error}"))?
        .as_raw();
    match (handle, display) {
        (RawWindowHandle::Xcb(handle), RawDisplayHandle::Xcb(display)) => {
            ensure!(
                display.connection.is_some(),
                "Missing live XCB display handle"
            );
            Ok(handle.window.get())
        },
        (RawWindowHandle::Xlib(handle), RawDisplayHandle::Xlib(display)) => {
            ensure!(
                display.display.is_some(),
                "Missing live Xlib display handle"
            );
            Ok(u32::try_from(handle.window)?)
        },
        _ => bail!("Expected matching X11 window and display handles"),
    }
}

const LATE_BACKGROUND: u32 = 0x11_33_55;
const LATE_STRIPE: u32 = 0x55_77_99;

struct LateColors;

impl Render for LateColors {
    fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(LATE_BACKGROUND))
            .child(div().w(px(32.0)).h_full().bg(rgb(LATE_STRIPE)))
    }
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance creates and observes GPUI windows on their owning UI thread"
)]
async fn check_late_window(cx: &AsyncApp) -> anyhow::Result<()> {
    cx.background_executor()
        .timer(Duration::from_millis(100))
        .await;
    let id = cx.update(|cx| {
        let (_publisher, updates) = watch::channel(Snapshot::default());
        let (_meter, levels) = watch::channel(Meter::default());
        let _pill = open(
            updates,
            levels,
            true,
            Theme::default(),
            Opening::Startup,
            cx,
        )?;
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
        colors.update(cx, |_, window, _| x11_window(window))?
    })?;
    let (connection, _) = XCBConnection::connect(None)?;
    let geometry = connection.get_geometry(id)?.reply()?;
    let deadline = Instant::now().checked_add(Duration::from_secs(3)).unwrap();
    loop {
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
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
        if colors.contains(&LATE_BACKGROUND) && colors.contains(&LATE_STRIPE) {
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
