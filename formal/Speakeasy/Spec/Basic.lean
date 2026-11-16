module

/-!
# Session machine vocabulary

The types of `speakeasy_core::machine` with every payload erased. The Rust machine is generic
over a payload family and never inspects payload content, so its decisions depend only on the
shapes kept here: an audio log becomes whether one is held, a transcript becomes its kind, and a
session id becomes a natural number. Time is a tick count.
-/

@[expose] public section

namespace Speakeasy

/-- What the app reports doing, as `machine::Phase`. -/
inductive Phase where
  /-- Nothing is happening. -/
  | idle
  /-- A session is opening its recorder. -/
  | starting
  /-- A session is recording. -/
  | recording
  /-- A session is finishing its recording. -/
  | stopping
  /-- A session is recognizing, publishing, or inserting. -/
  | processing
  /-- Dictation was inserted. -/
  | done
  /-- Text or a Show link went to the clipboard. -/
  | copied
  /-- A notice ended the last session. -/
  | notice
  /-- The recording held no speech. -/
  | empty
  /-- The user cancelled. -/
  | cancelled
  /-- The user chose no screen. -/
  | unchosen
  /-- The last session failed. -/
  | error

/-- Whether the local model can transcribe, as `machine::ModelState`. -/
inductive ModelState where
  /-- The model is loading. -/
  | loading
  /-- The model is ready. -/
  | ready
  /-- No model is loaded. -/
  | unavailable
  deriving DecidableEq

/-- Decides whether two model state values are equal. -/
add_decl_doc instDecidableEqModelState

/-- A model state value is the constructor its index names. -/
add_decl_doc ModelState.ofNat_ctorIdx

/-- Why a Show has less narration than it was recorded with. -/
inductive NarrationLoss where
  /-- Recovery found only part of the narration. -/
  | incomplete
  /-- The microphone stopped. -/
  | audioStopped
  /-- The microphone heard nothing. -/
  | audioSilent
  /-- Recognition gave up. -/
  | notTranscribed
  deriving DecidableEq

/-- Decides whether two narration loss values are equal. -/
add_decl_doc instDecidableEqNarrationLoss

/-- A narration loss value is the constructor its index names. -/
add_decl_doc NarrationLoss.ofNat_ctorIdx

/-- A sound the app plays. -/
inductive Cue where
  /-- Recording started. -/
  | start
  /-- Recording stopped. -/
  | stop
  /-- The session was cancelled. -/
  | cancel
  /-- A Show started. -/
  | showing
  /-- Something reached the clipboard. -/
  | copied

/-- How the microphone recorder hands audio to recognition. -/
inductive RecognitionMode where
  /-- The whole recording at once. -/
  | whole
  /-- Pieces while recording, then the tail. -/
  | segmented
  /-- A live stream. -/
  | streaming
  deriving DecidableEq

/-- Decides whether two recognition mode values are equal. -/
add_decl_doc instDecidableEqRecognitionMode

/-- A recognition mode value is the constructor its index names. -/
add_decl_doc RecognitionMode.ofNat_ctorIdx

/-- One of the two native recorders. -/
inductive Recorder where
  /-- The microphone. -/
  | microphone
  /-- The screen. -/
  | screen

/-- The settings the machine reads from the app configuration. -/
structure Settings where
  /-- Whether cues play. -/
  playSounds : Bool
  /-- How the microphone feeds recognition. -/
  recognition : RecognitionMode
  /-- Whether a recovering worker probes the accelerator. -/
  probe : Bool

/-- How a step ended, as `machine::Flow`. -/
inductive Flow where
  /-- The app keeps running. -/
  | running
  /-- The app shows a failure and stops. -/
  | failed
  /-- The app stops. -/
  | stopped
  deriving DecidableEq

/-- Decides whether two flow values are equal. -/
add_decl_doc instDecidableEqFlow

/-- A flow value is the constructor its index names. -/
add_decl_doc Flow.ofNat_ctorIdx

/-- Why the control interface refused a command. -/
inductive Refusal where
  /-- A session or practice is active. -/
  | busy
  /-- The desktop or model is not ready. -/
  | notReady
  /-- The start ended at once. -/
  | startStopped
  /-- The token names no active dictation. -/
  | notActive
  /-- A paste would interrupt a session or practice. -/
  | pasteBusy
  /-- The desktop is not ready for a paste. -/
  | pasteNotReady
  /-- There is no dictation to paste. -/
  | nothingToPaste
  /-- The paste ended at once. -/
  | pasteStopped
  /-- The command is not supported. -/
  | unsupported
  /-- A session or practice is active. -/
  | presetBusy
  /-- The preset was chosen for an earlier session or settings. -/
  | presetStale

/-- Why a session failed, as `machine::Failure`. -/
inductive Failure where
  /-- A component reported an error. -/
  | said
  /-- Keyboard input was lost. -/
  | inputLost
  /-- The text did not arrive; `said` is whether a reason came with it, and `replay` whether
  the last dictation can be pasted again. -/
  | undelivered (said : Bool) (replay : Bool)
  /-- Publishing stopped. -/
  | publishStopped
  /-- The Show was saved but its link did not reach the clipboard; `failure` is whether an
  earlier notice failure is reported with it. -/
  | notCopied (failure : Bool)
  /-- Recovery storage failed. -/
  | recoveryStorage

/-- What ended the last session, as `machine::Outcome`. -/
inductive Outcome where
  /-- Nothing has ended. -/
  | idle
  /-- There was nothing to paste. -/
  | nothingToPaste
  /-- Text was inserted. -/
  | done
  /-- Practice received its text. -/
  | practiced
  /-- Text went to the clipboard. -/
  | copied
  /-- A Show link went to the clipboard. -/
  | showCopied
  /-- The copied Show is gone. -/
  | showGone
  /-- A recovered Show was published. -/
  | recovered
  /-- The recording held no speech. -/
  | empty
  /-- The user cancelled. -/
  | cancelled
  /-- The user chose no screen. -/
  | unchosen
  /-- The session failed. -/
  | failed (failure : Failure)

/-- The phase an outcome reports. -/
def Outcome.phase : Outcome → Phase
  | .idle => .idle
  | .done | .practiced => .done
  | .copied | .showCopied => .copied
  | .nothingToPaste | .showGone | .recovered => .notice
  | .empty => .empty
  | .cancelled => .cancelled
  | .unchosen => .unchosen
  | .failed _ => .error

/-- A notice the app shows beside the phase, as `machine::Notice`. -/
inductive Notice where
  /-- The Show checkpoint stopped. -/
  | checkpointLost
  /-- The screen recording stopped early. -/
  | movieStopped
  /-- The model is loading. -/
  | loadingModel
  /-- New settings took effect. -/
  | settingsApplied
  /-- The model failed to load. -/
  | warmupFailed
  /-- A Show goes on without narration. -/
  | narrationLost
  deriving DecidableEq

/-- Decides whether two notice values are equal. -/
add_decl_doc instDecidableEqNotice

/-- A notice value is the constructor its index names. -/
add_decl_doc Notice.ofNat_ctorIdx

/-- Whether a notice carries a failure. -/
def Notice.failure : Notice → Bool
  | .warmupFailed | .narrationLost => true
  | .checkpointLost | .movieStopped | .loadingModel | .settingsApplied => false

/-- Whether a notice is a warning that a later session clears. -/
def Notice.isWarning : Notice → Bool
  | .checkpointLost | .movieStopped => true
  | .loadingModel | .settingsApplied | .warmupFailed | .narrationLost => false

/-- What recognition returns. -/
inductive Detail where
  /-- Plain text. -/
  | text
  /-- Timed words for a Show. -/
  | words

/-- Which part of a recording a transcription covers. -/
inductive Part where
  /-- The live stream. -/
  | stream
  /-- One piece, by number. -/
  | piece (number : Nat)
  /-- The final part. -/
  | final
  deriving DecidableEq

/-- Decides whether two part values are equal. -/
add_decl_doc instDecidableEqPart

/-- Compares two part values constructor by constructor. -/
add_decl_doc instDecidableEqPart.decEq

/-- Names one transcription: its session and part. -/
structure Ticket where
  /-- The session. -/
  session : Nat
  /-- The part. -/
  part : Part
  deriving DecidableEq

/-- Decides whether two ticket values are equal. -/
add_decl_doc instDecidableEqTicket

/-- Compares two ticket values constructor by constructor. -/
add_decl_doc instDecidableEqTicket.decEq

/-- Which audio a transcription reads. -/
inductive Source where
  /-- Recorded audio. -/
  | audio
  /-- The live stream. -/
  | live

/-- A transcription request. -/
structure Request where
  /-- The ticket its result carries. -/
  ticket : Ticket
  /-- The audio it reads. -/
  source : Source
  /-- What it returns. -/
  detail : Detail

/-- A transcription result. -/
inductive Transcript where
  /-- Plain text. -/
  | text
  /-- Timed words. -/
  | words

/-- How a segmented recording split: complete pieces and whether the tail is empty. -/
structure Partition where
  /-- How many pieces the recorder sent. -/
  pieces : Nat
  /-- Whether the tail after the pieces holds no audio. -/
  empty : Bool

/-- Where a publication came from. -/
inductive Origin where
  /-- A live Show. -/
  | live
  /-- A recovered Show. -/
  | recovered

/-- What a Show's cleanup does with its checkpoint, as `machine::Release`. -/
inductive Release where
  /-- Keep the bundle, with narration when it was kept; the flag says it was incomplete. -/
  | keep (narration : Option Bool)
  /-- Discard the bundle. -/
  | discard

/-- The status the control interface reports. -/
structure Status where
  /-- The phase. -/
  phase : Phase
  /-- The model state. -/
  model : ModelState
  /-- Whether the desktop is ready. -/
  desktopReady : Bool
  /-- The controllable dictation, if any. -/
  session : Option Nat
  /-- Whether a preset is applied. -/
  preset : Bool
  /-- Whether a dictation can be pasted again. -/
  lastDictation : Bool

/-- A question the machine asks its environment. -/
inductive Query where
  /-- May a session start? -/
  | admit
  /-- Can the focused window be shielded from capture? -/
  | shield
  /-- Is the app closing? -/
  | closed
  /-- Can a Show checkpoint be kept? -/
  | keep
  /-- Did the microphone open for this session and mode? -/
  | microphone (session : Nat) (mode : RecognitionMode)
  /-- Did the screen recorder open for this session, with or without a frame log? -/
  | screen (session : Nat) (frames : Bool)
  /-- Was the audio log kept? -/
  | keepAudio
  /-- Could the tail be cut? -/
  | cut
  /-- Did composing leave text? -/
  | compose
  /-- Is the practice window ready? -/
  | ready
  /-- Did practice receive its text? -/
  | practise

/-- How many answers a query has. Answer `0` is the success. -/
def Query.arity : Query → Nat
  | .keep => 3
  | .admit | .shield | .closed | .microphone _ _ | .screen _ _ | .keepAudio | .cut | .compose
  | .ready | .practise => 2

/-- Something the machine asks its environment to do, or a query and its answer. -/
inductive Effect where
  /-- A query and its answer. -/
  | asked (query : Query) (answer : Nat)
  /-- Revoke the session's insert permit. -/
  | revoke
  /-- Ask a recorder to finish. -/
  | finish (recorder : Recorder)
  /-- Retire a recorder. -/
  | retire (recorder : Recorder)
  /-- Switch the microphone to whole-recording audio. -/
  | listenWhole
  /-- Tell the microphone to expect a cue. -/
  | expectCue
  /-- Pin the screen recording. -/
  | pin
  /-- Show the microphone level for a session. -/
  | meter (session : Nat)
  /-- Load the model. -/
  | load
  /-- Reload the model for new settings. -/
  | reload
  /-- Replace a stuck worker, probing the accelerator or not. -/
  | recover (probe : Bool)
  /-- Start a transcription. -/
  | transcribe (request : Request)
  /-- Insert text into the focused app. -/
  | insert
  /-- Refuse a practice window. -/
  | refusePractice
  /-- Publish a Show. -/
  | publish (loss : Option NarrationLoss) (checkpoint : Bool) (origin : Origin) (receipt : Bool)
  /-- Reopen an interrupted Show. -/
  | reopen
  /-- Tell a recovery request it failed. -/
  | refuseRecovery (failure : Failure)
  /-- Retire a delivery that its session abandoned. -/
  | retireDelivery (session : Nat)
  /-- Release a Show checkpoint. -/
  | release (session : Nat) (release : Release)
  /-- Remember the published Show. -/
  | remember
  /-- Announce an outcome. -/
  | announce
  /-- Play a cue. -/
  | cue (cue : Cue)
  /-- Hold or release other apps' cues. -/
  | holdCues (held : Bool)
  /-- Answer a control call with a status. -/
  | respond (status : Status)
  /-- Refuse a control call. -/
  | refuse (refusal : Refusal)
  /-- Close keyboard input. -/
  | closeInput

/-- The environment a step runs against: the answers it will give, and the effects so far,
newest first. -/
structure Env where
  /-- Answers to the next queries, in order. -/
  answers : List Nat
  /-- Effects so far, newest first. -/
  effects : List Effect

/-- Records an effect. -/
def Env.emit (effect : Effect) (env : Env) : Env :=
  { env with effects := effect :: env.effects }

/-- Asks a query: the next answer if it is in range, otherwise `0`, recorded as an effect. -/
def Env.ask (query : Query) (env : Env) : Nat × Env :=
  let answer : Nat := match env.answers with
    | next :: _ => if next < query.arity then next else 0
    | [] => 0
  (answer, { answers := env.answers.drop 1, effects := .asked query answer :: env.effects })

end Speakeasy

end
