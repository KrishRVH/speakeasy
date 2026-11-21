use super::Choices;

#[test]
fn a_tray_choice_the_owner_accepted_outlives_a_later_use_it_refused() {
    let mut choices = Choices::default();
    let tray = choices.request();
    let used = choices.request();
    assert!(!choices.answer(used, false));
    assert!(
        choices.answer(tray, true),
        "A Use the owner refused discarded the tray choice it had accepted first"
    );
    assert!(choices.is_settled());
}

#[test]
fn only_the_newest_accepted_request_survives_and_a_rollback_outranks_it() {
    let mut choices = Choices::default();
    let tray = choices.request();
    let used = choices.request();
    assert!(choices.answer(used, true));
    assert!(
        !choices.answer(tray, true),
        "An older answer replaced the newer request the owner accepted after it"
    );
    let late = choices.request();
    choices.supersede();
    assert!(
        !choices.answer(late, true),
        "A request sent before a rollback outlived it"
    );
}

#[test]
fn a_cleared_choice_stays_cleared_when_its_answer_arrives_later() {
    let mut choices = Choices::default();
    let tray = choices.request();
    choices.forget();
    assert!(
        !choices.answer(tray, true),
        "An answer that arrived after Reset or a paused Use revived the choice they cleared"
    );
    assert!(choices.selected.is_none());
}
