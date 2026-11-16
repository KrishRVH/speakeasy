module

import Speakeasy.Spec.Machine
import Std.Data.HashMap.Raw

/-!
# speakeasy_model executable

Explores the session machine model breadth first, exactly as the Rust explorer in
`crates/core/src/machine/explore.rs` explores the Rust machine: the same events in the same
order, every answer the environment can give, the same effect text, and the same canonical
states. It prints one FNV-1a digest of each expanded state's transitions, so comparing its
output with the Rust explorer's shows the two machines agree on every transition explored.

The argument is the search depth. String formatting stays here, outside the constructive
library.
-/

open Speakeasy

/-- The scaled timing both explorers use: a tap, a double tap, and a recording limit. -/
def timing : Timing := { tap := 1, doubleTap := 2, limit := 3 }

/-- The settings an exploration moves between. -/
def configs : List Settings :=
  [{ playSounds := true, recognition := .segmented, probe := false },
    { playSounds := false, recognition := .streaming, probe := false },
    { playSounds := false, recognition := .whole, probe := true }]

/-- The first configuration, which the machine starts with. -/
def firstConfig : Settings :=
  { playSounds := true, recognition := .segmented, probe := false }

/-- The explorers reload the model when recognition changes. -/
def reloads (current : Settings) (next : Settings) : Bool :=
  current.recognition != next.recognition

/-- Explored states with more retiring sessions than this are not expanded. -/
def retiringBound : Nat := 2

/-- One explored state: the machine, the clock, and the recorder mode its microphone opened
with. -/
structure Explored where
  /-- The machine. -/
  machine : Machine
  /-- The clock. -/
  clock : Nat
  /-- The session and mode the microphone last opened for. -/
  recorder : Option (Nat × RecognitionMode)

/-- The recorder mode, while the session it opened for still captures. -/
def Explored.mode (state : Explored) : Option RecognitionMode :=
  match state.machine.session, state.recorder with
  | some session, some (id, mode) =>
    if session.id == id && session.isCapturing then some mode else none
  | _, _ => none

/-- An explored step: time passes, or an event arrives. -/
inductive Step where
  /-- One tick passes. -/
  | elapse
  /-- An event arrives. -/
  | event (event : Event)

/-- The stale session id events for earlier sessions carry. -/
def stale : Nat := 0

/-- The outcome as far as the machine itself observes it: its phase and whether it is a
copied Show. -/
def observed (outcome : Outcome) : Outcome :=
  match outcome with
  | .showCopied => .showCopied
  | outcome => match outcome.phase with
    | .done => .done
    | .copied => .copied
    | .notice => .showGone
    | .empty => .empty
    | .cancelled => .cancelled
    | .unchosen => .unchosen
    | .error => .failed .inputLost
    | .idle | .starting | .recording | .stopping | .processing => .idle

/-- The machine with its output-only outcome projected and a stale worker ticket replaced by
the stale id, which no session can match. -/
def shown (machine : Machine) : Machine :=
  let worker : Worker := match machine.worker with
    | .transcribing ticket =>
      let current : Bool := match machine.session with
        | some session => session.id == ticket.session
        | none => false
      if current then machine.worker else .transcribing { session := stale, part := .final }
    | worker => worker
  { machine with outcome := observed machine.outcome, worker }

/-- Inserts into a sorted list without duplicates. -/
def insertSorted (id : Nat) : List Nat → List Nat
  | [] => [id]
  | first :: rest =>
    if id < first then id :: first :: rest
    else if id == first then first :: rest
    else first :: insertSorted id rest

/-- Every session id a machine mentions, sorted and without duplicates. -/
def ids (machine : Machine) : List Nat :=
  let session : List Nat := match machine.session with
    | some session => [session.id, session.recognition.id]
    | none => []
  let worker : List Nat := match machine.worker with
    | .transcribing ticket => [ticket.session]
    | _ => []
  let all : List Nat := machine.latest.id :: session ++ worker ++ machine.remembered.toList ++
    machine.retiring
  all.foldl (fun (sorted : List Nat) (id : Nat) => insertSorted id sorted) []

/-- An id's rank among the ids, or `none` for an id the list lacks. -/
def rank (ids : List Nat) (id : Nat) : Option Nat :=
  ids.idxOf? id

/-- An id renamed by rank. -/
def renamed (ids : List Nat) (id : Nat) : Nat :=
  (rank ids id).getD id

/-- The name an effect gives a session: its rank in the source state, or `new`. -/
def name (ids : List Nat) (id : Nat) : String :=
  match rank ids id with
  | some rank => toString rank
  | none => "new"

/-- A moment's age, capped at the recording limit. -/
def age (clock : Nat) (moment : Nat) : Nat :=
  min (clock - moment) timing.limit

/-- The gesture with moments replaced by their ages. -/
def aged (clock : Nat) (gesture : Gesture) : Gesture :=
  let phase : GesturePhase := match gesture.phase with
    | .held started => .held (age clock started)
    | .pendingTap started released => .pendingTap (age clock started) (age clock released)
    | .handsFree started => .handsFree (age clock started)
    | .showing started => .showing (age clock started)
    | .showArmed started => .showArmed (age clock started)
    | phase => phase
  { gesture with phase }

/-- The canonical form both explorers deduplicate on: ids by rank and moments by age. -/
def canonical (clock : Nat) (machine : Machine) : Machine :=
  let machine : Machine := shown machine
  let ids : List Nat := ids machine
  let session : Option Session := machine.session.map (fun (session : Session) =>
    { session with id := renamed ids session.id,
                   recognition := { session.recognition with
                                    id := renamed ids session.recognition.id } })
  let worker : Worker := match machine.worker with
    | .transcribing ticket => .transcribing { ticket with session := renamed ids ticket.session }
    | worker => worker
  { machine with
    gesture := aged clock machine.gesture
    latest := { machine.latest with id := renamed ids machine.latest.id }
    session
    worker
    remembered := machine.remembered.map (renamed ids)
    retiring := machine.retiring.map (renamed ids) }

/-- Text for a Boolean. -/
def flag (value : Bool) : String :=
  if value then "1" else "0"

/-- Text for an maybe value. -/
def maybe {α : Type} (text : α → String) : Option α → String
  | some value => s!"<{text value}>"
  | none => "_"

/-- Text for a list. -/
def listed {α : Type} (text : α → String) (values : List α) : String :=
  "[" ++ ",".intercalate (values.map text) ++ "]"

/-- Key text for a failure. -/
def failureKey (failure : Failure) : String :=
  let fields : String := match failure with
    | .undelivered said replay => s!"{flag said}{flag replay}"
    | .notCopied failure => flag failure
    | .said | .inputLost | .publishStopped | .recoveryStorage => ""
  s!"{failure.ctorIdx}{fields}"

/-- Key text for an outcome. -/
def outcomeKey (outcome : Outcome) : String :=
  let fields : String := match outcome with
    | .failed failure => failureKey failure
    | _ => ""
  s!"{outcome.ctorIdx}{fields}"

/-- Key text for a gesture phase. -/
def phaseKey (phase : GesturePhase) : String :=
  let fields : List Nat := match phase with
    | .held started | .handsFree started | .showing started | .showArmed started => [started]
    | .pendingTap started released => [started, released]
    | .idle | .processing | .saving => []
  s!"{phase.ctorIdx}{listed toString fields}"

/-- Key text for a ticket. -/
def ticketKey (ticket : Ticket) : String :=
  let part : String := match ticket.part with
    | .piece number => s!"p{number}"
    | .stream => "s"
    | .final => "f"
  s!"{ticket.session}{part}"

/-- Key text for a stage. -/
def stageKey (stage : Stage) : String :=
  let fields : String := match stage with
    | .recognizing incomplete => flag incomplete
    | .awaitingFootage lost => maybe (fun (loss : NarrationLoss) => toString loss.ctorIdx) lost
    | .queued | .opening | .reopening | .recording | .stopping | .delivering => ""
  s!"{stage.ctorIdx}{fields}"

/-- Key text for recognition. -/
def recognitionKey (recognition : Recognition) : String :=
  let early : String := match recognition.early with
    | .segmented pieces =>
      "g" ++ listed (fun (piece : Piece) => toString piece.ctorIdx) pieces
    | .streaming stream => s!"s{stream.ctorIdx}"
    | .whole => "w"
  let part : Final → String := fun (part : Final) =>
    let mode : String := match part.mode with
      | .whole => "w"
      | .tail empty => s!"t{flag empty}"
    s!"{part.detail.ctorIdx}{mode}{part.progress.ctorIdx}{flag part.retried}"
  s!"{recognition.id};{early};{maybe part recognition.finalPart}"

/-- Key text for a Show's state. -/
def showKey (state : ShowState) : String :=
  s!"{state.screen.ctorIdx}{flag state.audio}{flag state.frames}{flag state.checkpoint}" ++
    s!"{state.disposition.ctorIdx}{state.origin.ctorIdx}"

/-- Key text for a session. -/
def sessionKey (session : Session) : String :=
  s!"{session.id};{session.purpose.ctorIdx}{flag session.pendingText};" ++
    s!"{stageKey session.stage};{recognitionKey session.recognition};" ++
    s!"{maybe showKey session.showState};{flag session.receipt}"

/-- Key text for the worker. -/
def workerKey (worker : Worker) : String :=
  match worker with
  | .transcribing ticket => s!"t{ticketKey ticket}"
  | worker => toString worker.ctorIdx

/-- Key text for a machine; the timing never changes, so it is left out. -/
def machineKey (machine : Machine) : String :=
  let config : Settings := machine.config
  s!"{flag config.playSounds}{config.recognition.ctorIdx}{flag config.probe}|" ++
    s!"{phaseKey machine.gesture.phase}{flag machine.gesture.down}|" ++
    s!"{flag machine.desktopReady}{machine.latest.id}{flag machine.latest.showing}|" ++
    s!"{maybe sessionKey machine.session}|" ++
    s!"{flag machine.practice}{flag machine.preset}{flag machine.lastDictation}|" ++
    s!"{maybe toString machine.remembered}|{workerKey machine.worker}|" ++
    s!"{machine.microphone.ctorIdx}{machine.screen.ctorIdx}|{listed toString machine.retiring}|" ++
    s!"{flag machine.cuesHeld}|{outcomeKey machine.outcome}|" ++
    s!"{maybe (fun (notice : Notice) => toString notice.ctorIdx) machine.notice}"

/-- The key a state is deduplicated on: its canonical machine and its recorder mode. -/
def key (state : Explored) : String :=
  s!"{machineKey (canonical state.clock state.machine)}|" ++
    s!"{maybe (fun (mode : RecognitionMode) => toString mode.ctorIdx) state.mode}"

/-- Rust's `Debug` text for a recognition mode. -/
def modeText : RecognitionMode → String
  | .whole => "Whole"
  | .segmented => "Segmented"
  | .streaming => "Streaming"

/-- Rust's `Debug` text for a narration loss. -/
def lossText : NarrationLoss → String
  | .incomplete => "Incomplete"
  | .audioStopped => "AudioStopped"
  | .audioSilent => "AudioSilent"
  | .notTranscribed => "NotTranscribed"

/-- Rust's `Debug` text for an maybe narration loss. -/
def optionalLoss : Option NarrationLoss → String
  | some loss => s!"Some({lossText loss})"
  | none => "None"

/-- Rust's text for a recorder. -/
def recorderText : Recorder → String
  | .microphone => "microphone"
  | .screen => "screen"

/-- Rust's `Debug` text for a part. -/
def partText : Part → String
  | .stream => "Stream"
  | .piece number => s!"Piece({number})"
  | .final => "Final"

/-- Rust's `Debug` text for a detail. -/
def detailText : Detail → String
  | .text => "Text"
  | .words => "Words"

/-- Rust's `Debug` text for an maybe unit. -/
def optionalUnit (present : Bool) : String :=
  if present then "Some(())" else "None"

/-- Rust's `Debug` text for a failure. -/
def failureText : Failure → String
  | .said => "Said(())"
  | .inputLost => "InputLost"
  | .undelivered said replay =>
    s!"Undelivered \{ said: {optionalUnit said}, replay: {replay} }"
  | .publishStopped => "PublishStopped"
  | .notCopied failure =>
    s!"NotCopied \{ saved: (), error: (), failure: {optionalUnit failure} }"
  | .recoveryStorage => "RecoveryStorage(())"

/-- Rust's `Debug` text for a release. -/
def releaseText : Release → String
  | .keep (some incomplete) => s!"Keep(Some(((), {incomplete})))"
  | .keep none => "Keep(None)"
  | .discard => "Discard"

/-- Rust's `Debug` text for a cue. -/
def cueText : Cue → String
  | .start => "Start"
  | .stop => "Stop"
  | .cancel => "Cancel"
  | .showing => "Show"
  | .copied => "Copied"

/-- Rust's `Debug` text for a phase. -/
def phaseText : Phase → String
  | .idle => "Idle"
  | .starting => "Starting"
  | .recording => "Recording"
  | .stopping => "Stopping"
  | .processing => "Processing"
  | .done => "Done"
  | .copied => "Copied"
  | .notice => "Notice"
  | .empty => "Empty"
  | .cancelled => "Cancelled"
  | .unchosen => "Unchosen"
  | .error => "Error"

/-- Rust's `Debug` text for a model state. -/
def modelText : ModelState → String
  | .loading => "Loading"
  | .ready => "Ready"
  | .unavailable => "Unavailable"

/-- Rust's `Debug` text for a refusal. -/
def refusalText : Refusal → String
  | .busy => "Busy"
  | .notReady => "NotReady"
  | .startStopped => "StartStopped"
  | .notActive => "NotActive"
  | .pasteBusy => "PasteBusy"
  | .pasteNotReady => "PasteNotReady"
  | .nothingToPaste => "NothingToPaste"
  | .pasteStopped => "PasteStopped"
  | .unsupported => "Unsupported"
  | .presetBusy => "PresetBusy"
  | .presetStale => "PresetStale"

/-- The answer names the Rust oracle prints for a query. -/
def answers : Query → List String
  | .admit => ["permit", "refused"]
  | .shield => ["shielded", "exposed"]
  | .closed => ["open", "closed"]
  | .keep => ["kept", "unneeded", "lost"]
  | .microphone _ _ | .screen _ _ => ["recording", "failed"]
  | .keepAudio => ["kept", "lost"]
  | .cut => ["cut", "uncut"]
  | .compose => ["text", "empty"]
  | .ready => ["ready", "closed"]
  | .practise => ["delivered", "closed"]

/-- The text the Rust oracle prints for a query. -/
def queryText (ids : List Nat) : Query → String
  | .admit => "admit"
  | .shield => "shield"
  | .closed => "closed"
  | .keep => "keep"
  | .microphone session mode => s!"microphone({name ids session},{modeText mode})"
  | .screen session frames => s!"screen({name ids session},{frames})"
  | .keepAudio => "keep_audio"
  | .cut => "cut"
  | .compose => "compose"
  | .ready => "ready"
  | .practise => "practise"

/-- The text the Rust oracle prints for an effect. -/
def effectText (ids : List Nat) : Effect → String
  | .asked query answer => s!"{queryText ids query}={(answers query).getD answer "?"}"
  | .revoke => "revoke"
  | .finish recorder => s!"finish({recorderText recorder})"
  | .retire recorder => s!"retire({recorderText recorder})"
  | .listenWhole => "listen_whole"
  | .expectCue => "expect_cue"
  | .pin => "pin"
  | .meter session => s!"meter({name ids session})"
  | .load => "load"
  | .reload => "reload"
  | .recover probe => s!"recover({probe})"
  | .transcribe request =>
    let source : String := match request.source with
      | .audio => "audio"
      | .live => "live"
    s!"transcribe({name ids request.ticket.session},{partText request.ticket.part},{source}," ++
      s!"{detailText request.detail})"
  | .insert => "insert"
  | .refusePractice => "refuse_practice"
  | .publish loss checkpoint origin receipt =>
    let origin : String := match origin with
      | .live => "live"
      | .recovered => "recovered"
    s!"publish({optionalLoss loss},{checkpoint},{origin},{receipt})"
  | .reopen => "reopen"
  | .refuseRecovery failure => s!"refuse_recovery({failureText failure})"
  | .retireDelivery session => s!"retire_delivery({name ids session})"
  | .release session release => s!"release({name ids session},{releaseText release})"
  | .remember => "remember"
  | .announce => "announce"
  | .cue cue => s!"cue({cueText cue})"
  | .holdCues held => s!"hold_cues({held})"
  | .respond status =>
    let session : String := match status.session with
      | some session => name ids session
      | none => "none"
    s!"respond({phaseText status.phase},{modelText status.model},{status.desktopReady}," ++
      s!"{session},{status.preset},{status.lastDictation})"
  | .refuse refusal => s!"respond({refusalText refusal})"
  | .closeInput => "close_input"

/-- Each item with its position, counting from `start`. -/
def numbered {α : Type} : Nat → List α → List (α × Nat)
  | _, [] => []
  | start, item :: rest => (item, start) :: numbered (start + 1) rest

/-- A labeled event. -/
def labeled (label : String) (event : Event) : String × Step :=
  (label, .event event)

/-- Input events, as the Rust alphabet lists them. -/
def inputSteps (machine : Machine) : List (String × Step) :=
  let inputs : List (String × Input) :=
    [("press", .press), ("release", .release), ("lock", .lock), ("show", .makeShow),
      ("pin", .pin), ("escape", .escape), ("cancel", .cancel), ("interrupt", .interrupt),
      ("toggle", .toggle), ("paste_last", .pasteLast), ("start_show", .startShow),
      ("unavailable", .unavailable)]
  let ready : List (String × Step) :=
    if machine.desktopReady then [] else [labeled "desktop_ready" (.input .desktopReady)]
  inputs.map (fun (entry : String × Input) => labeled entry.1 (.input entry.2)) ++ ready

/-- Control and preset events, as the Rust alphabet lists them. -/
def controlSteps (machine : Machine) : List (String × Step) :=
  let current : Nat := machine.latest.id
  let other : Nat := current + 1
  let commands : List (String × Command) :=
    [("control(status)", .status), ("control(start)", .start), ("control(start_styled)", .start),
      ("control(stop)", .stop (some current)), ("control(stop_other)", .stop (some other)),
      ("control(stop_unknown)", .stop none), ("control(cancel)", .cancel (some current)),
      ("control(cancel_other)", .cancel (some other)), ("control(cancel_unknown)", .cancel none),
      ("control(paste_last)", .pasteLast), ("control(unsupported)", .unsupported)]
  let presets : List (String × Step) := (numbered 0 configs).map (fun (entry : Settings × Nat) =>
    labeled s!"preset({entry.2})" (.preset entry.1 current false))
  commands.map (fun (entry : String × Command) => labeled entry.1 (.control entry.2)) ++
    presets ++
    [labeled "preset(other)" (.preset firstConfig other false),
      labeled "preset(changed)" (.preset firstConfig current true)]

/-- Microphone events for the live session, as the Rust alphabet lists them. -/
def captureSteps (session : Session) (mode : Option RecognitionMode) :
    List (String × Step) :=
  let staleSteps : List (String × Step) :=
    [labeled "capture(stale,ready)" (.capture stale .ready),
      labeled "capture(stale,finished)" (.capture stale (.finished (.sealed none)))]
  match mode with
  | none => staleSteps
  | some mode =>
    let common : List (String × Capture) :=
      [("ready", .ready), ("level", .level), ("finished(nothing)", .finished (.sealed none)),
        ("finished(whole)", .finished (.sealed (some none)))]
    let checkpoint : List (String × Capture) :=
      if session.isShow then [("checkpoint_lost", .checkpointLost)] else []
    let splits : List (String × Capture) :=
      [0, 1, 2].flatMap (fun (pieces : Nat) => [false, true].map (fun (empty : Bool) =>
        (s!"finished({pieces},{empty})", .finished (.sealed (some (some { pieces, empty }))))))
    let moded : List (String × Capture) := match mode with
      | .streaming => [("streaming", .streaming)]
      | .segmented => [("piece(0)", .piece 0), ("piece(1)", .piece 1)] ++ splits
      | .whole => []
    let failures : List (String × Capture) :=
      [false, true].map (fun (silent : Bool) =>
        (s!"failed({silent})", .finished (.unheard silent)))
    staleSteps ++ (common ++ checkpoint ++ moded ++ failures).map
      (fun (entry : String × Capture) =>
        labeled s!"capture({entry.1})" (.capture session.id entry.2))

/-- Screen events for the live session, as the Rust alphabet lists them. -/
def screenSteps (session : Session) : List (String × Step) :=
  let staleStep : String × Step := labeled "screen(stale,finished)" (.screening stale .finished)
  if !session.recordsScreen then [staleStep]
  else
    let screenings : List (String × Screening) :=
      [("checkpoint_lost", .checkpointLost), ("movie_stopped", .movieStopped),
        ("finished", .finished), ("stopped", .stopped false), ("unwound", .stopped true),
        ("unchosen", .unchosen)]
    staleStep :: screenings.map (fun (entry : String × Screening) =>
      labeled s!"screen({entry.1})" (.screening session.id entry.2))

/-- Publication results, as the Rust alphabet lists them. -/
def publications : List (String × Delivered) :=
  [("saved", .published (.saved true)), ("saved_uncopied", .published (.saved false)),
    ("recovered", .published .recovered), ("publish_cancelled", .published .cancelled),
    ("publish_failed", .published .failed)]

/-- Delivery results for the live session's stage, as the Rust alphabet lists them. -/
def deliverySteps (session : Session) : List (String × Step) :=
  let results : List (String × Delivered) :=
    if session.isReopening then
      [("reopened(speech,false)", .reopened (some (.speech false))),
        ("reopened(speech,true)", .reopened (some (.speech true))),
        ("reopened(narration,None)", .reopened (some (.narration none))),
        ("reopened(narration,Some(Incomplete))", .reopened (some (.narration (some .incomplete)))),
        ("reopened(narration,Some(AudioStopped))",
          .reopened (some (.narration (some .audioStopped)))),
        ("reopened(failed)", .reopened none), ("panicked", .panicked)]
    else if session.isDelivering && session.isShow then publications ++ [("panicked", .panicked)]
    else if session.isDelivering then
      [("inserted(sent)", .inserted .sent), ("inserted(copied)", .inserted .copied),
        ("inserted(unavailable)", .inserted .unavailable),
        ("inserted(cancelled)", .inserted .cancelled), ("inserted(failed)", .inserted .failed),
        ("panicked", .panicked)]
    else []
  results.map (fun (entry : String × Delivered) =>
    labeled s!"delivered({entry.1})" (.delivered entry.2))

/-- Worker completions for the worker's state, as the Rust alphabet lists them. -/
def workerSteps (machine : Machine) : List (String × Step) :=
  let completions : List (String × Completion) := match machine.worker with
    | .loading => [("loaded", .loaded true), ("load_failed", .loaded false)]
    | .transcribing ticket =>
      let words : Bool := ticket.part == .final && match machine.session with
        | some session => session.id == ticket.session && session.isShow
        | none => false
      let heard : String × Completion :=
        if words then ("words", .transcribed ticket (some .words))
        else ("text", .transcribed ticket (some .text))
      [heard, ("failed", .transcribed ticket none)] ++
        (if ticket.part == .stream then [("abandoned", .abandoned ticket)] else [])
    | .unavailable | .ready => []
  completions.map (fun (entry : String × Completion) =>
    labeled s!"worker({entry.1})" (.worker entry.2))

/-- Results for retiring sessions, as the Rust alphabet lists them. -/
def abandonedSteps (machine : Machine) : List (String × Step) :=
  let results : List (String × Delivered) :=
    [("released", .abandoned true), ("release_failed", .abandoned false),
      ("inserted", .inserted .sent), ("reopened(failed)", .reopened none),
      ("panicked", .panicked)] ++ publications
  (numbered 0 machine.retiring).flatMap (fun (entry : Nat × Nat) =>
    results.map (fun (result : String × Delivered) =>
      labeled s!"abandoned({entry.2},{result.1})" (.abandoned entry.1 result.2)))

/-- Every step enabled in a state, in the Rust alphabet's order. -/
def steps (state : Explored) : List (String × Step) :=
  let machine : Machine := state.machine
  let pending : Bool := match machine.gesture.deadline with
    | some deadline => state.clock < deadline
    | none => false
  let due : Bool := match machine.gesture.deadline with
    | some deadline => deadline ≤ state.clock
    | none => false
  let sessionSteps : List (String × Step) := match machine.session with
    | some session => captureSteps session state.mode ++ screenSteps session ++
      deliverySteps session
    | none => []
  let retired : List (String × Step) :=
    (if machine.microphone == .retiring then
      [labeled "retired(Microphone)" (.retired .microphone)] else []) ++
    (if machine.screen == .retiring then [labeled "retired(Screen)" (.retired .screen)] else [])
  (if pending then [("elapse", .elapse)] else []) ++
    [labeled "stop" .stop, labeled "input_lost" .inputLost] ++ inputSteps machine ++
    [labeled "report(failed)" (.report .failed),
      labeled "report(recurring)" (.report .recurring),
      labeled "report(show_copied)" (.report .showCopied),
      labeled "report(show_gone)" (.report .showGone)] ++
    (numbered 0 configs).map (fun (entry : Settings × Nat) =>
      labeled s!"configure({entry.2})" (.configure entry.1)) ++
    controlSteps machine ++ sessionSteps ++ workerSteps machine ++ abandonedSteps machine ++
    retired ++
    (if machine.session.isNone then [labeled "recover" .recover] else []) ++
    [labeled "practice" .practice] ++
    (if machine.practice then [labeled "practice_closed" .practiceClosed] else []) ++
    (if due then [labeled "timer" .timer] else [])

/-- Where a transition leads. -/
inductive Reached where
  /-- A running state. -/
  | state (state : Explored)
  /-- The app failed and stops. -/
  | failed
  /-- The app stopped. -/
  | stopped

/-- One transition: its effects and where it leads. -/
structure Transition where
  /-- Effect text, oldest first, separated by spaces. -/
  effects : String
  /-- Where it leads. -/
  reached : Reached

/-- The script that answers the next unexplored way, after a run that answered `chosen`. -/
def nextScript (chosen : List (Nat × Nat)) : Option (List Nat) :=
  match chosen.reverse.dropWhile (fun (choice : Nat × Nat) => choice.2 ≤ choice.1 + 1) with
  | [] => none
  | (choice, _) :: rest => some (rest.reverse.map Prod.fst ++ [choice + 1])

/-- Runs one event against a script of answers. Returns the transition and the next script. -/
def run (ids : List Nat) (state : Explored) (event : Event) (script : List Nat) :
    Transition × Option (List Nat) :=
  let start : Run := { toMachine := state.machine, env := { answers := script, effects := [] } }
  let (flow, after) := start.step reloads state.clock event
  let after : Run := if flow == .stopped then after.shutDown else after
  let effects : List Effect := after.env.effects.reverse
  let chosen : List (Nat × Nat) := effects.filterMap (fun (effect : Effect) => match effect with
    | .asked query answer => some (answer, query.arity)
    | _ => none)
  let opened : Option (Nat × RecognitionMode) := effects.foldl
    (fun (found : Option (Nat × RecognitionMode)) (effect : Effect) => match effect with
      | .asked (.microphone session mode) 0 => some (session, mode)
      | _ => found)
    none
  let reached : Reached := match flow with
    | .running =>
      .state { machine := after.toMachine, clock := state.clock,
               recorder := opened.orElse (fun (_ : Unit) => state.recorder) }
    | .failed => .failed
    | .stopped => .stopped
  let text : String := " ".intercalate (effects.map (effectText ids))
  ({ effects := text, reached }, nextScript chosen)

/-- Every transition an event has, one per way the environment can answer. -/
def runs (ids : List Nat) (state : Explored) (event : Event) : List Transition :=
  go 4096 [] []
where
  /-- Explores scripts in order until none is left or the fuel runs out. -/
  go : Nat → List Nat → List Transition → List Transition
    | 0, _, found => found.reverse
    | fuel + 1, script, found =>
      let (transition, next) := run ids state event script
      match next with
      | some next => go fuel next (transition :: found)
      | none => (transition :: found).reverse

/-- The FNV-1a offset basis. -/
def fnvOffset : UInt64 := 0xcbf29ce484222325

/-- The FNV-1a prime. -/
def fnvPrime : UInt64 := 0x100000001b3

/-- Hashes a line and its newline. -/
def fnvLine (hash : UInt64) (line : String) : UInt64 :=
  let hash : UInt64 := line.toUTF8.foldl
    (fun (hash : UInt64) (byte : UInt8) => (hash ^^^ byte.toUInt64) * fnvPrime) hash
  (hash ^^^ 10) * fnvPrime

/-- A hexadecimal digit. -/
def hexDigit (digit : UInt64) : Char :=
  if digit < 10 then Char.ofNat (48 + digit.toNat) else Char.ofNat (87 + digit.toNat)

/-- Sixteen hexadecimal digits. -/
def hex (value : UInt64) : String :=
  String.ofList ((List.range 16).reverse.map (fun (place : Nat) =>
    hexDigit ((value >>> (4 * place).toUInt64) &&& 15)))

/-- What the search has found so far. -/
structure Search where
  /-- Each canonical state's number. -/
  index : Std.HashMap.Raw String Nat
  /-- How many states have numbers. -/
  states : Nat
  /-- How many transitions were explored. -/
  transitions : Nat
  /-- How many transitions reached a state beyond the retiring bound. -/
  bounded : Nat
  /-- One digest per expanded state, the latest first. -/
  digests : List UInt64

/-- Expands one state: numbers what it reaches, adds new states to the front of the next
level, and digests its transitions. -/
def expand (search : Search) (next : List Explored) (state : Explored) :
    Search × List Explored :=
  let ids : List Nat := ids (shown state.machine)
  let (search, next, digest) := (steps state).foldl
    (fun (acc : Search × List Explored × UInt64) (entry : String × Step) =>
      let transitions : List Transition := match entry.2 with
        | .elapse => [{ effects := "", reached := .state { state with clock := state.clock + 1 } }]
        | .event event => runs ids state event
      transitions.foldl (fun (acc : Search × List Explored × UInt64) (transition : Transition) =>
        let (search, next, digest) := acc
        let (search, next, reached) : Search × List Explored × String :=
          match transition.reached with
          | .failed => (search, next, "failed")
          | .stopped => (search, next, "stopped")
          | .state found =>
            let canonicalKey : String := key found
            match search.index.get? canonicalKey with
            | some known => (search, next, toString known)
            | none =>
              if retiringBound < found.machine.retiring.length then
                ({ search with bounded := search.bounded + 1 }, next, "bounded")
              else
                ({ search with index := search.index.insert canonicalKey search.states,
                               states := search.states + 1 },
                  found :: next, toString search.states)
        let line : String := s!"{entry.1} [{transition.effects}] -> {reached}"
        ({ search with transitions := search.transitions + 1 }, next, fnvLine digest line))
        acc)
    (search, next, fnvOffset)
  ({ search with digests := digest :: search.digests }, next)

/-- Explores level by level down to the given depth; reports whether the frontier emptied. -/
def explore : Nat → Search → List Explored → Search × Bool
  | 0, search, level => (search, level.isEmpty)
  | depth + 1, search, level =>
    if level.isEmpty then (search, true)
    else
      let (search, next) := level.foldl
        (fun (acc : Search × List Explored) (state : Explored) => expand acc.1 acc.2 state)
        (search, [])
      explore depth search next.reverse

/-- The most levels an unbounded exploration may take before it reports a failure. -/
def unbounded : Nat := 4096

public section

/-- Explores to the depth given as the only argument, or everywhere for `all`, and prints the
digests. -/
def main (args : List String) : IO UInt32 := do
  let depth : Option (Option Nat) := match args with
    | ["all"] => some none
    | [depth] => depth.toNat?.map some
    | _ => none
  match depth with
  | some depth =>
    let initial : Explored :=
      { machine := Machine.start timing firstConfig false, clock := 0, recorder := none }
    let (search, complete) := explore (depth.getD unbounded)
      { index := (Std.HashMap.Raw.emptyWithCapacity 1024).insert (key initial) 0, states := 1,
        transitions := 0, bounded := 0, digests := [] }
      [initial]
    if depth.isNone && !complete then
      IO.eprintln s!"exploration did not finish within {unbounded} levels"
      return 1
    let stdout : IO.FS.Stream ← IO.getStdout
    stdout.putStrLn
      s!"states {search.states} transitions {search.transitions} bounded {search.bounded}"
    let mut state : Nat := 0
    for digest in search.digests.reverse do
      stdout.putStrLn s!"{state} {hex digest}"
      state := state + 1
    pure 0
  | none =>
    IO.eprintln "usage: speakeasy_model DEPTH|all"
    pure 2

end
