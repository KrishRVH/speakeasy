use anyhow::Context;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Reveal,
    Toggle,
    Cancel,
}

/// Owns the configuration-directory lock and a sleeping, loopback-only reopen
/// listener. Only fixed reveal/toggle/cancel requests cross it; never text.
pub(crate) struct Instance {
    _lock: File,
    endpoint: PathBuf,
    address: SocketAddr,
    requests: async_channel::Sender<Request>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Instance {
    pub(crate) fn acquire(
        path: &Path,
    ) -> anyhow::Result<Option<(Self, async_channel::Receiver<Request>)>> {
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
            Ok(()) => {},
            Err(std::fs::TryLockError::WouldBlock) => {
                // The owner may still be publishing its port during startup.
                for _ in 0..25 {
                    // Windows file locks also deny reads. Publish rendezvous
                    // metadata separately and confirm it belongs to Speakeasy.
                    if Self::command(path, Request::Reveal).is_ok() {
                        return Ok(None);
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                anyhow::bail!("Speakeasy is already running. Open Settings from its tray icon.");
            },
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
                    if stream
                        .set_read_timeout(Some(Duration::from_millis(100)))
                        .is_err()
                    {
                        continue;
                    }
                    let mut command = [0];
                    let request = match stream.read_exact(&mut command) {
                        Ok(()) => match command[0] {
                            0 => Request::Reveal,
                            1 => Request::Toggle,
                            2 => Request::Cancel,
                            _ => continue,
                        },
                        Err(_) => continue,
                    };
                    // This dedicated listener may wait for the UI. Toggle and
                    // Cancel cannot coalesce; closing the lane also wakes it.
                    if sender.send_blocking(request).is_err() {
                        break;
                    }
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
    pub(crate) fn command(path: &Path, command: Request) -> anyhow::Result<()> {
        let endpoint = path.with_file_name("instance.port");
        let mut stream = Self::connect(&endpoint)?;
        stream.write_all(&[match command {
            Request::Reveal => 0,
            Request::Toggle => 1,
            Request::Cancel => 2,
        }])?;
        Ok(())
    }

    fn connect(endpoint: &Path) -> anyhow::Result<TcpStream> {
        let mut port = String::new();
        File::open(endpoint)
            .context("Start Speakeasy before using a desktop shortcut")?
            .take(16)
            .read_to_string(&mut port)?;
        let port: u16 = port.trim().parse()?;
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(100))?;
        stream.set_read_timeout(Some(Duration::from_millis(100)))?;
        stream.set_write_timeout(Some(Duration::from_millis(100)))?;
        let mut reply = [0; 9];
        stream.read_exact(&mut reply)?;
        anyhow::ensure!(
            &reply == b"SPEAKEASY",
            "Speakeasy instance could not be verified"
        );
        Ok(stream)
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.requests.close();
        // Wake accept before joining; the lock stays held until cleanup ends.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A listener that has already exited may reject the wake connection; its owned thread is still joined"
        )]
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        if let Some(thread) = self.thread.take() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Drop cannot report a listener panic; joining keeps the configuration lock owned until exit"
            )]
            let _ = thread.join();
        }
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The released lock is authoritative; a stale port file cannot pass instance identification"
        )]
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
        for command in [Request::Reveal, Request::Toggle, Request::Cancel] {
            if command == Request::Reveal {
                assert!(Instance::acquire(&path)?.is_none());
            } else {
                Instance::command(&path, command)?;
            }
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if let Ok(received) = requests.try_recv() {
                    assert_eq!(received, command);
                    break;
                }
                anyhow::ensure!(
                    std::time::Instant::now() < deadline,
                    "Instance command was not delivered"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
        drop(instance);
        assert!(requests.is_closed());
        assert!(Instance::acquire(&path)?.is_some());
        Ok(())
    }

    #[test]
    fn pending_commands_wait_for_the_ui_instead_of_disappearing() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let (instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
        Instance::command(&path, Request::Toggle)?;
        Instance::command(&path, Request::Cancel)?;

        // The second handshake follows enqueueing Toggle. With the UI paused,
        // Cancel must retain the listener until that occupied slot is drained.
        let mut next = TcpStream::connect_timeout(&instance.address, Duration::from_secs(2))?;
        next.set_read_timeout(Some(Duration::from_millis(100)))?;
        assert!(next.read_exact(&mut [0; 9]).is_err());
        drop(next);

        for expected in [Request::Toggle, Request::Cancel] {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if let Ok(received) = requests.try_recv() {
                    assert_eq!(received, expected);
                    break;
                }
                anyhow::ensure!(
                    std::time::Instant::now() < deadline,
                    "Pending instance command was not delivered"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
        drop(instance);
        assert!(requests.is_closed());
        Ok(())
    }

    #[test]
    fn shutdown_wakes_a_listener_waiting_for_the_ui() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let (instance, _requests) = Instance::acquire(&path)?.context("First owner missing")?;
        Instance::command(&path, Request::Toggle)?;
        Instance::command(&path, Request::Cancel)?;
        let mut next = TcpStream::connect_timeout(&instance.address, Duration::from_secs(2))?;
        next.set_read_timeout(Some(Duration::from_millis(100)))?;
        assert!(next.read_exact(&mut [0; 9]).is_err());
        drop(next);

        let (finished, completion) = std::sync::mpsc::channel();
        let cleanup = thread::spawn(move || {
            drop(instance);
            finished.send(()).unwrap();
        });
        completion.recv_timeout(Duration::from_secs(2))?;
        cleanup.join().unwrap();
        assert!(Instance::acquire(&path)?.is_some());
        Ok(())
    }
}
