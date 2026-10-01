//! Regression tests for the vendored GPUI Windows timing arithmetic.

// Exercise the production Windows timing arithmetic without invoking native APIs.
#[path = "../../../vendor/gpui/src/platform/windows/vsync/interval.rs"]
mod interval;
