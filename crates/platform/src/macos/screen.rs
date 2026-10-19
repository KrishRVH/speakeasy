//! Screen capture, pointer sampling, and the Show overlay on macOS.

use std::{convert::Infallible, time::Duration};

use anyhow::bail;
use async_channel::{Receiver, Sender};
use raw_window_handle::RawWindowHandle;

use crate::screen::{PointerSample, ScreenEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Display;

pub(crate) fn display_under_pointer() -> anyhow::Result<crate::screen::Display> {
    bail!("Screen capture is not available on macOS yet")
}

pub(crate) fn capture(
    _: crate::screen::Display,
    _: Duration,
    _: u32,
    _: &Sender<ScreenEvent>,
    _: &Receiver<Infallible>,
) {
}

pub(crate) const fn pointer(_: Display) -> Option<PointerSample> {
    None
}

pub(crate) fn configure_overlay(_: RawWindowHandle, _: Display) -> anyhow::Result<()> {
    bail!("The Show overlay is not available on macOS yet")
}

pub(crate) const fn set_overlay_visible(_: RawWindowHandle, _: bool) {}

#[expect(
    clippy::unnecessary_wraps,
    reason = "A placeholder until capture exclusion, which can fail, is implemented"
)]
pub(crate) const fn set_excluded_from_capture(_: RawWindowHandle, _: bool) -> anyhow::Result<()> {
    Ok(())
}
