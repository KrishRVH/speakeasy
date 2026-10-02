//! Scheduling for latency-sensitive dictation work. User-initiated `QoS` puts capture, the session
//! owner, and the speech engine ahead of default and background work when cores are contended. An
//! activity keeps App Nap from throttling a menu-bar process that has no visible window; a throttled
//! process ran an 11-second recognition in 141 ms instead of 63 ms on an M4 Pro, and thread `QoS`
//! cannot lift that clamp.

#[cfg(target_os = "macos")]
use objc2::{rc::Retained, runtime::ProtocolObject};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};

/// Raises the calling thread to user-initiated `QoS`. New threads start at the default class
/// whatever their creator's, so each latency-sensitive thread calls this itself.
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
