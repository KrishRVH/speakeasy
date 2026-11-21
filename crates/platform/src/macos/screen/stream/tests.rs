use objc2_foundation::ns_string;

use super::*;

#[test]
fn only_a_native_already_stopped_error_acknowledges_stop() {
    let domain = unsafe { SCStreamErrorDomain };
    let code = SCStreamErrorCode::AttemptToStopStreamState.0;

    let (stopped, unrelated, refused) = unsafe {
        (
            NSError::errorWithDomain_code_userInfo(domain, code, None),
            NSError::errorWithDomain_code_userInfo(ns_string!("SyntheticError"), code, None),
            NSError::errorWithDomain_code_userInfo(domain, SCStreamErrorCode::UserDeclined.0, None),
        )
    };
    assert!(completion_result(Some(&stopped), Operation::Stop).is_ok());
    assert!(completion_result(Some(&stopped), Operation::Start).is_err());
    assert!(completion_result(Some(&unrelated), Operation::Stop).is_err());
    assert!(matches!(
        completion_result(Some(&refused), Operation::Stop),
        Err(Failure::Permission)
    ));
}

#[test]
fn a_did_stop_signal_acknowledges_a_stop_whose_completion_never_arrives() {
    let (retained, reply) = async_channel::bounded(1);
    let (did_stop, stopped) = async_channel::bounded::<Infallible>(1);
    did_stop.close();
    assert!(acknowledged(&reply, &stopped).is_ok());
    drop(retained);
}

#[test]
fn a_refused_stop_stays_unconfirmed_until_the_stream_stops() {
    let (completed, reply) = async_channel::bounded(1);
    let (did_stop, stopped) = async_channel::bounded::<Infallible>(1);
    assert!(completed.try_send(Err(Failure::Stalled)).is_ok());
    assert!(acknowledged(&reply, &stopped).is_err());
    assert!(completed.try_send(Err(Failure::Stalled)).is_ok());
    did_stop.close();
    assert!(acknowledged(&reply, &stopped).is_ok());
}

#[test]
fn stop_retries_back_off_and_give_up() {
    let mut pauses = Vec::new();
    let gave_up = retry_stop(|| Err(Failure::Stalled), |pause| pauses.push(pause));
    assert!(matches!(gave_up, Err(Failure::Unstopped)));
    assert_eq!(pauses, STOP_PAUSES);

    let mut answers = [Err(Failure::Stalled), Ok(())].into_iter();
    let mut pauses = Vec::new();
    let stopped = retry_stop(
        || answers.next().unwrap_or(Err(Failure::Stalled)),
        |pause| pauses.push(pause),
    );
    assert!(stopped.is_ok());
    assert_eq!(pauses, STOP_PAUSES[..2]);
}
