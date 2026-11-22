use std::{
    convert::Infallible,
    panic::{self, AssertUnwindSafe},
    time::{Duration, Instant},
};

use async_channel::Sender;
use objc2::{
    AllocAnyThread as _, DefinedClass as _, define_class, msg_send,
    rc::{Retained, autoreleasepool},
};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{CMClock, CMSampleBuffer, CMTime, CMTimeFlags};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSString};
use objc2_screen_capture_kit::{
    SCFrameStatus, SCStream, SCStreamDelegate, SCStreamFrameInfoDisplayTime,
    SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType,
};

use super::super::{Failure, frame, refused};
use crate::{Frame, Part};

pub(super) struct State {
    frames: Sender<Frame>,
    failures: Sender<Failure>,
    stopped: Sender<Infallible>,
    part: Part,
    clock: Clock,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = State]
    pub(super) struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn sample(&self, _: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                autoreleasepool(|_| self.read(sample, kind))
            }));
            if let Err(failure) = result.unwrap_or(Err(Failure::Stalled)) {
                self.fail(failure);
            }
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        fn stopped(&self, _: &SCStream, error: &NSError) {
            self.ivars().stopped.close();
            let failure = panic::catch_unwind(AssertUnwindSafe(|| {
                if refused(error) {
                    Failure::Permission
                } else {
                    Failure::Stalled
                }
            }))
            .unwrap_or(Failure::Stalled);
            self.fail(failure);
        }
    }
);

impl Output {
    pub(super) fn new(
        part: Part,
        frames: Sender<Frame>,
        failures: Sender<Failure>,
        stopped: Sender<Infallible>,
    ) -> Result<Retained<Self>, Failure> {
        let this = Self::alloc().set_ivars(State {
            frames,
            failures,
            stopped,
            part,
            clock: Clock::new()?,
        });

        Ok(unsafe { msg_send![super(this), init] })
    }

    fn read(&self, sample: &CMSampleBuffer, kind: SCStreamOutputType) -> Result<(), Failure> {
        let state = self.ivars();
        if kind != SCStreamOutputType::Screen || state.frames.is_closed() {
            return Ok(());
        }

        if !unsafe { sample.is_valid() && sample.data_is_ready() } {
            return Ok(());
        }

        let attachments = unsafe {
            let array = sample
                .sample_attachments_array(false)
                .ok_or(Failure::Stalled)?;
            CFRetained::cast_unchecked::<CFArray<CFDictionary<CFString, CFType>>>(array)
        };
        let info = attachments.get(0).ok_or(Failure::Stalled)?;

        let status = number(&info, unsafe { SCStreamFrameInfoStatus }).ok_or(Failure::Stalled)?;
        match SCFrameStatus(isize::try_from(status).map_err(|_status| Failure::Stalled)?) {
            SCFrameStatus::Idle | SCFrameStatus::Started => return Ok(()),
            SCFrameStatus::Complete => {},
            _ => return Err(Failure::Stalled),
        }
        if state.frames.is_full() {
            return Ok(());
        }

        let ticks = number(&info, unsafe { SCStreamFrameInfoDisplayTime })
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(Failure::Stalled)?;
        let Some(at) = state.clock.at(ticks, Instant::now()) else {
            return Ok(());
        };

        let frame = frame(sample, state.part, at).ok_or(Failure::Stalled)?;
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A full bounded lane drops a frame; a stopped owner no longer accepts pixels"
        )]
        let _ = state.frames.try_send(frame);
        Ok(())
    }

    fn fail(&self, failure: Failure) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The first terminal failure occupies its separate reserved lane; later failures cannot replace it"
        )]
        let _ = self.ivars().failures.try_send(failure);
    }
}

fn number(info: &CFDictionary<CFString, CFType>, key: &NSString) -> Option<i64> {
    let key = unsafe { &*std::ptr::from_ref(key).cast::<CFString>() };
    info.get(key)?.downcast::<CFNumber>().ok()?.as_i64()
}

struct Clock {
    origin: Duration,
    instant: Instant,
}

impl Clock {
    fn new() -> Result<Self, Failure> {
        let origin = host_time(unsafe { CMClock::host_time_clock().time() });
        Ok(Self {
            origin: origin.ok_or(Failure::Unstarted)?,
            instant: Instant::now(),
        })
    }

    fn at(&self, ticks: u64, now: Instant) -> Option<Instant> {
        let native = unsafe { CMClock::make_host_time_from_system_units(ticks) };
        let elapsed = host_time(native)?.checked_sub(self.origin)?;
        let at = self.instant.checked_add(elapsed)?;
        (at <= now).then_some(at)
    }
}

fn host_time(time: CMTime) -> Option<Duration> {
    if !time.flags.contains(CMTimeFlags::Valid)
        || time.flags.intersects(
            CMTimeFlags::PositiveInfinity | CMTimeFlags::NegativeInfinity | CMTimeFlags::Indefinite,
        )
        || time.epoch != 0
    {
        return None;
    }
    let value = u128::try_from(time.value).ok()?;
    let scale = u128::try_from(time.timescale).ok()?;
    let nanos = value.checked_mul(1_000_000_000)?.checked_div(scale)?;
    Some(Duration::from_nanos(u64::try_from(nanos).ok()?))
}

#[cfg(test)]
mod tests;
