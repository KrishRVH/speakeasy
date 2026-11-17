use std::fmt;

pub struct Awake {
    _held: native::Awake,
}

impl Awake {
    #[must_use]
    pub fn hold() -> Option<Self> {
        #[cfg(target_os = "macos")]
        let held = Some(native::Awake::hold());
        #[cfg(not(target_os = "macos"))]
        let held = native::Awake::hold();
        held.map(|held| Self { _held: held })
    }
}

impl fmt::Debug for Awake {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Awake").finish_non_exhaustive()
    }
}

#[cfg(target_os = "windows")]
use crate::windows::awake as native;

#[cfg(target_os = "macos")]
mod native {
    use objc2::{rc::Retained, runtime::ProtocolObject};
    use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};

    pub(super) struct Awake(Retained<ProtocolObject<dyn NSObjectProtocol>>);

    impl Awake {
        pub(super) fn hold() -> Self {
            Self(
                NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                    NSActivityOptions::IdleSystemSleepDisabled,
                    &NSString::from_str("Speakeasy is recording"),
                ),
            )
        }
    }

    impl Drop for Awake {
        fn drop(&mut self) {
            unsafe { NSProcessInfo::processInfo().endActivity(&self.0) };
        }
    }
}

#[cfg(target_os = "linux")]
mod native {
    use std::convert::Infallible;

    pub(super) struct Awake(Infallible);

    impl Awake {
        pub(super) const fn hold() -> Option<Self> {
            None
        }
    }
}
