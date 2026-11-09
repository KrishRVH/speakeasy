use async_channel::Receiver;
use cpal::traits::{DeviceTrait, HostTrait};
use speakeasy_platform::OwnedThread;

pub(crate) struct InputDevice {
    pub id: String,
    pub name: String,
}

type Listing = anyhow::Result<Vec<InputDevice>>;

pub(crate) struct MicrophoneScan {
    devices: Receiver<Listing>,
    thread: OwnedThread,
}

impl MicrophoneScan {
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

    fn spawn(
        finished: Option<Self>,
        enumerate: impl FnOnce() -> Listing + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (reply, devices) = async_channel::bounded(1);
        let thread = OwnedThread::spawn("microphone-scan", move || {
            drop(finished);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The lane closes only once the scan and every waiting Settings view drop their receivers, so nobody wants the list"
            )]
            let _ = reply.try_send(enumerate());
        })?;
        Ok(Self { devices, thread })
    }

    pub(crate) fn exited(&self) -> impl Future<Output = ()> + Send + use<> {
        self.thread.exited()
    }
}

fn enumerate_microphones() -> Listing {
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

    use anyhow::{Context as _, bail};
    use tokio::time::timeout;

    use super::*;
    use crate::testing::PATIENCE;

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
