use super::{super::RETURN_WITHHELD, *};
use crate::insertion::{Insertion, Reply, commit::deliver, tests::HELD};

#[test]
fn delivery_waits_on_its_native_shortcut_and_rechecks_it_before_sending() -> anyhow::Result<()> {
    for delivery in DELIVERIES {
        for releases in [vec![true, true], vec![false, true, true]] {
            let mut native = Fake::new();
            native.releases = releases.into();
            let request = request(delivery)?;
            assert_eq!(deliver(&mut native, &request)?.0, Inserted::Sent);
            assert!(native.sent());
        }
        let mut native = Fake::new();
        native.releases = [true, false].into();
        let request = request(delivery)?;
        assert_eq!(
            deliver(&mut native, &request)?.0,
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
        assert_eq!(deliver(&mut native, &request)?.0, Inserted::Sent);
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
        let refused = deliver(&mut blocked, &request)?.0;
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
    assert_eq!(deliver(&mut native, &request)?.0, RETURN_WITHHELD);
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
    assert_eq!(deliver(&mut native, &request)?.0, RETURN_WITHHELD);
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
    assert_eq!(deliver(&mut native, &request)?.0, RETURN_WITHHELD);
    Ok(())
}

#[test]
fn a_sent_dictation_leaves_its_text_on_the_clipboard() -> anyhow::Result<()> {
    let pressed = Insertion {
        enter: Enter::Pressed,
        ..request(Delivery::Paste)?
    };
    let (inserted, saved) = deliver(&mut Fake::new(), &pressed)?;
    assert_eq!(inserted, Inserted::Sent);
    assert!(
        saved.is_none(),
        "A slow app could paste the restored copy and send it"
    );
    let (_, saved) = deliver(&mut Fake::new(), &request(Delivery::Paste)?)?;
    assert!(saved.is_some());
    Ok(())
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
