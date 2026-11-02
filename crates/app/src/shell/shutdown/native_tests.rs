use std::{fs, sync::mpsc};

use tokio::sync::watch;

use super::*;
use crate::{
    config::Config,
    instance::Instance,
    runtime::{Meter, Snapshot},
    shell::LaunchMode,
    updates::Installer,
};

fn handoff(force_native_quit: bool) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let settings = root.path().join("config/settings.json");
    let acquired = Instance::acquire(&settings)?.expect("isolated configuration");
    let (candidate, path) = Installer::fixture(root.path(), settings.clone())?;
    let (release, held) = async_channel::bounded(1);
    let configuration = settings.clone();
    let (handoff, ready) = mpsc::channel();
    let (application, presentation) = crate::application::new().unwrap();
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
            if force_native_quit {
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
        if force_native_quit {
            cx.quit();
        } else {
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
        }
    });
    let replacement = Instance::acquire(&settings)?.expect("instance released before handoff");
    let received = ready.try_recv().ok();
    if force_native_quit {
        assert!(
            received.is_none(),
            "forced Quit cannot install before graceful drain"
        );
        assert!(!path.exists(), "unarmed staging is reclaimed");
    } else {
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
    }
    drop(replacement);
    Ok(())
}

#[test]
#[ignore = "Opt-in owned window; no microphone, screen capture, input hook, clipboard, or installer execution"]
fn native_update_handoff_follows_graceful_quit_and_instance_release() -> anyhow::Result<()> {
    handoff(false)
}

#[test]
#[ignore = "Opt-in owned window; no microphone, screen capture, input hook, clipboard, or installer execution"]
fn native_forced_quit_drops_an_unarmed_update() -> anyhow::Result<()> {
    handoff(true)
}
