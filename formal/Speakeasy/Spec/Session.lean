module

public import Speakeasy.Spec.Recognition

/-!
# Session model

`machine::session::Session`: one dictation or Show from admission to cleanup. A Show also
holds its screen recording, its checkpoint and logs, and what its cleanup may keep.
-/

@[expose] public section

namespace Speakeasy

/-- What a session's text is for. -/
inductive Purpose where
  /-- Ordinary dictation. -/
  | dictation
  /-- Pasting the last dictation again. -/
  | replay
  /-- A practice window receives the text. -/
  | practice
  deriving DecidableEq

/-- Decides whether two purpose values are equal. -/
add_decl_doc instDecidableEqPurpose

/-- A purpose value is the constructor its index names. -/
add_decl_doc Purpose.ofNat_ctorIdx

/-- A Show's screen recording. -/
inductive Screen where
  /-- Waiting for the recorder. -/
  | queued
  /-- Recording. -/
  | recording
  /-- Recorded footage waits for publication. -/
  | recorded
  /-- The footage was handed over. -/
  | spent
  deriving DecidableEq

/-- Decides whether two screen values are equal. -/
add_decl_doc instDecidableEqScreen

/-- A screen value is the constructor its index names. -/
add_decl_doc Screen.ofNat_ctorIdx

/-- Whether a Show may be kept. -/
inductive Disposition where
  /-- Still recording. -/
  | unfinished
  /-- Finished, so its bundle may be kept. -/
  | finished
  /-- Given up, so its bundle is discarded. -/
  | forfeited
  deriving DecidableEq

/-- Decides whether two disposition values are equal. -/
add_decl_doc instDecidableEqDisposition

/-- A disposition value is the constructor its index names. -/
add_decl_doc Disposition.ofNat_ctorIdx

/-- The Show part of a session. -/
structure ShowState where
  /-- The screen recording. -/
  screen : Screen
  /-- Whether the session still holds the audio log. -/
  audio : Bool
  /-- Whether the session still holds the frame log. -/
  frames : Bool
  /-- Whether the session holds a checkpoint. -/
  checkpoint : Bool
  /-- Whether the bundle may be kept. -/
  disposition : Disposition
  /-- Where the Show came from. -/
  origin : Origin

namespace ShowState

/-- A Show with no logs or checkpoint yet. -/
def new (screen : Screen) : ShowState :=
  { screen, audio := false, frames := false, checkpoint := false,
    disposition := .unfinished, origin := .live }

/-- An unfinished Show finishes. -/
def conclude (state : ShowState) : ShowState :=
  if state.disposition == .unfinished then { state with disposition := .finished } else state

/-- Drops the log producers. -/
def closeProducers (state : ShowState) : ShowState :=
  { state with audio := false, frames := false }

end ShowState

/-- Where a session is in its life. -/
inductive Stage where
  /-- Waiting for the microphone. -/
  | queued
  /-- The microphone is opening. -/
  | opening
  /-- A recovered Show is being reopened. -/
  | reopening
  /-- Recording. -/
  | recording
  /-- Finishing the recording. -/
  | stopping
  /-- Recognizing sealed audio; `incomplete` marks recovered narration that is partial. -/
  | recognizing (incomplete : Bool)
  /-- A Show waits for its footage, with the narration loss so far. -/
  | awaitingFootage (lost : Option NarrationLoss)
  /-- The text or Show is being delivered. -/
  | delivering
  deriving DecidableEq

/-- Decides whether two stage values are equal. -/
add_decl_doc instDecidableEqStage

/-- Compares two stage values constructor by constructor. -/
add_decl_doc instDecidableEqStage.decEq

/-- What reopening an interrupted Show found. -/
inductive Recognized where
  /-- Speech still to recognize; `incomplete` marks a partial recording. -/
  | speech (incomplete : Bool)
  /-- Narration already heard, with its loss. -/
  | narration (loss : Option NarrationLoss)

/-- One session. -/
structure Session where
  /-- Its id. -/
  id : Nat
  /-- What its text is for. -/
  purpose : Purpose
  /-- Whether composed text waits to be remembered for a later paste. -/
  pendingText : Bool
  /-- Its stage. -/
  stage : Stage
  /-- Its recognition. -/
  recognition : Recognition
  /-- Its Show part, if it is a Show. -/
  showState : Option ShowState
  /-- Whether a recovery request waits for its result. -/
  receipt : Bool

namespace Session

/-- A fresh dictation session. -/
def new (id : Nat) : Session :=
  { id, purpose := .dictation, pendingText := false, stage := .queued,
    recognition := Recognition.new id, showState := none, receipt := false }

/-- A session that reopens an interrupted Show. -/
def recovering (id : Nat) : Session :=
  { Session.new id with showState := some (ShowState.new .spent), receipt := true,
                        stage := .reopening }

/-- Applies a change to the Show part, if there is one. -/
def mapShow (change : ShowState → ShowState) (session : Session) : Session :=
  { session with showState := session.showState.map change }

/-- Whether the session is a Show. -/
def isShow (session : Session) : Bool :=
  session.showState.isSome

/-- Whether the session is practice. -/
def isPractice (session : Session) : Bool :=
  session.purpose == .practice

/-- Whether the session waits for the microphone. -/
def isQueued (session : Session) : Bool :=
  session.stage == .queued

/-- Whether the session is recording. -/
def isRecording (session : Session) : Bool :=
  session.stage == .recording

/-- Whether the microphone belongs to the session. -/
def isCapturing (session : Session) : Bool :=
  match session.stage with
  | .opening | .recording | .stopping => true
  | _ => false

/-- Whether the session recognizes sealed audio. -/
def isRecognizing (session : Session) : Bool :=
  match session.stage with
  | .recognizing _ => true
  | _ => false

/-- Whether the session is delivering. -/
def isDelivering (session : Session) : Bool :=
  session.stage == .delivering

/-- Whether the session is reopening a Show. -/
def isReopening (session : Session) : Bool :=
  session.stage == .reopening

/-- Whether a delivery task belongs to the session. -/
def awaitsDelivery (session : Session) : Bool :=
  session.isDelivering || session.isReopening

/-- Whether the screen recorder belongs to the session. -/
def recordsScreen (session : Session) : Bool :=
  match session.showState with
  | some state => state.screen == .recording
  | none => false

/-- Whether the final part waits for the worker. -/
def awaitsWorker (session : Session) : Bool :=
  session.isRecognizing && session.recognition.waiting

/-- Whether the final part is at the worker. -/
def isTranscribing (session : Session) : Bool :=
  session.isRecognizing && session.recognition.inFlight

/-- The phase the session reports. -/
def phase (session : Session) : Phase :=
  match session.stage with
  | .queued | .opening => .starting
  | .recording => .recording
  | .stopping => .stopping
  | .reopening | .recognizing _ | .awaitingFootage _ | .delivering => .processing

/-- Whether the Show's screen waits for its recorder. -/
def queuedScreen (session : Session) : Bool :=
  match session.showState with
  | some state => state.screen == .queued
  | none => false

/-- Takes the recovery receipt. -/
def takeReceipt (session : Session) : Bool × Session :=
  (session.receipt, { session with receipt := false })

/-- The recovered Show reopened, with speech to recognize or narration already heard. -/
def reopened (recognized : Recognized) (session : Session) : Session :=
  let state : ShowState :=
    { ShowState.new .recorded with checkpoint := true, disposition := .finished,
                                   origin := .recovered }
  match recognized with
  | .speech incomplete =>
    { session with recognition := session.recognition.sealAudio none .words,
                   stage := .recognizing incomplete, showState := some state }
  | .narration lost => { session with stage := .awaitingFootage lost, showState := some state }

/-- The Show kept its checkpoint and logs. -/
def keep (session : Session) : Session :=
  session.mapShow (fun (state : ShowState) =>
    { state with checkpoint := true, audio := true, frames := true })

/-- Takes the frame log. -/
def takeFrameLog (session : Session) : Bool × Session :=
  match session.showState with
  | some state => (state.frames, { session with showState := some { state with frames := false } })
  | none => (false, session)

/-- Takes the audio log while the microphone records. -/
def takeAudioLog (session : Session) : Bool × Session :=
  match session.stage, session.showState with
  | .recording, some state | .stopping, some state =>
    (state.audio, { session with showState := some { state with audio := false } })
  | _, _ => (false, session)

/-- The Show is given up. -/
def forfeit (session : Session) : Session :=
  session.mapShow (fun (state : ShowState) => { state with disposition := .forfeited })

/-- The Show finishes. -/
def conclude (session : Session) : Session :=
  session.mapShow ShowState.conclude

/-- The microphone opened. -/
def opened (session : Session) : Session :=
  if session.isQueued then { session with stage := .opening } else session

/-- The microphone delivers audio; `processing` says the gesture already finished. -/
def audioReady (processing : Bool) (session : Session) : Session :=
  let concluded : Session := if processing then session.conclude else session
  if concluded.stage == .opening then
    { concluded with stage := if processing then .stopping else .recording }
  else concluded

/-- The gesture finished. -/
def finish (session : Session) : Session :=
  let concluded : Session := session.conclude
  if concluded.stage == .recording then { concluded with stage := .stopping } else concluded

/-- The microphone finished with audio or without. Returns `false` when a dictation heard
nothing and ends. -/
def captured (sealed : Option (Option Partition)) (session : Session) : Bool × Session :=
  let detail : Detail := if session.isShow then .words else .text
  match session.stage, sealed with
  | .recording, some partition | .stopping, some partition =>
    (true, { session with recognition := session.recognition.sealAudio partition detail,
                          stage := .recognizing false }.conclude)
  | _, _ =>
    if session.isShow then (true, { session with stage := .awaitingFootage none }.conclude)
    else (false, session)

/-- A piece arrived. -/
def piece (number : Nat) (session : Session) : Session :=
  if session.isCapturing && !session.isShow then
    { session with recognition := session.recognition.piece number }
  else session

/-- A live stream arrived. -/
def stream (session : Session) : Session :=
  if session.isCapturing && !session.isShow then
    { session with recognition := session.recognition.stream }
  else session

/-- The next transcription to send. -/
def request (session : Session) (env : Env) : Option Request × Session × Env :=
  if session.isCapturing || session.isRecognizing then
    let (request, recognition, env) := session.recognition.request env
    (request, { session with recognition }, env)
  else (none, session, env)

/-- Whether a result with this ticket is still wanted. -/
def accepts (ticket : Ticket) (session : Session) : Bool :=
  (session.isCapturing || session.isRecognizing) && session.recognition.accepts ticket

/-- Timed words arrived: the Show waits for its footage. -/
def narrate (session : Session) : Session :=
  match session.stage, session.recognition.finalPart with
  | .recognizing incomplete, some _ =>
    { session with recognition := Recognition.new session.id,
                   stage := .awaitingFootage (if incomplete then some .incomplete else none) }
  | _, _ => session

/-- A result arrived. Returns whether text is ready to compose. -/
def heard (ticket : Ticket) (transcript : Transcript) (session : Session) : Bool × Session :=
  if !session.accepts ticket then (false, session)
  else
    let (result, recognition) := session.recognition.heard ticket transcript
    let session : Session := { session with recognition }
    match result with
    | some .text => (true, session)
    | some .words => (false, session.narrate)
    | none => (false, session)

/-- Finishes without another transcription when early results cover the audio. -/
def complete (session : Session) : Bool × Session :=
  if session.isRecognizing then
    let (result, recognition) := session.recognition.complete
    (result.isSome, { session with recognition })
  else (false, session)

/-- A transcription failed. Returns `true` when the failure is handled. -/
def failed (ticket : Ticket) (session : Session) : Bool × Session :=
  if !session.accepts ticket then (true, session)
  else
    let (handled, recognition) := session.recognition.failed ticket
    (handled, { session with recognition })

/-- The Show goes on without narration. -/
def goOnWithoutNarration (lost : NarrationLoss) (session : Session) : Session :=
  { session with recognition := Recognition.new session.id,
                 stage := .awaitingFootage (some lost) }.conclude

/-- The microphone failed. A recording Show goes on without narration; returns whether it did. -/
def audioFailed (silent : Bool) (session : Session) : Bool × Session :=
  let listened : Bool :=
    session.isShow && (session.stage == .recording || session.stage == .stopping)
  if listened then
    (true, session.goOnWithoutNarration (if silent then .audioSilent else .audioStopped))
  else (false, session)

/-- Recognition gave up. A Show goes on without narration; returns whether it did. -/
def notTranscribed (session : Session) : Bool × Session :=
  if session.isShow && session.isRecognizing && session.recognition.mayLoseNarration then
    (true, session.goOnWithoutNarration .notTranscribed)
  else (false, session)

/-- Inserts the text. -/
def insert (session : Session) (env : Env) : Session × Env :=
  ({ session with recognition := Recognition.new session.id, stage := .delivering },
    env.emit .insert)

/-- Turns a recording dictation into a Show when the focused window can be shielded. -/
def turnIntoShow (session : Session) (env : Env) : Bool × Session × Env :=
  if session.isPractice || session.isShow || !(session.isQueued || session.isCapturing) then
    (false, session, env)
  else
    let (answer, env) := env.ask .shield
    if answer != 0 then (false, session, env)
    else
      (true, { session with recognition := session.recognition.whole,
                            showState := some (ShowState.new .queued) }, env)

/-- The screen recorder opened. -/
def screenOpened (session : Session) : Session :=
  session.mapShow (fun (state : ShowState) =>
    if state.screen == .queued then { state with screen := .recording } else state)

/-- The screen recorder finished with footage. -/
def screenRecorded (session : Session) : Session :=
  session.mapShow (fun (state : ShowState) => { state with screen := .recorded })

/-- Publishes the Show once narration and footage are both ready. -/
def publish (session : Session) (env : Env) : Session × Env :=
  match session.stage, session.showState with
  | .awaitingFootage lost, some state =>
    if state.screen == .recorded then
      let handed : ShowState :=
        { state with
          screen := .spent, audio := false, frames := false, origin := .live, checkpoint := false }
      ({ session with stage := .delivering, showState := some handed, receipt := false },
        env.emit (.publish lost state.checkpoint state.origin session.receipt))
    else (session, env)
  | _, _ => (session, env)

/-- What cleanup releases: a finished Show keeps its bundle, with narration only when it was
kept whole or recovered incomplete. -/
def releaseOf (stage : Stage) (disposition : Disposition) : Release :=
  match disposition with
  | .finished => .keep (match stage with
    | .awaitingFootage none => some false
    | .awaitingFootage (some .incomplete) => some true
    | _ => none)
  | .unfinished | .forfeited => .discard

/-- Cleans up an abandoned session. Returns whether a delivery or checkpoint still retires. -/
def cleanup (session : Session) (env : Env) : Bool × Env :=
  if session.stage == .delivering || session.stage == .reopening then
    (true, env.emit (.retireDelivery session.id))
  else
    match session.showState with
    | some state =>
      if state.checkpoint then
        (true, env.emit (.release session.id (releaseOf session.stage state.disposition)))
      else (false, env)
    | none => (false, env)

end Session

end Speakeasy

end
