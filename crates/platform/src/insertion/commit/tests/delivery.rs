use super::*;
use crate::insertion::{Insertion, Reply};

#[test]
fn delivery_waits_on_its_native_shortcut_and_rechecks_it_before_sending() -> anyhow::Result<()> {
    for delivery in DELIVERIES {
        for releases in [vec![true, true], vec![false, true, true]] {
            let mut native = Fake::new();
            native.releases = releases.into();
            let request = request(delivery)?;
            assert_eq!(
                super::super::deliver(&mut native, &request)?,
                Inserted::Sent
            );
            assert!(native.sent());
        }
        let mut native = Fake::new();
        native.releases = [true, false].into();
        let request = request(delivery)?;
        assert_eq!(
            super::super::deliver(&mut native, &request)?,
            delivery.preflight(true, false).unwrap()
        );
        assert!(!native.sent());
    }
    Ok(())
}

fn request(delivery: Delivery) -> anyhow::Result<Insertion> {
    let (reply, _answer) = Reply::channel();
    Ok(Insertion {
        text: "PUBLIC_FIXTURE".into(),
        permit: permit()?,
        delivery,
        own_focus: unfocused(),
        reply,
    })
}
