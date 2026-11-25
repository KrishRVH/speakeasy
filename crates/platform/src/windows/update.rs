//! The update signal: one named manual-reset event per sign-in session. An agent's Show MCP server
//! maps both Speakeasy images, which Windows refuses to overwrite, so the updater sets the event
//! and each server ends, letting the installer replace them.

use std::{
    io,
    os::windows::io::{AsRawHandle, OwnedHandle},
    ptr::{null, null_mut},
};

use windows_sys::Win32::{
    Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, FALSE, TRUE, WAIT_OBJECT_0},
    System::Threading::{
        CreateEventW, GetCurrentProcess, INFINITE, SetEvent, WaitForMultipleObjects,
    },
};

use super::{ok_or_last_error, take_handle};
use crate::OwnedThread;

/// `Local\` scopes the event to this sign-in session, where the user's agents start their servers.
const UPDATE: &str = "Local\\io.github.krvh.speakeasy.update";

/// Runs its callback once an update of this session's Speakeasy begins, including one already
/// under way when the watch starts. Dropping the watch stops waiting.
#[derive(Debug)]
pub struct UpdateWatch {
    stop: OwnedHandle,
    _waiter: OwnedThread,
}

impl Drop for UpdateWatch {
    fn drop(&mut self) {
        // The waiter holds its own duplicate, so setting this one wakes it before the join.
        // SAFETY: `stop` is an open event handle owned by this watch.
        unsafe { SetEvent(self.stop.as_raw_handle()) };
    }
}

pub fn watch_for_update(on_update: impl FnOnce() + Send + 'static) -> io::Result<UpdateWatch> {
    watch(UPDATE, on_update)
}

/// Asks every watching process in this session to end, for as long as `installer` runs, so a
/// server an agent starts again before the installer replaces Speakeasy ends too.
pub fn announce_update(installer: &impl AsRawHandle) -> io::Result<()> {
    announce_for(UPDATE, installer)
}

fn watch(name: &str, on_update: impl FnOnce() + Send + 'static) -> io::Result<UpdateWatch> {
    let update = event(Some(name))?;
    let stop = event(None)?;
    let woken = stop.try_clone()?;
    let waiter = OwnedThread::spawn("update-watch", move || {
        let events = [update.as_raw_handle(), woken.as_raw_handle()];
        // SAFETY: both handles are open events owned by this closure for the whole wait.
        let woke = unsafe { WaitForMultipleObjects(2, events.as_ptr(), FALSE, INFINITE) };
        if woke == WAIT_OBJECT_0 {
            on_update();
        }
    })?;
    Ok(UpdateWatch {
        stop,
        _waiter: waiter,
    })
}

fn announce_for(name: &str, holder: &impl AsRawHandle) -> io::Result<()> {
    let update = event(Some(name))?;
    // SAFETY: `update` is an open event handle owned here.
    ok_or_last_error(unsafe { SetEvent(update.as_raw_handle()) })?;
    let mut held = null_mut();
    // The holder's copy keeps the set event alive until that process exits; it never learns of
    // the copy, so nothing closes it early or passes it on.
    // SAFETY: both process handles and `update` are open for the call; the duplicate belongs to
    // the holder's handle table.
    ok_or_last_error(unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            update.as_raw_handle(),
            holder.as_raw_handle(),
            &raw mut held,
            0,
            FALSE,
            DUPLICATE_SAME_ACCESS,
        )
    })
}

/// Opens the named event, creating it unset when no process holds it, or a private one.
fn event(name: Option<&str>) -> io::Result<OwnedHandle> {
    let name = name.map(|name| name.encode_utf16().chain([0]).collect::<Vec<_>>());
    let name = name.as_ref().map_or(null(), Vec::as_ptr);
    // SAFETY: `name` is null or a NUL-terminated string that outlives the call, and a failed call
    // returns null.
    unsafe { take_handle(CreateEventW(null(), TRUE, FALSE, name)) }
}

#[cfg(test)]
mod tests {
    use std::{
        os::windows::process::CommandExt as _,
        process::{Command, Stdio},
        sync::mpsc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn unique() -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("Local\\speakeasy-update-test-{}-{now}", std::process::id())
    }

    fn signal() -> (impl FnOnce() + Send + 'static, mpsc::Receiver<()>) {
        let (sent, received) = mpsc::channel();
        let notify = move || {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A finished test drops the receiver; nothing is left to tell"
            )]
            let _ = sent.send(());
        };
        (notify, received)
    }

    #[test]
    fn an_update_ends_running_and_starting_watches_until_its_holder_exits() -> io::Result<()> {
        /// Keeps the console host from opening a window for the child.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let name = unique();
        let (notify, running) = signal();
        let watch_running = watch(&name, notify)?;
        assert!(
            running.recv_timeout(Duration::from_millis(50)).is_err(),
            "A watch ran before any update"
        );
        // Stands in for the installer: it waits on stdin until it is ended.
        let mut holder = Command::new("cmd.exe")
            .args(["/d", "/c", "pause"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        let held = announce_for(&name, &holder);
        let outcome = held.and_then(|()| {
            running
                .recv_timeout(crate::PATIENCE)
                .map_err(|_timed_out| io::Error::other("A running watch missed the update"))?;
            // Joined, so only the holder still has the event open.
            drop(watch_running);
            let (notify, starting) = signal();
            let _watch_starting = watch(&name, notify)?;
            starting
                .recv_timeout(crate::PATIENCE)
                .map_err(|_timed_out| {
                    io::Error::other("A watch started while the holder ran missed the update")
                })
        });
        holder.kill()?;
        holder.wait()?;
        outcome?;

        let (notify, after) = signal();
        let _watch_after = watch(&name, notify)?;
        assert!(
            after.recv_timeout(Duration::from_millis(50)).is_err(),
            "The update outlived its holder"
        );
        let (notify, idle) = signal();
        drop(watch(&unique(), notify)?);
        assert!(
            idle.try_recv().is_err(),
            "Dropping a watch ran its callback"
        );
        Ok(())
    }
}
