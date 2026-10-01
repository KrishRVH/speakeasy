//! Compiles the vendored GPUI Windows timing module so its unit tests run on every host, without
//! native APIs.

#[path = "../../../vendor/gpui/src/platform/windows/vsync/interval.rs"]
mod interval;
