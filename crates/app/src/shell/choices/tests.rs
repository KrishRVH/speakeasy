use super::Choices;

#[test]
fn a_tray_choice_the_owner_accepted_outlives_a_later_use_it_refused() {
    let mut choices = Choices::default();
    let tray = choices.forward();
    let used = choices.forward();
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
    let tray = choices.forward();
    let used = choices.forward();
    assert!(choices.answer(used, true));
    assert!(
        !choices.answer(tray, true),
        "An older answer replaced the newer request the owner accepted after it"
    );
    let late = choices.forward();
    choices.forget();
    assert!(
        !choices.answer(late, true),
        "A request sent before a rollback outlived it"
    );
}

#[test]
fn a_refusal_leaves_an_earlier_request_outstanding() {
    let mut choices = Choices::default();
    let earlier = choices.forward();
    let later = choices.forward();
    choices.answer(later, false);
    assert!(
        !choices.is_settled(),
        "A refusal settled a request the owner had not answered"
    );
    choices.answer(earlier, true);
    assert!(choices.is_settled());
}
