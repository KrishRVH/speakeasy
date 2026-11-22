use super::super::{
    Cue, Env, Failure, Handoff, Keeping, Kept, Payload, Publication, RecognitionMode, Recorder,
    Refusal, Release, Request, SessionId, Settings, Status,
};

#[derive(Clone, Copy, Debug)]
pub(super) struct Abstract;

impl Payload for Abstract {
    type Audio = ();
    type AudioLog = ();
    type Call = ();
    type Checkpoint = ();
    type Config = Settings;
    type Footage = ();
    type FrameLog = ();
    type Heard = ();
    type Instant = ();
    type Interrupted = ();
    type Level = ();
    type Live = ();
    type Narration = ();
    type Pending = ();
    type Permit = ();
    type Practice = ();
    type Range = ();
    type Receipt = ();
    type Recovered = ();
    type Saved = ();
    type Scope = ();
    type Shown = ();
    type Span = ();
    type Style = ();
    type Taps = ();
    type Text = ();
    type Unchosen = ();

    fn settings(config: &Settings) -> Settings {
        *config
    }

    fn reloads(current: &Settings, next: &Settings) -> bool {
        current.recognition != next.recognition
    }

    fn offset((): (), (): ()) {}

    fn shift((): (), (): ()) {}

    fn delay((): (), (): ()) {}

    fn append((): &mut (), (): ()) {}
}

#[derive(Debug)]
pub(super) enum Query {
    Admit,
    Shield,
    Keep,
    Microphone {
        #[expect(dead_code, reason = "A failure trace prints it")]
        session: SessionId,
        #[expect(dead_code, reason = "A failure trace prints it")]
        mode: RecognitionMode,
    },
    Screen {
        #[expect(dead_code, reason = "A failure trace prints it")]
        session: SessionId,
        #[expect(dead_code, reason = "A failure trace prints it")]
        frames: bool,
    },
    KeepAudio,
    Cut,
    Compose,
    Ready,
    Practise,
}

#[derive(Debug)]
pub(super) enum Effect {
    Answered(
        Query,
        #[expect(dead_code, reason = "A failure trace prints it")] &'static str,
    ),
    Revoke,
    Finish(#[expect(dead_code, reason = "A failure trace prints it")] Recorder),
    Retire(#[expect(dead_code, reason = "A failure trace prints it")] Recorder),
    ListenWhole,
    ExpectCue,
    Pin,
    Meter(#[expect(dead_code, reason = "A failure trace prints it")] SessionId),
    Load,
    Reload,
    Recover {
        #[expect(dead_code, reason = "A failure trace prints it")]
        probe: bool,
    },
    Transcribe(#[expect(dead_code, reason = "A failure trace prints it")] Request<Abstract>),
    Insert,
    RefusePractice,
    Publish {
        #[expect(dead_code, reason = "A failure trace prints it")]
        publication: Publication<Abstract>,
        #[expect(dead_code, reason = "A failure trace prints it")]
        receipt: bool,
    },
    Reopen,
    RefuseRecovery(#[expect(dead_code, reason = "A failure trace prints it")] Failure<Abstract>),
    RetireDelivery(#[expect(dead_code, reason = "A failure trace prints it")] SessionId),
    Release(
        #[expect(dead_code, reason = "A failure trace prints it")] SessionId,
        #[expect(dead_code, reason = "A failure trace prints it")] Release,
    ),
    Remember,
    Announce,
    Cue(#[expect(dead_code, reason = "A failure trace prints it")] Cue),
    HoldCues(#[expect(dead_code, reason = "A failure trace prints it")] bool),
    Respond(
        #[expect(dead_code, reason = "A failure trace prints it")]
        Result<Status<Abstract>, Refusal>,
    ),
    CloseInput,
}

pub(super) struct Oracle {
    script: Vec<usize>,
    chosen: Vec<(usize, usize)>,
    pub(super) effects: Vec<Effect>,
    pub(super) opened: Option<(SessionId, RecognitionMode)>,
}

impl Oracle {
    pub(super) const fn new(script: Vec<usize>) -> Self {
        Self {
            script,
            chosen: Vec::new(),
            effects: Vec::new(),
            opened: None,
        }
    }

    pub(super) fn next_script(&self) -> Option<Vec<usize>> {
        let last = self
            .chosen
            .iter()
            .rposition(|&(choice, arity)| choice.saturating_add(1) < arity)?;
        let mut script: Vec<usize> = self.chosen[..last]
            .iter()
            .map(|&(choice, _)| choice)
            .collect();
        script.push(self.chosen[last].0.saturating_add(1));
        Some(script)
    }

    fn choose<T, const N: usize>(&mut self, query: Query, answers: [(&'static str, T); N]) -> T {
        let choice = self.script.get(self.chosen.len()).copied().unwrap_or(0);
        self.chosen.push((choice, N));
        let (label, answer) = answers
            .into_iter()
            .nth(choice)
            .expect("A script chooses among the answers offered");
        self.effects.push(Effect::Answered(query, label));
        answer
    }
}

impl Env<Abstract> for Oracle {
    fn admit(&mut self) -> Option<()> {
        self.choose(Query::Admit, [("permit", Some(())), ("refused", None)])
    }

    fn style(&mut self) {}

    fn shield(&mut self, (): &()) -> bool {
        self.choose(Query::Shield, [("shielded", true), ("exposed", false)])
    }

    fn revoke(&mut self, (): &()) {
        self.effects.push(Effect::Revoke);
    }

    fn keep(&mut self, (): ()) -> Keeping<Abstract> {
        let kept = Kept {
            checkpoint: (),
            audio: (),
            frames: (),
        };
        self.choose(
            Query::Keep,
            [
                ("kept", Keeping::Kept(kept)),
                ("unneeded", Keeping::Unneeded),
                ("lost", Keeping::Lost),
            ],
        )
    }

    fn record_microphone(
        &mut self,
        session: SessionId,
        _: &Settings,
        mode: RecognitionMode,
    ) -> Result<(), ()> {
        let query = Query::Microphone { session, mode };
        let recording = self.choose(query, [("recording", Ok(())), ("failed", Err(()))]);
        if recording.is_ok() {
            self.opened = Some((session, mode));
        }
        recording
    }

    fn record_screen(
        &mut self,
        session: SessionId,
        (): (),
        (): (),
        frames: Option<()>,
    ) -> Result<(), ()> {
        let query = Query::Screen {
            session,
            frames: frames.is_some(),
        };
        self.choose(query, [("recording", Ok(())), ("failed", Err(()))])
    }

    fn finish(&mut self, recorder: Recorder) {
        self.effects.push(Effect::Finish(recorder));
    }

    fn retire(&mut self, recorder: Recorder) {
        self.effects.push(Effect::Retire(recorder));
    }

    fn listen_whole(&mut self) {
        self.effects.push(Effect::ListenWhole);
    }

    fn expect_cue(&mut self) {
        self.effects.push(Effect::ExpectCue);
    }

    fn keep_audio(&mut self, (): (), (): (), _: &Settings) -> bool {
        self.choose(Query::KeepAudio, [("kept", true), ("lost", false)])
    }

    fn pin(&mut self, (): ()) {
        self.effects.push(Effect::Pin);
    }

    fn meter(&mut self, session: SessionId, (): ()) {
        self.effects.push(Effect::Meter(session));
    }

    fn load(&mut self, _: &Settings) {
        self.effects.push(Effect::Load);
    }

    fn reload(&mut self, _: &Settings) {
        self.effects.push(Effect::Reload);
    }

    fn recover(&mut self, _: &Settings, probe: bool) {
        self.effects.push(Effect::Recover { probe });
    }

    fn cut(&mut self, (): &(), (): &()) -> Option<()> {
        self.choose(Query::Cut, [("cut", Some(())), ("uncut", None)])
    }

    fn transcribe(&mut self, request: Request<Abstract>, _: &Settings) {
        self.effects.push(Effect::Transcribe(request));
    }

    fn compose(&mut self, (): (), (): (), _: &Settings) -> Option<()> {
        self.choose(Query::Compose, [("text", Some(())), ("empty", None)])
    }

    fn insert(&mut self, (): (), (): &(), _: &Settings, _: Handoff<Abstract>) {
        self.effects.push(Effect::Insert);
    }

    fn ready(&mut self, (): &()) -> bool {
        self.choose(Query::Ready, [("ready", true), ("closed", false)])
    }

    fn refuse_practice(&mut self, (): ()) {
        self.effects.push(Effect::RefusePractice);
    }

    fn practise(&mut self, (): &(), (): &(), (): ()) -> bool {
        self.choose(Query::Practise, [("delivered", true), ("closed", false)])
    }

    fn publish(&mut self, publication: Publication<Abstract>, (): &(), receipt: Option<()>) {
        self.effects.push(Effect::Publish {
            publication,
            receipt: receipt.is_some(),
        });
    }

    fn reopen(&mut self, (): ()) {
        self.effects.push(Effect::Reopen);
    }

    fn refuse_recovery(&mut self, (): (), failure: &Failure<Abstract>) {
        self.effects.push(Effect::RefuseRecovery(failure.clone()));
    }

    fn retire_delivery(&mut self, session: SessionId) {
        self.effects.push(Effect::RetireDelivery(session));
    }

    fn release(&mut self, session: SessionId, (): (), release: Release) {
        self.effects.push(Effect::Release(session, release));
    }

    fn remember(&mut self, (): ()) {
        self.effects.push(Effect::Remember);
    }

    fn announce(&mut self) {
        self.effects.push(Effect::Announce);
    }

    fn cue(&mut self, cue: Cue) {
        self.effects.push(Effect::Cue(cue));
    }

    fn hold_cues(&mut self, held: bool) {
        self.effects.push(Effect::HoldCues(held));
    }

    fn respond(&mut self, (): (), reply: Result<Status<Abstract>, Refusal>) {
        self.effects.push(Effect::Respond(reply));
    }

    fn close_input(&mut self) {
        self.effects.push(Effect::CloseInput);
    }
}
