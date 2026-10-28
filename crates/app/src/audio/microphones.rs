//! The microphones Settings offers, listed on a thread each scan owns.

use async_channel::Receiver;
use cpal::traits::{DeviceTrait, HostTrait};
use speakeasy_platform::OwnedThread;

pub(crate) struct InputDevice {
    /// The stable device identifier saved in settings.
    pub id: String,
    /// For display only; device names need not be unique.
    pub name: String,
}

/// The input devices one scan found, or why the driver could not list them.
type Listing = anyhow::Result<Vec<InputDevice>>;

/// A listing of the input devices, made without opening a stream. Drivers may block, and CPAL
/// leaves its calling thread in a single-threaded COM apartment, so enumeration runs on a thread
/// the scan owns; dropping the scan joins it.
pub(crate) struct MicrophoneScan {
    devices: Receiver<Listing>,
    thread: OwnedThread,
}

impl MicrophoneScan {
    /// The listing from the scan in `current` while it enumerates, or else from a new scan that
    /// replaces it, so a driver that blocks holds one thread however often Settings asks. One
    /// waiter receives each listing.
    pub(crate) fn listing(
        current: &mut Option<Self>,
    ) -> anyhow::Result<impl Future<Output = Listing> + use<>> {
        let devices = Self::share(current, enumerate_microphones)?;
        Ok(async move { devices.recv().await? })
    }

    fn share(
        current: &mut Option<Self>,
        enumerate: impl FnOnce() -> Listing + Send + 'static,
    ) -> anyhow::Result<Receiver<Listing>> {
        let scan = match current {
            Some(scan) if !scan.thread.is_finished() => scan,
            idle => {
                let finished = idle.take();
                idle.insert(Self::spawn(finished, enumerate)?)
            },
        };
        Ok(scan.devices.clone())
    }

    /// Starts a scan that first joins `finished`, the scan it replaces: that thread's enumeration
    /// has returned, but its COM apartment may still be tearing down, which the caller's thread
    /// must not wait for.
    fn spawn(
        finished: Option<Self>,
        enumerate: impl FnOnce() -> Listing + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (reply, devices) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("microphone-scan", move || {
            drop(finished);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Only a dropped scan closes the receiver, and then no Settings view waits for the list"
            )]
            let _ = reply.try_send(enumerate());
        })?;
        Ok(Self { devices, thread })
    }

    /// Resolves once enumeration has returned.
    pub(crate) fn exited(&self) -> impl Future<Output = ()> + Send + use<> {
        self.thread.exited()
    }
}

fn enumerate_microphones() -> Listing {
    // A device that cannot report its id or name cannot be chosen, so it drops out of the list
    // rather than hiding every other microphone.
    Ok(cpal::default_host()
        .input_devices()?
        .filter_map(|device| {
            Some(InputDevice {
                id: device.id().ok()?.to_string(),
                name: device.description().ok()?.name().to_owned(),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use anyhow::{Context, bail};
    use tokio::time::timeout;

    use super::*;

    const PATIENCE: Duration = Duration::from_secs(2);

    #[tokio::test]
    async fn a_blocked_microphone_scan_serves_the_next_request_instead_of_another_thread()
    -> anyhow::Result<()> {
        let (release, released) = mpsc::channel();
        let mut current = None;
        let abandoned = MicrophoneScan::share(&mut current, move || {
            released.recv()?;
            Ok(vec![InputDevice {
                id: "usb".into(),
                name: "USB".into(),
            }])
        })?;
        // The Settings view that asked first is gone while the driver still blocks. The driver
        // returns later, so a request that replaced the scan would join it, then fail, never hang.
        drop(abandoned);
        let driver = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            release.send(())
        });
        let waiting = MicrophoneScan::share(&mut current, || {
            bail!("A second scan started while one was enumerating")
        })?;
        let exited = current.as_ref().context("No scan is held")?.exited();
        let devices = timeout(PATIENCE, waiting.recv()).await???;
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "usb");
        timeout(PATIENCE, exited).await?;
        driver.join().unwrap()?;
        let fresh = MicrophoneScan::share(&mut current, || Ok(Vec::new()))?;
        assert!(timeout(PATIENCE, fresh.recv()).await???.is_empty());
        Ok(())
    }
}
