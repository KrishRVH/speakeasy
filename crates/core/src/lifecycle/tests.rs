use std::sync::mpsc;

use super::{Lifecycle, State};

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

    let (completion, dropped) = mpsc::channel();
    let mut lifecycle = live::<_, ()>(Owner(completion.clone()));
    assert!(lifecycle.validate().is_none());
    assert_eq!(
        lifecycle.start(|| Ok::<_, ()>(Owner(completion.clone()))),
        Ok(false)
    );
    assert!(dropped.try_recv().is_err());
    assert!(lifecycle.pause());
    assert!(lifecycle.validate().is_none());
    assert_eq!(
        lifecycle.start(|| Ok::<_, ()>(Owner(completion.clone()))),
        Ok(false)
    );
    assert!(dropped.try_recv().is_err());
    lifecycle.retired();
    dropped.try_recv().unwrap();
    let checking = lifecycle.validate().unwrap();
    assert!(lifecycle.is_running(|_| false));
    assert!(lifecycle.quit());
    assert!(!lifecycle.is_running(|_| false));
    assert!(!lifecycle.validated(checking));
    assert!(lifecycle.validate().is_none());
    assert_eq!(
        lifecycle.start(|| Ok::<_, ()>(Owner(completion))),
        Ok(false)
    );
}

#[test]
fn pause_invalidates_validation_and_repeated_pause_discards_resume() {
    let mut lifecycle = Lifecycle::<(), u8>::default();
    let checking = lifecycle.validate().unwrap();
    assert!(!lifecycle.pause());
    assert!(!lifecycle.validated(checking));
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
    assert!(!lifecycle.reconfigure());
    assert_eq!(lifecycle.retired(), None);
    assert!(lifecycle.is_quitting());
    assert!(lifecycle.active().is_none());
    assert!(lifecycle.retiring().is_none());
}

#[test]
fn a_pause_after_save_keeps_dictation_off_unless_resumed() {
    let mut lifecycle = live::<_, ()>(());
    let saved_at = lifecycle.epoch();
    assert!(lifecycle.pause());
    assert!(
        !lifecycle.save_may_enable(saved_at, |()| true),
        "A save requested before Pause would restart the owners Pause is retiring"
    );
    assert_eq!(lifecycle.retired(), None);
    assert!(
        !lifecycle.save_may_enable(saved_at, |()| true),
        "A save requested before Pause would enable dictation after it"
    );
    assert!(
        !lifecycle.save_may_enable(lifecycle.epoch(), |()| true),
        "A setting changed while paused would resume dictation"
    );
    lifecycle.validate().unwrap();
    assert!(
        lifecycle.save_may_enable(saved_at, |()| true),
        "A deliberate Resume after Pause still receives the durable settings"
    );
    lifecycle = Lifecycle::default();
    lifecycle.validate().unwrap();
    let saved_at = lifecycle.epoch();
    assert!(!lifecycle.pause());
    assert!(!lifecycle.save_may_enable(saved_at, |()| true));
    assert!(!lifecycle.save_may_enable(lifecycle.epoch(), |()| true));
    lifecycle = Lifecycle::default();
    assert!(
        lifecycle.save_may_enable(lifecycle.epoch(), |()| false),
        "Saving the first complete settings must enable dictation"
    );
    lifecycle = live(());
    assert!(lifecycle.pause());
    assert!(lifecycle.queue(()));
    assert_eq!(lifecycle.retired(), Some(()));
    assert!(
        lifecycle.save_may_enable(lifecycle.epoch(), |()| false),
        "A restart that applies settings is not a user pause"
    );
}

#[test]
fn settings_saved_after_a_fresh_start_enable_dictation_as_on_a_new_install() {
    let mut lifecycle = Lifecycle::<(), ()>::default();
    assert!(!lifecycle.pause());
    assert!(!lifecycle.save_may_enable(lifecycle.epoch(), |()| false));
    lifecycle.start_fresh();
    assert!(
        lifecycle.save_may_enable(lifecycle.epoch(), |()| false),
        "Setup after a reset left dictation paused"
    );
}

#[test]
fn a_save_during_a_resume_check_supersedes_the_check() {
    let mut lifecycle = Lifecycle::<u8, ()>::default();
    let checking = lifecycle.validate().unwrap();
    assert!(lifecycle.save_may_enable(lifecycle.epoch(), |_| false));
    assert!(lifecycle.reconfigure());
    assert!(!lifecycle.queue(()));
    assert_eq!(lifecycle.start(|| Ok::<_, ()>(1)), Ok(true));
    assert!(!lifecycle.validated(checking));
    assert_eq!(lifecycle.active(), Some(&1));
}

#[test]
fn owners_that_fail_to_start_still_end_the_check_they_superseded() {
    let mut lifecycle = Lifecycle::<u8, ()>::default();
    let checking = lifecycle.validate().unwrap();
    assert!(lifecycle.reconfigure());
    assert_eq!(lifecycle.start(|| Err::<u8, _>(())), Err(()));
    assert!(
        !lifecycle.validated(checking),
        "A check outlived newer settings whose owners failed to start"
    );
    assert!(matches!(lifecycle.state, State::Disabled));
}
