use std::{sync::mpsc, thread, time::Duration};

use anyhow::Context as _;
use speakeasy_show::Wsl;

use super::Lookup;
use crate::testing::PATIENCE;

#[test]
fn the_lookup_leaves_wsl_alone_until_a_show_needs_it() -> anyhow::Result<()> {
    let (started, looking) = mpsc::channel();
    let lookup = Lookup::spawn(
        move || {
            started.send(()).ok()?;
            Some(Wsl::configured(""))
        },
        PATIENCE,
    );
    assert!(
        looking.recv_timeout(Duration::from_millis(100)).is_err(),
        "The lookup reached into WSL before anything asked"
    );
    lookup.prepare();
    looking.recv_timeout(PATIENCE)?;
    assert_eq!(lookup.found(), Some(Wsl::configured("")));
    Ok(())
}

#[test]
fn a_slow_lookup_answers_nothing_rather_than_hold_a_show() -> anyhow::Result<()> {
    let (release, held) = mpsc::channel::<()>();
    let lookup = Lookup::spawn(
        move || held.recv().ok().map(|()| Wsl::configured("")),
        Duration::from_millis(20),
    );
    let (sent, answered) = mpsc::channel();
    let early = thread::scope(|scope| {
        scope.spawn(|| sent.send(lookup.found()));
        let early = answered.recv_timeout(PATIENCE);
        release.send(())?;
        anyhow::Ok(early)
    })?;
    let early = early.context("A slow lookup held its caller past its patience")?;
    assert_eq!(early, None);
    let patient = Lookup {
        patience: PATIENCE,
        ..lookup
    };
    assert_eq!(
        patient.found(),
        Some(Wsl::configured("")),
        "A finished lookup kept answering nothing"
    );
    Ok(())
}
