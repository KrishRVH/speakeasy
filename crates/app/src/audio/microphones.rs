use anyhow::Context as _;
use cpal::traits::{DeviceTrait, HostTrait};
use speakeasy_platform::OwnedThread;
use tokio::sync::watch;

#[derive(Clone)]
pub(crate) struct InputDevice {
    pub id: String,
    pub name: String,
}

type Listing = anyhow::Result<Vec<InputDevice>>;
type Scan = Result<Vec<InputDevice>, cpal::Error>;

pub(crate) struct MicrophoneScan {
    devices: watch::Receiver<Option<Scan>>,
    thread: OwnedThread,
}

impl MicrophoneScan {
    pub(crate) fn listing(
        current: &mut Option<Self>,
    ) -> anyhow::Result<impl Future<Output = Listing> + use<>> {
        Self::share(current, enumerate_microphones)
    }

    fn share<F: FnOnce() -> Scan + Send + 'static>(
        current: &mut Option<Self>,
        enumerate: F,
    ) -> anyhow::Result<impl Future<Output = Listing> + use<F>> {
        let scan = match current {
            Some(scan) if !scan.thread.is_finished() => scan,
            idle => {
                let finished = idle.take();
                idle.insert(Self::spawn(finished, enumerate)?)
            },
        };
        let mut devices = scan.devices.clone();
        Ok(async move {
            let result = devices.wait_for(Option::is_some).await?;
            result
                .clone()
                .context("Microphone enumeration did not finish")?
                .map_err(Into::into)
        })
    }

    fn spawn(
        finished: Option<Self>,
        enumerate: impl FnOnce() -> Scan + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (reply, devices) = watch::channel(None);
        let thread = OwnedThread::spawn("microphone-scan", move || {
            drop(finished);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A scan with no remaining waiters has no result to deliver"
            )]
            let _ = reply.send(Some(enumerate()));
        })?;
        Ok(Self { devices, thread })
    }

    pub(crate) fn exited(&self) -> impl Future<Output = ()> + Send + use<> {
        self.thread.exited()
    }
}

fn enumerate_microphones() -> Scan {
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
    use std::sync::mpsc;

    use tokio::time::timeout;

    use super::*;
    use crate::testing::PATIENCE;

    #[tokio::test]
    async fn every_waiter_receives_the_same_in_flight_microphone_scan() -> anyhow::Result<()> {
        let (release, released) = mpsc::channel();
        let mut current = None;
        let first = MicrophoneScan::share(&mut current, move || {
            released.recv().expect("Fixture released");
            Ok(vec![InputDevice {
                id: "usb".into(),
                name: "USB".into(),
            }])
        })?;
        let second = MicrophoneScan::share(&mut current, || {
            Err(cpal::ErrorKind::DeviceNotAvailable.into())
        })?;
        release.send(())?;
        assert_eq!(timeout(PATIENCE, first).await??[0].id, "usb");
        assert_eq!(timeout(PATIENCE, second).await??[0].id, "usb");
        Ok(())
    }

    #[tokio::test]
    async fn a_blocked_microphone_scan_serves_the_next_request_instead_of_another_thread()
    -> anyhow::Result<()> {
        let (release, released) = mpsc::channel();
        let mut current = None;
        let abandoned = MicrophoneScan::share(&mut current, move || {
            released.recv().expect("Fixture released");
            Ok(vec![InputDevice {
                id: "usb".into(),
                name: "USB".into(),
            }])
        })?;

        drop(abandoned);
        let waiting = MicrophoneScan::share(&mut current, || {
            Err(cpal::ErrorKind::DeviceNotAvailable.into())
        })?;
        let exited = current.as_ref().context("No scan is held")?.exited();
        release.send(())?;
        let devices = timeout(PATIENCE, waiting).await??;
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "usb");
        timeout(PATIENCE, exited).await?;
        let fresh = MicrophoneScan::share(&mut current, || Ok(Vec::new()))?;
        assert!(timeout(PATIENCE, fresh).await??.is_empty());
        Ok(())
    }
}
