use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use util::ResultExt;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{DWM_TIMING_INFO, DwmFlush, DwmGetCompositionTimingInfo},
    System::Performance::QueryPerformanceFrequency,
};

mod interval;

use interval::{VSYNC_INTERVAL_THRESHOLD, select_interval};

static QPC_TICKS_PER_SECOND: LazyLock<u64> = LazyLock::new(|| {
    let mut frequency = 0;
    // On systems that run Windows XP or later, the function will always succeed and
    // will thus never return zero.
    unsafe { QueryPerformanceFrequency(&mut frequency).unwrap() };
    frequency as u64
});

const DEFAULT_VSYNC_INTERVAL: Duration = Duration::from_micros(16_666); // ~60Hz

pub(crate) struct VSyncProvider {
    interval: Duration,
    f: Box<dyn Fn() -> bool>,
}

impl VSyncProvider {
    pub(crate) fn new() -> Self {
        let interval = get_dwm_interval()
            .context("Failed to get DWM interval")
            .log_err()
            .unwrap_or(DEFAULT_VSYNC_INTERVAL);
        let f = Box::new(|| unsafe { DwmFlush().is_ok() });
        Self { interval, f }
    }

    pub(crate) fn wait_for_vsync(&self) {
        let vsync_start = Instant::now();
        let wait_succeeded = (self.f)();
        let elapsed = vsync_start.elapsed();
        // DwmFlush and DCompositionWaitForCompositorClock returns very early
        // instead of waiting until vblank when the monitor goes to sleep or is
        // unplugged (nothing to present due to desktop occlusion). We use 1ms as
        // a threshold for the duration of the wait functions and fallback to
        // Sleep() if it returns before that. This could happen during normal
        // operation for the first call after the vsync thread becomes non-idle,
        // but it shouldn't happen often.
        if !wait_succeeded || elapsed < VSYNC_INTERVAL_THRESHOLD {
            log::trace!("VSyncProvider::wait_for_vsync() took less time than expected");
            std::thread::sleep(self.interval);
        }
    }
}

fn get_dwm_interval() -> Result<Duration> {
    let mut timing_info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut timing_info) }?;
    // Check for interval values that are impossibly low. A 29 microsecond
    // interval was seen (from a qpcRefreshPeriod of 60).
    select_interval(
        timing_info.qpcRefreshPeriod,
        *QPC_TICKS_PER_SECOND,
        timing_info.rateRefresh.uiNumerator,
        timing_info.rateRefresh.uiDenominator,
    )
    .context("DWM reported no valid refresh interval")
}
