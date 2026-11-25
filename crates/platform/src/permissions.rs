#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Permission {
    #[default]
    Unknown,
    Granted,
    NotGranted,
    NotRequested,
    Restricted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Permissions {
    pub microphone: Permission,
    pub accessibility: Permission,
    pub screen: Permission,
}

impl Permissions {
    #[must_use]
    pub fn read() -> Self {
        #[cfg(target_os = "macos")]
        {
            use objc2_application_services::AXIsProcessTrusted;
            use objc2_core_graphics::CGPreflightScreenCaptureAccess;

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

/// macOS 27.3 grants posting synthetic keys apart from Accessibility, under System & App
/// Protection Override, and `CGEventPost` silently drops them without it; earlier versions grant
/// both together. The answer holds from launch, so a new grant takes effect after a restart.
#[cfg(target_os = "macos")]
pub(crate) fn may_post_keys() -> bool {
    use objc2_core_graphics::CGPreflightPostEventAccess;
    use objc2_foundation::{NSOperatingSystemVersion, NSProcessInfo};

    let split = NSOperatingSystemVersion {
        majorVersion: 27,
        minorVersion: 3,
        patchVersion: 0,
    };
    !NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(split)
        || CGPreflightPostEventAccess()
}

#[cfg(target_os = "macos")]
fn microphone() -> Permission {
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

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
