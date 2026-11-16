use super::{
    super::{
        Cue, Env, Failure, Keeping, Kept, Origin, Payload, Publication, RecognitionMode, Recorder,
        Refusal, Release, Request, SessionId, Settings, Source, Status,
    },
    canonical,
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

pub(super) struct Oracle {
    script: Vec<usize>,
    chosen: Vec<(usize, usize)>,
    ids: Vec<SessionId>,
    pub(super) effects: String,
    pub(super) opened: Option<(SessionId, RecognitionMode)>,
}

impl Oracle {
    pub(super) fn new(script: Vec<usize>, ids: Vec<SessionId>) -> Self {
        Self {
            script,
            chosen: Vec::new(),
            ids,
            effects: String::new(),
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

    fn effect(&mut self, effect: &str) {
        if !self.effects.is_empty() {
            self.effects.push(' ');
        }
        self.effects.push_str(effect);
    }

    fn choose(&mut self, query: &str, answers: &[&str]) -> usize {
        let choice = self.script.get(self.chosen.len()).copied().unwrap_or(0);
        self.chosen.push((choice, answers.len()));
        self.effect(&format!("{query}={}", answers[choice]));
        choice
    }

    fn name(&self, id: SessionId) -> String {
        canonical::name(&self.ids, id)
    }
}

const fn recorder(recorder: Recorder) -> &'static str {
    match recorder {
        Recorder::Microphone => "microphone",
        Recorder::Screen => "screen",
    }
}

impl Env<Abstract> for Oracle {
    fn admit(&mut self) -> Option<()> {
        (self.choose("admit", &["permit", "refused"]) == 0).then_some(())
    }

    fn style(&mut self) {}

    fn shield(&mut self, (): &()) -> bool {
        self.choose("shield", &["shielded", "exposed"]) == 0
    }

    fn revoke(&mut self, (): &()) {
        self.effect("revoke");
    }

    fn closed(&mut self) -> bool {
        self.choose("closed", &["open", "closed"]) == 1
    }

    fn keep(&mut self, (): ()) -> Keeping<Abstract> {
        match self.choose("keep", &["kept", "unneeded", "lost"]) {
            0 => Keeping::Kept(Kept {
                checkpoint: (),
                audio: (),
                frames: (),
            }),
            1 => Keeping::Unneeded,
            _ => Keeping::Lost,
        }
    }

    fn record_microphone(
        &mut self,
        session: SessionId,
        _: &Settings,
        mode: RecognitionMode,
    ) -> Result<(), ()> {
        let query = format!("microphone({},{mode:?})", self.name(session));
        if self.choose(&query, &["recording", "failed"]) == 0 {
            self.opened = Some((session, mode));
            Ok(())
        } else {
            Err(())
        }
    }

    fn record_screen(
        &mut self,
        session: SessionId,
        (): (),
        (): (),
        frames: Option<()>,
    ) -> Result<(), ()> {
        let query = format!("screen({},{})", self.name(session), frames.is_some());
        (self.choose(&query, &["recording", "failed"]) == 0)
            .then_some(())
            .ok_or(())
    }

    fn finish(&mut self, which: Recorder) {
        self.effect(&format!("finish({})", recorder(which)));
    }

    fn retire(&mut self, which: Recorder) {
        self.effect(&format!("retire({})", recorder(which)));
    }

    fn listen_whole(&mut self) {
        self.effect("listen_whole");
    }

    fn expect_cue(&mut self) {
        self.effect("expect_cue");
    }

    fn keep_audio(&mut self, (): (), (): (), _: &Settings) -> bool {
        self.choose("keep_audio", &["kept", "lost"]) == 0
    }

    fn pin(&mut self, (): ()) {
        self.effect("pin");
    }

    fn meter(&mut self, session: SessionId, (): ()) {
        let effect = format!("meter({})", self.name(session));
        self.effect(&effect);
    }

    fn load(&mut self, _: &Settings) {
        self.effect("load");
    }

    fn reload(&mut self, _: &Settings) {
        self.effect("reload");
    }

    fn recover(&mut self, _: &Settings, probe: bool) {
        self.effect(&format!("recover({probe})"));
    }

    fn cut(&mut self, (): &(), (): &()) -> Option<()> {
        (self.choose("cut", &["cut", "uncut"]) == 0).then_some(())
    }

    fn transcribe(&mut self, request: Request<Abstract>, _: &Settings) {
        let source = match request.source {
            Source::Audio(()) => "audio",
            Source::Live(()) => "live",
        };
        let effect = format!(
            "transcribe({},{:?},{source},{:?})",
            self.name(request.ticket.session),
            request.ticket.part,
            request.detail
        );
        self.effect(&effect);
    }

    fn compose(&mut self, (): (), (): (), _: &Settings) -> Option<()> {
        (self.choose("compose", &["text", "empty"]) == 0).then_some(())
    }

    fn insert(&mut self, (): (), (): &(), _: &Settings) {
        self.effect("insert");
    }

    fn ready(&mut self, (): &()) -> bool {
        self.choose("ready", &["ready", "closed"]) == 0
    }

    fn refuse_practice(&mut self, (): ()) {
        self.effect("refuse_practice");
    }

    fn practise(&mut self, (): &(), (): &(), (): ()) -> bool {
        self.choose("practise", &["delivered", "closed"]) == 0
    }

    fn publish(&mut self, publication: Publication<Abstract>, (): &(), receipt: Option<()>) {
        let origin = match publication.origin {
            Origin::Live => "live",
            Origin::Recovered { pending: () } => "recovered",
        };
        self.effect(&format!(
            "publish({:?},{},{origin},{})",
            publication.loss,
            publication.checkpoint.is_some(),
            receipt.is_some()
        ));
    }

    fn reopen(&mut self, (): ()) {
        self.effect("reopen");
    }

    fn refuse_recovery(&mut self, (): (), failure: &Failure<Abstract>) {
        self.effect(&format!("refuse_recovery({failure:?})"));
    }

    fn retire_delivery(&mut self, session: SessionId) {
        let effect = format!("retire_delivery({})", self.name(session));
        self.effect(&effect);
    }

    fn release(&mut self, session: SessionId, (): (), release: Release<Abstract>) {
        let effect = format!("release({},{release:?})", self.name(session));
        self.effect(&effect);
    }

    fn remember(&mut self, (): ()) {
        self.effect("remember");
    }

    fn announce(&mut self) {
        self.effect("announce");
    }

    fn cue(&mut self, cue: Cue) {
        self.effect(&format!("cue({cue:?})"));
    }

    fn hold_cues(&mut self, held: bool) {
        self.effect(&format!("hold_cues({held})"));
    }

    fn respond(&mut self, (): (), reply: Result<Status<Abstract>, Refusal>) {
        let detail = match reply {
            Ok(status) => {
                let session = status
                    .session
                    .map_or_else(|| "none".into(), |session| self.name(session));
                let Status {
                    phase,
                    model,
                    desktop_ready,
                    preset,
                    last_dictation,
                    ..
                } = status;
                format!(
                    "{phase:?},{model:?},{desktop_ready},{session},{},{last_dictation}",
                    preset.is_some()
                )
            },
            Err(refusal) => format!("{refusal:?}"),
        };
        self.effect(&format!("respond({detail})"));
    }

    fn close_input(&mut self) {
        self.effect("close_input");
    }
}
