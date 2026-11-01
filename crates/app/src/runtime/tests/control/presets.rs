use super::*;

#[tokio::test(start_paused = true)]
async fn preset_adoption_is_idle_and_does_not_reload_for_text_choices() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    ready(&mut h).await?;
    let baseline = h.runtime.configuration.borrow().clone();
    let mut config = baseline.clone();
    config.language = "fr".into();
    let (call, receipt) = Call::new(Command::Preset {
        name: "French".into(),
    });
    h.runtime.preset(
        "French".into(),
        ValidConfig::assume(config.clone()),
        baseline.clone(),
        0,
        call,
    );
    assert_eq!(receive(&receipt).await?.result, Disposition::Ok);
    assert!(h.loads.try_recv().is_err());
    h.start().await?;
    let (call, receipt) = Call::new(Command::Preset {
        name: "Busy".into(),
    });
    h.runtime.preset(
        "Busy".into(),
        ValidConfig::assume(baseline.clone()),
        baseline.clone(),
        1,
        call,
    );
    assert_eq!(receive(&receipt).await?.result, Disposition::Refused);
    h.input(Input::Toggle);
    let (language, reply) = receive(&h.jobs).await?;
    assert_eq!(language, "fr");
    answer(reply, "Fixture.")?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    config.threads = config.threads.saturating_add(1);
    let (call, receipt) = Call::new(Command::Preset {
        name: "Faster".into(),
    });
    h.runtime.preset(
        "Faster".into(),
        ValidConfig::assume(config),
        baseline,
        1,
        call,
    );
    assert_eq!(receive(&receipt).await?.result, Disposition::Ok);
    ready(&mut h).await?;
    assert!(h.loads.try_recv().is_err());
    assert_eq!(h.snapshot().preset.as_deref(), Some("Faster"));
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn preset_validation_cannot_cross_a_recording_or_settings_change() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    ready(&mut h).await?;
    let baseline = h.runtime.configuration.borrow().clone();
    h.start().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    let candidate = ValidConfig::assume(baseline.clone());
    let (call, receipt) = Call::new(Command::Preset { name: "Old".into() });
    h.runtime
        .preset("Old".into(), candidate.clone(), baseline.clone(), 0, call);
    assert_eq!(receive(&receipt).await?.result, Disposition::Refused);
    let mut changed = baseline.clone();
    changed.language = "de".into();
    h.runtime.configure(ValidConfig::assume(changed));
    let (call, receipt) = Call::new(Command::Preset { name: "Old".into() });
    h.runtime.preset("Old".into(), candidate, baseline, 1, call);
    assert_eq!(receive(&receipt).await?.result, Disposition::Refused);
    assert!(
        control(&h, Command::Status)
            .await?
            .status
            .context("No status")?
            .preset
            .is_none()
    );
    h.close().await
}

#[tokio::test(start_paused = true)]
async fn resumed_preset_keeps_saved_settings_separate_from_effective_choices() -> anyhow::Result<()>
{
    let baseline = Config::default();
    let mut effective = baseline.clone();
    effective.language = "fr".into();
    let saved = baseline.clone();
    let mut h = Harness::host(
        effective,
        None,
        |_| {},
        |config, styles, snapshots, meter, ports| {
            let (input, inputs) = input_lane();
            Runtime::host(
                Configuration {
                    effective: config,
                    saved,
                    styles,
                },
                snapshots,
                meter,
                ports,
                input,
                inputs,
                |wiring| Ok(OwnedTask::spawn(owner::run(wiring))),
            )
        },
    )?;
    ready(&mut h).await?;
    h.start().await?;
    h.runtime.configure(ValidConfig::assume(baseline.clone()));
    h.input(Input::Toggle);
    let (language, reply) = receive(&h.jobs).await?;
    assert_eq!(language, "fr");
    answer(reply, "Fixture.")?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    let (call, receipt) = Call::new(Command::Preset {
        name: "Default".into(),
    });
    h.runtime.preset(
        "Default".into(),
        ValidConfig::assume(baseline.clone()),
        baseline,
        1,
        call,
    );
    assert_eq!(receive(&receipt).await?.result, Disposition::Ok);
    h.close().await
}
