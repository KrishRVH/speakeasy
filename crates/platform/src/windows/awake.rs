use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Power::{
            PowerClearRequest, PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest,
        },
        Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0},
    },
};

use super::to_wide;

const CONTEXT_VERSION: u32 = 0;

pub(crate) struct Awake(HANDLE);

impl Awake {
    pub(crate) fn hold() -> Option<Self> {
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
        let awake = Self(request);
        (unsafe { PowerSetRequest(awake.0, PowerRequestSystemRequired) } != 0).then_some(awake)
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        unsafe {
            PowerClearRequest(self.0, PowerRequestSystemRequired);
            CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Awake;

    #[test]
    fn windows_grants_and_releases_a_recording_power_request() {
        let first = Awake::hold().expect("Windows refused the power request");
        let second = Awake::hold().expect("A second request was refused");
        drop(first);
        drop(second);
    }
}
