use std::{
    io::{BufRead as _, BufReader},
    os::{fd::OwnedFd, unix::net::UnixStream},
    process::{Child, Command, Stdio},
    sync::mpsc,
};

use anyhow::{bail, ensure};
use tokio::task::{JoinHandle, LocalSet};
use zbus::{connection::Builder, message::Type, names::UniqueName};

use super::{super::*, fixture_session};
use crate::{
    Delivery, Enter, FocusReport, Inserted, PATIENCE,
    insertion::Reply,
    linux::{
        COPIED_FOR_MANUAL_PASTE, HostSession,
        x11::{ClipboardAccess, ClipboardWorker},
    },
};

const TEXT: &str = "text/plain;charset=utf-8";

struct Bus {
    daemon: Child,
    address: String,
    _directory: tempfile::TempDir,
}

impl Bus {
    fn start() -> anyhow::Result<Self> {
        let directory = tempfile::tempdir()?;
        let config = directory.path().join("bus.conf");
        std::fs::write(
            &config,
            format!(
                "<busconfig><type>session</type><listen>unix:dir={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\"/>\
                 <allow receive_sender=\"*\"/><allow own=\"*\"/></policy></busconfig>",
                directory.path().display()
            ),
        )?;
        let mut daemon = Command::new("dbus-daemon")
            .arg("--nofork")
            .arg("--print-address=1")
            .arg(format!("--config-file={}", config.display()))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Install dbus-daemon to run the private-bus portal tests")?;
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().context("Bus address")?).read_line(&mut address)?;
        Ok(Self {
            daemon,
            address: address.trim().to_owned(),
            _directory: directory,
        })
    }

    async fn connect(&self) -> anyhow::Result<Connection> {
        let connecting = Builder::address(self.address.as_str())?.build();
        Ok(tokio::time::timeout(PATIENCE, connecting)
            .await
            .context("The private bus did not accept a connection")??)
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A private bus that already exited needs no further cleanup"
        )]
        let _ = self.daemon.kill();
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Reaping the private bus is best effort once it was killed"
        )]
        let _ = self.daemon.wait();
    }
}

struct FakePortal {
    connection: Connection,
    calls: MessageStream,
    session: OwnedObjectPath,
}

impl FakePortal {
    async fn start(bus: &Bus) -> anyhow::Result<Self> {
        let connection = bus.connect().await?;
        let calls = MessageStream::from(&connection);
        connection.request_name(DESTINATION).await?;
        Ok(Self {
            connection,
            calls,
            session: fixture_session()?,
        })
    }

    async fn call(&mut self) -> anyhow::Result<Message> {
        loop {
            let message = tokio::time::timeout(PATIENCE, self.calls.next())
                .await
                .context("The client stopped calling the portal")?
                .context("The portal connection closed")??;
            if message.message_type() == Type::MethodCall {
                return Ok(message);
            }
        }
    }

    async fn expect(&mut self, member: &str) -> anyhow::Result<Message> {
        let call = self.call().await?;
        ensure!(
            call.header().member().map(MemberName::as_str) == Some(member),
            "The client called {:?} instead of {member}",
            call.header().member()
        );
        Ok(call)
    }

    async fn reply(&self, call: &Message) -> anyhow::Result<()> {
        Ok(self.connection.reply(&call.header(), &()).await?)
    }

    async fn pipe(&self, call: &Message) -> anyhow::Result<UnixStream> {
        let (reader, writer) = UnixStream::pair()?;
        self.connection
            .reply(
                &call.header(),
                &(zbus::zvariant::OwnedFd::from(OwnedFd::from(writer)),),
            )
            .await?;
        Ok(reader)
    }

    async fn transfer(&self, serial: u32) -> anyhow::Result<()> {
        Ok(self
            .connection
            .emit_signal(
                None::<&str>,
                DESKTOP,
                CLIPBOARD,
                "SelectionTransfer",
                &(&self.session, TEXT, serial),
            )
            .await?)
    }

    async fn own(&self) -> anyhow::Result<()> {
        let values = Results::from([("session_is_owner".into(), OwnedValue::from(true))]);
        Ok(self
            .connection
            .emit_signal(
                None::<&str>,
                DESKTOP,
                CLIPBOARD,
                "SelectionOwnerChanged",
                &(&self.session, values),
            )
            .await?)
    }

    async fn select(&mut self) -> anyhow::Result<()> {
        let call = self.expect("SetSelection").await?;
        self.reply(&call).await?;
        self.own().await
    }

    async fn served(&mut self, serial: u32) -> anyhow::Result<(Vec<u8>, bool)> {
        let write = self.expect("SelectionWrite").await?;
        let (_, written): (OwnedObjectPath, u32) = write.body().deserialize()?;
        ensure!(
            written == serial,
            "The client wrote serial {written}, not {serial}"
        );
        let bytes = read_all(self.pipe(&write).await?).await?;
        let done = self.expect("SelectionWriteDone").await?;
        let (_, reported, success): (OwnedObjectPath, u32, bool) = done.body().deserialize()?;
        ensure!(
            reported == serial,
            "The client reported serial {reported}, not {serial}"
        );
        Ok((bytes, success))
    }
}

async fn read_all(reader: UnixStream) -> anyhow::Result<Vec<u8>> {
    reader.set_nonblocking(true)?;
    let reader = tokio::net::UnixStream::from_std(reader)?;
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    tokio::time::timeout(PATIENCE, async {
        loop {
            reader.readable().await?;
            match reader.try_read(&mut chunk) {
                Ok(0) => return Ok::<_, anyhow::Error>(()),
                Ok(read) => bytes.extend_from_slice(chunk.get(..read).context("Read length")?),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {},
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await
    .context("The client kept the transfer open")??;
    Ok(bytes)
}

struct Focus {
    entered: async_channel::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl ClipboardAccess for Focus {
    fn external_target(&self, _: HostSession) -> anyhow::Result<bool> {
        self.entered.send_blocking(())?;
        self.release.recv_timeout(PATIENCE)?;
        Ok(true)
    }

    fn write_text(&mut self, _: &str, _: &InsertPermit) -> anyhow::Result<bool> {
        bail!("The portal clipboard used the X11 clipboard")
    }

    fn owner(&self) -> anyhow::Result<x11rb::protocol::xproto::Window> {
        bail!("The portal clipboard used the X11 clipboard")
    }

    fn holds_text(&mut self, _: &str) -> anyhow::Result<bool> {
        bail!("The portal clipboard used the X11 clipboard")
    }
}

struct Client {
    name: String,
    input: InputSender,
    _events: Receiver<Input>,
    requests: Sender<Insertion>,
    _stop: Sender<Infallible>,
    focused: Receiver<()>,
    release: mpsc::Sender<()>,
    served: JoinHandle<anyhow::Result<()>>,
}

impl Client {
    async fn start(bus: &Bus) -> anyhow::Result<Self> {
        let connection = bus.connect().await?;
        let (owner, changes) = watch_portal(&connection).await?;
        let name = connection.unique_name().context("Client name")?.to_string();
        let sender_segment = name.trim_start_matches(':').replace('.', "_");
        let portal = Portal {
            connection,
            owner,
            sender_segment,
            unanswered: Vec::new(),
            sessions: Vec::new(),
            nonce: "fixture".into(),
            sequence: 0,
        };
        let (events, received) = async_channel::bounded(64);
        let input = InputSender::new(events);
        let (requests, incoming) = async_channel::bounded(1);
        let (stop, stopping) = async_channel::bounded(1);
        let (entered, focused) = async_channel::bounded(1);
        let (release, waiting) = mpsc::channel();
        let worker = ClipboardWorker::spawn(input.clone(), move || {
            Ok(Focus {
                entered,
                release: waiting,
            })
        })?;
        let bound = Bound {
            shortcut_session: None,
            sender: None,
            clipboard_session: Some(fixture_session()?),
            labels: ShortcutLabels {
                shortcut: "fixture".into(),
                cancel: "fixture".into(),
            },
            paste: LinuxPaste::Manual,
        };
        let served = tokio::task::spawn_local({
            let input = input.clone();
            async move { serve(&portal, bound, changes, worker, &input, incoming, &stopping).await }
        });
        match tokio::time::timeout(PATIENCE, received.recv()).await?? {
            Input::DesktopReady { .. } => {},
            other => bail!("The client started with {other:?}"),
        }
        Ok(Self {
            name,
            input,
            _events: received,
            requests,
            _stop: stop,
            focused,
            release,
            served,
        })
    }

    async fn paste(
        &self,
        text: &str,
    ) -> anyhow::Result<(InsertPermit, Receiver<anyhow::Result<Inserted>>)> {
        let permit = self.input.begin().context("Recording")?;
        let (reply, answer) = Reply::channel();
        self.requests
            .send(Insertion {
                text: text.into(),
                permit: permit.clone(),
                delivery: Delivery::Paste,
                enter: Enter::Withheld,
                own_focus: FocusReport::default().subscribe(),
                reply,
            })
            .await?;
        Ok((permit, answer))
    }

    async fn focus(&self) -> anyhow::Result<()> {
        tokio::time::timeout(PATIENCE, self.focused.recv()).await??;
        Ok(())
    }

    fn release_focus(&self) -> anyhow::Result<()> {
        Ok(self.release.send(())?)
    }

    async fn copy(&self, portal: &mut FakePortal, text: &str) -> anyhow::Result<()> {
        let (_, answer) = self.paste(text).await?;
        portal.select().await?;
        self.focus().await?;
        self.release_focus()?;
        let copied = tokio::time::timeout(PATIENCE, answer.recv()).await???;
        ensure!(
            copied == COPIED_FOR_MANUAL_PASTE,
            "The copy ended {copied:?}"
        );
        Ok(())
    }

    fn alive(&self) -> bool {
        !self.served.is_finished()
    }
}

fn on_private_bus(test: impl AsyncFnOnce(Bus) -> anyhow::Result<()>) -> anyhow::Result<()> {
    let bus = Bus::start()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _context = runtime.enter();
    let local = LocalSet::new();
    runtime.block_on(local.run_until(test(bus)))
}

#[test]
fn private_bus_a_replacement_serves_nothing_once_the_portal_accepts_it_until_it_commits()
-> anyhow::Result<()> {
    on_private_bus(async |bus| {
        let mut portal = FakePortal::start(&bus).await?;
        let client = Client::start(&bus).await?;
        client.copy(&mut portal, "alpha").await?;

        let (_, answer) = client.paste("bravo").await?;
        let selecting = portal.expect("SetSelection").await?;
        client.input.deliver(Input::Cancel);
        portal.reply(&selecting).await?;
        assert!(
            matches!(
                tokio::time::timeout(PATIENCE, answer.recv()).await??,
                Ok(Inserted::Cancelled)
            ),
            "Cancelling during SetSelection did not cancel the paste"
        );
        portal.own().await?;
        portal.transfer(1).await?;
        assert!(
            portal.served(1).await? == (Vec::new(), false),
            "The desktop offered a copy cancelled during SetSelection and Speakeasy served the previous one for it"
        );

        client.copy(&mut portal, "alpha").await?;
        let (_, answer) = client.paste("charlie").await?;
        let selecting = portal.expect("SetSelection").await?;
        portal.transfer(2).await?;
        assert!(
            portal.served(2).await? == (Vec::new(), false),
            "A transfer while SetSelection was pending served text"
        );
        portal.reply(&selecting).await?;
        portal.transfer(3).await?;
        assert!(
            portal.served(3).await? == (Vec::new(), false),
            "A transfer while the desktop confirmed a replacement served text"
        );
        portal.own().await?;
        client.focus().await?;
        portal.transfer(4).await?;
        assert!(
            portal.served(4).await? == (Vec::new(), false),
            "A transfer during a replacement served text before the paste committed"
        );
        client.input.deliver(Input::Cancel);
        client.release_focus()?;
        assert!(
            matches!(
                tokio::time::timeout(PATIENCE, answer.recv()).await??,
                Ok(Inserted::Cancelled)
            ),
            "Cancelling during preparation did not cancel the paste"
        );
        portal.transfer(5).await?;
        assert!(
            portal.served(5).await? == (Vec::new(), false),
            "The desktop offered a cancelled copy and Speakeasy served the previous one for it"
        );
        assert!(client.alive());
        Ok(())
    })
}

#[test]
fn private_bus_a_replacement_the_desktop_confirms_after_the_wait_serves_neither_copy()
-> anyhow::Result<()> {
    on_private_bus(async |bus| {
        let mut portal = FakePortal::start(&bus).await?;
        let client = Client::start(&bus).await?;
        client.copy(&mut portal, "alpha").await?;

        let (_, answer) = client.paste("bravo").await?;
        let selecting = portal.expect("SetSelection").await?;
        portal.own().await?;
        portal.reply(&selecting).await?;
        let outcome = tokio::time::timeout(PATIENCE, answer.recv()).await???;
        assert!(
            matches!(outcome, Inserted::Unavailable(_)),
            "A replacement confirmed only by ownership from before its reply ended {outcome:?}"
        );
        portal.own().await?;
        portal.transfer(1).await?;
        assert!(
            portal.served(1).await? == (Vec::new(), false),
            "The desktop confirmed a replacement after the wait and Speakeasy served the copy it replaced"
        );
        assert!(client.alive());
        Ok(())
    })
}

#[test]
fn private_bus_a_selection_the_portal_never_answers_serves_neither_copy() -> anyhow::Result<()> {
    on_private_bus(async |bus| {
        let mut portal = FakePortal::start(&bus).await?;
        let client = Client::start(&bus).await?;
        client.copy(&mut portal, "alpha").await?;

        let (_, answer) = client.paste("bravo").await?;
        let selecting = portal.expect("SetSelection").await?;
        assert!(
            tokio::time::timeout(PATIENCE, answer.recv())
                .await??
                .is_err(),
            "An unanswered selection reported success"
        );
        portal.reply(&selecting).await?;
        portal.own().await?;
        portal.transfer(1).await?;
        assert!(
            portal.served(1).await? == (Vec::new(), false),
            "The desktop set a selection Speakeasy stopped waiting for and Speakeasy served the copy it replaced"
        );
        assert!(client.alive());
        Ok(())
    })
}

#[test]
fn private_bus_a_refused_selection_keeps_serving_the_previous_copy() -> anyhow::Result<()> {
    on_private_bus(async |bus| {
        let mut portal = FakePortal::start(&bus).await?;
        let client = Client::start(&bus).await?;
        client.copy(&mut portal, "alpha").await?;

        let (_, answer) = client.paste("bravo").await?;
        let selecting = portal.expect("SetSelection").await?;
        portal
            .connection
            .reply_error(
                &selecting.header(),
                "org.freedesktop.portal.Error.Failed",
                &("Refused",),
            )
            .await?;
        assert!(
            tokio::time::timeout(PATIENCE, answer.recv())
                .await??
                .is_err(),
            "A refused selection reported success"
        );
        portal.transfer(1).await?;
        assert_eq!(
            portal.served(1).await?,
            (b"alpha".to_vec(), true),
            "A refused SetSelection discarded the copy the clipboard still holds"
        );
        Ok(())
    })
}

#[test]
fn private_bus_only_the_portal_signals_and_malformed_ones_are_skipped() -> anyhow::Result<()> {
    on_private_bus(async |bus| {
        let mut portal = FakePortal::start(&bus).await?;
        let client = Client::start(&bus).await?;
        client.copy(&mut portal, "alpha").await?;

        let forger = bus.connect().await?;
        let lost = Results::from([("session_is_owner".into(), OwnedValue::from(false))]);
        forger
            .emit_signal(
                Some(client.name.as_str()),
                DESKTOP,
                CLIPBOARD,
                "SelectionOwnerChanged",
                &(&portal.session, lost),
            )
            .await?;
        forger
            .emit_signal(
                Some(client.name.as_str()),
                DESKTOP,
                CLIPBOARD,
                "SelectionTransfer",
                &(&portal.session, TEXT, 99_u32),
            )
            .await?;
        portal
            .connection
            .emit_signal(
                None::<&str>,
                DESKTOP,
                CLIPBOARD,
                "SelectionTransfer",
                &(&portal.session, TEXT),
            )
            .await?;
        forger
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "GetId",
                &(),
            )
            .await?;
        portal.transfer(1).await?;
        assert_eq!(
            portal.served(1).await?,
            (b"alpha".to_vec(), true),
            "A forged or malformed signal changed what the clipboard serves"
        );
        assert!(
            client.alive(),
            "A malformed portal signal stopped the service"
        );
        Ok(())
    })
}

#[derive(Default)]
struct Ledger {
    written: Vec<(u32, UnixStream)>,
    reported: Vec<(u32, bool)>,
}

impl Ledger {
    async fn answer(&mut self, portal: &FakePortal, call: &Message) -> anyhow::Result<()> {
        match call.header().member().map(MemberName::as_str) {
            Some("SetSelection") => {
                portal.reply(call).await?;
                portal.own().await?;
            },
            Some("SelectionWrite") => {
                let (_, serial): (OwnedObjectPath, u32) = call.body().deserialize()?;
                self.written.push((serial, portal.pipe(call).await?));
            },
            Some("SelectionWriteDone") => {
                let (_, serial, success): (OwnedObjectPath, u32, bool) =
                    call.body().deserialize()?;
                self.reported.push((serial, success));
                portal.reply(call).await?;
            },
            other => bail!("The client called {other:?}"),
        }
        Ok(())
    }

    async fn settle(&mut self, portal: &mut FakePortal, serials: usize) -> anyhow::Result<()> {
        while self.reported.len() < serials {
            let call = portal.call().await?;
            self.answer(portal, &call).await?;
        }
        Ok(())
    }
}

#[test]
fn private_bus_every_transfer_past_full_transfers_and_refusals_is_still_refused()
-> anyhow::Result<()> {
    on_private_bus(async |bus| {
        let mut portal = FakePortal::start(&bus).await?;
        let client = Client::start(&bus).await?;
        client.copy(&mut portal, "alpha").await?;
        for serial in 1..=4 {
            portal.transfer(serial).await?;
            portal.expect("SelectionWrite").await?;
        }
        let last = u32::try_from(2 * SIGNAL_BACKLOG)?.saturating_add(4);
        for serial in 5..=last {
            portal.transfer(serial).await?;
        }

        let (_, answer) = client.paste("bravo").await?;
        let mut ledger = Ledger::default();
        let copied = loop {
            tokio::select! {
                copied = tokio::time::timeout(PATIENCE, answer.recv()) => break copied???,
                entered = client.focused.recv() => {
                    entered?;
                    client.release_focus()?;
                },
                call = portal.call() => {
                    ledger.answer(&portal, &call?).await?;
                },
            }
        };
        assert_eq!(copied, COPIED_FOR_MANUAL_PASTE);
        ledger.settle(&mut portal, usize::try_from(last)?).await?;
        let mut reported: Vec<u32> = ledger.reported.iter().map(|(serial, _)| *serial).collect();
        reported.sort_unstable();
        assert_eq!(
            reported,
            (1..=last).collect::<Vec<_>>(),
            "A transfer went unreported or was reported twice"
        );
        let mut written: Vec<u32> = ledger.written.iter().map(|(serial, _)| *serial).collect();
        written.sort_unstable();
        assert_eq!(
            written,
            (5..=last).collect::<Vec<_>>(),
            "A transfer declined past the refusal limit never reached SelectionWrite"
        );
        assert!(
            ledger.reported.iter().all(|(_, success)| !success),
            "A refused or invalidated transfer reported success"
        );
        for (_, refusal) in ledger.written {
            assert!(
                read_all(refusal).await?.is_empty(),
                "A refused transfer wrote data"
            );
        }
        assert!(client.alive(), "The flood stopped the service");
        Ok(())
    })
}

#[test]
fn private_bus_the_client_watches_for_a_portal_restart_before_it_reads_the_owner_it_calls()
-> anyhow::Result<()> {
    on_private_bus(async |bus| {
        let mut portal = FakePortal::start(&bus).await?;
        let owner = portal
            .connection
            .unique_name()
            .context("Portal name")?
            .to_string();
        let monitor = bus.connect().await?;
        let mut observed = MessageStream::from(&monitor);
        monitor
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus.Monitoring"),
                "BecomeMonitor",
                &(vec!["type='method_call'"], 0_u32),
            )
            .await?;
        let connection = bus.connect().await?;
        let client = connection.unique_name().context("Client name")?.to_string();
        let (opened, registered) = tokio::join!(Portal::open(connection), async {
            let call = portal.expect("Register").await?;
            portal.reply(&call).await
        });
        registered?;
        let (opened, _changes) = opened?;
        ensure!(
            opened.owner.as_str() == owner,
            "The client read another owner"
        );
        let clipboard = opened.proxy(CLIPBOARD).await?;
        let (requested, answered) =
            tokio::join!(clipboard.call_method("RequestClipboard", &()), async {
                let call = portal.expect("RequestClipboard").await?;
                portal.reply(&call).await
            });
        answered?;
        requested?;

        let mut calls = Vec::new();
        while calls
            .last()
            .map(|(member, _, _): &(String, String, String)| member.as_str())
            != Some("RequestClipboard")
        {
            let message = tokio::time::timeout(PATIENCE, observed.next())
                .await
                .context("The monitor missed the client's portal calls")?
                .context("The monitor closed")??;
            let header = message.header();
            if header.sender().map(UniqueName::as_str) != Some(client.as_str()) {
                continue;
            }
            let member = header.member().map_or_default(MemberName::to_string);
            let destination = header.destination().map_or_default(ToString::to_string);
            let rule = message.body().deserialize::<String>().unwrap_or_default();
            calls.push((member, destination, rule));
        }
        let position = |found: &dyn Fn(&(String, String, String)) -> bool| {
            calls
                .iter()
                .position(found)
                .context("The client never made that call")
        };
        let watched = position(&|(member, _, rule)| {
            member == "AddMatch" && rule.contains("NameOwnerChanged")
        })?;
        let read = position(&|(member, _, _)| member == "GetNameOwner")?;
        assert!(
            watched < read,
            "The client read the portal's owner before watching for a restart: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .filter(|(_, destination, _)| destination != "org.freedesktop.DBus")
                .all(|(_, destination, _)| *destination == owner),
            "The client called the portal by a name a restart can move: {calls:?}"
        );
        Ok(())
    })
}
