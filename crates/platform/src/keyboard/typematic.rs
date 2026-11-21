use std::time::Duration;

#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SPI_GETFILTERKEYS, SPI_GETKEYBOARDDELAY, SPI_GETKEYBOARDSPEED, SystemParametersInfoW,
};

const FILTER_KEYS_LONGEST: Duration = Duration::from_secs(20);

const SLACK: Duration = Duration::from_millis(250);

const LONGEST_DELAY: u64 = 1_000;

const DELAYS: [u64; 4] = [250, 500, 750, LONGEST_DELAY];

const SLOWEST_PERIOD: u64 = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Repeat {
    After { delay: Duration, period: Duration },
    Never,
}

#[derive(Clone, Copy)]
pub(super) struct FilterKeys {
    pub(super) on: bool,
    pub(super) wait: u32,
    pub(super) delay: u32,
    pub(super) repeat: u32,
    pub(super) bounce: u32,
}

impl Repeat {
    pub(super) fn from_settings(
        delay: Option<u32>,
        speed: Option<u32>,
        filter: Option<FilterKeys>,
    ) -> Self {
        let Some(filter) = filter else {
            return Self::After {
                delay: FILTER_KEYS_LONGEST,
                period: FILTER_KEYS_LONGEST,
            };
        };
        if filter.on && filter.bounce == 0 {
            if filter.delay == 0 || filter.repeat == 0 {
                return Self::Never;
            }
            return Self::After {
                delay: Duration::from_millis(u64::from(filter.wait))
                    .saturating_add(Duration::from_millis(u64::from(filter.delay))),
                period: Duration::from_millis(u64::from(filter.repeat)),
            };
        }
        let delay = delay
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| DELAYS.get(index).copied())
            .unwrap_or(LONGEST_DELAY);
        let period = speed
            .and_then(|speed| {
                let repeats_per_62_s = u64::from(speed.min(31))
                    .saturating_mul(55)
                    .saturating_add(155);
                62_000_u64.checked_div(repeats_per_62_s)
            })
            .unwrap_or(SLOWEST_PERIOD);
        Self::After {
            delay: Duration::from_millis(delay),
            period: Duration::from_millis(period),
        }
    }

    pub(super) fn quiet_limit(self) -> Option<Duration> {
        match self {
            Self::After { delay, period } => {
                Some(delay.max(period).saturating_mul(2).saturating_add(SLACK))
            },
            Self::Never => None,
        }
    }
}

#[cfg(all(target_os = "windows", not(test)))]
pub(super) fn read() -> Repeat {
    let (delay, speed, filter) = settings();
    Repeat::from_settings(delay, speed, filter)
}

#[cfg(target_os = "windows")]
#[repr(C)]
struct NativeFilterKeys {
    size: u32,
    flags: u32,
    wait: u32,
    delay: u32,
    repeat: u32,
    bounce: u32,
}

#[cfg(target_os = "windows")]
const FILTER_KEYS_ON: u32 = 1;

#[cfg(target_os = "windows")]
fn settings() -> (Option<u32>, Option<u32>, Option<FilterKeys>) {
    let mut delay = 0_i32;
    let mut speed = 0_u32;
    let size = u32::try_from(size_of::<NativeFilterKeys>()).unwrap_or_default();
    let mut filter = NativeFilterKeys {
        size,
        flags: 0,
        wait: 0,
        delay: 0,
        repeat: 0,
        bounce: 0,
    };
    let [read_delay, read_speed, read_filter] = unsafe {
        [
            SystemParametersInfoW(SPI_GETKEYBOARDDELAY, 0, (&raw mut delay).cast(), 0),
            SystemParametersInfoW(SPI_GETKEYBOARDSPEED, 0, (&raw mut speed).cast(), 0),
            SystemParametersInfoW(SPI_GETFILTERKEYS, size, (&raw mut filter).cast(), 0),
        ]
    }
    .map(|read| read != 0);
    (
        read_delay
            .then_some(delay)
            .and_then(|delay| u32::try_from(delay).ok()),
        read_speed.then_some(speed),
        read_filter.then_some(FilterKeys {
            on: filter.flags & FILTER_KEYS_ON != 0,
            wait: filter.wait,
            delay: filter.delay,
            repeat: filter.repeat,
            bounce: filter.bounce,
        }),
    )
}

#[cfg(test)]
mod tests;
