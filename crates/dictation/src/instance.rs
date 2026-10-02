//! The configuration-directory lock, and a loopback listener for later launches' requests.
//!
//! Later launches send fixed reveal, toggle, and cancel requests. The listener port is published in
//! a file beside the lock, as every earlier release does, and callers verify it through an
//! identification handshake, so any edition can reveal the instance that owns the directory.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, bail, ensure};
use async_channel::{Receiver, Sender};
use speakeasy_platform::OwnedThread;

const LOCK_FILE: &str = "instance.lock";
const PORT_FILE: &str = "instance.port";
/// Bounds the read of a damaged port file; a decimal port needs at most five bytes.
const MAX_PORT_FILE_BYTES: u64 = 16;
const IDENTIFICATION: &[u8] = b"SPEAKEASY";
const IO_TIMEOUT: Duration = Duration::from_millis(100);
const REVEAL_ATTEMPTS: u32 = 25;
const REVEAL_RETRY: Duration = Duration::from_millis(20);

/// A fixed command a later launch sends to the running instance; never text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Request {
    /// Shows Settings.
    Reveal = 0,
    /// Starts or finishes hands-free dictation.
    Toggle = 1,
    /// Discards the active recording.
    Cancel = 2,
}

impl Request {
    const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Reveal),
            1 => Some(Self::Toggle),
            2 => Some(Self::Cancel),
            _ => None,
        }
    }
}

/// Owns the configuration-directory lock and a loopback server sleeping in accept. Dropping it
/// wakes and joins the server, then removes the port file, and only then releases the lock.
pub struct Instance {
    requests: Sender<Request>,
    address: SocketAddr,
    // Declaration order is release order: a relaunch that took the lock first would have its new
    // port file deleted by this one's `PortFile`.
    _server: OwnedThread,
    _port_file: PortFile,
    _lock: File,
}

impl Instance {
    /// Takes the single-instance lock, or returns `None` after asking its holder to show Settings.
    ///
    /// # Errors
    /// Returns an error if the settings directory, lock, or listener cannot be prepared.
    pub fn acquire(path: &Path) -> anyhow::Result<Option<(Self, Receiver<Request>)>> {
        let directory = path.parent().context("Settings path has no directory")?;
        fs::create_dir_all(directory)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join(LOCK_FILE))?;
        match lock.try_lock() {
            Ok(()) => {},
            Err(fs::TryLockError::WouldBlock) => return reveal_running(path).map(|()| None),
            Err(error) => return Err(error.into()),
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let port_file = directory.join(PORT_FILE);
        fs::write(&port_file, address.port().to_string())?;
        let (requests, received) = async_channel::bounded(1);
        let server = OwnedThread::spawn("instance-server", {
            let requests = requests.clone();
            move || serve(&listener, &requests)
        })?;
        let instance = Self {
            requests,
            address,
            _server: server,
            _port_file: PortFile(port_file),
            _lock: lock,
        };
        Ok(Some((instance, received)))
    }

    /// Sends `request` to the running instance once it has identified itself.
    ///
    /// # Errors
    /// Returns an error if no verified instance answers.
    pub fn send(path: &Path, request: Request) -> anyhow::Result<()> {
        let mut stream = connect(&path.with_file_name(PORT_FILE))?;
        stream.write_all(&[request as u8])?;
        Ok(())
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.requests.close();
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A server that has already exited may refuse the wake connection; its owned thread is still joined"
        )]
        let _ = TcpStream::connect_timeout(&self.address, IO_TIMEOUT);
    }
}

/// The published listener port, removed on drop.
struct PortFile(PathBuf);

impl Drop for PortFile {
    fn drop(&mut self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The released lock is authoritative; a stale port file cannot pass instance identification"
        )]
        let _ = fs::remove_file(&self.0);
    }
}

/// The running owner may still be publishing its port, so a reveal is retried briefly.
fn reveal_running(path: &Path) -> anyhow::Result<()> {
    for _ in 0..REVEAL_ATTEMPTS {
        if Instance::send(path, Request::Reveal).is_ok() {
            return Ok(());
        }
        thread::sleep(REVEAL_RETRY);
    }
    bail!("Speakeasy is already running. Open Settings from its tray icon.");
}

fn serve(listener: &TcpListener, requests: &Sender<Request>) {
    while let Ok((mut stream, _)) = listener.accept() {
        if requests.is_closed() {
            break;
        }
        let Ok(Some(request)) = handshake(&mut stream) else {
            continue;
        };
        // Blocking keeps Toggle and Cancel from coalescing while the UI is busy; closing the lane
        // at shutdown also wakes this send.
        if requests.send_blocking(request).is_err() {
            break;
        }
    }
}

fn handshake(stream: &mut TcpStream) -> io::Result<Option<Request>> {
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(IDENTIFICATION)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    let mut request = [0];
    stream.read_exact(&mut request)?;
    let [byte] = request;
    Ok(Request::from_byte(byte))
}

fn connect(port_file: &Path) -> anyhow::Result<TcpStream> {
    let mut port = String::new();
    File::open(port_file)
        .context("Start Speakeasy before using a desktop shortcut")?
        .take(MAX_PORT_FILE_BYTES)
        .read_to_string(&mut port)?;
    let port: u16 = port.trim().parse()?;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut reply = [0; IDENTIFICATION.len()];
    stream.read_exact(&mut reply)?;
    ensure!(
        reply == IDENTIFICATION,
        "Speakeasy instance could not be verified"
    );
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    const PATIENCE: Duration = Duration::from_secs(2);

    fn next_request(requests: &Receiver<Request>) -> anyhow::Result<Request> {
        let started = Instant::now();
        loop {
            if let Ok(request) = requests.try_recv() {
                return Ok(request);
            }
            ensure!(
                started.elapsed() < PATIENCE,
                "Instance request was not delivered"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Queues Toggle and then Cancel while the UI drains nothing: Cancel's send must keep the
    /// server occupied, so a further caller receives no identification.
    fn occupy_server_with_pending_requests(instance: &Instance, path: &Path) -> anyhow::Result<()> {
        Instance::send(path, Request::Toggle)?;
        Instance::send(path, Request::Cancel)?;
        let mut next = TcpStream::connect_timeout(&instance.address, PATIENCE)?;
        next.set_read_timeout(Some(IO_TIMEOUT))?;
        assert!(next.read_exact(&mut [0; IDENTIFICATION.len()]).is_err());
        Ok(())
    }

    #[test]
    fn relaunch_requests_settings_and_shutdown_releases_the_lock() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let (instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
        for request in [Request::Reveal, Request::Toggle, Request::Cancel] {
            if request == Request::Reveal {
                assert!(Instance::acquire(&path)?.is_none());
            } else {
                Instance::send(&path, request)?;
            }
            assert_eq!(next_request(&requests)?, request);
        }
        drop(instance);
        assert!(requests.is_closed());
        assert!(!directory.path().join(PORT_FILE).exists());
        assert!(Instance::acquire(&path)?.is_some());
        Ok(())
    }

    #[test]
    fn pending_commands_wait_for_the_ui_instead_of_disappearing() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let (instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
        occupy_server_with_pending_requests(&instance, &path)?;
        for expected in [Request::Toggle, Request::Cancel] {
            assert_eq!(next_request(&requests)?, expected);
        }
        drop(instance);
        assert!(requests.is_closed());
        Ok(())
    }

    #[test]
    fn shutdown_wakes_a_server_waiting_for_the_ui() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let (instance, _requests) = Instance::acquire(&path)?.context("First owner missing")?;
        occupy_server_with_pending_requests(&instance, &path)?;
        let (finished, completion) = std::sync::mpsc::channel();
        let cleanup = thread::spawn(move || {
            drop(instance);
            finished.send(()).unwrap();
        });
        completion.recv_timeout(PATIENCE)?;
        cleanup.join().unwrap();
        assert!(Instance::acquire(&path)?.is_some());
        Ok(())
    }
}
