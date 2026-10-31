//! Non-prompting permission observations. These describe OS consent, not device availability or
//! successful delivery; the session owner still handles failures when resources are opened.

/// The last OS observation of one permission; it may change immediately after being read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Permission {
    /// No reliable preflight is available, or the permission has not been observed.
    #[default]
    Unknown,
    /// The OS reports permission granted, without opening the resource.
    Granted,
    /// The OS reports no grant; some APIs cannot distinguish denied from never requested.
    NotGranted,
    /// The OS confirms no consent decision has been made yet.
    NotRequested,
    /// System policy prevents the user from granting access.
    Restricted,
}

/// Permission observations for this process, separate from the runtime's resource readiness.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Permissions {
    /// Access to audio input, not the availability of a particular microphone.
    pub microphone: Permission,
    /// macOS Accessibility consent used by the shortcut and text insertion.
    pub accessibility: Permission,
    /// Consent to screen capture, not the availability of a particular surface.
    pub screen: Permission,
}

impl Permissions {
    /// Reads current consent without prompting, opening devices, or installing input hooks.
    ///
    /// Call on the native UI thread. Windows desktop and Linux access is left unknown: privacy
    /// settings and portal availability do not prove that this process can use a resource.
    #[must_use]
    pub fn read() -> Self {
        #[cfg(target_os = "macos")]
        {
            use objc2_application_services::AXIsProcessTrusted;
            use objc2_core_graphics::CGPreflightScreenCaptureAccess;

            // SAFETY: This process-wide query has no arguments, requests no prompt, and owns no
            // resources. It does not install an event tap or exercise Accessibility control.
            let accessibility = unsafe { AXIsProcessTrusted() };
            Self {
                microphone: microphone(),
                accessibility: consent(accessibility),
                screen: consent(CGPreflightScreenCaptureAccess()),
            }
        }
        #[cfg(not(target_os = "macos"))]
        Self::default()
    }
}

#[cfg(target_os = "macos")]
fn microphone() -> Permission {
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

    // SAFETY: AVMediaTypeAudio is an immutable framework constant. The query accepts this media
    // type, reads process authorization only, and neither enumerates devices nor requests consent.
    let status = unsafe {
        let Some(audio) = AVMediaTypeAudio else {
            return Permission::Unknown;
        };
        AVCaptureDevice::authorizationStatusForMediaType(audio)
    };
    match status {
        AVAuthorizationStatus::Authorized => Permission::Granted,
        AVAuthorizationStatus::Denied => Permission::NotGranted,
        AVAuthorizationStatus::Restricted => Permission::Restricted,
        AVAuthorizationStatus::NotDetermined => Permission::NotRequested,
        _ => Permission::Unknown,
    }
}

#[cfg(target_os = "macos")]
const fn consent(granted: bool) -> Permission {
    if granted {
        Permission::Granted
    } else {
        Permission::NotGranted
    }
}
