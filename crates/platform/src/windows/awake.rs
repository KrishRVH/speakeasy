use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Power::{
            POWER_REQUEST_TYPE, PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired,
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
    set: Vec<POWER_REQUEST_TYPE>,
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
        let mut awake = Self {
            request,
            set: Vec::new(),
        };
        let wanted: &[POWER_REQUEST_TYPE] = match keep {
            Keep::System => &[PowerRequestSystemRequired],
            Keep::Display => &[PowerRequestSystemRequired, PowerRequestDisplayRequired],
        };
        for &kind in wanted {
            if unsafe { PowerSetRequest(awake.request, kind) } == 0 {
                return None;
            }
            awake.set.push(kind);
        }
        Some(awake)
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        for &kind in &self.set {
            unsafe { PowerClearRequest(self.request, kind) };
        }
        unsafe { CloseHandle(self.request) };
    }
}

#[cfg(test)]
mod tests {
    use super::{Awake, Keep};

    #[test]
    fn windows_grants_and_releases_recording_power_requests() {
        let system = Awake::hold(Keep::System).expect("Windows refused the system request");
        let display = Awake::hold(Keep::Display).expect("Windows refused the display request");
        assert_eq!(display.set.len(), 2);
        drop(system);
        drop(display);
    }
}
