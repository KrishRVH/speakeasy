use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Power::{
            PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired,
            PowerRequestSystemRequired, PowerSetRequest,
        },
        Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0},
    },
};

use super::to_wide;
use crate::awake::Keep;

const CONTEXT_VERSION: u32 = 0;

pub(crate) struct Awake {
    request: HANDLE,
    display: bool,
}

unsafe impl Send for Awake {}

impl Awake {
    pub(crate) fn hold(keep: Keep) -> Option<Self> {
        let mut reason = to_wide("Speakeasy is recording");
        let context = REASON_CONTEXT {
            Version: CONTEXT_VERSION,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: reason.as_mut_ptr(),
            },
        };
        let request = unsafe { PowerCreateRequest(&raw const context) };
        if request.is_null() || request == INVALID_HANDLE_VALUE {
            return None;
        }
        if unsafe { PowerSetRequest(request, PowerRequestSystemRequired) } == 0 {
            unsafe { CloseHandle(request) };
            return None;
        }
        let display = keep == Keep::Display
            && unsafe { PowerSetRequest(request, PowerRequestDisplayRequired) } != 0;
        Some(Self { request, display })
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        unsafe { PowerClearRequest(self.request, PowerRequestSystemRequired) };
        if self.display {
            unsafe { PowerClearRequest(self.request, PowerRequestDisplayRequired) };
        }
        unsafe { CloseHandle(self.request) };
    }
}

#[cfg(test)]
mod tests {
    use super::{Awake, Keep};

    #[test]
    fn a_refused_display_request_still_keeps_the_system_awake() {
        let system = Awake::hold(Keep::System).expect("Windows refused the system request");
        assert!(!system.display);
        Awake::hold(Keep::Display).expect("A refused display request lost the system request");
    }
}
