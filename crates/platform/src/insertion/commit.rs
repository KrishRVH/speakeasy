use std::panic::{self, AssertUnwindSafe};

use anyhow::anyhow;
use async_channel::Receiver;

use super::{
    Delivery, INSERTION_STOPPED, Inserted, Inserter, Insertion, OwnFocus,
    wait_for_released_modifiers,
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
        serve_one: impl Fn(&Insertion) -> anyhow::Result<Inserted> + Send + 'static,
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

fn serve(
    requests: &Receiver<Insertion>,
    serve_one: impl Fn(&Insertion) -> anyhow::Result<Inserted>,
) {
    while let Ok(request) = requests.recv_blocking() {
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| serve_one(&request)))
            .unwrap_or_else(|_| Err(anyhow!(INSERTION_STOPPED)));
        request.reply.send(outcome);
    }
}

pub(crate) fn deliver<N: NativeInsert>(
    native: &mut N,
    request: &Insertion,
) -> anyhow::Result<Inserted> {
    wait_for_released_modifiers(&request.permit, || Ok(!native.shortcut_released()))?;
    insert(
        native,
        &request.text,
        &request.permit,
        request.delivery,
        &request.own_focus,
    )
}

const CLIPBOARD_CHANGED: Inserted = Inserted::Unavailable(
    "The clipboard changed, so the paste was not sent and its new contents were kept.",
);

const CLIPBOARD_CHANGED_BEFORE_PASTE: Inserted =
    Inserted::Unavailable("The clipboard changed before pasting, and its new contents were kept.");

pub(crate) trait NativeInsert {
    type Mark: PartialEq;

    type Lease;

    type Keys;

    fn acquire(&mut self) -> anyhow::Result<Self::Lease>;

    fn write(&mut self, lease: Self::Lease, text: &str) -> anyhow::Result<()>;

    fn mark(&mut self) -> Self::Mark;

    fn holds(&mut self, text: &str, mark: &Self::Mark) -> anyhow::Result<bool>;

    fn paste_keys(&mut self) -> anyhow::Result<Self::Keys>;

    fn text_keys(&mut self, text: &str) -> anyhow::Result<Self::Keys>;

    fn external_focus(&mut self) -> bool;

    fn shortcut_released(&mut self) -> bool;

    fn send(&mut self, keys: Self::Keys) -> anyhow::Result<Inserted>;
}

pub(crate) fn insert<N: NativeInsert>(
    native: &mut N,
    text: &str,
    permit: &InsertPermit,
    delivery: Delivery,
    own_focus: &OwnFocus,
) -> anyhow::Result<Inserted> {
    if !permit.active() {
        return Ok(Inserted::Cancelled);
    }
    let keys = match delivery {
        Delivery::Paste => {
            let lease = native.acquire()?;

            if !permit.active() {
                return Ok(Inserted::Cancelled);
            }
            native.write(lease, text)?;
            let mark = native.mark();
            if !native.holds(text, &mark)? {
                return Ok(CLIPBOARD_CHANGED);
            }
            let keys = native.paste_keys()?;
            if let Some(outcome) = delivery.preflight(
                !own_focus.held() && native.external_focus(),
                native.shortcut_released(),
            ) {
                return Ok(outcome);
            }
            if native.mark() != mark {
                return Ok(CLIPBOARD_CHANGED_BEFORE_PASTE);
            }
            keys
        },
        Delivery::Direct => {
            let keys = native.text_keys(text)?;
            if let Some(outcome) = delivery.preflight(
                !own_focus.held() && native.external_focus(),
                native.shortcut_released(),
            ) {
                return Ok(outcome);
            }
            keys
        },
    };
    if !permit.commit() {
        return Ok(Inserted::Cancelled);
    }
    native.send(keys)
}

#[cfg(test)]
mod tests;
