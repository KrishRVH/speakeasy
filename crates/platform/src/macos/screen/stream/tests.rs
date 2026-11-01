use objc2_foundation::ns_string;

use super::*;

#[test]
fn only_a_native_already_stopped_error_acknowledges_stop() {
    let domain = unsafe { objc2_screen_capture_kit::SCStreamErrorDomain };
    let code = objc2_screen_capture_kit::SCStreamErrorCode::AttemptToStopStreamState.0;

    let (stopped, unrelated, refused) = unsafe {
        (
            NSError::errorWithDomain_code_userInfo(domain, code, None),
            NSError::errorWithDomain_code_userInfo(ns_string!("SyntheticError"), code, None),
            NSError::errorWithDomain_code_userInfo(
                domain,
                objc2_screen_capture_kit::SCStreamErrorCode::UserDeclined.0,
                None,
            ),
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
