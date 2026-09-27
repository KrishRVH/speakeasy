use anyhow::Context;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

/// Owns the configuration-directory lock and a sleeping, loopback-only reopen
/// listener. Connecting requests Settings; no audio, text or commands cross it.
pub struct Instance {
    _lock: File,
    endpoint: PathBuf,
    address: SocketAddr,
    requests: async_channel::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Instance {
    pub fn acquire(path: &Path) -> anyhow::Result<Option<(Self, async_channel::Receiver<()>)>> {
        let directory = path.parent().context("Settings path has no directory")?;
        std::fs::create_dir_all(directory)?;
        let lock_path = directory.join("instance.lock");
        let endpoint = directory.join("instance.port");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                // The owner may still be publishing its port during startup.
                for _ in 0..25 {
                    // Windows file locks also deny reads. Publish rendezvous
                    // metadata separately and confirm it belongs to Speakeasy.
                    let mut port = String::new();
                    if let Ok(file) = File::open(&endpoint) {
                        let _ = file.take(16).read_to_string(&mut port);
                    }
                    if let Ok(port) = port.trim().parse::<u16>() {
                        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
                        if let Ok(mut stream) =
                            TcpStream::connect_timeout(&address, Duration::from_millis(100))
                        {
                            stream.set_read_timeout(Some(Duration::from_millis(100)))?;
                            let mut reply = [0; 9];
                            if stream.read_exact(&mut reply).is_ok() && &reply == b"SPEAKEASY" {
                                return Ok(None);
                            }
                        }
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                anyhow::bail!("Speakeasy is already running. Open Settings from its tray icon.");
            }
            Err(error) => return Err(error.into()),
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        std::fs::write(&endpoint, address.port().to_string())?;
        let (requests, receiver) = async_channel::bounded(1);
        let sender = requests.clone();
        let thread = thread::Builder::new()
            .name("settings-reopen".into())
            .spawn(move || {
                while let Ok((mut stream, _)) = listener.accept() {
                    if sender.is_closed() {
                        break;
                    }
                    if stream
                        .set_write_timeout(Some(Duration::from_millis(100)))
                        .is_err()
                        || stream.write_all(b"SPEAKEASY").is_err()
                    {
                        continue;
                    }
                    let _ = sender.try_send(());
                }
            })?;
        Ok(Some((
            Self {
                _lock: lock,
                endpoint,
                address,
                requests,
                thread: Some(thread),
            },
            receiver,
        )))
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.requests.close();
        // Wake accept before joining; the lock stays held until cleanup ends.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.endpoint);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunch_requests_settings_and_shutdown_releases_the_lock() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let (instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
        assert!(Instance::acquire(&path)?.is_none());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while requests.try_recv().is_err() {
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "Reopen was not delivered"
            );
            thread::sleep(Duration::from_millis(5));
        }
        drop(instance);
        assert!(requests.is_closed());
        assert!(Instance::acquire(&path)?.is_some());
        Ok(())
    }
}
