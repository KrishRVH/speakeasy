use super::*;
use async_channel::{Receiver, Sender};
use speakeasy_platform::Inserted;
use tokio::sync::oneshot;

type InsertControls = (Sender<speakeasy_platform::InsertPermit>, Receiver<()>);
struct FakePorts {
    desktop_pending: bool,
    load_results: Option<Receiver<anyhow::Result<()>>>,
    delayed_finish: bool,
    record_error: Option<&'static str>,
    insertion: Option<InsertControls>,
    captures: Sender<(SessionId, Sender<Event>)>,
    retirement: Option<(Sender<()>, Receiver<()>)>,
    jobs: Sender<(String, oneshot::Sender<anyhow::Result<String>>)>,
    pasted: Sender<String>,
    loads: Sender<()>,
    stop: Option<(Sender<()>, Receiver<()>)>,
    recovery: Option<Receiver<anyhow::Result<()>>>,
}
struct FakeRecording {
    delayed_finish: bool,
    retirement: Option<(Sender<()>, Receiver<()>)>,
    id: SessionId,
    events: Sender<Event>,
}
impl Recording for FakeRecording {
    async fn retire(self) {
        if let Some((entered, release)) = self.retirement {
            let _ = entered.send(()).await;
            // Harness disposal closes this gate on assertion failure.
            let _ = release.recv().await;
        }
    }
    fn finish(&self) {
        if self.delayed_finish {
            return;
        }
        assert!(
            self.events
                .try_send(Event::AudioDone(self.id, Ok(Some(vec![1, 2]))))
                .is_ok()
        );
    }
}
struct FakeSpeech {
    jobs: Sender<(String, oneshot::Sender<anyhow::Result<String>>)>,
    stop: Option<(Sender<()>, Receiver<()>)>,
    recovery: Option<Receiver<anyhow::Result<()>>>,
}
impl Speech for FakeSpeech {
    async fn transcribe(&self, _: Vec<u8>, language: &str) -> anyhow::Result<String> {
        let (reply, result) = oneshot::channel();
        self.jobs.send((language.to_owned(), reply)).await?;
        result.await?
    }
    async fn idle(&self) -> anyhow::Result<()> {
        match &self.recovery {
            Some(result) => result.recv().await?,
            None => Ok(()),
        }
    }
    async fn stop(&mut self) {
        if let Some((entered, release)) = &self.stop {
            let _ = entered.send(()).await;
            let _ = release.recv().await;
        }
    }
}
impl Ports for FakePorts {
    fn prepares_desktop(&self) -> bool {
        self.desktop_pending
    }
    type Recording = FakeRecording;
    type Speech = FakeSpeech;
    fn record(
        &self,
        id: SessionId,
        _: Option<String>,
        events: Sender<Event>,
    ) -> anyhow::Result<FakeRecording> {
        if let Some(error) = self.record_error {
            anyhow::bail!(error);
        }
        self.captures.try_send((id, events.clone()))?;
        events.try_send(Event::Ready(id))?;
        Ok(FakeRecording {
            delayed_finish: self.delayed_finish,
            id,
            events,
            retirement: self.retirement.clone(),
        })
    }
    fn load(
        &self,
        _: Config,
        mut cancelled: watch::Receiver<bool>,
    ) -> impl std::future::Future<Output = anyhow::Result<FakeSpeech>> + Send + 'static {
        let load_results = self.load_results.clone();
        let jobs = self.jobs.clone();
        let loads = self.loads.clone();
        let stop = self.stop.clone();
        let recovery = self.recovery.clone();
        async move {
            loads.send(()).await?;
            if let Some(results) = load_results {
                tokio::select! {
                    biased;
                    _ = cancelled.changed() => anyhow::bail!("Fake model load cancelled"),
                    result = results.recv() => result??,
                }
            }
            Ok(FakeSpeech {
                jobs,
                stop,
                recovery,
            })
        }
    }
    fn insert(
        &self,
        text: String,
        gate: speakeasy_platform::InsertPermit,
        _: bool,
    ) -> impl std::future::Future<Output = anyhow::Result<Inserted>> + Send + 'static {
        let pasted = self.pasted.clone();
        let insertion = self.insertion.clone();
        async move {
            if let Some((entered, release)) = insertion {
                entered.send(gate.clone()).await?;
                release.recv().await?;
            }
            if !gate.commit() {
                return Ok(Inserted::Cancelled);
            }
            pasted.try_send(text)?;
            Ok(Inserted::Sent)
        }
    }
}
struct Harness {
    cleanup: Vec<Receiver<()>>,
    runtime: Runtime,
    output: watch::Sender<Snapshot>,
    updates: watch::Receiver<Snapshot>,
    captures: Receiver<(SessionId, Sender<Event>)>,
    jobs: Receiver<(String, oneshot::Sender<anyhow::Result<String>>)>,
    pasted: Receiver<String>,
    loads: Receiver<()>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.runtime.request_stop();
        // Unblock only fakes on failed assertions, independently of paused time.
        for gate in &self.cleanup {
            gate.close();
        }
    }
}
impl Harness {
    fn new() -> anyhow::Result<Self> {
        Self::with_controls(None, None)
    }
    fn with_controls(
        stop: Option<(Sender<()>, Receiver<()>)>,
        recovery: Option<Receiver<anyhow::Result<()>>>,
    ) -> anyhow::Result<Self> {
        Self::with_insertion(stop, recovery, None)
    }
    fn with_insertion(
        stop: Option<(Sender<()>, Receiver<()>)>,
        recovery: Option<Receiver<anyhow::Result<()>>>,
        insertion: Option<InsertControls>,
    ) -> anyhow::Result<Self> {
        Self::with_options(Config::default(), stop, recovery, insertion, None)
    }
    fn with_options(
        config: Config,
        stop: Option<(Sender<()>, Receiver<()>)>,
        recovery: Option<Receiver<anyhow::Result<()>>>,
        insertion: Option<InsertControls>,
        retirement: Option<(Sender<()>, Receiver<()>)>,
    ) -> anyhow::Result<Self> {
        let (capture_tx, captures) = async_channel::bounded(8);
        let (job_tx, jobs) = async_channel::bounded(8);
        let (paste_tx, pasted) = async_channel::bounded(8);
        let (load_tx, loads) = async_channel::bounded(8);
        let (output, updates) = watch::channel(Snapshot::default());
        let cleanup = [&stop, &retirement]
            .into_iter()
            .flatten()
            .map(|(_, release)| release.clone())
            .chain(insertion.iter().map(|(_, release)| release.clone()))
            .collect();
        let runtime = Runtime::start_with(
            config,
            output.clone(),
            FakePorts {
                desktop_pending: false,
                load_results: None,
                delayed_finish: false,
                record_error: None,
                retirement,
                insertion,
                captures: capture_tx,
                jobs: job_tx,
                pasted: paste_tx,
                loads: load_tx,
                stop,
                recovery,
            },
        )?;
        Ok(Self {
            cleanup,
            output,
            runtime,
            updates,
            captures,
            jobs,
            pasted,
            loads,
        })
    }
    fn input(&self, input: Input) {
        speakeasy_platform::deliver(&self.runtime.input, input);
    }
    async fn phase(&mut self, wanted: Phase) -> anyhow::Result<()> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if self.updates.borrow_and_update().phase == wanted {
                    return Ok::<_, anyhow::Error>(());
                }
                self.updates.changed().await?;
            }
        })
        .await??;
        Ok(())
    }
    async fn start(&mut self) -> anyhow::Result<(SessionId, Sender<Event>)> {
        self.input(Input::Toggle);
        self.phase(Phase::Recording).await?;
        receive(&self.captures).await
    }
    async fn finish(&mut self) -> anyhow::Result<oneshot::Sender<anyhow::Result<String>>> {
        self.input(Input::Toggle);
        let (language, reply) = receive(&self.jobs).await?;
        assert_eq!(language, self.runtime.configuration.borrow().language);
        Ok(reply)
    }
}
async fn receive<T>(receiver: &Receiver<T>) -> anyhow::Result<T> {
    Ok(tokio::time::timeout(Duration::from_secs(2), receiver.recv()).await??)
}

// Exercise the production owner inside this test's clock. Production keeps its
// dedicated thread; a separate runtime would not observe time::advance here.
struct PausedHarness {
    harness: Harness,
    owner: tokio::task::JoinHandle<()>,
}

impl PausedHarness {
    fn new(desktop_pending: bool, record_error: Option<&'static str>) -> Self {
        Self::with_ports(desktop_pending, record_error, |_| {})
    }
    fn with_ports(
        desktop_pending: bool,
        record_error: Option<&'static str>,
        customize: impl FnOnce(&mut FakePorts),
    ) -> Self {
        let (capture_tx, captures) = async_channel::bounded(8);
        let (job_tx, jobs) = async_channel::bounded(8);
        let (paste_tx, pasted) = async_channel::bounded(8);
        let (load_tx, loads) = async_channel::bounded(8);
        let (output, updates) = watch::channel(Snapshot::default());
        let (sender, input) = async_channel::bounded(64);
        let gate = speakeasy_platform::InputSender::new(sender);
        let (configuration, changes) = watch::channel(Config::default());
        let (stop, stopping) = async_channel::bounded(1);
        let (complete, finished) = async_channel::bounded(1);
        let owner_gate = gate.clone();
        let mut ports = FakePorts {
            desktop_pending,
            record_error,
            load_results: None,
            delayed_finish: false,
            insertion: None,
            captures: capture_tx,
            retirement: None,
            jobs: job_tx,
            pasted: paste_tx,
            loads: load_tx,
            stop: None,
            recovery: None,
        };
        customize(&mut ports);
        let epoch = output.borrow().epoch;
        let publisher = output.clone();
        let owner = tokio::spawn(async move {
            run(changes, input, output, owner_gate, stopping, ports, epoch).await;
            let _ = complete.try_send(());
        });
        Self {
            owner,
            harness: Harness {
                cleanup: Vec::new(),
                output: publisher,
                runtime: Runtime {
                    input: gate,
                    thread: None,
                    configuration,
                    stop,
                    finished,
                },
                updates,
                captures,
                jobs,
                pasted,
                loads,
            },
        }
    }

    async fn close(self) -> anyhow::Result<()> {
        self.harness.runtime.request_stop();
        self.harness.runtime.stopped().await;
        Ok(())
    }
}

impl Drop for PausedHarness {
    fn drop(&mut self) {
        // Assertion failures cannot leave fake jobs alive after their owner.
        self.harness.runtime.request_stop();
        self.owner.abort();
    }
}

async fn observed(
    updates: &mut watch::Receiver<Snapshot>,
    predicate: impl Fn(&Snapshot) -> bool,
) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if predicate(&updates.borrow_and_update()) {
                return Ok::<_, anyhow::Error>(());
            }
            updates.changed().await?;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn desktop_readiness_blocks_capture_until_native_startup_completes() -> anyhow::Result<()> {
    let mut paused = PausedHarness::new(true, None);
    let h = &mut paused.harness;
    receive(&h.loads).await?;
    h.input(Input::Press);
    h.input(Input::Toggle);
    h.input(Input::DesktopReady {
        shortcut: "fixture shortcut".into(),
        cancel: "fixture cancel".into(),
    });
    observed(&mut h.updates, |s| s.desktop_ready).await?;
    assert!(h.captures.try_recv().is_err());
    assert_eq!(&*h.updates.borrow().shortcut, "fixture shortcut");
    assert_eq!(&*h.updates.borrow().cancel_shortcut, "fixture cancel");
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    paused.close().await
}

#[tokio::test(start_paused = true)]
async fn held_shortcut_release_finishes_through_the_owner() -> anyhow::Result<()> {
    let mut paused = PausedHarness::new(false, None);
    let h = &mut paused.harness;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    tokio::time::advance(Duration::from_millis(250)).await;
    h.input(Input::Release);
    let (_, reply) = receive(&h.jobs).await?;
    assert!(reply.send(Ok(String::new())).is_ok());
    h.phase(Phase::Empty).await?;
    assert!(h.pasted.try_recv().is_err());
    paused.close().await
}

#[tokio::test(start_paused = true)]
async fn locking_a_hold_keeps_the_same_capture_until_the_next_press() -> anyhow::Result<()> {
    let mut paused = PausedHarness::new(false, None);
    let h = &mut paused.harness;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    h.input(Input::Lock);
    observed(&mut h.updates, |s| s.hands_free).await?;
    h.input(Input::Release);
    events.send(Event::Level(id, 0.25)).await?;
    observed(&mut h.updates, |s| s.level == 0.25).await?;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(h.jobs.try_recv().is_err());
    assert!(h.captures.try_recv().is_err());
    h.input(Input::Press);
    receive(&h.jobs).await?;
    paused.close().await
}

#[tokio::test(start_paused = true)]
async fn double_tap_uses_one_capture_and_single_tap_obeys_its_deadline() -> anyhow::Result<()> {
    let mut paused = PausedHarness::new(false, None);
    let h = &mut paused.harness;
    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    tokio::time::advance(Duration::from_millis(50)).await;
    h.input(Input::Release);
    events.send(Event::Level(id, 0.25)).await?;
    observed(&mut h.updates, |s| s.level == 0.25).await?;
    tokio::time::advance(Duration::from_millis(100)).await;
    h.input(Input::Press);
    observed(&mut h.updates, |s| s.hands_free).await?;
    assert!(h.captures.try_recv().is_err());
    assert!(h.jobs.try_recv().is_err());
    h.input(Input::Release);
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;

    h.input(Input::Press);
    h.phase(Phase::Recording).await?;
    let (id, events) = receive(&h.captures).await?;
    tokio::time::advance(Duration::from_millis(50)).await;
    h.input(Input::Release);
    events.send(Event::Level(id, 0.5)).await?;
    observed(&mut h.updates, |s| s.level == 0.5).await?;
    tokio::time::advance(Duration::from_millis(299)).await;
    assert!(h.jobs.try_recv().is_err());
    tokio::time::advance(Duration::from_millis(1)).await;
    receive(&h.jobs).await?;
    paused.close().await
}

#[tokio::test(start_paused = true)]
async fn owner_finishes_capture_at_the_five_minute_deadline() -> anyhow::Result<()> {
    let mut paused = PausedHarness::new(false, None);
    let h = &mut paused.harness;
    h.start().await?;
    tokio::time::advance(speakeasy_core::gesture::RECORDING_LIMIT - Duration::from_millis(1)).await;
    assert!(h.jobs.try_recv().is_err());
    tokio::time::advance(Duration::from_millis(1)).await;
    receive(&h.jobs).await?;
    assert!(h.captures.try_recv().is_err());
    paused.close().await
}

#[tokio::test(start_paused = true)]
async fn failed_capture_start_preserves_its_reason_and_allows_shutdown() -> anyhow::Result<()> {
    let mut paused = PausedHarness::new(false, Some("thread resources exhausted"));
    let h = &mut paused.harness;
    h.input(Input::Press);
    h.phase(Phase::Error).await?;
    assert!(
        h.updates
            .borrow()
            .message
            .contains("thread resources exhausted")
    );
    assert!(h.captures.try_recv().is_err());
    paused.close().await
}

#[tokio::test]
async fn cancelled_capture_retires_before_retry_and_pause_waits_for_it() -> anyhow::Result<()> {
    let (entered, retiring) = async_channel::bounded(2);
    let (release, waiting) = async_channel::bounded(2);
    let mut h = Harness::with_options(
        Config::default(),
        None,
        None,
        None,
        Some((entered, waiting)),
    )?;
    h.start().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    receive(&retiring).await?;
    h.input(Input::Toggle);
    h.phase(Phase::Starting).await?;
    assert!(
        h.captures.try_recv().is_err(),
        "Retry overlapped a retiring microphone"
    );
    release.send(()).await?;
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.runtime.request_stop();
    receive(&retiring).await?;
    let stopped = h.runtime.stopped();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), stopped)
            .await
            .is_err()
    );
    release.send(()).await?;
    tokio::time::timeout(Duration::from_secs(2), h.runtime.stopped()).await?;
    Ok(())
}

#[tokio::test]
async fn pause_during_a_queued_capture_never_opens_the_new_device() -> anyhow::Result<()> {
    let (entered, retiring) = async_channel::bounded(1);
    let (release, waiting) = async_channel::bounded(1);
    let mut h = Harness::with_options(
        Config::default(),
        None,
        None,
        None,
        Some((entered, waiting)),
    )?;
    h.start().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    receive(&retiring).await?;
    h.input(Input::Toggle);
    h.phase(Phase::Starting).await?;
    h.runtime.request_stop();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), h.runtime.stopped())
            .await
            .is_err()
    );
    release.send(()).await?;
    tokio::time::timeout(Duration::from_secs(2), h.runtime.stopped()).await?;
    assert!(h.captures.try_recv().is_err());
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn completed_audio_does_not_allow_retry_before_device_teardown() -> anyhow::Result<()> {
    let (entered, retiring) = async_channel::bounded(2);
    let (release, waiting) = async_channel::bounded(2);
    let mut h = Harness::with_options(
        Config::default(),
        None,
        None,
        None,
        Some((entered, waiting)),
    )?;
    h.start().await?;
    assert!(h.finish().await?.send(Ok("fixture".into())).is_ok());
    receive(&retiring).await?;
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;
    h.input(Input::Toggle);
    h.phase(Phase::Starting).await?;
    assert!(h.captures.try_recv().is_err());
    release.send(()).await?;
    h.phase(Phase::Recording).await?;
    receive(&h.captures).await?;
    h.runtime.request_stop();
    receive(&retiring).await?;
    release.send(()).await?;
    tokio::time::timeout(Duration::from_secs(2), h.runtime.stopped()).await?;
    Ok(())
}

#[tokio::test]
async fn pause_starts_capture_and_model_cleanup_together() -> anyhow::Result<()> {
    let (capture_entered, captures) = async_channel::bounded(1);
    let (capture_release, capture_waiting) = async_channel::bounded(1);
    let (speech_entered, speech) = async_channel::bounded(1);
    let (speech_release, speech_waiting) = async_channel::bounded(1);
    let mut h = Harness::with_options(
        Config::default(),
        Some((speech_entered, speech_waiting)),
        None,
        None,
        Some((capture_entered, capture_waiting)),
    )?;
    receive(&h.loads).await?;
    h.start().await?;
    h.runtime.request_stop();
    receive(&captures).await?;
    receive(&speech).await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), h.runtime.stopped())
            .await
            .is_err()
    );
    capture_release.send(()).await?;
    speech_release.send(()).await?;
    tokio::time::timeout(Duration::from_secs(2), h.runtime.stopped()).await?;
    Ok(())
}

#[tokio::test]
async fn cpu_cancellation_reaps_before_replacement_without_blocking_input() -> anyhow::Result<()> {
    let (entered, stopping) = async_channel::bounded(2);
    let (release, waiting) = async_channel::bounded(2);
    let mut h = Harness::with_options(
        Config {
            use_gpu: false,
            ..Config::default()
        },
        Some((entered, waiting)),
        None,
        None,
        None,
    )?;
    receive(&h.loads).await?;
    h.start().await?;
    let mut old_reply = h.finish().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    receive(&stopping).await?;
    tokio::time::timeout(Duration::from_secs(2), old_reply.closed()).await?;
    assert!(old_reply.send(Ok("stale fixture".into())).is_err());
    assert!(
        h.loads.try_recv().is_err(),
        "Replacement started before old worker exit"
    );
    h.start().await?;
    h.input(Input::Toggle);
    h.phase(Phase::Processing).await?;
    assert!(h.jobs.try_recv().is_err());
    release.send(()).await?;
    receive(&h.loads).await?;
    let (_, reply) = receive(&h.jobs).await?;
    assert!(reply.send(Ok("current fixture".into())).is_ok());
    assert_eq!(receive(&h.pasted).await?, "current fixture");
    h.phase(Phase::Done).await?;
    release.send(()).await?;
    Ok(())
}

#[tokio::test]
async fn preparing_insertion_cancels_without_blocking_a_new_recording() -> anyhow::Result<()> {
    let (entered, preparing) = async_channel::bounded(2);
    let (release, waiting) = async_channel::bounded(2);
    let mut h = Harness::with_insertion(None, None, Some((entered, waiting)))?;
    h.start().await?;
    assert!(h.finish().await?.send(Ok("old fixture".into())).is_ok());
    let old = receive(&preparing).await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    h.start().await?;
    assert!(!old.active(), "A newer capture authorized stale insertion");
    release.send(()).await?;
    assert!(h.finish().await?.send(Ok("new fixture".into())).is_ok());
    let next = receive(&preparing).await?;
    assert!(next.active());
    assert!(!old.commit());
    release.send(()).await?;
    assert_eq!(receive(&h.pasted).await?, "new fixture");
    h.phase(Phase::Done).await?;
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn shutdown_revokes_and_owns_pending_insertion_cleanup() -> anyhow::Result<()> {
    let (entered, preparing) = async_channel::bounded(1);
    let (release, waiting) = async_channel::bounded(1);
    let mut h = Harness::with_insertion(None, None, Some((entered, waiting)))?;
    h.start().await?;
    assert!(h.finish().await?.send(Ok("fixture".into())).is_ok());
    let permit = receive(&preparing).await?;
    h.runtime.request_stop();
    assert!(!permit.active());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), h.runtime.stopped())
            .await
            .is_err()
    );
    release.send(()).await?;
    tokio::time::timeout(Duration::from_secs(2), h.runtime.stopped()).await?;
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn silence_and_empty_recognition_never_report_submission() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    let (id, events) = h.start().await?;
    events.send(Event::AudioDone(id, Ok(None))).await?;
    h.phase(Phase::Empty).await?;
    assert!(h.jobs.try_recv().is_err());
    assert!(h.pasted.try_recv().is_err());
    h.start().await?;
    assert!(h.finish().await?.send(Ok(String::new())).is_ok());
    h.phase(Phase::Empty).await?;
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn dictation_inserts_once_and_request_preferences_keep_the_warm_worker() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    h.start().await?;
    assert!(h.finish().await?.send(Ok("fixture one".into())).is_ok());
    assert_eq!(receive(&h.pasted).await?, "fixture one");
    h.phase(Phase::Done).await?;
    h.input(Input::Cancel);
    let config = Config {
        reduced_motion: true,
        preserve_clipboard: true,
        language: "auto".into(),
        ..Config::default()
    };
    h.runtime.configure(config);
    h.phase(Phase::Idle).await?;
    h.start().await?;
    assert!(h.finish().await?.send(Ok("fixture two".into())).is_ok());
    assert_eq!(receive(&h.pasted).await?, "fixture two");
    h.phase(Phase::Done).await?;
    assert!(
        h.loads.try_recv().is_err(),
        "Request preferences reloaded the model"
    );
    assert!(h.pasted.try_recv().is_err(), "A result was submitted twice");
    let mut config = h.runtime.configuration.borrow().clone();
    config.engine = crate::config::Engine::Parakeet;
    h.runtime.configure(config);
    receive(&h.loads).await?;
    h.start().await?;
    assert!(h.finish().await?.send(Ok("new engine".into())).is_ok());
    assert_eq!(receive(&h.pasted).await?, "new engine");
    Ok(())
}

#[tokio::test]
async fn cancelled_inference_and_late_audio_cannot_affect_the_next_session() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    let (old_id, old_events) = h.start().await?;
    let mut old_reply = h.finish().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    tokio::time::timeout(Duration::from_secs(2), old_reply.closed()).await?;
    assert!(
        old_reply.send(Ok("discard this".into())).is_err(),
        "Cancelled inference stayed alive"
    );
    let (new_id, _) = h.start().await?;
    assert_ne!(new_id, old_id);
    old_events
        .send(Event::AudioDone(
            old_id,
            Err(anyhow::anyhow!("old device failed")),
        ))
        .await?;
    assert!(h.finish().await?.send(Ok("new session".into())).is_ok());
    assert_eq!(receive(&h.pasted).await?, "new session");
    h.phase(Phase::Done).await?;
    assert!(
        h.loads.try_recv().is_err(),
        "Cancelled GPU inference reloaded a healthy model"
    );
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn failures_recover_the_worker_and_hook_loss_stops_without_inserting() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    let (id, events) = h.start().await?;
    events
        .send(Event::AudioDone(
            id,
            Err(anyhow::anyhow!("Microphone disconnected")),
        ))
        .await?;
    h.phase(Phase::Error).await?;
    assert!(
        h.updates
            .borrow()
            .message
            .contains("Microphone disconnected")
    );
    h.start().await?;
    assert!(
        h.finish()
            .await?
            .send(Err(anyhow::anyhow!("Fixture request failed")))
            .is_ok()
    );
    h.phase(Phase::Error).await?;
    receive(&h.loads).await?;
    assert!(
        h.updates
            .borrow()
            .message
            .contains("Fixture request failed")
    );
    h.start().await?;
    h.runtime.input.close();
    h.phase(Phase::Error).await?;
    assert!(!h.runtime.is_running());
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}
#[tokio::test]
async fn failed_recovery_stops_worker_before_loading_replacement() -> anyhow::Result<()> {
    let (entered, stopping) = async_channel::bounded(2);
    let (release, waiting) = async_channel::bounded(2);
    let (recover, recovery) = async_channel::bounded(1);
    let mut h = Harness::with_controls(Some((entered, waiting)), Some(recovery))?;
    receive(&h.loads).await?;
    h.start().await?;
    let mut old_reply = h.finish().await?;
    h.input(Input::Cancel);
    h.phase(Phase::Cancelled).await?;
    tokio::time::timeout(Duration::from_secs(2), old_reply.closed()).await?;
    assert!(old_reply.send(Ok("discard this".into())).is_err());
    recover
        .send(Err(anyhow::anyhow!("Recovery failed")))
        .await?;
    receive(&stopping).await?;
    assert!(
        h.loads.try_recv().is_err(),
        "Replacement overlapped worker cleanup"
    );
    h.start().await?;
    h.input(Input::Toggle);
    h.phase(Phase::Processing).await?;
    assert!(
        h.jobs.try_recv().is_err(),
        "Inference began before recovery finished"
    );
    release.send(()).await?;
    receive(&h.loads).await?;
    let (_, reply) = receive(&h.jobs).await?;
    assert!(reply.send(Ok("after recovery".into())).is_ok());
    assert_eq!(receive(&h.pasted).await?, "after recovery");
    h.phase(Phase::Done).await?;
    assert!(h.pasted.try_recv().is_err());
    release.send(()).await?; // Permit final native cleanup when the harness drops.
    Ok(())
}

#[tokio::test]
async fn stop_cancels_input_without_waiting_for_native_cleanup() -> anyhow::Result<()> {
    let (entered, stopping) = async_channel::bounded(1);
    let (release, waiting) = async_channel::bounded(1);
    let mut h = Harness::with_controls(Some((entered, waiting)), None)?;
    h.start().await?;
    assert!(h.finish().await?.send(Ok("fixture".into())).is_ok());
    receive(&h.pasted).await?;
    h.phase(Phase::Done).await?;

    h.start().await?;
    h.runtime.request_stop();
    receive(&stopping).await?;
    assert!(h.runtime.input.is_closed());
    assert!(!h.runtime.input.active());
    let mut stopped = std::pin::pin!(h.runtime.stopped());
    assert!(
        std::future::Future::poll(
            stopped.as_mut(),
            &mut std::task::Context::from_waker(std::task::Waker::noop()),
        )
        .is_pending(),
        "Completion was reported before native cleanup finished"
    );
    release.send(()).await?;
    tokio::time::timeout(Duration::from_secs(2), stopped).await?;
    assert!(h.pasted.try_recv().is_err());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn warmup_failure_while_stopping_retains_capture_and_retries_on_demand() -> anyhow::Result<()>
{
    let (results, loading) = async_channel::bounded(2);
    let mut h = PausedHarness::with_ports(false, None, |ports| {
        ports.load_results = Some(loading);
        ports.delayed_finish = true;
    });
    receive(&h.harness.loads).await?;
    let (id, events) = h.harness.start().await?;
    h.harness.input(Input::Toggle);
    h.harness.phase(Phase::Stopping).await?;
    results
        .send(Err(anyhow::anyhow!("Model warmup failed")))
        .await?;
    observed(&mut h.harness.updates, |s| {
        s.message == "Model warmup failed"
    })
    .await?;
    assert!(matches!(h.harness.updates.borrow().phase, Phase::Stopping));
    h.harness.input(Input::Press);
    events.send(Event::Level(id, 0.4)).await?;
    observed(&mut h.harness.updates, |s| s.level == 0.4).await?;
    assert!(h.harness.captures.try_recv().is_err());
    assert!(
        h.harness.loads.try_recv().is_err(),
        "Failed warmup must not loop"
    );
    events
        .send(Event::AudioDone(id, Ok(Some(vec![1, 2]))))
        .await?;
    receive(&h.harness.loads).await?;
    results.send(Ok(())).await?;
    let (_, reply) = receive(&h.harness.jobs).await?;
    assert!(reply.send(Ok("fixture".into())).is_ok());
    assert_eq!(receive(&h.harness.pasted).await?, "fixture");
    h.harness.phase(Phase::Done).await?;
    assert!(h.harness.updates.borrow().message.is_empty());
    h.close().await?;
    Ok(())
}

#[tokio::test]
async fn configuration_revokes_insertion_authority_before_owner_observation() -> anyhow::Result<()>
{
    let mut h = Harness::new()?;
    h.start().await?;
    assert!(h.runtime.input.active());
    h.runtime.configure(Config::default());
    assert!(!h.runtime.input.active());
    h.phase(Phase::Idle).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn retired_publisher_cannot_overwrite_shell_lifecycle_notice() -> anyhow::Result<()> {
    let mut h = PausedHarness::new(false, None);
    h.harness.start().await?;
    h.harness.output.send_modify(|snapshot| {
        snapshot.epoch += 1;
        snapshot.phase = Phase::Idle;
        snapshot.message = "Pausing dictation…".into();
    });
    h.harness.input(Input::Toggle);
    let (_, reply) = receive(&h.harness.jobs).await?;
    assert!(reply.send(Ok("fixture".into())).is_ok());
    receive(&h.harness.pasted).await?;
    let updates = h.harness.updates.clone();
    h.close().await?;
    assert_eq!(updates.borrow().message, "Pausing dictation…");
    assert!(matches!(updates.borrow().phase, Phase::Idle));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn delayed_owner_start_cannot_claim_a_newer_shell_publisher() -> anyhow::Result<()> {
    let h = PausedHarness::new(false, None);
    h.harness.output.send_modify(|s| {
        s.epoch += 1;
        s.message = "Dictation paused".into();
    });
    let updates = h.harness.updates.clone();
    h.close().await?;
    assert_eq!(updates.borrow().message, "Dictation paused");
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn recovery_failure_keeps_a_queued_dictation_until_microphone_retirement()
-> anyhow::Result<()> {
    let (results, loading) = async_channel::bounded(4);
    let (entered, retiring) = async_channel::bounded(4);
    let (release, released) = async_channel::bounded(4);
    let mut h = PausedHarness::with_ports(false, None, |ports| {
        ports.load_results = Some(loading);
        ports.retirement = Some((entered, released));
    });
    receive(&h.harness.loads).await?;
    h.harness.start().await?;
    results
        .send(Err(anyhow::anyhow!("Initial warmup failed")))
        .await?;
    observed(&mut h.harness.updates, |s| {
        s.message == "Initial warmup failed"
    })
    .await?;
    h.harness.input(Input::Cancel);
    h.harness.phase(Phase::Cancelled).await?;
    receive(&retiring).await?;
    receive(&h.harness.loads).await?;
    h.harness.input(Input::Press);
    h.harness.phase(Phase::Starting).await?;
    results
        .send(Err(anyhow::anyhow!("Recovery failed")))
        .await?;
    observed(&mut h.harness.updates, |s| s.message == "Recovery failed").await?;
    assert!(matches!(h.harness.updates.borrow().phase, Phase::Starting));
    assert!(h.harness.captures.try_recv().is_err());
    release.send(()).await?;
    receive(&h.harness.captures).await?;
    h.harness.phase(Phase::Recording).await?;
    h.harness.input(Input::Toggle);
    receive(&h.harness.loads).await?;
    results.send(Ok(())).await?;
    let (_, reply) = receive(&h.harness.jobs).await?;
    assert!(reply.send(Ok("fixture".into())).is_ok());
    receive(&h.harness.pasted).await?;
    h.harness.phase(Phase::Done).await?;
    assert!(h.harness.updates.borrow().message.is_empty());
    receive(&retiring).await?;
    release.send(()).await?;
    h.close().await?;
    Ok(())
}

// A focused diagnostic using the real owner and fake side effects. No timing
// assertion: scheduler noise must not turn performance profiling into a gate.
#[tokio::test]
#[ignore = "Reports controller reply-to-insertion latency with fake devices"]
async fn profile_ready_text_latency() -> anyhow::Result<()> {
    let mut h = Harness::new()?;
    receive(&h.loads).await?;
    let mut samples = Vec::with_capacity(100);
    for _ in 0..100 {
        h.start().await?;
        let reply = h.finish().await?;
        let started = Instant::now();
        assert!(reply.send(Ok("fixture".into())).is_ok());
        receive(&h.pasted).await?;
        samples.push(started.elapsed().as_micros());
        h.phase(Phase::Done).await?;
    }
    samples.sort_unstable();
    eprintln!(
        "Ready text -> fake insertion: median={}us p95={}us n={}",
        samples[50],
        samples[95],
        samples.len()
    );
    Ok(())
}
