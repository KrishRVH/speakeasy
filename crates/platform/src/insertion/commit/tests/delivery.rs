use super::{super::RETURN_WITHHELD, *};
use crate::insertion::{Insertion, Reply, commit::deliver, tests::HELD};

#[test]
fn delivery_waits_on_its_native_shortcut_and_rechecks_it_before_sending() -> anyhow::Result<()> {
    for delivery in DELIVERIES {
        for releases in [vec![true, true], vec![false, true, true]] {
            let mut native = Fake::new();
            native.releases = releases.into();
            let request = request(delivery)?;
            assert_eq!(deliver(&mut native, &request)?, Inserted::Sent);
            assert!(native.sent());
        }
        let mut native = Fake::new();
        native.releases = [true, false].into();
        let request = request(delivery)?;
        assert_eq!(
            deliver(&mut native, &request)?,
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
        assert_eq!(deliver(&mut native, &request)?, Inserted::Sent);
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
        let refused = deliver(&mut blocked, &request)?;
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
    assert_eq!(deliver(&mut native, &request)?, RETURN_WITHHELD);
    assert!(native.sent());
    assert!(!native.steps.contains(&Step::Send(Keys::Return)));
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
