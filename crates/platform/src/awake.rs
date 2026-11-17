use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    System,
    Display,
}

pub struct Awake {
    _held: native::Awake,
}

impl Awake {
    #[must_use]
    pub fn hold(keep: Keep) -> Option<Self> {
        #[cfg(target_os = "macos")]
        let held = Some(native::Awake::hold(keep));
        #[cfg(not(target_os = "macos"))]
        let held = native::Awake::hold(keep);
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

    use super::Keep;

    pub(super) struct Awake(Retained<ProtocolObject<dyn NSObjectProtocol>>);

    #[expect(
        clippy::non_send_fields_in_send_ty,
        reason = "The activity token is only retained, released, and handed back to the thread-safe NSProcessInfo"
    )]
    unsafe impl Send for Awake {}

    impl Awake {
        pub(super) fn hold(keep: Keep) -> Self {
            let options = match keep {
                Keep::System => NSActivityOptions::IdleSystemSleepDisabled,
                Keep::Display => {
                    NSActivityOptions::IdleSystemSleepDisabled
                        | NSActivityOptions::IdleDisplaySleepDisabled
                },
            };
            Self(
                NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                    options,
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

    use super::Keep;

    pub(super) struct Awake(Infallible);

    impl Awake {
        pub(super) const fn hold(_: Keep) -> Option<Self> {
            None
        }
    }
}
