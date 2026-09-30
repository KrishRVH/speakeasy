// Exercise the same production registry with fake CoreVideo and dispatch handles.
// Native GUI acceptance invokes the real dependency, which is not built cfg(test).
extern crate self as util;

pub trait ResultExt<T> {
    fn log_err(self) -> Option<T>;
}

impl<T, E> ResultExt<T> for Result<T, E> {
    fn log_err(self) -> Option<T> {
        self.ok()
    }
}

#[path = "../../../vendor/gpui/src/platform/mac/display_link.rs"]
mod display_link;
