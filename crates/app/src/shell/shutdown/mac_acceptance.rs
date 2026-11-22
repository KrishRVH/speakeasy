use std::{fs, io::Write as _, path::PathBuf, time::Duration};

use anyhow::{Context as _, ensure};
use async_channel::{Receiver, RecvError};
use gpui::{App, AsyncApp, Global, Window, WindowHandle};
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSApplicationTerminateReply, NSRunningApplication,
};
use tempfile::TempDir;
use tokio::sync::watch;

use super::request_quit;
use crate::{
    config::Config,
    instance::Instance,
    pill::Pill,
    runtime::Snapshot,
    shell::{LaunchMode, Services},
};

const SENTINEL: &[u8] = b"owned configuration sentinel";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    External,
    Borrowed,
    Foreground,
    Repeated,
    Approved,
}

#[derive(Default)]
struct Progress {
    requested: bool,
    rendered: bool,
    driven: Option<anyhow::Result<()>>,
}

impl Global for Progress {}

pub(crate) fn native_quit_check() -> anyhow::Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let mut next = move || arguments.next()?.into_string().ok();
    match next().as_deref() {
        Some("--send-owned-quit") => send_quit(next().context("Missing owned process")?.parse()?),
        Some("--check-owned-quit") => exercise(match next().as_deref() {
            Some("external") => Mode::External,
            Some("borrowed") => Mode::Borrowed,
            Some("foreground") => Mode::Foreground,
            Some("repeated") => Mode::Repeated,
            Some("approved") => Mode::Approved,
            _ => anyhow::bail!("Choose an owned quit case"),
        }),
        _ => anyhow::bail!("Owned native acceptance requires an explicit case"),
    }
}

fn send_quit(pid: i32) -> anyhow::Result<()> {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .context("Owned application is unavailable")?;
    ensure!(
        app.terminate(),
        "Owned application refused the quit request"
    );
    Ok(())
}

fn exercise(mode: Mode) -> anyhow::Result<()> {
    let main = MainThreadMarker::new().context("Native quit requires the main thread")?;
    let (application, presentation) = crate::application::new()?;
    let application = application.with_assets(crate::icons::Icons);
    application.on_quit_requested(|cx| {
        cx.global_mut::<Progress>().requested = true;
        request_quit(cx);
    });
    application.run(move |cx| {
        cx.set_global(presentation);
        if let Err(error) = start(mode, main, cx) {
            report(Err(error));
            cx.quit();
        }
    });
    anyhow::bail!("AppKit returned without terminating the owned app")
}

fn start(mode: Mode, main: MainThreadMarker, cx: &mut App) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let settings = root.path().join("settings.json");
    fs::write(&settings, SENTINEL)?;
    let (instance, _requests) =
        Instance::acquire(&settings)?.context("Owned configuration is busy")?;
    crate::load_brand_font(cx);
    let config = Config::default();
    let (output, snapshots) = watch::channel(Snapshot::default());
    let (meter, levels) = watch::channel(0.0_f32);
    let (canvas, _ink) = watch::channel(crate::show::Canvas::default());
    let pill = crate::pill::open(
        snapshots,
        levels,
        || false,
        true,
        config.theme,
        crate::pill::Opening::Startup,
        cx,
    )?;
    pill.update(cx, |view, _, cx| {
        view.show_tray_hint(cx);
    })?;
    cx.set_global(Services::new(
        settings.clone(),
        false,
        config,
        output,
        meter,
        canvas,
        pill,
        Some(instance),
        LaunchMode::DemoTray,
    ));
    cx.set_global(Progress::default());
    let (release, held) = async_channel::bounded(1);
    let accepted = cx.global_mut::<Services>().show_edits.held_until(held)?;
    let rescue = release.clone();
    let mut directory = Some(root);
    cx.on_app_quit(move |cx| {
        let checked = verify(cx.remove_global::<Progress>(), mode);
        if checked.is_err() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A failed native check releases its synthetic held save before synchronous emergency teardown"
            )]
            let _ = rescue.try_send(());
        }
        let released = crate::release_services(cx);
        let cleaned = confirm_cleanup(directory.take(), released, checked, settings.clone());
        async move { report(cleaned.await) }
    })
    .detach();
    cx.spawn(async move |cx| {
        let driven = drive(mode, main, pill, &accepted, cx).await;
        cx.update(|cx| {
            cx.global_mut::<Progress>().driven = Some(driven);
            request_quit(cx);
        });
        if let Err(error) = release.try_send(()) {
            report(Err(error.into()));
        }
    })
    .detach();
    if mode == Mode::Borrowed {
        NSApplication::sharedApplication(main).terminate(None);
    }
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "The quit script drives GPUI and AppKit through their main-thread async context"
)]
async fn drive(
    mode: Mode,
    main: MainThreadMarker,
    pill: WindowHandle<Pill>,
    accepted: &Receiver<Result<(), RecvError>>,
    cx: &mut AsyncApp,
) -> anyhow::Result<()> {
    cx.background_executor()
        .timer(Duration::from_millis(100))
        .await;
    match mode {
        Mode::External | Mode::Repeated => {
            writeln!(std::io::stdout().lock(), "Owned Mac quit ready")?;
        },
        Mode::Borrowed => {},
        Mode::Foreground => NSApplication::sharedApplication(main).terminate(None),
        Mode::Approved => cx.update(request_quit),
    }
    while !cx.update(|cx| cx.global::<Services>().is_quitting()) {
        cx.background_executor()
            .timer(Duration::from_millis(10))
            .await;
    }
    check_reply(mode, main)?;
    if mode == Mode::Repeated {
        cx.update(|_| NSApplication::sharedApplication(main).terminate(None));
        check_reply(mode, main)?;
    }
    for _ in 0..3 {
        pill.update(cx, |_, window, _| {
            window.on_next_frame(frame);
            window.refresh();
        })?;
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
        ensure!(
            cx.update(|cx| cx.global::<Services>().is_quitting()),
            "Native quit finished before the held save was released"
        );
        ensure!(accepted.is_empty(), "Held save acknowledged prematurely");
    }
    Ok(())
}

async fn confirm_cleanup(
    directory: Option<TempDir>,
    released: impl Future<Output = ()>,
    checked: anyhow::Result<()>,
    settings: PathBuf,
) -> anyhow::Result<()> {
    let _directory = directory.context("Native termination repeated")?;
    released.await;
    checked?;
    let _instance =
        Instance::acquire(&settings)?.context("The instance remained held after cleanup")?;
    ensure!(
        fs::read(settings)? == SENTINEL,
        "Native quit changed the configuration"
    );
    Ok(())
}

fn frame(_: &mut Window, cx: &mut App) {
    cx.global_mut::<Progress>().rendered = true;
}

fn check_reply(mode: Mode, main: MainThreadMarker) -> anyhow::Result<()> {
    let expected = match mode {
        Mode::External | Mode::Repeated => NSApplicationTerminateReply::TerminateLater,
        Mode::Borrowed | Mode::Foreground => NSApplicationTerminateReply::TerminateCancel,
        Mode::Approved => return Ok(()),
    };
    let application = NSApplication::sharedApplication(main);
    let delegate = application.delegate().context("Native delegate missing")?;
    ensure!(
        delegate.applicationShouldTerminate(&application) == expected,
        "Native quit changed its pending reply"
    );
    Ok(())
}

fn verify(progress: Progress, mode: Mode) -> anyhow::Result<()> {
    progress
        .driven
        .context("Native quit bypassed the accepted save")??;
    ensure!(progress.rendered, "Render callbacks stopped during drain");
    ensure!(
        progress.requested == (mode != Mode::Approved),
        "Native quit requests did not match this case"
    );
    Ok(())
}

fn report(result: anyhow::Result<()>) {
    let output = match result {
        Ok(()) => writeln!(std::io::stdout().lock(), "Owned Mac quit passed"),
        Err(error) => writeln!(std::io::stderr().lock(), "Owned Mac quit failed: {error:#}"),
    };
    #[expect(
        clippy::let_underscore_must_use,
        reason = "The external acceptance runner requires a success marker; a closed output cannot pass"
    )]
    let _ = output;
}
