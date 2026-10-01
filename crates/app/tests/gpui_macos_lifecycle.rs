//! Regression tests for the vendored GPUI display-link registry with fake native handles.

// Exercise the same production registry with fake CoreVideo and dispatch handles.
// Native GUI acceptance invokes the real dependency, which is not built cfg(test).
extern crate self as util;

trait ResultExt<T> {
    fn log_err(self) -> Option<T>;
}

impl<T, E> ResultExt<T> for Result<T, E> {
    fn log_err(self) -> Option<T> {
        self.ok()
    }
}

#[expect(
    unreachable_pub,
    reason = "This test compiles vendored GPUI with its upstream ownership and FFI conventions; first-party application code follows the workspace policy"
)]
#[expect(
    clippy::disallowed_types,
    reason = "This test compiles vendored GPUI with its upstream ownership and FFI conventions; first-party application code follows the workspace policy"
)]
#[expect(
    clippy::doc_markdown,
    reason = "This test compiles vendored GPUI with its upstream ownership and FFI conventions; first-party application code follows the workspace policy"
)]
#[expect(
    clippy::unused_self,
    reason = "This test compiles vendored GPUI with its upstream ownership and FFI conventions; first-party application code follows the workspace policy"
)]
#[expect(
    clippy::map_err_ignore,
    reason = "This test compiles vendored GPUI with its upstream ownership and FFI conventions; first-party application code follows the workspace policy"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "This test compiles vendored GPUI with its upstream ownership and FFI conventions; first-party application code follows the workspace policy"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "This test compiles vendored GPUI with its upstream ownership and FFI conventions; first-party application code follows the workspace policy"
)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "This test compiles vendored GPUI with its upstream ownership and FFI conventions; first-party application code follows the workspace policy"
)]
#[expect(
    clippy::ptr_cast_constness,
    reason = "This test compiles vendored GPUI with its upstream ownership and FFI conventions; first-party application code follows the workspace policy"
)]
#[expect(
    clippy::used_underscore_items,
    reason = "This test compiles vendored GPUI with its upstream ownership and FFI conventions; first-party application code follows the workspace policy"
)]
#[path = "../../../vendor/gpui/src/platform/mac/display_link.rs"]
mod display_link;
