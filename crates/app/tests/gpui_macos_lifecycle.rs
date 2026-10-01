//! Regression tests for the vendored GPUI display-link registry. They exercise the production
//! registry with fake `CoreVideo` and dispatch handles; native GUI acceptance covers the real
//! dependency, which is not built with `cfg(test)`.

// The vendored module imports `util::ResultExt`; this test crate stands in for `util`.
extern crate self as util;

trait ResultExt<T> {
    fn log_err(self) -> Option<T>;
}

impl<T, E> ResultExt<T> for Result<T, E> {
    fn log_err(self) -> Option<T> {
        self.ok()
    }
}

#[expect(unreachable_pub, reason = "Vendored upstream code")]
#[expect(clippy::disallowed_types, reason = "Vendored upstream code")]
#[expect(clippy::doc_markdown, reason = "Vendored upstream code")]
#[expect(clippy::unused_self, reason = "Vendored upstream code")]
#[expect(clippy::map_err_ignore, reason = "Vendored upstream code")]
#[expect(clippy::significant_drop_tightening, reason = "Vendored upstream code")]
#[expect(clippy::cast_possible_truncation, reason = "Vendored upstream code")]
#[expect(clippy::arithmetic_side_effects, reason = "Vendored upstream code")]
#[expect(clippy::ptr_cast_constness, reason = "Vendored upstream code")]
#[expect(clippy::used_underscore_items, reason = "Vendored upstream code")]
#[path = "../../../vendor/gpui/src/platform/mac/display_link.rs"]
mod display_link;
