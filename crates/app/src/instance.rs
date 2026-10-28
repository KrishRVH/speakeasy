//! The configuration-directory lock and a loopback listener through which later launches send fixed
//! reveal, toggle, and cancel requests. Windows file locks also deny reads, so the listener's port
//! and a per-launch secret are published in a separate owner-only file, and the listener answers
//! only callers that present the secret.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write as _},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail, ensure};
use async_channel::{Receiver, Sender};
use speakeasy_platform::OwnedThread;

const LOCK_FILE: &str = "instance.lock";
const PORT_FILE: &str = "instance.port";
/// Bounds the read of a damaged port file; a port and its secret need at most 38 bytes.
const MAX_PORT_FILE_BYTES: u64 = 64;
/// The listener's reply to a caller that presented the secret.
const ACKNOWLEDGED: u8 = 0x06;
const NOT_RUNNING: &str = "Speakeasy is not running. Start it before using a desktop shortcut";
/// A port file this version cannot parse comes from an older version or is damaged; a restart
/// rewrites it.
const OUTDATED: &str = "Restart Speakeasy to use desktop shortcuts";
const IO_TIMEOUT: Duration = Duration::from_millis(100);
const REVEAL_ATTEMPTS: u32 = 25;
const REVEAL_RETRY: Duration = Duration::from_millis(20);
/// The pause after a failed accept: well within the `IO_TIMEOUT` a caller waits for its
/// acknowledgement, yet long enough that a lasting failure, such as running out of file
/// descriptors, cannot spin a core.
const ACCEPT_RETRY: Duration = Duration::from_millis(10);

/// A fixed command a later launch sends to the running instance; never text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Request {
    Reveal = 0,
    Toggle = 1,
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
pub(crate) struct Instance {
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
    pub(crate) fn acquire(path: &Path) -> anyhow::Result<Option<(Self, Receiver<Request>)>> {
        let directory = path.parent().context("Settings path has no directory")?;
        fs::create_dir_all(directory)
            .with_context(|| format!("Cannot create {}", directory.display()))?;
        let lock_file = directory.join(LOCK_FILE);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_file)
            .with_context(|| format!("Cannot open {}", lock_file.display()))?;
        match lock.try_lock() {
            Ok(()) => {},
            Err(fs::TryLockError::WouldBlock) => return reveal_running(path).map(|()| None),
            // The I/O error itself, since `TryLockError` neither displays nor sources it.
            Err(fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("Cannot lock {}", lock_file.display()));
            },
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let (Endpoint { nonce, .. }, port_file) = Endpoint::publish(address.port(), directory)?;
        let accept = move || listener.accept().map(|(stream, _)| stream);
        let (requests, received) = async_channel::bounded(1);
        let server = OwnedThread::spawn("instance-server", {
            let requests = requests.clone();
            move || serve(accept, nonce, &requests)
        })?;
        let instance = Self {
            requests,
            address,
            _server: server,
            _port_file: port_file,
            _lock: lock,
        };
        Ok(Some((instance, received)))
    }

    /// Sends `request` to the running instance once it has acknowledged the secret.
    pub(crate) fn send(path: &Path, request: Request) -> anyhow::Result<()> {
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

/// Where the running instance listens, and the per-launch secret a caller presents to prove it can
/// read the owner-only port file.
struct Endpoint {
    port: u16,
    nonce: u128,
}

impl Endpoint {
    /// Mints a secret for `port` and publishes both in `directory`. The file is created owner-only
    /// on Unix and inherits the per-user profile's ACL on Windows, then atomically replaces any
    /// older one.
    fn publish(port: u16, directory: &Path) -> anyhow::Result<(Self, PortFile)> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes)?;
        let nonce = u128::from_le_bytes(bytes);
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        write!(file, "{port} {nonce:032x}")?;
        let path = directory.join(PORT_FILE);
        file.persist(&path).map_err(|error| error.error)?;
        Ok((Self { port, nonce }, PortFile(path)))
    }

    /// Reads a published endpoint. Errors never quote the file.
    fn read(port_file: &Path) -> anyhow::Result<Self> {
        let mut text = String::new();
        File::open(port_file)
            .context(NOT_RUNNING)?
            .take(MAX_PORT_FILE_BYTES)
            .read_to_string(&mut text)
            .context(OUTDATED)?;
        text.trim()
            .split_once(' ')
            .and_then(|(port, nonce)| {
                Some(Self {
                    port: port.parse().ok()?,
                    nonce: u128::from_str_radix(nonce, 16).ok()?,
                })
            })
            .context(OUTDATED)
    }
}

/// The published endpoint, removed on drop.
struct PortFile(PathBuf);

impl Drop for PortFile {
    fn drop(&mut self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The released lock is authoritative; a stale port file carries a dead secret"
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
    let icon = if cfg!(target_os = "macos") {
        "menu bar"
    } else {
        "tray"
    };
    bail!("Speakeasy is already running. Open Settings from its {icon} icon.");
}

/// Answers callers until the owner closes `requests`. A failed accept is retried after a pause; the
/// lane is checked after every accept, so shutdown still ends a server whose accepts keep failing.
fn serve(
    mut accept: impl FnMut() -> io::Result<TcpStream>,
    nonce: u128,
    requests: &Sender<Request>,
) {
    loop {
        let accepted = accept();
        if requests.is_closed() {
            break;
        }
        let Ok(mut stream) = accepted else {
            thread::sleep(ACCEPT_RETRY);
            continue;
        };
        let Ok(Some(request)) = handshake(&mut stream, nonce) else {
            continue;
        };
        // Blocking keeps Toggle and Cancel from coalescing while the UI is busy; closing the lane
        // at shutdown also wakes this send.
        if requests.send_blocking(request).is_err() {
            break;
        }
    }
}

/// Says nothing until the caller presents `nonce`, then acknowledges and reads one request. A caller
/// that gives up before the acknowledgement has written no request, so nothing is delivered.
fn handshake(stream: &mut TcpStream, nonce: u128) -> io::Result<Option<Request>> {
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut caller = Caller {
        stream,
        accepted: Instant::now(),
    };
    let mut presented = [0; size_of::<u128>()];
    caller.read_exact(&mut presented)?;
    if u128::from_le_bytes(presented) != nonce {
        return Ok(None);
    }
    caller.stream.write_all(&[ACKNOWLEDGED])?;
    let mut request = [0];
    caller.read_exact(&mut request)?;
    let [byte] = request;
    Ok(Request::from_byte(byte))
}

/// A connection whose reads share one `IO_TIMEOUT`. A read timeout alone restarts with every byte,
/// so a caller that trickles bytes could hold the serial accept loop once per byte.
struct Caller<'stream> {
    stream: &'stream mut TcpStream,
    accepted: Instant,
}

impl Read for Caller<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = IO_TIMEOUT
            .checked_sub(self.accepted.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(io::ErrorKind::TimedOut)?;
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(buffer)
    }
}

fn connect(port_file: &Path) -> anyhow::Result<TcpStream> {
    let endpoint = Endpoint::read(port_file)?;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT).context(NOT_RUNNING)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(&endpoint.nonce.to_le_bytes())?;
    let mut reply = [0];
    stream
        .read_exact(&mut reply)
        .context("Speakeasy did not answer; try the shortcut again")?;
    ensure!(
        reply == [ACKNOWLEDGED],
        "Speakeasy instance could not be verified"
    );
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::PATIENCE;

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
    /// server occupied, so a further caller presenting the secret is not acknowledged.
    fn occupy_server_with_pending_requests(instance: &Instance, path: &Path) -> anyhow::Result<()> {
        Instance::send(path, Request::Toggle)?;
        Instance::send(path, Request::Cancel)?;
        let endpoint = Endpoint::read(&path.with_file_name(PORT_FILE))?;
        let mut next = TcpStream::connect_timeout(&instance.address, PATIENCE)?;
        next.set_read_timeout(Some(IO_TIMEOUT))?;
        next.write_all(&endpoint.nonce.to_le_bytes())?;
        assert!(next.read_exact(&mut [0]).is_err());
        Ok(())
    }

    #[test]
    fn relaunch_requests_settings_and_shutdown_releases_the_lock() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let (instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
        assert!(Instance::acquire(&path)?.is_none());
        assert_eq!(next_request(&requests)?, Request::Reveal);
        for request in [Request::Toggle, Request::Cancel] {
            Instance::send(&path, request)?;
            assert_eq!(next_request(&requests)?, request);
        }
        drop(instance);
        assert!(requests.is_closed());
        assert!(!directory.path().join(PORT_FILE).exists());
        assert!(Instance::acquire(&path)?.is_some());
        Ok(())
    }

    #[test]
    fn requests_without_the_secret_are_never_delivered() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let (instance, requests) = Instance::acquire(&path)?.context("First owner missing")?;
        let endpoint = Endpoint::read(&path.with_file_name(PORT_FILE))?;
        let mut forged = TcpStream::connect_timeout(&instance.address, PATIENCE)?;
        forged.set_read_timeout(Some(PATIENCE))?;
        forged.write_all(&(endpoint.nonce ^ 1).to_le_bytes())?;
        forged.write_all(&[Request::Cancel as u8])?;
        assert!(
            forged.read_exact(&mut [0]).is_err(),
            "A wrong secret was acknowledged"
        );
        Instance::send(&path, Request::Toggle)?;
        assert_eq!(next_request(&requests)?, Request::Toggle);
        Ok(())
    }

    #[test]
    fn a_secret_trickled_past_the_timeout_is_never_acknowledged() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let (instance, _requests) = Instance::acquire(&path)?.context("First owner missing")?;
        let endpoint = Endpoint::read(&path.with_file_name(PORT_FILE))?;
        let mut slow = TcpStream::connect_timeout(&instance.address, PATIENCE)?;
        slow.set_nodelay(true)?;
        // Each byte arrives well within IO_TIMEOUT of the last; the whole secret does not.
        for byte in endpoint.nonce.to_le_bytes() {
            if slow.write_all(&[byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        slow.set_read_timeout(Some(PATIENCE))?;
        assert!(
            slow.read_exact(&mut [0]).is_err(),
            "A trickled secret held the server past its timeout"
        );
        Ok(())
    }

    #[test]
    fn unreachable_instances_name_their_remedy() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let stale = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
            .local_addr()?
            .port();
        for (published, remedy) in [
            (format!("{stale} {:032x}", 1), NOT_RUNNING),
            (stale.to_string(), OUTDATED),
        ] {
            fs::write(directory.path().join(PORT_FILE), published)?;
            let error = Instance::send(&path, Request::Toggle)
                .err()
                .context("An unreachable instance accepted a request")?;
            assert!(format!("{error:#}").starts_with(remedy), "{error:#}");
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn only_the_owner_can_read_the_published_secret() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let _owner = Instance::acquire(&path)?.context("First owner missing")?;
        let mode = fs::metadata(directory.path().join(PORT_FILE))?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
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

    /// Accepts fail on both sides of one that reaches the listener: the server must answer the
    /// caller in between, retry a lasting failure at most once per pause, and still end once its
    /// lane closes while accepts keep failing.
    #[test]
    fn failed_accepts_are_retried_until_shutdown() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("settings.json");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let (Endpoint { nonce, .. }, _port_file) =
            Endpoint::publish(listener.local_addr()?.port(), directory.path())?;
        let (failure, failures) = std::sync::mpsc::channel();
        let mut reaches_listener = [false, true].into_iter();
        let accept = move || {
            if reaches_listener.next() == Some(true) {
                return listener.accept().map(|(stream, _)| stream);
            }
            failure.send(()).unwrap();
            Err(io::ErrorKind::ConnectionAborted.into())
        };
        let (requests, received) = async_channel::bounded(1);
        let started = Instant::now();
        let server = OwnedThread::spawn("instance-server", {
            let requests = requests.clone();
            move || serve(accept, nonce, &requests)
        })?;
        // Outcomes are collected before asserting: an early return would join a server never told
        // to stop.
        let delivered =
            Instance::send(&path, Request::Toggle).and_then(|()| next_request(&received));
        thread::sleep(ACCEPT_RETRY * 5);
        requests.close();
        let (finished, completion) = std::sync::mpsc::channel();
        let cleanup = thread::spawn(move || {
            drop(server);
            finished.send(()).unwrap();
        });
        let stopped = completion.recv_timeout(PATIENCE);
        let elapsed = started.elapsed();
        assert_eq!(delivered?, Request::Toggle);
        stopped.context("A server whose accepts kept failing outlived shutdown")?;
        // A full pause follows every failure but the one that meets the closed lane; one more
        // absorbs timer granularity.
        let most = elapsed.as_millis() / ACCEPT_RETRY.as_millis() + 2;
        let retried = failures.try_iter().count();
        assert!(
            retried <= usize::try_from(most)?,
            "{retried} failed accepts in {elapsed:?}"
        );
        cleanup.join().unwrap();
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
