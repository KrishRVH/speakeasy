use std::sync::mpsc;

use super::Lifecycle;

fn live<Owners, Restart>(owners: Owners) -> Lifecycle<Owners, Restart> {
    let mut lifecycle = Lifecycle::default();
    assert_eq!(lifecycle.start(|| Ok::<_, ()>(owners)), Ok(true));
    lifecycle
}

#[test]
fn validating_and_starting_never_discard_live_or_retiring_owners() {
    struct Owner(mpsc::Sender<()>);

    impl Drop for Owner {
        fn drop(&mut self) {
            self.0.send(()).unwrap();
        }
    }

    let (drops, dropped) = mpsc::channel();
    let mut lifecycle = live::<_, ()>(Owner(drops.clone()));
    assert!(!lifecycle.validate());
    assert_eq!(
        lifecycle.start(|| Ok::<_, ()>(Owner(drops.clone()))),
        Ok(false)
    );
    assert!(dropped.try_recv().is_err());
    assert!(lifecycle.pause());
    assert!(!lifecycle.validate());
    assert_eq!(lifecycle.start(|| Ok::<_, ()>(Owner(drops))), Ok(false));
    assert!(dropped.try_recv().is_err());
    lifecycle.retired();
    dropped.try_recv().unwrap();
}

#[test]
fn a_resume_check_counts_as_running_until_quit_ends_it() {
    let mut lifecycle = Lifecycle::<(), ()>::default();
    assert!(lifecycle.validate());
    assert!(lifecycle.is_running(|()| false));
    assert!(lifecycle.quit());
    assert!(!lifecycle.is_running(|()| false));
    assert!(!lifecycle.validated());
    assert!(!lifecycle.validate());
    assert_eq!(lifecycle.start(|| Ok::<_, ()>(())), Ok(false));
}

#[test]
fn pause_invalidates_validation_and_repeated_pause_discards_resume() {
    let mut lifecycle = Lifecycle::<(), u8>::default();
    assert!(lifecycle.validate());
    assert!(!lifecycle.pause());
    assert!(!lifecycle.validated());
    lifecycle = live(());
    assert!(lifecycle.pause());
    assert!(lifecycle.queue(1));
    assert!(lifecycle.queue(2));
    assert!(!lifecycle.pause());
    assert!(!lifecycle.has_pending());
    assert_eq!(lifecycle.retired(), None);
}

#[test]
fn retirement_applies_only_latest_request_once_and_quit_is_terminal() {
    let mut lifecycle = live(());
    assert!(lifecycle.pause());
    assert!(lifecycle.queue(1));
    assert!(lifecycle.queue(2));
    assert_eq!(lifecycle.retired(), Some(2));
    assert_eq!(lifecycle.retired(), None);
    lifecycle = live(());
    assert!(lifecycle.pause());
    assert!(lifecycle.queue(3));
    assert!(lifecycle.quit());
    assert!(!lifecycle.quit());
    assert!(!lifecycle.queue(4));
    assert!(!lifecycle.pause());
    assert_eq!(lifecycle.retired(), None);
    assert!(lifecycle.is_quitting());
    assert!(lifecycle.active().is_none());
    assert!(lifecycle.retiring().is_none());
}

#[test]
fn a_pause_after_save_keeps_dictation_off_unless_resumed() {
    let mut lifecycle = live::<_, ()>(());
    assert!(lifecycle.pause());
    assert!(
        !lifecycle.save_may_enable(|()| true),
        "A save would restart the owners Pause is retiring"
    );
    assert_eq!(lifecycle.retired(), None);
    assert!(
        !lifecycle.save_may_enable(|()| true),
        "A setting changed while paused would resume dictation"
    );
    assert!(lifecycle.validate());
    assert!(
        lifecycle.save_may_enable(|()| true),
        "A deliberate Resume after Pause still receives the saved settings"
    );
    lifecycle = Lifecycle::default();
    assert!(
        lifecycle.save_may_enable(|()| false),
        "Saving the first complete settings must enable dictation"
    );
    lifecycle = live(());
    assert!(lifecycle.pause());
    assert!(lifecycle.queue(()));
    assert_eq!(lifecycle.retired(), Some(()));
    assert!(
        lifecycle.save_may_enable(|()| false),
        "A restart that applies settings isn't a user pause"
    );
}

#[test]
fn settings_saved_after_a_reset_enable_dictation_as_on_a_new_install() {
    let mut lifecycle = Lifecycle::<(), ()>::default();
    assert!(!lifecycle.pause());
    assert!(!lifecycle.save_may_enable(|()| false));
    lifecycle.forget_pause();
    assert!(
        lifecycle.save_may_enable(|()| false),
        "Setup after a reset left dictation paused"
    );
}
