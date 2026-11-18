use super::{super::RETURN_WITHHELD, *};
use crate::insertion::{Insertion, Reply, commit::deliver, tests::HELD};

#[test]
fn delivery_waits_on_its_native_shortcut_and_rechecks_it_before_sending() -> anyhow::Result<()> {
    for delivery in DELIVERIES {
        for releases in [vec![true, true], vec![false, true, true]] {
            let mut native = Fake::new();
            native.releases = releases.into();
            let request = request(delivery)?;
            assert_eq!(
                deliver(&mut native, &request, &mut Continuation::default())?.0,
                Inserted::Sent
            );
            assert!(native.sent());
        }
        let mut native = Fake::new();
        native.releases = [true, false].into();
        let request = request(delivery)?;
        assert_eq!(
            deliver(&mut native, &request, &mut Continuation::default())?.0,
            delivery.preflight(EXTERNAL_FOCUS, HELD).unwrap()
        );
        assert!(!native.sent());
    }
    Ok(())
}

#[test]
fn a_submitted_insertion_presses_return_only_after_its_text_lands() -> anyhow::Result<()> {
    for delivery in DELIVERIES {
        let mut native = Fake::new();
        let request = Insertion {
            enter: Enter::Pressed,
            ..request(delivery)?
        };
        assert_eq!(
            deliver(&mut native, &request, &mut Continuation::default())?.0,
            Inserted::Sent
        );
        assert_eq!(
            native.steps.iter().rev().take(2).collect::<Vec<_>>(),
            [&Step::Send(Keys::Return), &Step::ShortcutReleased]
        );
        assert_eq!(
            native
                .steps
                .iter()
                .filter(|step| matches!(step, Step::Send(_)))
                .count(),
            2
        );

        let mut blocked = Fake::new();
        blocked.external_focus = !EXTERNAL_FOCUS;
        let refused = deliver(&mut blocked, &request, &mut Continuation::default())?.0;
        assert_ne!(refused, Inserted::Sent);
        assert!(!blocked.steps.contains(&Step::Send(Keys::Return)));
    }
    Ok(())
}

#[test]
fn return_waits_for_focus_and_released_keys_after_the_text_lands() -> anyhow::Result<()> {
    let request = Insertion {
        enter: Enter::Pressed,
        ..request(Delivery::Paste)?
    };
    let mut native = Fake::new();
    native.releases = [true, true, false].into();
    assert_eq!(
        deliver(&mut native, &request, &mut Continuation::default())?.0,
        RETURN_WITHHELD
    );
    assert!(native.sent());
    assert!(!native.steps.contains(&Step::Send(Keys::Return)));
    Ok(())
}

#[test]
fn return_goes_only_to_the_app_that_took_the_text() -> anyhow::Result<()> {
    let request = Insertion {
        enter: Enter::Pressed,
        ..request(Delivery::Paste)?
    };
    let mut native = Fake::new();
    native.targets = [1, 2].into();
    assert_eq!(
        deliver(&mut native, &request, &mut Continuation::default())?.0,
        RETURN_WITHHELD
    );
    assert!(native.sent());
    assert!(!native.steps.contains(&Step::Send(Keys::Return)));
    Ok(())
}

#[test]
fn a_failed_return_never_offers_the_landed_text_again() -> anyhow::Result<()> {
    let request = Insertion {
        enter: Enter::Pressed,
        ..request(Delivery::Paste)?
    };
    let mut native = Fake::new();
    native.return_fails = true;
    assert_eq!(
        deliver(&mut native, &request, &mut Continuation::default())?.0,
        RETURN_WITHHELD
    );
    Ok(())
}

#[test]
fn a_sent_dictation_leaves_its_text_on_the_clipboard() -> anyhow::Result<()> {
    let pressed = Insertion {
        enter: Enter::Pressed,
        ..request(Delivery::Paste)?
    };
    let (inserted, saved) = deliver(&mut Fake::new(), &pressed, &mut Continuation::default())?;
    assert_eq!(inserted, Inserted::Sent);
    assert!(
        saved.is_none(),
        "A slow app could paste the restored copy and send it"
    );
    let (_, saved) = deliver(
        &mut Fake::new(),
        &request(Delivery::Paste)?,
        &mut Continuation::default(),
    )?;
    assert!(saved.is_some());
    Ok(())
}

#[test]
fn a_dictation_right_after_the_last_one_is_spaced_from_it() -> anyhow::Result<()> {
    for delivery in DELIVERIES {
        let mut native = Fake::new();
        let mut continuation = Continuation::default();
        for text in ["Hi, how's your day?", "Oh yeah, how are you?"] {
            let said = said(text, delivery)?;
            assert_eq!(
                deliver(&mut native, &said, &mut continuation)?.0,
                Inserted::Sent
            );
        }
        assert_eq!(
            native.texts,
            ["Hi, how's your day?", " Oh yeah, how are you?"],
            "{delivery:?}"
        );
    }
    Ok(())
}

#[test]
fn typing_pointing_or_another_window_ends_the_trail() -> anyhow::Result<()> {
    assert_eq!(second_after(|_, _| {}, |_| {})?, " Second");
    assert_eq!(
        second_after(|_, _| {}, |native| native.activity = 1)?,
        "Second",
        "Typed or moved the pointer"
    );
    assert_eq!(
        second_after(|_, _| {}, |native| native.targets = [2].into())?,
        "Second",
        "Focused another window"
    );
    Ok(())
}

#[test]
fn only_text_left_in_place_starts_a_trail() -> anyhow::Result<()> {
    assert_eq!(
        second_after(|native, _| native.external_focus = OWN_FOCUS, |_| {})?,
        "Second",
        "Left to paste by hand"
    );
    assert_eq!(
        second_after(|_, first| first.enter = Enter::Pressed, |_| {})?,
        "Second",
        "Sent with Return"
    );
    Ok(())
}

#[test]
fn a_cancelled_dictation_keeps_the_trail_it_found() -> anyhow::Result<()> {
    let mut native = Fake::new();
    let mut continuation = Continuation::default();
    deliver(
        &mut native,
        &said("First.", Delivery::Paste)?,
        &mut continuation,
    )?;
    let cancelled = said("Never sent", Delivery::Paste)?;
    cancelled.permit.revoke();
    assert_eq!(
        deliver(&mut native, &cancelled, &mut continuation)?.0,
        Inserted::Cancelled
    );
    deliver(
        &mut native,
        &said("Second", Delivery::Paste)?,
        &mut continuation,
    )?;
    assert_eq!(native.texts, ["First.", " Second"]);
    Ok(())
}

fn second_after(
    shape_first: impl FnOnce(&mut Fake, &mut Insertion),
    between: impl FnOnce(&mut Fake),
) -> anyhow::Result<String> {
    let mut native = Fake::new();
    let mut continuation = Continuation::default();
    let mut first = said("First.", Delivery::Paste)?;
    shape_first(&mut native, &mut first);
    deliver(&mut native, &first, &mut continuation)?;
    native.external_focus = EXTERNAL_FOCUS;
    between(&mut native);
    deliver(
        &mut native,
        &said("Second", Delivery::Paste)?,
        &mut continuation,
    )?;
    native.texts.pop().context("Nothing was written")
}

fn said(text: &str, delivery: Delivery) -> anyhow::Result<Insertion> {
    Ok(Insertion {
        text: text.into(),
        ..request(delivery)?
    })
}

fn request(delivery: Delivery) -> anyhow::Result<Insertion> {
    let (reply, _answer) = Reply::channel();
    Ok(Insertion {
        text: "PUBLIC_FIXTURE".into(),
        permit: permit()?,
        delivery,
        enter: Enter::Withheld,
        own_focus: unfocused(),
        reply,
    })
}
