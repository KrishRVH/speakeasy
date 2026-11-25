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

/// `POWER_REQUEST_CONTEXT_VERSION`, copied to spare enabling windows-sys's large `SystemServices`.
const CONTEXT_VERSION: u32 = 0;

pub(crate) struct Awake {
    request: HANDLE,
}

// SAFETY: a power request is a kernel handle, usable and closable from any thread.
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
        if keep == Keep::Display {
            unsafe { PowerSetRequest(request, PowerRequestDisplayRequired) };
        }
        Some(Self { request })
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        unsafe {
            PowerClearRequest(self.request, PowerRequestSystemRequired);
            PowerClearRequest(self.request, PowerRequestDisplayRequired);
            CloseHandle(self.request);
        }
    }
}
