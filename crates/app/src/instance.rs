use std::{
    fs::{self, File},
    io::{self, Read as _, Write as _},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::mpsc::{self, SyncSender},
    thread,
    time::Duration,
};

use anyhow::{Context as _, bail};
use async_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use speakeasy_platform::OwnedThread;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream as AsyncStream,
    runtime::Runtime,
};

use crate::control::{Call, Command, Reply};

const LOCK_FILE: &str = "instance.lock";
const PORT_FILE: &str = "instance.port";
/// Fails a stale port fast; Windows retries a refused loopback connection before failing.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(100);
const REVEAL_ATTEMPTS: u32 = 25;
const REVEAL_RETRY: Duration = Duration::from_millis(20);

const NOT_RUNNING: &str = "Speakeasy is not running. Start it, then try again.";
const UNCONFIRMED: &str =
    "Speakeasy did not confirm the command. Check its state before trying again.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Request {
    Reveal,
    Toggle,
    Cancel,
}

pub(crate) enum Event {
    Shortcut(Request),
    Control(Call),
}

#[derive(Serialize, Deserialize)]
enum Message {
    Shortcut(Request),
    Control(Command),
}

pub(crate) struct Instance {
    requests: Sender<Event>,
    port_file: PathBuf,
    server: OwnedThread,
    _lock: File,
}

impl Instance {
    pub(crate) fn acquire(path: &Path) -> anyhow::Result<Option<(Self, Receiver<Event>)>> {
        let directory = path.parent().context("Settings path has no directory")?;
        fs::create_dir_all(directory)
            .with_context(|| format!("Can't create {}", directory.display()))?;
        let lock_file = directory.join(LOCK_FILE);
        let lock = File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_file)
            .with_context(|| format!("Can't open {}", lock_file.display()))?;
        match lock.try_lock() {
            Ok(()) => {},
            Err(fs::TryLockError::WouldBlock) => return reveal_running(path).map(|()| None),
            Err(fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("Can't lock {}", lock_file.display()));
            },
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        // `tokio::net::TcpListener::from_std` on the server thread needs a nonblocking socket.
        listener.set_nonblocking(true)?;
        let port_file = directory.join(PORT_FILE);
        fs::write(&port_file, listener.local_addr()?.port().to_string())
            .with_context(|| format!("Can't write {}", port_file.display()))?;
        let (requests, received) = async_channel::bounded(1);
        let (started, startup) = mpsc::sync_channel(1);
        let server = OwnedThread::spawn("instance-server", {
            let requests = requests.clone();
            move || serve(listener, requests, &started)
        })?;
        let instance = Self {
            requests,
            port_file,
            server,
            _lock: lock,
        };
        startup
            .recv()?
            .context("Can't start Speakeasy's shortcut listener")?;
        Ok(Some((instance, received)))
    }

    pub(crate) fn send(path: &Path, request: Request) -> anyhow::Result<()> {
        connect(path, &Message::Shortcut(request))
            .map(drop)
            .context("Speakeasy is not running. Start it before using a desktop shortcut")
    }

    pub(crate) fn control(path: &Path, command: Command) -> Reply {
        let Ok(mut stream) = connect(path, &Message::Control(command)) else {
            return Reply::unknown(NOT_RUNNING);
        };
        let mut reply = Vec::new();
        stream
            .read_to_end(&mut reply)
            .ok()
            .and_then(|_| serde_json::from_slice(&reply).ok())
            .unwrap_or_else(|| Reply::unknown(UNCONFIRMED))
    }

    pub(crate) async fn close(self) {
        self.requests.close();
        self.server.exited().await;
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.requests.close();
        #[expect(
            clippy::let_underscore_must_use,
            reason = "The released lock is authoritative; a stale port file only names a closed port"
        )]
        let _ = fs::remove_file(&self.port_file);
    }
}

/// Retried briefly: the owner takes the lock before it writes the port file.
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

fn connect(path: &Path, message: &Message) -> anyhow::Result<TcpStream> {
    let port: u16 = fs::read_to_string(path.with_file_name(PORT_FILE))?
        .trim()
        .parse()?;
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        CONNECT_TIMEOUT,
    )?;
    stream.write_all(&serde_json::to_vec(message)?)?;
    // Half-close: the server reads the message to EOF before it replies.
    stream.shutdown(Shutdown::Write)?;
    Ok(stream)
}

fn serve(listener: TcpListener, requests: Sender<Event>, started: &SyncSender<io::Result<()>>) {
    match listen(listener) {
        Ok((runtime, listener)) => {
            if started.send(Ok(())).is_ok() {
                runtime.block_on(accept(&listener, &requests));
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

/// Runs on the server thread, which also drops the runtime it builds.
///
/// The acquiring caller may be inside an async context, where dropping a runtime panics.
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

async fn accept(listener: &tokio::net::TcpListener, requests: &Sender<Event>) {
    loop {
        tokio::select! {
            () = requests.closed() => break,
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    let requests = requests.clone();
                    tokio::spawn(async move {
                        #[expect(
                            clippy::let_underscore_must_use,
                            reason = "A caller that has left no longer reads its reply"
                        )]
                        let _ = answer(stream, &requests).await;
                    });
                }
            },
        }
    }
}

async fn answer(mut stream: AsyncStream, requests: &Sender<Event>) -> io::Result<()> {
    let mut message = Vec::new();
    stream.read_to_end(&mut message).await?;
    let reply = match serde_json::from_slice(&message) {
        Ok(Message::Shortcut(request)) => {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A closed lane means Speakeasy is quitting and no longer takes shortcuts"
            )]
            let _ = requests.send(Event::Shortcut(request)).await;
            return Ok(());
        },
        Ok(Message::Control(command)) => {
            let (call, receipt) = Call::new(command);
            let deadline = call.deadline();
            let answered = async {
                requests.send(Event::Control(call)).await.ok()?;
                receipt.recv().await.ok()
            };
            tokio::time::timeout_at(deadline.into(), answered)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| Reply::unknown(UNCONFIRMED))
        },
        Err(_) => {
            Reply::refused("Speakeasy can't read this command. Restart Speakeasy, then try again.")
        },
    };
    stream.write_all(&serde_json::to_vec(&reply)?).await
}

#[cfg(test)]
mod tests;
