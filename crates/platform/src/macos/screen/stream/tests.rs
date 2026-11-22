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

const BRIEF: Duration = Duration::from_millis(20);

fn signal_after(pause: Duration, lane: Sender<Infallible>) {
    tokio::spawn(async move {
        tokio::time::sleep(pause).await;
        lane.close();
    });
}

async fn settled<T>(wait: impl Future<Output = T>) -> Option<T> {
    tokio::time::timeout(Duration::from_secs(1), wait)
        .await
        .ok()
}

#[tokio::test]
async fn a_did_stop_signal_during_the_wait_acknowledges_a_retained_completion() {
    let (_retained, reply) = async_channel::bounded(1);
    let (did_stop, stopped) = async_channel::bounded::<Infallible>(1);
    signal_after(BRIEF, did_stop);
    let waited = settled(acknowledged(&reply, &stopped, PATIENCE)).await;
    assert!(matches!(waited, Some(Ok(()))));
}

#[tokio::test]
async fn a_late_successful_stop_confirms_the_next_attempt() {
    let (confirm, stopped) = async_channel::bounded::<Infallible>(1);
    let (first, first_reply) = async_channel::bounded(1);
    let waited = settled(acknowledged(&first_reply, &stopped, BRIEF)).await;
    assert!(matches!(waited, Some(Err(Failure::Stalled))));
    drop(first_reply);
    deliver(Ok(()), Some(&confirm), &first);
    let (_second, second_reply) = async_channel::bounded(1);
    let waited = settled(acknowledged(&second_reply, &stopped, BRIEF)).await;
    assert!(matches!(waited, Some(Ok(()))));
}

#[tokio::test]
async fn a_refused_stop_stays_unconfirmed_until_the_stream_stops() {
    let (confirm, stopped) = async_channel::bounded::<Infallible>(1);
    let (completed, reply) = async_channel::bounded(1);
    deliver(Err(Failure::Stalled), Some(&confirm), &completed);
    let waited = settled(acknowledged(&reply, &stopped, PATIENCE)).await;
    assert!(matches!(waited, Some(Err(Failure::Stalled))));
    deliver(Err(Failure::Stalled), Some(&confirm), &completed);
    confirm.close();
    let waited = settled(acknowledged(&reply, &stopped, PATIENCE)).await;
    assert!(matches!(waited, Some(Ok(()))));
}

#[tokio::test]
async fn a_start_ends_when_capture_stops_or_is_cancelled_before_its_completion() {
    let (_retained, reply) = async_channel::bounded(1);
    let (did_stop, stopped) = async_channel::bounded::<Infallible>(1);
    let (_request, stopping) = async_channel::bounded::<Infallible>(1);
    signal_after(BRIEF, did_stop);
    let started = settled(starting(&reply, &stopped, &stopping)).await;
    assert!(matches!(started, Some(Err(Failure::Unstarted))));

    let (_retained, reply) = async_channel::bounded(1);
    let (_did_stop, stopped) = async_channel::bounded::<Infallible>(1);
    let (request, stopping) = async_channel::bounded::<Infallible>(1);
    signal_after(BRIEF, request);
    let started = settled(starting(&reply, &stopped, &stopping)).await;
    assert!(matches!(started, Some(Ok(Started::Cancelled))));
}
