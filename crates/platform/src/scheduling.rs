//! Scheduling for latency-sensitive dictation work. User-initiated `QoS` keeps capture, the session
//! owner, and the speech engine's threads on performance cores; at background `QoS` an 11-second
//! recognition took 141 ms instead of 63 ms on an M4 Pro. An activity keeps App Nap from
//! throttling a menu-bar process that has no visible window.

#[cfg(target_os = "macos")]
use objc2::{rc::Retained, runtime::ProtocolObject};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};

/// Raises the calling thread to user-initiated `QoS`; threads it creates afterwards inherit it.
pub fn prefer_responsive_thread() {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: the call changes only the calling thread's scheduling class and takes no
        // pointers. A refusal leaves scheduling unchanged, which is the safe outcome.
        let refused = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0)
        };
        debug_assert!(refused == 0, "QoS change refused: {refused}");
    }
}

/// Keeps App Nap from throttling this process while held, without preventing idle system sleep.
pub struct Responsive {
    #[cfg(target_os = "macos")]
    activity: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl Responsive {
    /// Begins an activity that `reason` describes in system diagnostics.
    #[must_use]
    pub fn begin(reason: &str) -> Self {
        Self {
            #[cfg(target_os = "macos")]
            activity: NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
                &NSString::from_str(reason),
            ),
        }
    }
}

impl Drop for Responsive {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        // SAFETY: the token came from beginActivityWithOptions:reason: on this process and is ended
        // exactly once, here.
        unsafe {
            NSProcessInfo::processInfo().endActivity(&self.activity);
        }
    }
}
