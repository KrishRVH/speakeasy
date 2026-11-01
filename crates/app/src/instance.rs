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
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream as AsyncStream,
    task::JoinSet,
};

use crate::control::{self, Call, Command, Reply};

const LOCK_FILE: &str = "instance.lock";
const PORT_FILE: &str = "instance.port";

const MAX_PORT_FILE_BYTES: u64 = 64;

const ACKNOWLEDGED: u8 = 0x06;
const NOT_RUNNING: &str = "Speakeasy is not running. Start it before using a desktop shortcut";

const INVALID_ENDPOINT: &str = "Restart Speakeasy to use desktop shortcuts";
const IO_TIMEOUT: Duration = Duration::from_millis(100);
const REVEAL_ATTEMPTS: u32 = 25;
const REVEAL_RETRY: Duration = Duration::from_millis(20);

const ACCEPT_RETRY: Duration = Duration::from_millis(10);
const MAX_CONTROL_CALLS: usize = 8;
const CONTROL: u8 = 3;
const UNCONFIRMED: &str = "Speakeasy did not confirm the command. Check its state before trying again; no retry was sent.";
const CONTROL_DISCONNECTED: &str = "Speakeasy closed without confirming the command. Restart Speakeasy to use this command, and check its state before trying again.";

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

#[derive(Debug)]
pub(crate) enum Event {
    Shortcut(Request),
    Control(Call),
}

pub(crate) struct Instance {
    requests: Sender<Event>,
    server: OwnedThread,
    _port_file: PortFile,
    _lock: File,
}

impl Instance {
    pub(crate) fn acquire(path: &Path) -> anyhow::Result<Option<(Self, Receiver<Event>)>> {
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
            Err(fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("Cannot lock {}", lock_file.display()));
            },
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let (Endpoint { nonce, .. }, port_file) = Endpoint::publish(address.port(), directory)?;
        let (requests, received) = async_channel::bounded(1);
        let server = OwnedThread::spawn("instance-server", {
            let requests = requests.clone();
            move || serve(listener, nonce, &requests)
        })?;
        let instance = Self {
            requests,
            server,
            _port_file: port_file,
            _lock: lock,
        };
        Ok(Some((instance, received)))
    }

    pub(crate) fn send(path: &Path, request: Request) -> anyhow::Result<()> {
        let mut stream = connect(&path.with_file_name(PORT_FILE))?;
        stream.write_all(&[request as u8])?;
        Ok(())
    }

    pub(crate) fn control(path: &Path, command: Command) -> Reply {
        let bytes = match command.encode() {
            Ok(bytes) => bytes,
            Err(reason) => return Reply::refused(reason),
        };
        let Ok(mut stream) = connect(&path.with_file_name(PORT_FILE)) else {
            return Reply::unknown(
                "Speakeasy is not running or could not be verified. Start or restart it before using this command.",
            );
        };
        let sent = (|| {
            let mut frame = vec![CONTROL];
            write_frame(&mut frame, &bytes)?;
            stream.write_all(&frame)
        })();
        if sent.is_err() {
            return Reply::unknown(CONTROL_DISCONNECTED);
        }
        let mut caller = Caller {
            stream: &mut stream,
            accepted: Instant::now(),
            timeout: control::TIMEOUT.saturating_add(IO_TIMEOUT),
        };
        match read_frame(&mut caller) {
            Ok(bytes) => Reply::decode(&bytes).unwrap_or_else(|_| Reply::unknown(UNCONFIRMED)),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::BrokenPipe
                ) =>
            {
                Reply::unknown(CONTROL_DISCONNECTED)
            },
            Err(_) => Reply::unknown(UNCONFIRMED),
        }
    }

    pub(crate) async fn close(self) {
        self.requests.close();
        self.server.exited().await;
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.requests.close();
    }
}

struct Endpoint {
    port: u16,
    nonce: u128,
}

impl Endpoint {
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

    fn read(port_file: &Path) -> anyhow::Result<Self> {
        let mut text = String::new();
        File::open(port_file)
            .context(NOT_RUNNING)?
            .take(MAX_PORT_FILE_BYTES)
            .read_to_string(&mut text)
            .context(INVALID_ENDPOINT)?;
        text.trim()
            .split_once(' ')
            .and_then(|(port, nonce)| {
                Some(Self {
                    port: port.parse().ok()?,
                    nonce: u128::from_str_radix(nonce, 16).ok()?,
                })
            })
            .context(INVALID_ENDPOINT)
    }
}

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

fn serve(listener: TcpListener, nonce: u128, requests: &Sender<Event>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        requests.close();
        return;
    };
    runtime.block_on(async {
        let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
            requests.close();
            return;
        };
        accept_connections(
            || async { listener.accept().await.map(|(stream, _)| stream) },
            nonce,
            requests,
        )
        .await;
    });
}

async fn accept_connections<F>(
    mut accept: impl FnMut() -> F + Send,
    nonce: u128,
    requests: &Sender<Event>,
) where
    F: Future<Output = io::Result<AsyncStream>> + Send,
{
    let mut controls = JoinSet::new();
    loop {
        let accepted = tokio::select! {
            biased;
            () = requests.closed() => break,
            Some(completed) = controls.join_next(), if !controls.is_empty() => {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A failed connection drops its receipt and socket; other callers remain independent"
                )]
                let _ = completed;
                continue;
            },
            accepted = accept() => accepted,
        };
        let Ok(mut stream) = accepted else {
            tokio::select! {
                () = requests.closed() => break,
                () = tokio::time::sleep(ACCEPT_RETRY) => {},
            }
            continue;
        };
        let Ok(Some(request)) = handshake(&mut stream, nonce).await else {
            continue;
        };

        match request {
            Incoming::Shortcut(request) => {
                if requests.send(Event::Shortcut(request)).await.is_err() {
                    break;
                }
            },
            Incoming::Control(_) if controls.len() >= MAX_CONTROL_CALLS => {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A refused caller may disconnect before receiving the busy response"
                )]
                let _ = serve_control(
                    stream,
                    Err("Speakeasy is busy. Try again when a control call finishes."),
                    requests,
                )
                .await;
            },
            Incoming::Control(command) => {
                let requests = requests.clone();
                controls.spawn(async move { serve_control(stream, command, &requests).await });
            },
        }
    }
    controls.shutdown().await;
}

enum Incoming {
    Shortcut(Request),
    Control(Result<Command, &'static str>),
}

async fn handshake(stream: &mut AsyncStream, nonce: u128) -> io::Result<Option<Incoming>> {
    stream.set_nodelay(true)?;
    let read = async {
        let mut presented = [0; size_of::<u128>()];
        stream.read_exact(&mut presented).await?;
        if u128::from_le_bytes(presented) != nonce {
            return Ok(None);
        }
        stream.write_all(&[ACKNOWLEDGED]).await?;
        let byte = stream.read_u8().await?;
        if byte == CONTROL {
            let length = usize::from(stream.read_u16().await?);
            if length > control::MAX_BYTES {
                return Ok(Some(Incoming::Control(Err(
                    "The control command exceeds its size limit.",
                ))));
            }
            let mut bytes = vec![0; length];
            stream.read_exact(&mut bytes).await?;
            return Ok(Some(Incoming::Control(Command::decode(&bytes))));
        }
        Ok(Request::from_byte(byte).map(Incoming::Shortcut))
    };
    tokio::time::timeout(IO_TIMEOUT, read)
        .await
        .map_err(|_elapsed| io::ErrorKind::TimedOut)?
}

struct Caller<'stream> {
    stream: &'stream mut TcpStream,
    accepted: Instant,
    timeout: Duration,
}

impl Read for Caller<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self
            .timeout
            .checked_sub(self.accepted.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(io::ErrorKind::TimedOut)?;
        self.stream.set_read_timeout(Some(remaining))?;
        self.stream.read(buffer)
    }
}

fn read_frame(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0; 2];
    reader.read_exact(&mut length)?;
    let length = usize::from(u16::from_be_bytes(length));
    if length > control::MAX_BYTES {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn write_frame(writer: &mut impl io::Write, bytes: &[u8]) -> io::Result<()> {
    let length = u16::try_from(bytes.len()).map_err(|_error| io::ErrorKind::InvalidData)?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(bytes)
}

async fn serve_control(
    mut stream: AsyncStream,
    command: Result<Command, &'static str>,
    requests: &Sender<Event>,
) -> io::Result<()> {
    match stream.try_read(&mut [0]) {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {},
        Err(error) => return Err(error),
    }
    let reply = match command {
        Ok(command) => {
            let (call, receipt) = Call::new(command);
            let deadline = call.deadline;
            match requests.try_send(Event::Control(call)) {
                Ok(()) => {
                    let mut extra = [0];
                    let received = tokio::select! {
                        biased;
                        _ = stream.read(&mut extra) => return Ok(()),
                        result = tokio::time::timeout_at(deadline.into(), receipt.recv()) => result,
                    };
                    match received {
                        Ok(Ok(reply)) => reply,
                        Ok(Err(_)) | Err(_) => Reply::unknown(UNCONFIRMED),
                    }
                },
                Err(async_channel::TrySendError::Full(_)) => Reply::refused(
                    "Speakeasy is busy. Try again when the current command finishes.",
                ),
                Err(async_channel::TrySendError::Closed(_)) => {
                    Reply::refused("Speakeasy is stopping.")
                },
            }
        },
        Err(reason) => Reply::refused(reason),
    };
    let bytes = reply
        .encode()
        .or_else(|_| Reply::unknown(UNCONFIRMED).encode())
        .map_err(|_error| io::ErrorKind::InvalidData)?;
    let length = u16::try_from(bytes.len()).map_err(|_error| io::ErrorKind::InvalidData)?;
    let write = async {
        stream.write_all(&length.to_be_bytes()).await?;
        stream.write_all(&bytes).await
    };
    tokio::time::timeout(IO_TIMEOUT, write)
        .await
        .map_err(|_error| io::ErrorKind::TimedOut)?
}

fn connect(port_file: &Path) -> anyhow::Result<TcpStream> {
    let endpoint = Endpoint::read(port_file)?;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT).context(NOT_RUNNING)?;
    stream.set_nodelay(true)?;
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
mod tests;
