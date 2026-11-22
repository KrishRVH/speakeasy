use std::{
    mem,
    panic::{self, AssertUnwindSafe},
    thread,
    time::Duration,
};

use anyhow::anyhow;
use async_channel::Receiver;

use super::{
    Delivery, Enter, INSERTION_STOPPED, Inserted, Inserter, Insertion, MODIFIER_WAIT, OwnFocus,
    continuation::Continuation, wait_for_released_modifiers,
};
use crate::{InputSender, InsertPermit, OwnedThread};

pub(crate) struct Worker {
    thread: OwnedThread,
    requests: Receiver<Insertion>,
    input: InputSender,
}

impl Worker {
    pub(crate) fn start(
        input: InputSender,
        own_focus: OwnFocus,
        serve_one: impl FnMut(&Insertion) -> Settled + Send + 'static,
    ) -> std::io::Result<(Self, Inserter)> {
        let (inserter, requests) = Inserter::lane(own_focus);
        let thread = OwnedThread::spawn("insertion", {
            let requests = requests.clone();
            move || serve(&requests, serve_one)
        })?;
        let worker = Self {
            thread,
            requests,
            input,
        };
        Ok((worker, inserter))
    }

    pub(crate) fn request_stop(&self) {
        self.input.cancel();
        self.requests.close();
    }

    pub(crate) fn stopped(&self) -> impl Future<Output = ()> + Send + use<> {
        self.thread.exited()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.request_stop();
    }
}

pub(crate) struct Settled {
    outcome: anyhow::Result<Inserted>,
    restore: Option<Box<dyn FnOnce() -> anyhow::Result<()>>>,
}

impl From<anyhow::Result<Inserted>> for Settled {
    fn from(outcome: anyhow::Result<Inserted>) -> Self {
        Self {
            outcome,
            restore: None,
        }
    }
}

fn serve(requests: &Receiver<Insertion>, mut serve_one: impl FnMut(&Insertion) -> Settled) {
    while let Ok(request) = requests.recv_blocking() {
        let settled = panic::catch_unwind(AssertUnwindSafe(|| serve_one(&request)))
            .unwrap_or_else(|_| Err(anyhow!(INSERTION_STOPPED)).into());
        request.reply.send(settled.outcome);
        if let Some(restore) = settled.restore {
            thread::sleep(RESTORE_AFTER);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The dictation already reported its delivery; a clipboard that cannot be restored keeps the dictated text"
            )]
            let _ = panic::catch_unwind(AssertUnwindSafe(restore));
        }
    }
}

pub(crate) fn settle<N: NativeInsert + 'static>(
    mut native: N,
    request: &Insertion,
    continuation: &mut Continuation<N>,
) -> Settled {
    match deliver(&mut native, request, continuation) {
        Ok((outcome, saved)) => Settled {
            outcome: Ok(outcome),
            restore: saved.map(|saved| {
                Box::new(move || native.restore(Some(saved.contents), &saved.mark))
                    as Box<dyn FnOnce() -> anyhow::Result<()>>
            }),
        },
        Err(error) => Err(error).into(),
    }
}

struct Saved<N: NativeInsert> {
    contents: N::Contents,
    mark: N::Mark,
}

fn deliver<N: NativeInsert>(
    native: &mut N,
    request: &Insertion,
    continuation: &mut Continuation<N>,
) -> anyhow::Result<(Inserted, Option<Saved<N>>)> {
    let patience = match request.enter {
        Enter::Withheld => MODIFIER_WAIT,
        Enter::Pressed => SUBMIT_RELEASE_WAIT,
    };
    wait_for_released_modifiers(
        &request.permit,
        patience,
        || Ok(!native.shortcut_released()),
    )?;
    let previous = mem::take(continuation);
    let text = previous.lead(native, &request.text);
    let (inserted, saved) = insert(
        native,
        &text,
        &request.permit,
        request.delivery,
        &request.own_focus,
    )?;
    if request.enter == Enter::Pressed && inserted == Inserted::Sent {
        let target = native.target();
        thread::sleep(SUBMIT_SETTLE);
        return Ok((submit(native, &request.own_focus, target), None));
    }
    *continuation = previous.follow(native, &text, &inserted);
    Ok((inserted, saved))
}

const SUBMIT_SETTLE: Duration = Duration::from_millis(100);

const SUBMIT_RELEASE_WAIT: Duration = Duration::from_secs(5);

const RESTORE_AFTER: Duration = Duration::from_millis(500);

const RETURN_WITHHELD: Inserted = Inserted::Copied(
    "Your words are in, but Speakeasy couldn't press Return. Press Return to send them.",
);

pub(crate) trait NativeInsert {
    type Mark;

    type Keys;

    type Contents;

    type Target: PartialEq;

    type Activity: PartialEq;

    fn copy(&mut self, text: &str) -> anyhow::Result<(Self::Mark, Option<Self::Contents>)>;

    fn restore(
        &mut self,
        contents: Option<Self::Contents>,
        mark: &Self::Mark,
    ) -> anyhow::Result<()>;

    fn paste_keys(&mut self) -> anyhow::Result<Self::Keys>;

    fn text_keys(&mut self, text: &str) -> anyhow::Result<Self::Keys>;

    fn return_keys(&mut self) -> anyhow::Result<Self::Keys>;

    fn external_focus(&mut self) -> bool;

    fn target(&mut self) -> Option<Self::Target>;

    fn activity(&mut self) -> Option<Self::Activity>;

    fn shortcut_released(&mut self) -> bool;

    fn send(&mut self, keys: Self::Keys) -> anyhow::Result<Inserted>;
}

type Written<N> = (
    <N as NativeInsert>::Mark,
    Option<<N as NativeInsert>::Contents>,
);

fn insert<N: NativeInsert>(
    native: &mut N,
    text: &str,
    permit: &InsertPermit,
    delivery: Delivery,
    own_focus: &OwnFocus,
) -> anyhow::Result<(Inserted, Option<Saved<N>>)> {
    if !permit.active() {
        return Ok((Inserted::Cancelled, None));
    }
    let staged = match delivery {
        Delivery::Paste => native
            .paste_keys()
            .and_then(|keys| Ok((keys, Some(native.copy(text)?)))),
        Delivery::Direct => native.text_keys(text).map(|keys| (keys, None)),
    };
    let (keys, clipboard) = match staged {
        Ok(staged) => staged,
        Err(_) if !permit.active() => return Ok((Inserted::Cancelled, None)),
        Err(error) => return Err(error),
    };
    let preflight = delivery.preflight(
        !own_focus.held() && native.external_focus(),
        native.shortcut_released(),
    );
    if !permit.active() {
        restore_now(native, clipboard);
        return Ok((Inserted::Cancelled, None));
    }
    if let Some(outcome) = preflight {
        return Ok((outcome, None));
    }
    if !permit.commit() {
        restore_now(native, clipboard);
        return Ok((Inserted::Cancelled, None));
    }
    let inserted = native.send(keys)?;
    let saved = clipboard
        .filter(|_| inserted == Inserted::Sent)
        .and_then(|(mark, contents)| {
            Some(Saved {
                contents: contents?,
                mark,
            })
        });
    Ok((inserted, saved))
}

fn restore_now<N: NativeInsert>(native: &mut N, clipboard: Option<Written<N>>) {
    if let Some((mark, contents)) = clipboard {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Restoring after an unsent paste is best effort; the dictation reports the cancellation that stopped it, and a clipboard that cannot be reopened keeps the dictated copy"
        )]
        let _ = native.restore(contents, &mark);
    }
}

fn submit<N: NativeInsert>(
    native: &mut N,
    own_focus: &OwnFocus,
    target: Option<N::Target>,
) -> Inserted {
    if target.is_none()
        || own_focus.held()
        || !native.external_focus()
        || native.target() != target
        || !native.shortcut_released()
    {
        return RETURN_WITHHELD;
    }
    match native.return_keys().and_then(|keys| native.send(keys)) {
        Ok(Inserted::Sent) => Inserted::Sent,
        Ok(Inserted::Copied(_) | Inserted::Cancelled | Inserted::Unavailable(_)) | Err(_) => {
            RETURN_WITHHELD
        },
    }
}

#[cfg(test)]
mod tests;
