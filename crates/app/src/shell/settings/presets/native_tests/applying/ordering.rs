use super::*;

fn saved(cx: &AsyncApp) -> Config {
    cx.update(|cx| cx.global::<Services>().config.clone())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and a fake owner on GPUI's UI thread"
)]
pub(super) async fn resumed_after_use(cx: &mut AsyncApp) -> anyhow::Result<()> {
    let saved = saved(cx);
    let french = Config {
        language: "fr".into(),
        ..saved.clone()
    };
    let used = cx
        .update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                services.launch = Launch::Fake;
                services.use_preset(french, |_| true, cx)
            })
        })
        .map_err(anyhow::Error::msg)?;
    used.recv()
        .await?
        .map(Applied::load)
        .map_err(anyhow::Error::msg)?;
    cx.update(|cx| cx.update_global::<Services, _>(|services, cx| services.pause(cx)));
    until(cx, "The fake owner did not retire", |cx| {
        Ok(cx.update(|cx| {
            let services = cx.global::<Services>();
            services.runtime().is_none() && services.service_state() != ServiceState::Pausing
        }))
    })
    .await?;
    cx.update(|cx| cx.update_global::<Services, _>(|services, cx| services.resume(cx)));
    let started = until(cx, "Resume did not start the fake owner", |cx| {
        Ok(cx.update(|cx| cx.global::<Services>().is_running())
            && snapshot(cx).model == ModelState::Ready)
    })
    .await;
    let launched = cx.update(|cx| {
        cx.global::<Services>()
            .launched()
            .map(|config| config.language.clone())
    });
    relaunch(&saved, cx).await?;
    started?;
    ensure!(
        launched.as_deref() == Some("fr"),
        "Pause and Resume after a Use the owner accepted launched the older saved settings"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and a fake owner on GPUI's UI thread"
)]
pub(super) async fn tray_after_use(cx: &mut AsyncApp) -> anyhow::Result<()> {
    let saved = saved(cx);
    let german = Config {
        language: "de".into(),
        ..saved.clone()
    };
    let (call, chosen) = Call::new(Command::Preset {
        name: "French".into(),
    });
    let used = cx
        .update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                let used = services.use_preset(german, |_| true, cx);
                services.control(call, cx);
                used
            })
        })
        .map_err(anyhow::Error::msg)?;
    let used = used.recv().await?;
    let chosen = chosen.recv().await?;
    settle(cx).await;
    let named = preset_name(cx);
    relaunch(&saved, cx).await?;
    ensure!(
        used.is_err() && chosen.result == Disposition::Ok && named.as_deref() == Some("French"),
        "A Use clicked before a tray choice overtook it"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and a fake owner on GPUI's UI thread"
)]
pub(super) async fn cancelled_choices(cx: &mut AsyncApp) -> anyhow::Result<()> {
    let saved = saved(cx);
    let (first, replaced) = Call::new(Command::Preset {
        name: "French".into(),
    });
    let (second, chosen) = Call::new(Command::Preset {
        name: "Saved".into(),
    });
    cx.update(|cx| {
        cx.update_global::<Services, _>(|services, cx| {
            services.control(first, cx);
            services.control(second, cx);
        });
    });
    let replaced = replaced.recv().await;
    let chosen = chosen.recv().await;
    let french = Config {
        language: "fr".into(),
        ..saved.clone()
    };
    let (paused, stopped) = Call::new(Command::Preset {
        name: "French".into(),
    });
    let used = cx
        .update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                services.launch = Launch::Fake;
                services.control(paused, cx);
                let used = services.use_preset(french, |_| true, cx);
                services.pause(cx);
                used
            })
        })
        .map_err(anyhow::Error::msg)?;
    let stopped = stopped.recv().await;
    let used = used.recv().await;
    relaunch(&saved, cx).await?;
    ensure!(
        matches!(&replaced, Ok(reply) if reply.result == Disposition::Refused)
            && matches!(&chosen, Ok(reply) if reply.result == Disposition::Ok),
        "A tray choice replaced while it validated closed its caller without a refusal"
    );
    ensure!(
        matches!(&stopped, Ok(reply) if reply.result == Disposition::Refused),
        "A tray choice that Pause stopped while it validated closed its caller without a refusal"
    );
    ensure!(
        used.is_ok(),
        "A Use that Pause stopped while it validated closed its receipt without an answer"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and a fake owner on GPUI's UI thread"
)]
pub(super) async fn dropped_use(cx: &mut AsyncApp) -> anyhow::Result<()> {
    let saved = saved(cx);
    let french = Config {
        language: "fr".into(),
        ..saved.clone()
    };
    let used = cx
        .update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                services.use_preset(french, |_| true, cx)
            })
        })
        .map_err(anyhow::Error::msg)?;
    until(cx, "The owner did not answer the Use", |_| {
        Ok(!used.is_empty())
    })
    .await?;
    drop(used);
    settle(cx).await;
    settle(cx).await;
    let restored = cx.update(|cx| {
        let services = cx.global::<Services>();
        services.admitted.is_none()
            && services
                .runtime()
                .is_some_and(|runtime| runtime.baseline().language == saved.language)
    });
    relaunch(&saved, cx).await?;
    ensure!(
        restored,
        "A Use dropped after the owner accepted it left dictation on settings nobody saved"
    );
    Ok(())
}

#[expect(
    clippy::future_not_send,
    reason = "Native acceptance drives Services and a fake owner on GPUI's UI thread"
)]
pub(super) async fn refused_after_leaving(cx: &mut AsyncApp) -> anyhow::Result<()> {
    let saved = saved(cx);
    let french = Config {
        language: "fr".into(),
        ..saved.clone()
    };
    let used = cx
        .update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                services.use_preset(french, |_| true, cx)
            })
        })
        .map_err(anyhow::Error::msg)?;
    used.recv()
        .await?
        .map(Applied::load)
        .map_err(anyhow::Error::msg)?;
    let german = Config {
        language: "de".into(),
        ..saved.clone()
    };
    let left = cx
        .update(|cx| {
            cx.update_global::<Services, _>(|services, cx| {
                services.use_preset(german, |_| false, cx)
            })
        })
        .map_err(anyhow::Error::msg)?;
    drop(left);
    until(cx, "The refused Use did not finish", |cx| {
        Ok(cx.update(|cx| cx.global::<Services>().choices.is_idle()))
    })
    .await?;
    settle(cx).await;
    let kept = cx.update(|cx| {
        let services = cx.global::<Services>();
        services
            .admitted
            .as_ref()
            .is_some_and(|admitted| admitted.language == "fr")
            && services
                .runtime()
                .is_some_and(|runtime| runtime.baseline().language == "fr")
    });
    relaunch(&saved, cx).await?;
    ensure!(
        kept,
        "A refused Use whose caller left undid the Use accepted before it"
    );
    Ok(())
}
