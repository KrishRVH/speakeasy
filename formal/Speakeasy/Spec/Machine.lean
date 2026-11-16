module

public import Speakeasy.Spec.Gesture
public import Speakeasy.Spec.Session

/-!
# Session machine model

`speakeasy_core::machine::Machine` with its payload erased. `Run.step` mirrors `Machine::step`
handler for handler: the same checks in the same order, the same queries to the environment,
and the same effects. A `Run` is the machine with the environment it talks to. Two Rust
behaviors have no counterpart: session ids are natural numbers, so the overflow stop after
2^64 sessions cannot happen, and payloads that the machine only moves are gone.
-/

@[expose] public section

namespace Speakeasy

/-- The worker's state, as the machine mirrors it. -/
inductive Worker where
  /-- No model is loaded. -/
  | unavailable
  /-- The model is loading. -/
  | loading
  /-- The worker is idle. -/
  | ready
  /-- The worker transcribes for this ticket. -/
  | transcribing (ticket : Ticket)
  deriving DecidableEq

/-- Decides whether two worker values are equal. -/
add_decl_doc instDecidableEqWorker

/-- Compares two worker values constructor by constructor. -/
add_decl_doc instDecidableEqWorker.decEq

namespace Worker

/-- The model state the worker reports. -/
def model : Worker → ModelState
  | .unavailable => .unavailable
  | .loading => .loading
  | .ready | .transcribing _ => .ready

/-- Whether the worker transcribes anything. -/
def isTranscribing : Worker → Bool
  | .transcribing _ => true
  | .unavailable | .loading | .ready => false

/-- Whether the worker transcribes this session's live stream. -/
def isStreaming (session : Nat) : Worker → Bool
  | .transcribing ticket => ticket.session == session && ticket.part == .stream
  | .unavailable | .loading | .ready => false

end Worker

/-- A native recorder's state, as the machine mirrors it. -/
inductive Device where
  /-- No recording. -/
  | free
  /-- Recording for the current session. -/
  | opened
  /-- Retiring; it frees once it reports back. -/
  | retiring
  deriving DecidableEq

/-- Decides whether two device values are equal. -/
add_decl_doc instDecidableEqDevice

/-- A device value is the constructor its index names. -/
add_decl_doc Device.ofNat_ctorIdx

/-- The latest session the machine started. -/
structure Latest where
  /-- Its id. -/
  id : Nat
  /-- Whether it is a Show. -/
  showing : Bool

/-- The machine state. -/
structure Machine where
  /-- The settings. -/
  config : Settings
  /-- The gesture. -/
  gesture : Gesture
  /-- Whether the desktop accepts input. -/
  desktopReady : Bool
  /-- The latest session. -/
  latest : Latest
  /-- The live session. -/
  session : Option Session
  /-- Whether a practice window is armed. -/
  practice : Bool
  /-- Whether a preset is applied. -/
  preset : Bool
  /-- Whether a dictation can be pasted again. -/
  lastDictation : Bool
  /-- The newest session whose Show was remembered. -/
  remembered : Option Nat
  /-- The worker. -/
  worker : Worker
  /-- The microphone. -/
  microphone : Device
  /-- The screen recorder. -/
  screen : Device
  /-- Sessions whose delivery or checkpoint still retires. -/
  retiring : List Nat
  /-- Whether other apps' cues are held. -/
  cuesHeld : Bool
  /-- What ended the last session. -/
  outcome : Outcome
  /-- The notice beside the phase. -/
  notice : Option Notice

/-- The machine as the app starts it, before its first step. -/
def Machine.start (timing : Timing) (config : Settings) (desktopReady : Bool) : Machine :=
  { config, gesture := Gesture.new timing, desktopReady, latest := { id := 0, showing := false },
    session := none, practice := false, preset := false, lastDictation := false,
    remembered := none, worker := .loading, microphone := .free, screen := .free,
    retiring := [], cuesHeld := false, outcome := .idle, notice := some .loadingModel }

/-- Whether the live session is the latest one and its recognition belongs to it. While this
holds, an event for any other session carries a stale id. -/
def Machine.current (machine : Machine) : Bool :=
  match machine.session with
  | some session => session.id == machine.latest.id && session.recognition.id == session.id
  | none => true

/-- Keyboard and desktop input. -/
inductive Input where
  /-- The dictation key goes down. -/
  | press
  /-- The dictation key comes up. -/
  | release
  /-- Lock a held recording. -/
  | lock
  /-- Turn the recording into a Show. -/
  | makeShow
  /-- Pin the screen recording. -/
  | pin
  /-- Escape. -/
  | escape
  /-- Cancel. -/
  | cancel
  /-- Another app took over. -/
  | interrupt
  /-- Toggle hands-free. -/
  | toggle
  /-- Paste the last dictation again. -/
  | pasteLast
  /-- Start a Show. -/
  | startShow
  /-- Input is unavailable. -/
  | unavailable
  /-- The desktop became ready. -/
  | desktopReady

/-- A report from another part of the app. -/
inductive Report where
  /-- A component failed. -/
  | failed
  /-- A component failed again. -/
  | recurring
  /-- A Show link was copied. -/
  | showCopied
  /-- The copied Show is gone. -/
  | showGone
  deriving DecidableEq

/-- Decides whether two report values are equal. -/
add_decl_doc instDecidableEqReport

/-- A report value is the constructor its index names. -/
add_decl_doc Report.ofNat_ctorIdx

/-- A control-interface command. Tokens name dictations. -/
inductive Command where
  /-- Report status. -/
  | status
  /-- Start a dictation. -/
  | start
  /-- Stop the named dictation. -/
  | stop (token : Option Nat)
  /-- Cancel the named dictation. -/
  | cancel (token : Option Nat)
  /-- Paste the last dictation again. -/
  | pasteLast
  /-- An unsupported command. -/
  | unsupported

/-- How the microphone recording ended. -/
inductive CaptureEnd where
  /-- With audio, split or whole, or with nothing at all. -/
  | sealed (partition : Option (Option Partition))
  /-- The microphone failed; `silent` says it heard nothing. -/
  | unheard (silent : Bool)

/-- A microphone event. -/
inductive Capture where
  /-- A live stream is available. -/
  | streaming
  /-- Audio is flowing. -/
  | ready
  /-- The checkpoint stopped. -/
  | checkpointLost
  /-- A level reading. -/
  | level
  /-- A numbered piece. -/
  | piece (number : Nat)
  /-- The recording ended. -/
  | finished (result : CaptureEnd)

/-- A screen recorder event. -/
inductive Screening where
  /-- The checkpoint stopped. -/
  | checkpointLost
  /-- The recording stopped early. -/
  | movieStopped
  /-- The footage is ready. -/
  | finished
  /-- The recorder stopped; `unwound` says the Show was already undone. -/
  | stopped (unwound : Bool)
  /-- The user chose no screen. -/
  | unchosen

/-- A worker completion. -/
inductive Completion where
  /-- The worker gave up on a ticket. -/
  | abandoned (ticket : Ticket)
  /-- The model loaded, or failed to. -/
  | loaded (ok : Bool)
  /-- A transcription finished, or failed with `none`. -/
  | transcribed (ticket : Ticket) (result : Option Transcript)

/-- How an insertion ended. -/
inductive Inserted where
  /-- Typed into the focused app. -/
  | sent
  /-- Copied to the clipboard instead. -/
  | copied
  /-- No app could take it. -/
  | unavailable
  /-- Cancelled. -/
  | cancelled
  /-- Failed. -/
  | failed

/-- How a publication ended. -/
inductive Published where
  /-- Saved; `copied` says its link reached the clipboard. -/
  | saved (copied : Bool)
  /-- A recovered Show was saved. -/
  | recovered
  /-- Cancelled. -/
  | cancelled
  /-- Failed. -/
  | failed

/-- How a delivery ended. -/
inductive Delivered where
  /-- An insertion ended. -/
  | inserted (inserted : Inserted)
  /-- A publication ended. -/
  | published (published : Published)
  /-- An abandoned delivery was released, or failed to be. -/
  | abandoned (ok : Bool)
  /-- A recovered Show reopened, or failed to with `none`. -/
  | reopened (result : Option Recognized)
  /-- The delivery task panicked. -/
  | panicked

/-- Everything the machine reacts to. -/
inductive Event where
  /-- The app stops. -/
  | stop
  /-- Keyboard input was lost. -/
  | inputLost
  /-- Input. -/
  | input (input : Input)
  /-- A report. -/
  | report (report : Report)
  /-- New settings. -/
  | configure (config : Settings)
  /-- A control command. -/
  | control (command : Command)
  /-- A preset chosen for a session, possibly since changed. -/
  | preset (config : Settings) (session : Nat) (changed : Bool)
  /-- A microphone event for a session. -/
  | capture (session : Nat) (capture : Capture)
  /-- A screen recorder event for a session. -/
  | screening (session : Nat) (screening : Screening)
  /-- A worker completion. -/
  | worker (completion : Completion)
  /-- The live session's delivery ended. -/
  | delivered (delivered : Delivered)
  /-- A retired session's delivery ended. -/
  | abandoned (session : Nat) (delivered : Delivered)
  /-- A retiring recorder reported back. -/
  | retired (recorder : Recorder)
  /-- Recovery found an interrupted Show. -/
  | recover
  /-- A practice window opened. -/
  | practice
  /-- The practice window closed. -/
  | practiceClosed
  /-- The gesture deadline passed. -/
  | timer

/-- Whether a delivery saved a Show that no later remembered Show supersedes. -/
def savedAnyway (remembered : Option Nat) (session : Nat) : Delivered → Bool
  | .published (.saved _) | .published .recovered =>
    match remembered with
    | some later => decide (later < session)
    | none => true
  | _ => false

/-- The machine with the environment it talks to. -/
structure Run extends Machine where
  /-- The environment. -/
  env : Env

namespace Run

/-- Records an effect. -/
def emit (effect : Effect) (run : Run) : Run :=
  { run with env := run.env.emit effect }

/-- Asks the environment a query. -/
def ask (query : Query) (run : Run) : Nat × Run :=
  let (answer, env) := run.env.ask query
  (answer, { run with env })

/-- Loads the model when none is loaded; reports whether it did. -/
def revive (run : Run) : Bool × Run :=
  if run.worker == .unavailable then (true, { run.emit .load with worker := .loading })
  else (false, run)

/-- Replaces a stuck worker. -/
def recoverWorker (run : Run) : Run :=
  let streaming : Bool := match run.worker with
    | .transcribing ticket => ticket.part == .stream
    | .unavailable | .loading | .ready => false
  { run.emit (.recover (run.config.probe || streaming)) with worker := .loading }

/-- Plays a cue when sounds are on. -/
def cue (cue : Cue) (run : Run) : Run :=
  if run.config.playSounds then run.emit (.cue cue) else run

/-- Plays a cue the microphone will hear. -/
def cueHeard (cue : Cue) (run : Run) : Run :=
  if run.config.playSounds then
    (if run.microphone == .opened then run.emit .expectCue else run).emit (.cue cue)
  else run

/-- Holds other apps' cues exactly while a session runs with sounds on. -/
def holdCues (run : Run) : Run :=
  let held : Bool := run.config.playSounds && run.session.isSome
  if held != run.cuesHeld then { run.emit (.holdCues held) with cuesHeld := held } else run

/-- Clears a warning. -/
def clearWarning (run : Run) : Run :=
  match run.notice with
  | some notice => if notice.isWarning then { run with notice := none } else run
  | none => run

/-- Shows a warning unless a failure is showing. -/
def warn (warning : Notice) (run : Run) : Run :=
  match run.notice with
  | some notice => if notice.failure then run else { run with notice := some warning }
  | none => { run with notice := some warning }

/-- A recorder's state. -/
def device (recorder : Recorder) (run : Run) : Device :=
  match recorder with
  | .microphone => run.microphone
  | .screen => run.screen

/-- Sets a recorder's state. -/
def setDevice (recorder : Recorder) (device : Device) (run : Run) : Run :=
  match recorder with
  | .microphone => { run with microphone := device }
  | .screen => { run with screen := device }

/-- Asks an open recorder to finish. -/
def finishRecorder (recorder : Recorder) (run : Run) : Run :=
  if run.device recorder == .opened then run.emit (.finish recorder) else run

/-- Retires an open recorder. -/
def retireRecorder (recorder : Recorder) (run : Run) : Run :=
  if run.device recorder == .opened then (run.setDevice recorder .retiring).emit (.retire recorder)
  else run

/-- A recorder opened; it finishes at once when the gesture already did. -/
def openRecorder (recorder : Recorder) (run : Run) : Run :=
  let opened : Run := run.setDevice recorder .opened
  if opened.gesture.state == .processing then opened.emit (.finish recorder) else opened

/-- Drops the live session, retiring what it owned. -/
def abandon (run : Run) : Run :=
  let run : Run := run.clearWarning
  match run.session with
  | none => run
  | some session =>
    let run : Run := { run with session := none }.emit .revoke
    let run : Run := if session.isShow then run.retireRecorder .screen else run
    let run : Run := if session.isCapturing then run.retireRecorder .microphone else run
    let stuck : Bool := (session.isTranscribing && run.worker.isTranscribing) ||
      run.worker.isStreaming session.id
    let run : Run := if stuck then run.recoverWorker else run
    let (retires, env) := session.cleanup run.env
    let run : Run := { run with env }
    if retires then { run with retiring := run.retiring ++ [session.id] } else run

/-- Ends the live session with an outcome. -/
def endSession (outcome : Outcome) (run : Run) : Run :=
  let run : Run := match run.session with
    | some _ => { run with session := none }.emit .revoke
    | none => run
  { run.clearWarning with gesture := run.gesture.complete, outcome }

/-- Drops the live session as cancelled. -/
def discard (run : Run) : Run :=
  { run.abandon.revive.2 with outcome := .cancelled }

/-- Drops the live session as failed. -/
def fail (failure : Failure) (run : Run) : Run :=
  { { run with gesture := run.gesture.abandon.2 }.abandon with outcome := .failed failure }

/-- Hands the audio log to the open microphone. -/
def attachAudio (run : Run) : Run :=
  match run.session with
  | none => run
  | some session =>
    let (held, session) := session.takeAudioLog
    let run : Run := { run with session := some session }
    if !held then run
    else if run.microphone == .opened then
      let (answer, run) := run.ask .keepAudio
      if answer == 0 then run else run.warn .checkpointLost
    else run.warn .checkpointLost

/-- Opens the microphone for a queued session. -/
def openQueuedCapture (run : Run) : Run :=
  if run.microphone != .free then run
  else
    match run.session with
    | none => run
    | some session =>
      if !session.isQueued then run
      else
        let (closed, run) := run.ask .closed
        if closed == 1 then run
        else
          let mode : RecognitionMode :=
            if session.isShow then .whole else run.config.recognition
          let (answer, run) := run.ask (.microphone session.id mode)
          if answer == 0 then { run with session := some session.opened }.openRecorder .microphone
          else run.fail .said

/-- Opens the screen recorder for a queued Show. -/
def openQueuedScreen (run : Run) : Run :=
  if run.screen != .free then run
  else
    match run.session with
    | none => run
    | some session =>
      if !session.queuedScreen then run
      else
        let (closed, run) := run.ask .closed
        if closed == 1 then run
        else
          let (frames, session) := session.takeFrameLog
          let (answer, run) := { run with session := some session }.ask (.screen session.id frames)
          if answer == 0 then
            { run with session := some session.screenOpened }.openRecorder .screen
          else { run with session := some session.forfeit }.fail .said

/-- Composes recognized text and delivers it: to practice, or into the focused app. -/
def insertTranscript (run : Run) : Run :=
  match run.session with
  | none => run
  | some session =>
    if !session.isRecognizing then run
    else
      let (composed, run) := run.ask .compose
      if composed != 0 then run.endSession .empty
      else if session.purpose == .practice then
        let (delivered, run) := run.ask .practise
        run.endSession (if delivered == 0 then .practiced else .cancelled)
      else
        let (session, env) := { session with pendingText := true }.insert run.env
        { run with session := some session, env }

/-- Sends the live session's next transcription when the worker is idle. -/
def transcribe (run : Run) : Run :=
  if run.worker != .ready then run
  else
    match run.session with
    | none => run
    | some session =>
      let (request, session, env) := session.request run.env
      let run : Run := { run with session := some session, env }
      match request with
      | some request =>
        { run with worker := .transcribing request.ticket }.emit (.transcribe request)
      | none => run

/-- Finishes recognition that needs no worker. -/
def completeRecognition (run : Run) : Run :=
  match run.session with
  | some session =>
    let (done, session) := session.complete
    let run : Run := { run with session := some session }
    if done then run.insertTranscript else run
  | none => run

/-- Loads the model the live session waits for, then feeds the worker. -/
def feedWorker (run : Run) : Run :=
  match run.session with
  | none => run
  | some session =>
    let run : Run :=
      if session.awaitsWorker then
        let (revived, run) := run.revive
        if revived then { run with notice := some .loadingModel } else run
      else run
    run.transcribe

/-- Finishes recognition that needs no worker, then feeds the worker. -/
def startTranscription (run : Run) : Run :=
  run.completeRecognition.feedWorker

/-- Publishes a ready Show. -/
def startPublishing (run : Run) : Run :=
  match run.session with
  | some session =>
    let (session, env) := session.publish run.env
    { run with session := some session, env }
  | none => run

/-- Moves the live session as far as it can go without new events. -/
def advance (run : Run) : Run :=
  run.openQueuedCapture.attachAudio.openQueuedScreen.startTranscription.startPublishing

/-- Starts a session when the environment admits one. -/
def startSession (run : Run) : Run :=
  let (admitted, run) := run.ask .admit
  if admitted != 0 then { run with gesture := run.gesture.abandon.2 }
  else
    let id : Nat := run.latest.id + 1
    let session : Session := Session.new id
    let session : Session := if run.practice then { session with purpose := .practice } else session
    { run with latest := { id, showing := false }, outcome := .idle, notice := none,
               session := some session }

/-- Carries out what a gesture transition asks. -/
def handleAction (action : Option Action) (run : Run) : Run :=
  match action with
  | some .start => run.startSession
  | some .finish =>
    let run : Run := (run.finishRecorder .microphone).finishRecorder .screen
    { run with session := run.session.map Session.finish }.cue .stop
  | some .cancel => run.discard.cue .cancel
  | none => run

/-- Turns a held or hands-free recording into a Show. -/
def startShow (run : Run) : Run :=
  let held : Bool := run.gesture.state == .held || run.gesture.state == .handsFree
  match run.session with
  | none => run
  | some session =>
    if !held then run
    else
      let (turned, session, env) := session.turnIntoShow run.env
      let run : Run := { run with session := some session, env }
      if !turned then run
      else
        let run : Run := if run.microphone == .opened then run.emit .listenWhole else run
        let (kept, run) := run.ask .keep
        let run : Run :=
          if kept == 0 then { run with session := some session.keep }
          else if kept == 1 then run
          else run.warn .checkpointLost
        let run : Run := { run with latest.showing := true }
        let run : Run := if session.isRecording then run.cueHeard .showing else run
        { run with gesture := run.gesture.present }

/-- Ends a held or hands-free plain dictation, so its text cannot race a paste. -/
def endPlain (run : Run) : Run :=
  let plain : Bool := match run.session with
    | some session => !session.isShow && !session.isPractice
    | none => false
  if (run.gesture.state == .held || run.gesture.state == .handsFree) && plain then
    { run with gesture := run.gesture.abandon.2 }.abandon.revive.2
  else run

/-- Starts a replay session that inserts the last dictation again, when the gesture is idle. -/
def replay (run : Run) : Run :=
  let (delivering, gesture) := run.gesture.deliver
  if !delivering then run
  else
    let run : Run := { run with gesture }.startSession
    match run.session with
    | some session =>
      let (session, env) := { session with purpose := .replay }.insert run.env
      { run with session := some session, env }
    | none => run

/-- Pastes the last dictation again, ending a plain recording in progress. -/
def pasteLast (run : Run) : Run :=
  if run.practice then run
  else if !run.lastDictation then
    if run.session.isNone then { run with outcome := .nothingToPaste } else run
  else
    let run : Run := run.endPlain
    if run.session.isSome then run else run.replay

/-- Arms a practice window when no session runs. -/
def armPractice (run : Run) : Run :=
  if run.session.isSome then run.emit .refusePractice
  else
    let (ready, run) := run.ask .ready
    if ready == 0 then { run with practice := true } else run

/-- Disarms practice, cancelling a practice session. -/
def endPractice (run : Run) : Run :=
  let run : Run := { run with practice := false }
  match run.session with
  | some session =>
    if session.isPractice then { run with gesture := run.gesture.abandon.2 }.discard.cue .cancel
    else run
  | none => run

/-- Reopens an interrupted Show when idle, admitted, and shielded. -/
def startRecovery (run : Run) : Run :=
  let (saving, gesture) := run.gesture.save
  if !saving then run
  else
    let (admitted, run) := { run with gesture }.ask .admit
    if admitted != 0 then { run with gesture := run.gesture.abandon.2 }
    else
      let (shielded, run) := run.ask .shield
      if shielded != 0 then { run with gesture := run.gesture.abandon.2 }
      else
        let id : Nat := run.latest.id + 1
        let run : Run :=
          { run with latest := { id, showing := true }, outcome := .idle, notice := none }
        { run.emit .reopen with session := some (Session.recovering id) }

/-- Shows a report when no session runs. -/
def report (report : Report) (run : Run) : Run :=
  if run.session.isSome then run
  else
    let run : Run := if report == .recurring then run else run.emit .announce
    { run with outcome := match report with
      | .failed | .recurring => .failed .said
      | .showCopied => .showCopied
      | .showGone => .showGone }

/-- Applies settings, reloading the model when `reloads` says the change needs it. -/
def applySettings (reloads : Settings → Settings → Bool) (config : Settings) (run : Run) :
    Run :=
  let reload : Bool := reloads run.config config
  let run : Run := { run with config }
  let run : Run := if reload then { run.emit .reload with worker := .loading } else run
  let run : Run := run.abandon.revive.2
  { run with notice := some (if run.worker.model == .ready then .settingsApplied
                             else .loadingModel) }

/-- New settings disarm practice and presets and cancel the live session. -/
def configure (reloads : Settings → Settings → Bool) (config : Settings) (run : Run) : Run :=
  let (cancelled, gesture) := run.gesture.abandon
  let run : Run :=
    { run with practice := false, preset := false, gesture }.applySettings reloads config
  if cancelled.isSome then { run.cue .cancel with outcome := .cancelled }
  else { run with outcome := .idle }

/-- The microphone finished: it retires, the screen finishes, and so does the gesture. -/
def concludeCapture (run : Run) : Run :=
  let run : Run := (run.retireRecorder .microphone).finishRecorder .screen
  let (finished, gesture) := run.gesture.finish
  let run : Run := { run with gesture }
  if finished.isSome then run.cue .stop else run

/-- A microphone event for the capturing session with this id. -/
def handleCapture (id : Nat) (capture : Capture) (run : Run) : Run :=
  let processing : Bool := run.gesture.state == .processing
  let streamReady : Bool := run.worker == .ready
  match run.session with
  | none => run
  | some session =>
    if !(session.id == id && session.isCapturing) then run
    else
      match capture with
      | .streaming => if streamReady then { run with session := some session.stream } else run
      | .piece number => { run with session := some (session.piece number) }
      | .checkpointLost => run.warn .checkpointLost
      | .ready =>
        let session : Session := session.audioReady processing
        let run : Run := { run with session := some session }
        if session.isRecording then run.cueHeard (if session.isShow then .showing else .start)
        else run
      | .level => run.emit (.meter id)
      | .finished (.sealed sealed) =>
        let (delivers, session) := session.captured sealed
        let run : Run := { run with session := some session }.concludeCapture
        if delivers then run else run.endSession .empty
      | .finished (.unheard silent) =>
        let (listened, session) := session.audioFailed silent
        if listened then
          { run with session := some session, notice := some .narrationLost }.concludeCapture
        else run.fail .said

/-- Whether the live session can ride out a model that failed to load. -/
def sessionSurvivesWarmupFailure (run : Run) : Bool :=
  match run.session with
  | some session => !session.awaitsWorker
  | none => false

/-- A worker completion. -/
def handleWorker (completion : Completion) (run : Run) : Run :=
  let run : Run := { run with worker := .unavailable }
  match completion with
  | .abandoned ticket =>
    let run : Run := { run with worker := .ready }
    match run.session with
    | some session => { run with session := some (session.failed ticket).2 }
    | none => run
  | .loaded true =>
    let run : Run := { run with worker := .ready }
    match run.notice with
    | some .loadingModel | some .warmupFailed => { run with notice := none }
    | _ => run
  | .loaded false =>
    if run.sessionSurvivesWarmupFailure then
      if run.notice == some .narrationLost then run
      else { run with notice := some .warmupFailed }
    else
      match run.session with
      | some session =>
        let (gaveUp, session) := session.notTranscribed
        if gaveUp then { run with session := some session, notice := some .warmupFailed }
        else run.fail .said
      | none => run.fail .said
  | .transcribed ticket (some transcript) =>
    let run : Run := { run with worker := .ready }
    match run.session with
    | some session =>
      let (text, session) := session.heard ticket transcript
      let run : Run := { run with session := some session }
      if text then run.insertTranscript else run
    | none => run
  | .transcribed ticket none =>
    let (replaced, run) := run.revive
    match run.session with
    | none => run
    | some session =>
      if !session.accepts ticket then run
      else
        let run : Run :=
          if replaced && session.isTranscribing then { run with notice := some .loadingModel }
          else run
        let (handled, session) := session.failed ticket
        let run : Run := { run with session := some session }
        if handled then run
        else
          let (gaveUp, session) := session.notTranscribed
          if gaveUp then { run with session := some session, notice := some .narrationLost }
          else run.fail .said

/-- A screen recorder event for the Show with this id. -/
def handleShow (id : Nat) (screening : Screening) (run : Run) : Option Action × Run :=
  match run.session with
  | none => (none, run)
  | some session =>
    if !(session.id == id && session.isShow) then (none, run)
    else
      match screening with
      | .checkpointLost => (none, run.warn .checkpointLost)
      | .movieStopped => (none, run.warn .movieStopped)
      | .finished =>
        let run : Run := { run with session := some session.screenRecorded }.retireRecorder .screen
        let (action, gesture) := run.gesture.finish
        (action, { run with gesture })
      | .stopped unwound =>
        let run : Run := if unwound then run else { run with session := some session.forfeit }
        (none, run.fail .said)
      | .unchosen =>
        let run : Run :=
          { run with session := some session.forfeit, gesture := run.gesture.abandon.2 }
        (none, { run.discard.cue .cancel with outcome := .unchosen })

/-- Remembers a published Show. -/
def remember (session : Nat) (run : Run) : Run :=
  { run.emit .remember with remembered := some session }

/-- The outcome a delivery ends the live session with. -/
def deliveredOutcome (presenting : Bool) (delivered : Delivered) (run : Run) : Outcome × Run :=
  match delivered with
  | .inserted .sent => (.done, run)
  | .inserted .copied => (.copied, run)
  | .published (.saved copied) =>
    let run : Run := run.remember run.latest.id
    let failure : Bool := match run.notice with
      | some notice => notice.failure
      | none => false
    (if copied then .showCopied else .failed (.notCopied failure), run)
  | .inserted .cancelled | .published .cancelled | .abandoned true => (.cancelled, run)
  | .inserted .unavailable => (.failed (.undelivered true false), run)
  | .published .recovered => (.recovered, run.remember run.latest.id)
  | .abandoned false | .published .failed | .reopened none => (.failed .said, run)
  | .reopened (some _) => (.idle, run)
  | .panicked => (.failed (if presenting then .publishStopped else .undelivered false false), run)
  | .inserted .failed => (.failed (.undelivered false false), run)

/-- The live session's delivery ended while the session awaited it. -/
def deliveredBy (delivered : Delivered) (run : Run) : Run :=
  let presenting : Bool := match run.session with
    | some session => session.isShow
    | none => false
  let remembered : Bool := match delivered with
    | .inserted .sent | .inserted .copied | .inserted .unavailable | .inserted .failed
    | .panicked => true
    | _ => false
  let run : Run :=
    if remembered then
      match run.session with
      | some session =>
        if session.pendingText then
          { run with session := some { session with pendingText := false },
                     lastDictation := true }
        else run
      | none => run
    else run
  match delivered with
  | .reopened (some recognized) =>
    { run with session := run.session.map (Session.reopened recognized) }
  | _ =>
    let (outcome, run) := run.deliveredOutcome presenting delivered
    let outcome : Outcome := match outcome with
      | .failed (.undelivered said _) =>
        if !presenting && remembered && run.lastDictation then .failed (.undelivered said true)
        else outcome
      | _ => outcome
    let run : Run := match outcome, run.session with
      | .failed failure, some session =>
        if session.receipt then
          { run with session := some { session with receipt := false } }.emit
            (.refuseRecovery failure)
        else run
      | _, _ => run
    let run : Run := match outcome with
      | .copied | .showCopied => run.cue .copied
      | _ => run
    run.endSession outcome

/-- The live session's delivery ended. A result with no delivery in flight is ignored. -/
def handleDelivered (delivered : Delivered) (run : Run) : Run :=
  match run.session with
  | some session => if session.awaitsDelivery then run.deliveredBy delivered else run
  | none => run

/-- A retired session's delivery ended. A result for no retiring session is ignored. -/
def handleAbandoned (id : Nat) (delivered : Delivered) (run : Run) : Run :=
  if !run.retiring.contains id then run
  else
    let run : Run := { run with retiring := withoutFirst id run.retiring }
    match delivered with
    | .abandoned false =>
      if run.session.isNone && run.latest.id == id then
        { run with outcome := .failed .recoveryStorage }
      else run
    | delivered => if savedAnyway run.remembered id delivered then run.remember id else run
where
  /-- The sessions without the first occurrence of this one. -/
  withoutFirst (id : Nat) : List Nat → List Nat
    | [] => []
    | first :: rest => if first == id then rest else first :: withoutFirst id rest

/-- The dictation the control interface may stop or cancel. -/
def controlSession (run : Run) : Option Nat :=
  match run.session with
  | some session => if !session.isShow && session.purpose == .dictation then some session.id
    else none
  | none => none

/-- The status the control interface reports. -/
def status (run : Run) : Status :=
  { phase := match run.session with
      | some session => session.phase
      | none => run.outcome.phase,
    model := run.worker.model, desktopReady := run.desktopReady,
    session := run.controlSession, preset := run.preset, lastDictation := run.lastDictation }

/-- Answers a control call. -/
def respond (refused : Option Refusal) (run : Run) : Run :=
  match refused with
  | some refusal => run.emit (.refuse refusal)
  | none => run.emit (.respond run.status)

/-- A control command, answered with a status or a refusal. -/
def control (now : Nat) (command : Command) (run : Run) : Run :=
  let (refused, run) : Option Refusal × Run := match command with
    | .status => (none, run)
    | .start =>
      if run.session.isSome || run.practice then (some .busy, run)
      else if !run.desktopReady || run.worker.model != .ready then (some .notReady, run)
      else
        let (action, gesture) := run.gesture.toggle now
        let run : Run := { run with gesture }.handleAction action
        (if run.controlSession.isNone then some .startStopped else none, run)
    | .stop token =>
      if token.isNone || run.controlSession != token then (some .notActive, run)
      else
        let (action, gesture) := run.gesture.finish
        (none, { run with gesture }.handleAction action)
    | .cancel token =>
      if token.isNone || run.controlSession != token then (some .notActive, run)
      else
        let (action, gesture) := run.gesture.cancel
        (none, { run with gesture }.handleAction action)
    | .pasteLast =>
      if run.session.isSome || run.practice then (some .pasteBusy, run)
      else if !run.desktopReady then (some .pasteNotReady, run)
      else if !run.lastDictation then (some .nothingToPaste, run)
      else
        let run : Run := run.pasteLast
        let replaying : Bool := match run.session with
          | some session => session.purpose == .replay
          | none => false
        (if replaying then none else some .pasteStopped, run)
    | .unsupported => (some .unsupported, run)
  run.respond refused

/-- Applies a preset chosen for the latest session, unless a session runs or it went stale. -/
def adoptPreset (reloads : Settings → Settings → Bool) (config : Settings) (session : Nat)
    (changed : Bool) (run : Run) : Option Refusal × Run :=
  if run.session.isSome || run.practice then (some .presetBusy, run)
  else if run.latest.id != session || changed then (some .presetStale, run)
  else (none, { run.applySettings reloads config with preset := true })

/-- Keyboard and desktop input. Returns the gesture's action. -/
def handleInput (now : Nat) (input : Input) (run : Run) : Option Action × Run :=
  let ready : Bool := run.desktopReady
  let gestured (result : Option Action × Gesture) : Option Action × Run :=
    (result.1, { run with gesture := result.2 })
  match input with
  | .press => if ready then gestured (run.gesture.press now) else (none, run)
  | .toggle => if ready then gestured (run.gesture.toggle now) else (none, run)
  | .pasteLast => (none, if ready then run.pasteLast else run)
  | .startShow =>
    if ready && run.gesture.state == .idle then
      let run : Run := run.endPractice
      let (action, gesture) := run.gesture.toggle now
      (none, { run with gesture }.handleAction action |>.startShow)
    else (none, run)
  | .release => gestured (run.gesture.release now)
  | .lock => (none, { run with gesture := run.gesture.lock })
  | .makeShow => (none, run.startShow)
  | .pin => (none, if run.screen == .opened then run.emit .pin else run)
  | .escape => gestured run.gesture.escape
  | .cancel => gestured run.gesture.cancel
  | .interrupt =>
    let (action, gesture) := run.gesture.interrupt
    let run : Run := { run with gesture }
    match action with
    | some .cancel => (none, run.discard)
    | action => (action, run)
  | .unavailable => (none, run)
  | .desktopReady => (none, { run with desktopReady := true })

/-- Handles one event. A flow other than `running` ends the app. -/
def handle (reloads : Settings → Settings → Bool) (now : Nat) (event : Event) (run : Run) :
    Flow × Run :=
  match event with
  | .stop => (.stopped, run)
  | .inputLost => (.failed, run.fail .inputLost)
  | .input .unavailable => (.failed, run.fail .said)
  | .input input =>
    let (action, run) := run.handleInput now input
    (.running, run.handleAction action)
  | .report report => (.running, run.report report)
  | .configure config => (.running, run.configure reloads config)
  | .control command => (.running, run.control now command)
  | .preset config session changed =>
    let (refused, run) := run.adoptPreset reloads config session changed
    (.running, run.respond refused)
  | .capture session capture => (.running, run.handleCapture session capture)
  | .screening session screening =>
    let (action, run) := run.handleShow session screening
    (.running, run.handleAction action)
  | .worker completion => (.running, run.handleWorker completion)
  | .delivered delivered => (.running, run.handleDelivered delivered)
  | .abandoned session delivered => (.running, run.handleAbandoned session delivered)
  | .retired recorder =>
    (.running, if run.device recorder == .retiring then run.setDevice recorder .free else run)
  | .recover => (.running, run.startRecovery)
  | .practice => (.running, run.armPractice)
  | .practiceClosed => (.running, run.endPractice)
  | .timer =>
    let (action, gesture) := run.gesture.tick now
    (.running, { run with gesture }.handleAction action)

/-- One step: handle the event, then advance the session and hold cues. -/
def step (reloads : Settings → Settings → Bool) (now : Nat) (event : Event) (run : Run) :
    Flow × Run :=
  match run.handle reloads now event with
  | (.running, run) => (.running, run.advance.holdCues)
  | stopped => stopped

/-- Shuts down: input closes and the live session is dropped. -/
def shutDown (run : Run) : Run :=
  (run.emit .closeInput).abandon

end Run

end Speakeasy

end
