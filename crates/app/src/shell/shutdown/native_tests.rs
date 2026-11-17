use std::{fs, sync::mpsc};

use anyhow::Context as _;
use tokio::sync::watch;

use super::*;
use crate::{
    config::Config,
    instance::Instance,
    runtime::{Meter, Snapshot},
    shell::LaunchMode,
    updates::Installer,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quit {
    Graceful,
    Forced,
}

fn handoff(quit: Quit) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let settings = root.path().join("config/settings.json");
    let acquired = Instance::acquire(&settings)?.context("The isolated configuration is busy")?;
    let (candidate, path) = Installer::fixture(root.path(), settings.clone())?;
    let (release, held) = async_channel::bounded(1);
    let configuration = settings.clone();
    let (handoff, ready) = mpsc::channel();
    let (application, presentation) = crate::application::new()?;
    application.with_assets(crate::icons::Icons).run(move |cx| {
        cx.set_global(presentation);
        crate::load_brand_font(cx);
        let config = Config::default();
        let (output, snapshots) = watch::channel(Snapshot::default());
        let (meter, levels) = watch::channel(Meter::default());
        let (canvas, _ink) = watch::channel(crate::show::Canvas::default());
        let pill = crate::pill::open(
            snapshots,
            levels,
            || false,
            true,
            config.theme,
            crate::pill::Opening::Startup,
            cx,
        )
        .expect("owned pill");
        let (instance, _requests) = acquired;
        cx.set_global(Services::new(
            configuration,
            false,
            config,
            output,
            meter,
            canvas,
            pill,
            Some(instance),
            LaunchMode::DemoTray,
        ));
        let _accepted = cx
            .global_mut::<Services>()
            .show_edits
            .held_until(held)
            .expect("held Show write");
        let on_quit = release.clone();
        cx.on_app_quit(move |cx| {
            if quit == Quit::Forced {
                on_quit.try_send(()).expect("release forced teardown");
            }
            crate::release_services(cx, handoff.clone())
        })
        .detach();
        request_install(candidate, cx);
        assert!(
            matches!(
                cx.global::<Services>().installation,
                Some(Installation::Waiting(_))
            ),
            "installation cannot arm synchronously before cleanup"
        );
        match quit {
            Quit::Forced => cx.quit(),
            Quit::Graceful => {
                request_quit(cx);
                cx.spawn(async move |cx| {
                    cx.update(|cx| {
                        assert!(
                            matches!(
                                cx.global::<Services>().installation,
                                Some(Installation::Waiting(_))
                            ),
                            "accepted writes still block installation"
                        );
                    });
                    release.try_send(()).expect("finish accepted write");
                })
                .detach();
            },
        }
    });
    let replacement =
        Instance::acquire(&settings)?.context("The instance stayed held after the handoff")?;
    let received = ready.try_recv().ok();
    match quit {
        Quit::Forced => {
            assert!(
                received.is_none(),
                "forced Quit cannot install before graceful drain"
            );
            assert!(!path.exists(), "unarmed staging is reclaimed");
        },
        Quit::Graceful => {
            assert!(
                received.is_some(),
                "explicit acceptance survives a subsequent ordinary Quit"
            );
            assert!(path.is_file());
            assert!(
                fs::remove_file(&path).is_err(),
                "handoff retains the verified file lease"
            );
            drop(received);
            assert!(!path.exists());
        },
    }
    drop(replacement);
    Ok(())
}

#[test]
#[ignore = "Opt-in owned window; no microphone, screen capture, input hook, clipboard, or installer execution"]
fn native_update_handoff_follows_graceful_quit_and_instance_release() -> anyhow::Result<()> {
    handoff(Quit::Graceful)
}

#[test]
#[ignore = "Opt-in owned window; no microphone, screen capture, input hook, clipboard, or installer execution"]
fn native_forced_quit_drops_an_unarmed_update() -> anyhow::Result<()> {
    handoff(Quit::Forced)
}
