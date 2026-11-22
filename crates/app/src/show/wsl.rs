use std::{iter, sync::mpsc, thread, time::Duration};

use speakeasy_show::Wsl;

const PATIENCE: Duration = Duration::from_secs(1);

/// Answers from one detached thread that detects WSL at most once.
///
/// Reading the WSL share can block in the redirector while WSL starts, so nothing joins the
/// thread and each caller bounds its own wait.
#[derive(Clone)]
pub(crate) struct Lookup {
    asks: mpsc::Sender<Ask>,
    patience: Duration,
}

enum Ask {
    Prepare,
    Answer(mpsc::SyncSender<Option<Wsl>>),
}

impl Lookup {
    pub(crate) fn native() -> Self {
        Self::spawn(speakeasy_platform::wsl, PATIENCE)
    }

    pub(super) fn spawn(
        detect: impl FnOnce() -> Option<Wsl> + Send + 'static,
        patience: Duration,
    ) -> Self {
        let (asks, questions) = mpsc::channel();
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Nothing joins a lookup that can block for as long as WSL takes to start, and one that cannot start leaves every answer empty"
        )]
        let _ = thread::Builder::new()
            .name("wsl-lookup".to_owned())
            .spawn(move || serve(questions, detect));
        Self { asks, patience }
    }

    pub(crate) fn prepare(&self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A lookup whose thread never started has nothing to prepare"
        )]
        let _ = self.asks.send(Ask::Prepare);
    }

    pub(super) fn found(&self) -> Option<Wsl> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.asks.send(Ask::Answer(reply)).ok()?;
        answer.recv_timeout(self.patience).ok().flatten()
    }
}

fn serve(questions: mpsc::Receiver<Ask>, detect: impl FnOnce() -> Option<Wsl>) {
    // Detect only once asked: reading the share can start WSL, which Speakeasy never does alone.
    let Ok(first) = questions.recv() else {
        return;
    };
    let found = detect();
    for ask in iter::once(first).chain(questions) {
        if let Ask::Answer(reply) = ask {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A caller that stopped waiting has dropped its reply"
            )]
            let _ = reply.try_send(found.clone());
        }
    }
}

#[cfg(test)]
mod tests;
