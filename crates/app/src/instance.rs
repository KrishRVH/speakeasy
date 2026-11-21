use std::{
    collections::VecDeque,
    fmt,
    fs::{self, File},
    future,
    io::{self, Read, Write as _},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::mpsc::{self, SyncSender},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail, ensure};
use async_channel::{Receiver, Sender};
use speakeasy_platform::OwnedThread;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream as AsyncStream,
    runtime::Runtime,
    task::{AbortHandle, Id, JoinError, JoinSet},
};

use crate::control::{self, Call, Command, Reply};

const LOCK_FILE: &str = "instance.lock";
const PORT_FILE: &str = "instance.port";
const MAX_PORT_FILE_BYTES: u64 = 64;

const ACKNOWLEDGED: u8 = 0x06;
const CONTROL: u8 = 3;
const MAX_HANDSHAKES: usize = 16;
const MAX_UNDELIVERED: usize = 2;
const MAX_CONTROL_CALLS: usize = 8;
const IO_TIMEOUT: Duration = Duration::from_millis(100);
const ACCEPT_RETRY: Duration = Duration::from_millis(10);
const REVEAL_ATTEMPTS: u32 = 25;
const REVEAL_RETRY: Duration = Duration::from_millis(20);

const NOT_RUNNING: &str = "Speakeasy is not running. Start it before using a desktop shortcut";
const INVALID_ENDPOINT: &str = "Restart Speakeasy to use desktop shortcuts";
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
        let lock = speakeasy_platform::open_lock(&lock_file)
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
        let (Endpoint { nonce, .. }, port_file) =
            Endpoint::publish(listener.local_addr()?.port(), directory)?;
        let (requests, received) = async_channel::bounded(1);
        let (started, startup) = mpsc::sync_channel(1);
        let server = OwnedThread::spawn("instance-server", {
            let requests = requests.clone();
            move || serve(listener, nonce, &requests, &started)
        })?;
        startup
            .recv()?
            .context("Cannot start Speakeasy's shortcut listener")?;
        let instance = Self {
            requests,
            server,
            _port_file: port_file,
            _lock: lock,
        };
        Ok(Some((instance, received)))
    }

    pub(crate) fn send(path: &Path, request: Request) -> anyhow::Result<()> {
        connect(&path.with_file_name(PORT_FILE), &[request as u8])?;
        Ok(())
    }

    pub(crate) fn control(path: &Path, command: Command) -> Reply {
        let bytes = match command.encode() {
            Ok(bytes) => bytes,
            Err(reason) => return Reply::refused(reason),
        };
        let mut frame = vec![CONTROL];
        if write_frame(&mut frame, &bytes).is_err() {
            return Reply::refused("The control command exceeds its size limit.");
        }
        let mut stream = match connect(&path.with_file_name(PORT_FILE), &frame) {
            Ok(stream) => stream,
            Err(error) if error.is::<Unconfirmed>() => return Reply::unknown(UNCONFIRMED),
            Err(_) => {
                return Reply::unknown(
                    "Speakeasy is not running or could not be verified. Start or restart it before using this command.",
                );
            },
        };
        let mut reply = ReplyDeadline {
            stream: &mut stream,
            sent: Instant::now(),
            timeout: command.timeout().saturating_add(IO_TIMEOUT),
        };
        match read_frame(&mut reply) {
            Ok(bytes) => Reply::decode(&bytes).unwrap_or_else(|| Reply::unknown(UNCONFIRMED)),
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

fn serve(
    listener: TcpListener,
    nonce: u128,
    requests: &Sender<Event>,
    started: &SyncSender<io::Result<()>>,
) {
    match listen(listener) {
        Ok((runtime, listener)) => {
            if started.send(Ok(())).is_ok() {
                runtime.block_on(accept_connections(
                    || async { listener.accept().await.map(|(stream, _)| stream) },
                    nonce,
                    requests,
                ));
            }
        },
        Err(error) => {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Acquisition waits for this outcome unless it has already unwound"
            )]
            let _ = started.send(Err(error));
        },
    }
}

fn listen(listener: TcpListener) -> io::Result<(Runtime, tokio::net::TcpListener)> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let listener = {
        let _context = runtime.enter();
        tokio::net::TcpListener::from_std(listener)?
    };
    Ok((runtime, listener))
}

async fn accept_connections<F>(
    mut accept: impl FnMut() -> F + Send,
    nonce: u128,
    requests: &Sender<Event>,
) where
    F: Future<Output = io::Result<AsyncStream>> + Send,
{
    let mut arrivals = Arrivals::default();
    let mut controls = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            () = requests.closed() => break,
            Some(completed) = controls.join_next(), if !controls.is_empty() => {
                #[expect(
                    clippy::let_underscore_must_use,
                    reason = "A failed connection drops its receipt and socket; other callers remain independent"
                )]
                let _ = completed;
            },
            delivered = deliver(arrivals.next_shortcut(), requests) => {
                if delivered.is_err() {
                    break;
                }
                arrivals.delivered();
            },
            Some(joined) = arrivals.tasks.join_next_with_id(), if !arrivals.tasks.is_empty() => {
                let Some((stream, command)) = arrivals.finish(joined, requests.len()).await else {
                    continue;
                };
                if controls.len() >= MAX_CONTROL_CALLS {
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
                } else {
                    let requests = requests.clone();
                    controls.spawn(async move { serve_control(stream, command, &requests).await });
                }
            },
            accepted = accept() => {
                if let Ok(stream) = accepted {
                    arrivals.begin(stream, nonce);
                    tokio::task::yield_now().await;
                } else {
                    tokio::select! {
                        () = requests.closed() => break,
                        () = tokio::time::sleep(ACCEPT_RETRY) => {},
                    }
                }
            },
        }
    }
    arrivals.tasks.shutdown().await;
    controls.shutdown().await;
}

async fn deliver(
    next: Option<Request>,
    requests: &Sender<Event>,
) -> Result<(), async_channel::SendError<Event>> {
    match next {
        Some(request) => requests.send(Event::Shortcut(request)).await,
        None => future::pending().await,
    }
}

#[derive(Default)]
struct Arrivals {
    tasks: JoinSet<Option<(AsyncStream, Incoming)>>,
    order: VecDeque<Arrival>,
}

enum Arrival {
    Handshake(AbortHandle),
    Admitted(Request),
}

impl Arrivals {
    fn begin(&mut self, mut stream: AsyncStream, nonce: u128) {
        if self.handshakes() >= MAX_HANDSHAKES
            && let Some(oldest) = self
                .order
                .iter()
                .position(|arrival| matches!(arrival, Arrival::Handshake(_)))
            && let Some(Arrival::Handshake(task)) = self.order.remove(oldest)
        {
            task.abort();
        }
        let task = self.tasks.spawn(async move {
            let request = handshake(&mut stream, nonce).await.ok().flatten()?;
            Some((stream, request))
        });
        self.order.push_back(Arrival::Handshake(task));
    }

    async fn finish(
        &mut self,
        joined: Result<(Id, Option<(AsyncStream, Incoming)>), JoinError>,
        mailbox: usize,
    ) -> Option<(AsyncStream, Result<Command, &'static str>)> {
        let (id, outcome) = joined.unwrap_or_else(|error| (error.id(), None));
        let position = self
            .order
            .iter()
            .position(|arrival| matches!(arrival, Arrival::Handshake(task) if task.id() == id))?;
        let control = match outcome {
            Some((mut stream, Incoming::Shortcut(request))) => {
                if self.admitted().saturating_add(mailbox) < MAX_UNDELIVERED
                    && !departed(&stream)
                    && acknowledge(&mut stream).await
                    && let Some(arrival) = self.order.get_mut(position)
                {
                    *arrival = Arrival::Admitted(request);
                    return None;
                }
                None
            },
            Some((stream, Incoming::Control(command))) => Some((stream, command)),
            None => None,
        };
        self.order.remove(position);
        control
    }

    fn next_shortcut(&self) -> Option<Request> {
        match self.order.front() {
            Some(&Arrival::Admitted(request)) => Some(request),
            _ => None,
        }
    }

    fn delivered(&mut self) {
        self.order.pop_front();
    }

    fn handshakes(&self) -> usize {
        self.order
            .iter()
            .filter(|arrival| matches!(arrival, Arrival::Handshake(_)))
            .count()
    }

    fn admitted(&self) -> usize {
        self.order.len().saturating_sub(self.handshakes())
    }
}

fn departed(stream: &AsyncStream) -> bool {
    !matches!(stream.try_read(&mut [0]), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
}

async fn acknowledge(stream: &mut AsyncStream) -> bool {
    matches!(
        tokio::time::timeout(IO_TIMEOUT, stream.write_all(&[ACKNOWLEDGED])).await,
        Ok(Ok(()))
    )
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
        let byte = stream.read_u8().await?;
        if byte == CONTROL {
            let length = stream.read_u16().await?;
            stream.write_all(&[ACKNOWLEDGED]).await?;
            if usize::from(length) > control::MAX_BYTES {
                write_reply(
                    stream,
                    Reply::refused("The control command exceeds its size limit."),
                )
                .await?;
                stream.shutdown().await?;
                tokio::io::copy(&mut stream.take(u64::from(length)), &mut tokio::io::sink())
                    .await?;
                return Ok(None);
            }
            let mut bytes = vec![0; usize::from(length)];
            stream.read_exact(&mut bytes).await?;
            return Ok(Some(Incoming::Control(Command::decode(&bytes))));
        }
        Ok(Request::from_byte(byte).map(Incoming::Shortcut))
    };
    tokio::time::timeout(IO_TIMEOUT, read)
        .await
        .map_err(|_elapsed| io::ErrorKind::TimedOut)?
}

struct ReplyDeadline<'stream> {
    stream: &'stream mut TcpStream,
    sent: Instant,
    timeout: Duration,
}

impl Read for ReplyDeadline<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self
            .timeout
            .checked_sub(self.sent.elapsed())
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
    if length > control::MAX_REPLY_BYTES {
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
    let reply = match command {
        Ok(command) => {
            if departed(&stream) {
                return Ok(());
            }
            let (call, receipt) = Call::new(command);
            let deadline = call.deadline();
            match requests.try_send(Event::Control(call)) {
                Ok(()) => {
                    let mut extra = [0];
                    let received = tokio::select! {
                        biased;
                        read = stream.read(&mut extra) => return read.map(drop),
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
    write_reply(&mut stream, reply).await
}

async fn write_reply(stream: &mut AsyncStream, reply: Reply) -> io::Result<()> {
    let bytes = reply
        .encode()
        .or_else(|_| Reply::unknown(UNCONFIRMED).encode())
        .map_err(|_error| io::ErrorKind::InvalidData)?;
    let mut frame = Vec::new();
    write_frame(&mut frame, &bytes)?;
    tokio::time::timeout(IO_TIMEOUT, stream.write_all(&frame))
        .await
        .map_err(|_error| io::ErrorKind::TimedOut)?
}

fn connect(port_file: &Path, request: &[u8]) -> anyhow::Result<TcpStream> {
    let endpoint = Endpoint::read(port_file)?;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let mut stream = TcpStream::connect_timeout(&address, IO_TIMEOUT).context(NOT_RUNNING)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(&[endpoint.nonce.to_le_bytes().as_slice(), request].concat())?;
    let mut reply = [0];
    if let Err(error) = stream.read_exact(&mut reply) {
        if matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ) {
            return Err(error).context(Unconfirmed);
        }
        return Err(error).context("Speakeasy did not answer; try the shortcut again");
    }
    ensure!(
        reply == [ACKNOWLEDGED],
        "Speakeasy instance could not be verified"
    );
    Ok(stream)
}

#[derive(Debug)]
struct Unconfirmed;

impl fmt::Display for Unconfirmed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "Speakeasy did not confirm the request in time; check its state before trying again",
        )
    }
}

#[cfg(test)]
mod tests;
