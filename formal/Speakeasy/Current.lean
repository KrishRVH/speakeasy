module

public import Speakeasy.Spec.Machine

/-!
# The live session is current

Every step keeps `Machine.current`: the live session is the latest session, and its
recognition belongs to it. Each layer's operations keep session ids, from recognition up to
the event handlers. Each lemma is a rewrite rule, so a handler's proof splits the handler into
its branches and rewrites each branch back to the invariant it started from.
-/

@[expose] public section

namespace Speakeasy

namespace Recognition

/-- Abandoning early results keeps the id. -/
theorem whole_id (recognition : Recognition) : recognition.whole.id = recognition.id := rfl

/-- A stream keeps the id. -/
theorem stream_id (recognition : Recognition) : recognition.stream.id = recognition.id := by
  unfold Recognition.stream
  try dsimp only
  split <;> rfl

/-- A piece keeps the id. -/
theorem piece_id (number : Nat) (recognition : Recognition) :
    (recognition.piece number).id = recognition.id := by
  unfold Recognition.piece
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals rfl

/-- Sealing keeps the id. -/
theorem sealAudio_id (partition : Option Partition) (detail : Detail)
    (recognition : Recognition) : (recognition.sealAudio partition detail).id = recognition.id := by
  unfold Recognition.sealAudio
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals rfl

/-- A request keeps the id. -/
theorem request_id (recognition : Recognition) (env : Env) :
    (recognition.request env).2.1.id = recognition.id := by
  unfold Recognition.request Recognition.requestFinal
  try dsimp only
  repeat' (split <;> try dsimp only)

/-- Completion keeps the id. -/
theorem complete_id (recognition : Recognition) :
    recognition.complete.2.id = recognition.id := by
  unfold Recognition.complete Recognition.join
  try dsimp only
  repeat' (split <;> try dsimp only)

/-- A result keeps the id. -/
theorem heard_id (ticket : Ticket) (transcript : Transcript) (recognition : Recognition) :
    (recognition.heard ticket transcript).2.id = recognition.id := by
  unfold Recognition.heard Recognition.join
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals first
    | rfl
    | exact complete_id _

/-- A failure keeps the id. -/
theorem failed_id (ticket : Ticket) (recognition : Recognition) :
    (recognition.failed ticket).2.id = recognition.id := by
  unfold Recognition.failed
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals rfl

end Recognition

namespace Session

/-- A fresh session has the id it was given. -/
theorem new_id (id : Nat) : (Session.new id).id = id := rfl

/-- A fresh session's recognition belongs to it. -/
theorem new_recognition (id : Nat) : (Session.new id).recognition.id = id := rfl

/-- A recovering session has the id it was given. -/
theorem recovering_id (id : Nat) : (Session.recovering id).id = id := rfl

/-- A recovering session's recognition belongs to it. -/
theorem recovering_recognition (id : Nat) : (Session.recovering id).recognition.id = id := rfl

/-- Keeping the bundle keeps the id. -/
theorem keep_id (session : Session) : session.keep.id = session.id := rfl

/-- Keeping the bundle keeps recognition. -/
theorem keep_recognition (session : Session) : session.keep.recognition = session.recognition :=
  rfl

/-- Forfeiting keeps the id. -/
theorem forfeit_id (session : Session) : session.forfeit.id = session.id := rfl

/-- Forfeiting keeps recognition. -/
theorem forfeit_recognition (session : Session) :
    session.forfeit.recognition = session.recognition := rfl

/-- An opened screen recorder keeps the id. -/
theorem screenOpened_id (session : Session) : session.screenOpened.id = session.id := rfl

/-- An opened screen recorder keeps recognition. -/
theorem screenOpened_recognition (session : Session) :
    session.screenOpened.recognition = session.recognition := rfl

/-- Recorded footage keeps the id. -/
theorem screenRecorded_id (session : Session) : session.screenRecorded.id = session.id := rfl

/-- Recorded footage keeps recognition. -/
theorem screenRecorded_recognition (session : Session) :
    session.screenRecorded.recognition = session.recognition := rfl

/-- Reopening keeps the id. -/
theorem reopened_id (recognized : Recognized) (session : Session) :
    (session.reopened recognized).id = session.id := by
  unfold Session.reopened
  try dsimp only
  split <;> rfl

/-- Reopening keeps the recognition id. -/
theorem reopened_recognition (recognized : Recognized) (session : Session) :
    (session.reopened recognized).recognition.id = session.recognition.id := by
  unfold Session.reopened
  try dsimp only
  split
  · exact Recognition.sealAudio_id none .words session.recognition
  · rfl

/-- Taking the frame log keeps the id. -/
theorem takeFrameLog_id (session : Session) : session.takeFrameLog.2.id = session.id := by
  unfold Session.takeFrameLog
  try dsimp only
  split <;> rfl

/-- Taking the frame log keeps recognition. -/
theorem takeFrameLog_recognition (session : Session) :
    session.takeFrameLog.2.recognition = session.recognition := by
  unfold Session.takeFrameLog
  try dsimp only
  split <;> rfl

/-- Taking the audio log keeps the id. -/
theorem takeAudioLog_id (session : Session) : session.takeAudioLog.2.id = session.id := by
  unfold Session.takeAudioLog
  try dsimp only
  split <;> rfl

/-- Taking the audio log keeps recognition. -/
theorem takeAudioLog_recognition (session : Session) :
    session.takeAudioLog.2.recognition = session.recognition := by
  unfold Session.takeAudioLog
  try dsimp only
  split <;> rfl

/-- Opening keeps the id. -/
theorem opened_id (session : Session) : session.opened.id = session.id := by
  unfold Session.opened
  try dsimp only
  split <;> rfl

/-- Opening keeps recognition. -/
theorem opened_recognition (session : Session) :
    session.opened.recognition = session.recognition := by
  unfold Session.opened
  try dsimp only
  split <;> rfl

/-- Audio becoming ready keeps the id. -/
theorem audioReady_id (processing : Bool) (session : Session) :
    (session.audioReady processing).id = session.id := by
  unfold Session.audioReady Session.conclude Session.mapShow
  try dsimp only
  repeat' (split <;> try dsimp only)

/-- Audio becoming ready keeps recognition. -/
theorem audioReady_recognition (processing : Bool) (session : Session) :
    (session.audioReady processing).recognition = session.recognition := by
  unfold Session.audioReady Session.conclude Session.mapShow
  try dsimp only
  repeat' (split <;> try dsimp only)

/-- Finishing keeps the id. -/
theorem finish_id (session : Session) : session.finish.id = session.id := by
  unfold Session.finish
  dsimp only
  split <;> rfl

/-- Finishing keeps recognition. -/
theorem finish_recognition (session : Session) :
    session.finish.recognition = session.recognition := by
  unfold Session.finish
  dsimp only
  split <;> rfl

/-- Sealing the recording keeps the id. -/
theorem captured_id (sealed : Option (Option Partition)) (session : Session) :
    (session.captured sealed).2.id = session.id := by
  unfold Session.captured
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals rfl

/-- Sealing the recording keeps the recognition id. -/
theorem captured_recognition (sealed : Option (Option Partition)) (session : Session) :
    (session.captured sealed).2.recognition.id = session.recognition.id := by
  unfold Session.captured
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals first
    | rfl
    | exact Recognition.sealAudio_id _ _ _

/-- A piece keeps the id. -/
theorem piece_id (number : Nat) (session : Session) : (session.piece number).id = session.id := by
  unfold Session.piece
  try dsimp only
  split <;> rfl

/-- A piece keeps the recognition id. -/
theorem piece_recognition (number : Nat) (session : Session) :
    (session.piece number).recognition.id = session.recognition.id := by
  unfold Session.piece
  try dsimp only
  split
  · exact Recognition.piece_id number session.recognition
  · rfl

/-- A stream keeps the id. -/
theorem stream_id (session : Session) : session.stream.id = session.id := by
  unfold Session.stream
  try dsimp only
  split <;> rfl

/-- A stream keeps the recognition id. -/
theorem stream_recognition (session : Session) :
    session.stream.recognition.id = session.recognition.id := by
  unfold Session.stream
  try dsimp only
  split
  · exact Recognition.stream_id session.recognition
  · rfl

/-- A request keeps the id. -/
theorem request_id (session : Session) (env : Env) :
    (session.request env).2.1.id = session.id := by
  unfold Session.request
  dsimp only
  split <;> rfl

/-- A request keeps the recognition id. -/
theorem request_recognition (session : Session) (env : Env) :
    (session.request env).2.1.recognition.id = session.recognition.id := by
  unfold Session.request
  dsimp only
  split
  · exact Recognition.request_id session.recognition env
  · rfl

/-- Narration keeps the id. -/
theorem narrate_id (session : Session) : session.narrate.id = session.id := by
  unfold Session.narrate
  try dsimp only
  split <;> rfl

/-- Narration resets recognition to the session's own. -/
theorem narrate_recognition (session : Session) (aligned : session.recognition.id = session.id) :
    session.narrate.recognition.id = session.id := by
  unfold Session.narrate
  try dsimp only
  split
  · rfl
  · exact aligned

/-- A result keeps the id. -/
theorem heard_id (ticket : Ticket) (transcript : Transcript) (session : Session) :
    (session.heard ticket transcript).2.id = session.id := by
  unfold Session.heard
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals first
    | rfl
    | exact narrate_id _

/-- A result keeps a session's recognition its own. -/
theorem heard_recognition (ticket : Ticket) (transcript : Transcript) (session : Session)
    (aligned : session.recognition.id = session.id) :
    (session.heard ticket transcript).2.recognition.id = session.id := by
  have heard : (session.recognition.heard ticket transcript).2.id = session.id :=
    (Recognition.heard_id ticket transcript session.recognition).trans aligned
  unfold Session.heard
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals first
    | exact aligned
    | exact heard
    | exact narrate_recognition _ heard

/-- Completion keeps the id. -/
theorem complete_id (session : Session) : session.complete.2.id = session.id := by
  unfold Session.complete
  dsimp only
  split <;> rfl

/-- Completion keeps the recognition id. -/
theorem complete_recognition (session : Session) :
    session.complete.2.recognition.id = session.recognition.id := by
  unfold Session.complete
  dsimp only
  split
  · exact Recognition.complete_id session.recognition
  · rfl

/-- A failure keeps the id. -/
theorem failed_id (ticket : Ticket) (session : Session) :
    (session.failed ticket).2.id = session.id := by
  unfold Session.failed
  dsimp only
  split <;> rfl

/-- A failure keeps the recognition id. -/
theorem failed_recognition (ticket : Ticket) (session : Session) :
    (session.failed ticket).2.recognition.id = session.recognition.id := by
  unfold Session.failed
  dsimp only
  split
  · rfl
  · exact Recognition.failed_id ticket session.recognition

/-- A microphone failure keeps the id. -/
theorem audioFailed_id (silent : Bool) (session : Session) :
    (session.audioFailed silent).2.id = session.id := by
  unfold Session.audioFailed
  dsimp only
  split <;> rfl

/-- A microphone failure keeps a session's recognition its own. -/
theorem audioFailed_recognition (silent : Bool) (session : Session)
    (aligned : session.recognition.id = session.id) :
    (session.audioFailed silent).2.recognition.id = session.id := by
  unfold Session.audioFailed
  dsimp only
  split
  · rfl
  · exact aligned

/-- Giving up on transcription keeps the id. -/
theorem notTranscribed_id (session : Session) : session.notTranscribed.2.id = session.id := by
  unfold Session.notTranscribed
  try dsimp only
  split <;> rfl

/-- Giving up on transcription keeps a session's recognition its own. -/
theorem notTranscribed_recognition (session : Session)
    (aligned : session.recognition.id = session.id) :
    session.notTranscribed.2.recognition.id = session.id := by
  unfold Session.notTranscribed
  try dsimp only
  split
  · rfl
  · exact aligned

/-- Insertion keeps the id. -/
theorem insert_id (session : Session) (env : Env) : (session.insert env).1.id = session.id := rfl

/-- Insertion resets recognition to the session's own. -/
theorem insert_recognition (session : Session) (env : Env) :
    (session.insert env).1.recognition.id = session.id := rfl

/-- Turning into a Show keeps the id. -/
theorem turnIntoShow_id (session : Session) (env : Env) :
    (session.turnIntoShow env).2.1.id = session.id := by
  unfold Session.turnIntoShow
  try dsimp only
  repeat' (split <;> try dsimp only)

/-- Turning into a Show keeps the recognition id. -/
theorem turnIntoShow_recognition (session : Session) (env : Env) :
    (session.turnIntoShow env).2.1.recognition.id = session.recognition.id := by
  unfold Session.turnIntoShow
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals rfl

/-- Publishing keeps the id. -/
theorem publish_id (session : Session) (env : Env) : (session.publish env).1.id = session.id := by
  unfold Session.publish
  try dsimp only
  repeat' (split <;> try dsimp only)

/-- Publishing keeps recognition. -/
theorem publish_recognition (session : Session) (env : Env) :
    (session.publish env).1.recognition = session.recognition := by
  unfold Session.publish
  try dsimp only
  repeat' (split <;> try dsimp only)

end Session

namespace Run

/-- Recording an effect keeps the live session. -/
theorem emit_session (effect : Effect) (run : Run) : (run.emit effect).session = run.session :=
  rfl

/-- Recording an effect keeps the latest session. -/
theorem emit_latest (effect : Effect) (run : Run) : (run.emit effect).latest = run.latest := rfl

/-- Asking a query keeps the live session. -/
theorem ask_session (query : Query) (run : Run) : (run.ask query).2.session = run.session := rfl

/-- Asking a query keeps the latest session. -/
theorem ask_latest (query : Query) (run : Run) : (run.ask query).2.latest = run.latest := rfl

/-- Reviving the worker keeps the live session. -/
theorem revive_session (run : Run) : run.revive.2.session = run.session := by
  unfold Run.revive
  try dsimp only
  split <;> rfl

/-- Reviving the worker keeps the latest session. -/
theorem revive_latest (run : Run) : run.revive.2.latest = run.latest := by
  unfold Run.revive
  try dsimp only
  split <;> rfl

/-- Replacing the worker keeps the live session. -/
theorem recoverWorker_session (run : Run) : run.recoverWorker.session = run.session := rfl

/-- Replacing the worker keeps the latest session. -/
theorem recoverWorker_latest (run : Run) : run.recoverWorker.latest = run.latest := rfl

/-- A cue keeps the live session. -/
theorem cue_session (cue : Cue) (run : Run) : (run.cue cue).session = run.session := by
  unfold Run.cue
  try dsimp only
  split <;> rfl

/-- A cue keeps the latest session. -/
theorem cue_latest (cue : Cue) (run : Run) : (run.cue cue).latest = run.latest := by
  unfold Run.cue
  try dsimp only
  split <;> rfl

/-- A heard cue keeps the live session. -/
theorem cueHeard_session (cue : Cue) (run : Run) :
    (run.cueHeard cue).session = run.session := by
  unfold Run.cueHeard
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals rfl

/-- A heard cue keeps the latest session. -/
theorem cueHeard_latest (cue : Cue) (run : Run) : (run.cueHeard cue).latest = run.latest := by
  unfold Run.cueHeard
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals rfl

/-- Holding cues keeps the live session. -/
theorem holdCues_session (run : Run) : run.holdCues.session = run.session := by
  unfold Run.holdCues
  try dsimp only
  split <;> rfl

/-- Holding cues keeps the latest session. -/
theorem holdCues_latest (run : Run) : run.holdCues.latest = run.latest := by
  unfold Run.holdCues
  try dsimp only
  split <;> rfl

/-- Clearing a warning keeps the live session. -/
theorem clearWarning_session (run : Run) : run.clearWarning.session = run.session := by
  unfold Run.clearWarning
  try dsimp only
  repeat' (split <;> try dsimp only)

/-- Clearing a warning keeps the latest session. -/
theorem clearWarning_latest (run : Run) : run.clearWarning.latest = run.latest := by
  unfold Run.clearWarning
  try dsimp only
  repeat' (split <;> try dsimp only)

/-- A warning keeps the live session. -/
theorem warn_session (warning : Notice) (run : Run) :
    (run.warn warning).session = run.session := by
  unfold Run.warn
  try dsimp only
  repeat' (split <;> try dsimp only)

/-- A warning keeps the latest session. -/
theorem warn_latest (warning : Notice) (run : Run) : (run.warn warning).latest = run.latest := by
  unfold Run.warn
  try dsimp only
  repeat' (split <;> try dsimp only)

/-- Setting a recorder keeps the live session. -/
theorem setDevice_session (recorder : Recorder) (device : Device) (run : Run) :
    (run.setDevice recorder device).session = run.session := by
  unfold Run.setDevice
  try dsimp only
  split <;> rfl

/-- Setting a recorder keeps the latest session. -/
theorem setDevice_latest (recorder : Recorder) (device : Device) (run : Run) :
    (run.setDevice recorder device).latest = run.latest := by
  unfold Run.setDevice
  try dsimp only
  split <;> rfl

/-- Finishing a recorder keeps the live session. -/
theorem finishRecorder_session (recorder : Recorder) (run : Run) :
    (run.finishRecorder recorder).session = run.session := by
  unfold Run.finishRecorder
  try dsimp only
  split <;> rfl

/-- Finishing a recorder keeps the latest session. -/
theorem finishRecorder_latest (recorder : Recorder) (run : Run) :
    (run.finishRecorder recorder).latest = run.latest := by
  unfold Run.finishRecorder
  try dsimp only
  split <;> rfl

/-- Retiring a recorder keeps the live session. -/
theorem retireRecorder_session (recorder : Recorder) (run : Run) :
    (run.retireRecorder recorder).session = run.session := by
  unfold Run.retireRecorder
  try dsimp only
  split
  · simp only [emit_session, setDevice_session]
  · rfl

/-- Retiring a recorder keeps the latest session. -/
theorem retireRecorder_latest (recorder : Recorder) (run : Run) :
    (run.retireRecorder recorder).latest = run.latest := by
  unfold Run.retireRecorder
  try dsimp only
  split
  · simp only [emit_latest, setDevice_latest]
  · rfl

/-- Opening a recorder keeps the live session. -/
theorem openRecorder_session (recorder : Recorder) (run : Run) :
    (run.openRecorder recorder).session = run.session := by
  unfold Run.openRecorder
  dsimp only
  split <;> simp only [emit_session, setDevice_session]

/-- Opening a recorder keeps the latest session. -/
theorem openRecorder_latest (recorder : Recorder) (run : Run) :
    (run.openRecorder recorder).latest = run.latest := by
  unfold Run.openRecorder
  dsimp only
  split <;> simp only [emit_latest, setDevice_latest]

/-- Remembering a Show keeps the live session. -/
theorem remember_session (session : Nat) (run : Run) :
    (run.remember session).session = run.session := rfl

/-- Remembering a Show keeps the latest session. -/
theorem remember_latest (session : Nat) (run : Run) :
    (run.remember session).latest = run.latest := rfl

/-- Answering a control call keeps the live session. -/
theorem respond_session (refused : Option Refusal) (run : Run) :
    (run.respond refused).session = run.session := by
  unfold Run.respond
  try dsimp only
  split <;> rfl

/-- Answering a control call keeps the latest session. -/
theorem respond_latest (refused : Option Refusal) (run : Run) :
    (run.respond refused).latest = run.latest := by
  unfold Run.respond
  try dsimp only
  split <;> rfl

/-- A report keeps the live session. -/
theorem report_session (report : Report) (run : Run) :
    (run.report report).session = run.session := by
  unfold Run.report
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals rfl

/-- A report keeps the latest session. -/
theorem report_latest (report : Report) (run : Run) :
    (run.report report).latest = run.latest := by
  unfold Run.report
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals rfl

/-- Concluding a capture keeps the live session. -/
theorem concludeCapture_session (run : Run) : run.concludeCapture.session = run.session := by
  unfold Run.concludeCapture
  dsimp only
  split <;> simp only [cue_session, finishRecorder_session, retireRecorder_session]

/-- Concluding a capture keeps the latest session. -/
theorem concludeCapture_latest (run : Run) : run.concludeCapture.latest = run.latest := by
  unfold Run.concludeCapture
  dsimp only
  split <;> simp only [cue_latest, finishRecorder_latest, retireRecorder_latest]

/-- Choosing a delivery's outcome keeps the live session. -/
theorem deliveredOutcome_session (presenting : Bool) (delivered : Delivered) (run : Run) :
    (run.deliveredOutcome presenting delivered).2.session = run.session := by
  unfold Run.deliveredOutcome
  try dsimp only
  split <;> rfl

/-- Choosing a delivery's outcome keeps the latest session. -/
theorem deliveredOutcome_latest (presenting : Bool) (delivered : Delivered) (run : Run) :
    (run.deliveredOutcome presenting delivered).2.latest = run.latest := by
  unfold Run.deliveredOutcome
  try dsimp only
  split <;> rfl

/-- A retired session's result keeps the live session. -/
theorem handleAbandoned_session (id : Nat) (delivered : Delivered) (run : Run) :
    (run.handleAbandoned id delivered).session = run.session := by
  unfold Run.handleAbandoned
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals rfl

/-- A retired session's result keeps the latest session. -/
theorem handleAbandoned_latest (id : Nat) (delivered : Delivered) (run : Run) :
    (run.handleAbandoned id delivered).latest = run.latest := by
  unfold Run.handleAbandoned
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals rfl

/-- Dropping the live session leaves none. -/
theorem abandon_session (run : Run) : run.abandon.session = none := by
  unfold Run.abandon
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals simp_all only [clearWarning_session, emit_session, retireRecorder_session,
    recoverWorker_session]

/-- Dropping the live session keeps the latest session. -/
theorem abandon_latest (run : Run) : run.abandon.latest = run.latest := by
  unfold Run.abandon
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals simp only [clearWarning_latest, emit_latest, retireRecorder_latest,
    recoverWorker_latest]

/-- Ending the live session leaves none. -/
theorem endSession_session (outcome : Outcome) (run : Run) :
    (run.endSession outcome).session = none := by
  unfold Run.endSession
  dsimp only
  split <;> simp_all only [clearWarning_session, emit_session]

/-- Ending the live session keeps the latest session. -/
theorem endSession_latest (outcome : Outcome) (run : Run) :
    (run.endSession outcome).latest = run.latest := by
  unfold Run.endSession
  dsimp only
  split <;> simp only [clearWarning_latest, emit_latest]

/-- Discarding leaves no live session. -/
theorem discard_session (run : Run) : run.discard.session = none := by
  unfold Run.discard
  try dsimp only
  simp only [revive_session, abandon_session]

/-- Discarding keeps the latest session. -/
theorem discard_latest (run : Run) : run.discard.latest = run.latest := by
  unfold Run.discard
  try dsimp only
  simp only [revive_latest, abandon_latest]

/-- Failing leaves no live session. -/
theorem fail_session (failure : Failure) (run : Run) : (run.fail failure).session = none := by
  unfold Run.fail
  try dsimp only
  simp only [abandon_session]

/-- Failing keeps the latest session. -/
theorem fail_latest (failure : Failure) (run : Run) : (run.fail failure).latest = run.latest := by
  unfold Run.fail
  try dsimp only
  simp only [abandon_latest]

/-- Applying settings drops the live session. -/
theorem applySettings_session (reloads : Settings → Settings → Bool) (config : Settings)
    (run : Run) : (run.applySettings reloads config).session = none := by
  unfold Run.applySettings
  dsimp only
  split <;> simp only [revive_session, abandon_session]

/-- Applying settings keeps the latest session. -/
theorem applySettings_latest (reloads : Settings → Settings → Bool) (config : Settings)
    (run : Run) : (run.applySettings reloads config).latest = run.latest := by
  unfold Run.applySettings
  dsimp only
  split <;> simp only [revive_latest, abandon_latest, emit_latest]

/-- The invariant reads only the live session and the latest session. -/
theorem current_frame (before after : Run) (session : after.session = before.session)
    (latest : after.latest = before.latest) (current : before.current = true) :
    after.current = true := by
  unfold Machine.current at current ⊢
  try dsimp only
  rw [session, latest]
  exact current

/-- With no live session, the invariant holds. -/
theorem current_none (run : Run) (idle : run.session = none) : run.current = true := by
  unfold Machine.current
  try dsimp only
  rw [idle]

/-- A session whose ids all name it is current. -/
theorem current_fresh (id : Nat) : (id == id && id == id) = true := by
  have same : (id == id) = true := decide_eq_true rfl
  rw [same]
  rfl

/-- Changing the live session in place keeps the invariant when the change keeps its ids. -/
theorem current_map (change : Session → Session) (run : Run)
    (id : ∀ (session : Session), (change session).id = session.id)
    (recognition : ∀ (session : Session),
      (change session).recognition.id = session.recognition.id)
    (current : run.current = true) :
    ({ run with session := run.session.map change } : Run).current = true := by
  unfold Machine.current at current ⊢
  cases live : run.session with
  | none => rfl
  | some session =>
    rw [live] at current
    simp only [Option.map, id, recognition]
    exact current

/-- Handing over the audio log keeps the invariant. -/
theorem attachAudio_current (run : Run) (current : run.current = true) :
    run.attachAudio.current = true := by
  unfold Run.attachAudio
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals simp_all only [Machine.current, ask_session, ask_latest, warn_session, warn_latest,
    Session.takeAudioLog_id, Session.takeAudioLog_recognition]

/-- Opening the microphone keeps the invariant. -/
theorem openQueuedCapture_current (run : Run) (current : run.current = true) :
    run.openQueuedCapture.current = true := by
  unfold Run.openQueuedCapture
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals simp_all only [Machine.current, ask_session, ask_latest, openRecorder_session,
    openRecorder_latest, fail_session, Session.opened_id, Session.opened_recognition]

/-- Opening the screen recorder keeps the invariant. -/
theorem openQueuedScreen_current (run : Run) (current : run.current = true) :
    run.openQueuedScreen.current = true := by
  unfold Run.openQueuedScreen
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals simp_all only [Machine.current, ask_session, ask_latest, openRecorder_session,
    openRecorder_latest, fail_session, Session.takeFrameLog_id,
    Session.takeFrameLog_recognition, Session.screenOpened_id, Session.screenOpened_recognition]

/-- Delivering recognized text keeps the invariant. -/
theorem insertTranscript_current (run : Run) (current : run.current = true) :
    run.insertTranscript.current = true := by
  unfold Run.insertTranscript
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals simp_all only [Machine.current, ask_latest, endSession_session, Session.insert_id,
    Session.insert_recognition, Bool.and_eq_true, beq_iff_eq, and_true]

/-- Sending a transcription keeps the invariant. -/
theorem transcribe_current (run : Run) (current : run.current = true) :
    run.transcribe.current = true := by
  unfold Run.transcribe
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals simp_all only [Machine.current, emit_session, emit_latest, Session.request_id,
    Session.request_recognition]

/-- Finishing recognition keeps the invariant. -/
theorem completeRecognition_current (run : Run) (current : run.current = true) :
    run.completeRecognition.current = true := by
  unfold Run.completeRecognition
  dsimp only
  split
  · rename_i session live
    have kept : ({ run with session := some session.complete.2 } : Run).current = true := by
      simp_all only [Machine.current, Session.complete_id, Session.complete_recognition]
    split
    · exact insertTranscript_current _ kept
    · exact kept
  · exact current

/-- Feeding the worker keeps the invariant. -/
theorem feedWorker_current (run : Run) (current : run.current = true) :
    run.feedWorker.current = true := by
  unfold Run.feedWorker
  dsimp only
  split
  · exact current
  · apply transcribe_current
    repeat' (split <;> try dsimp only)
    all_goals simp_all only [Machine.current, revive_session, revive_latest]

/-- Starting transcription keeps the invariant. -/
theorem startTranscription_current (run : Run) (current : run.current = true) :
    run.startTranscription.current = true :=
  feedWorker_current _ (completeRecognition_current _ current)

/-- Publishing keeps the invariant. -/
theorem startPublishing_current (run : Run) (current : run.current = true) :
    run.startPublishing.current = true := by
  unfold Run.startPublishing
  dsimp only
  split
  · simp_all only [Machine.current, Session.publish_id, Session.publish_recognition]
  · exact current

/-- Advancing keeps the invariant. -/
theorem advance_current (run : Run) (current : run.current = true) :
    run.advance.current = true := by
  unfold Run.advance
  try dsimp only
  exact startPublishing_current _ (startTranscription_current _ (openQueuedScreen_current _
    (attachAudio_current _ (openQueuedCapture_current _ current))))

/-- Starting a session makes it the latest. -/
theorem startSession_current (run : Run) (current : run.current = true) :
    run.startSession.current = true := by
  unfold Run.startSession
  try dsimp only
  split
  · exact current_frame _ _ rfl rfl current
  · split <;> exact current_fresh _

/-- Carrying out a gesture's action keeps the invariant. -/
theorem handleAction_current (action : Option Action) (run : Run)
    (current : run.current = true) : (run.handleAction action).current = true := by
  unfold Run.handleAction
  split
  · exact startSession_current run current
  · simp only [Machine.current, cue_session, cue_latest, finishRecorder_session,
      finishRecorder_latest] at current ⊢
    split at current
    all_goals simp_all only [Option.map, Session.finish_id, Session.finish_recognition]
  · exact current_none _ (by simp only [cue_session, discard_session])
  · exact current

/-- Turning into a Show keeps the invariant. -/
theorem startShow_current (run : Run) (current : run.current = true) :
    run.startShow.current = true := by
  unfold Run.startShow
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals simp_all only [Machine.current, ask_session, ask_latest, warn_session, warn_latest,
    emit_session, emit_latest, cueHeard_session, cueHeard_latest, Session.turnIntoShow_id,
    Session.turnIntoShow_recognition, Session.keep_id, Session.keep_recognition]

/-- Ending a plain recording keeps the invariant. -/
theorem endPlain_current (run : Run) (current : run.current = true) :
    run.endPlain.current = true := by
  unfold Run.endPlain
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals first
    | exact current_none _ (by simp only [revive_session, abandon_session])
    | exact current

/-- Starting a replay keeps the invariant. -/
theorem replay_current (run : Run) (current : run.current = true) :
    run.replay.current = true := by
  unfold Run.replay
  try dsimp only
  split
  · exact current
  · have started : ({ run with gesture := run.gesture.deliver.2 } : Run).startSession.current =
        true := startSession_current _ (current_frame _ _ rfl rfl current)
    split
    · rename_i session live
      unfold Machine.current at started ⊢
      rw [live] at started
      simp only [Session.insert_id, Session.insert_recognition, Bool.and_eq_true, beq_iff_eq,
        and_true] at started ⊢
      exact started.1
    · exact started

/-- Pasting the last dictation keeps the invariant. -/
theorem pasteLast_current (run : Run) (current : run.current = true) :
    run.pasteLast.current = true := by
  unfold Run.pasteLast
  dsimp only
  have ended : run.endPlain.current = true := endPlain_current _ current
  repeat' (split <;> try dsimp only)
  all_goals first
    | exact current
    | exact current_frame _ _ rfl rfl current
    | exact ended
    | exact replay_current _ ended

/-- Arming practice keeps the invariant. -/
theorem armPractice_current (run : Run) (current : run.current = true) :
    run.armPractice.current = true := by
  unfold Run.armPractice
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals simp_all only [Machine.current, emit_session, emit_latest, ask_session, ask_latest]

/-- Disarming practice keeps the invariant. -/
theorem endPractice_current (run : Run) (current : run.current = true) :
    run.endPractice.current = true := by
  unfold Run.endPractice
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals first
    | exact current_none _ (by simp only [cue_session, discard_session])
    | simp_all only [Machine.current]

/-- Reopening an interrupted Show makes it the latest session. -/
theorem startRecovery_current (run : Run) (current : run.current = true) :
    run.startRecovery.current = true := by
  unfold Run.startRecovery
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals first
    | exact current
    | exact current_frame _ _ rfl rfl current
    | exact current_fresh _

/-- New settings keep the invariant. -/
theorem configure_current (reloads : Settings → Settings → Bool) (config : Settings)
    (run : Run) : (run.configure reloads config).current = true := by
  unfold Run.configure
  dsimp only
  split
  · exact current_none _ (by simp only [cue_session, applySettings_session])
  · exact current_none _ (applySettings_session _ _ _)

/-- A microphone event keeps the invariant. -/
theorem handleCapture_current (id : Nat) (capture : Capture) (run : Run)
    (current : run.current = true) : (run.handleCapture id capture).current = true := by
  unfold Run.handleCapture
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals simp_all only [Machine.current, warn_session, warn_latest, emit_session, emit_latest,
    cueHeard_session, cueHeard_latest, concludeCapture_session, concludeCapture_latest,
    endSession_session, fail_session, Session.stream_id, Session.stream_recognition,
    Session.piece_id, Session.piece_recognition, Session.audioReady_id,
    Session.audioReady_recognition, Session.captured_id, Session.captured_recognition,
    Session.audioFailed_id, Session.audioFailed_recognition, Bool.and_eq_true, beq_iff_eq,
    and_self]

/-- A worker completion keeps the invariant. -/
theorem handleWorker_current (completion : Completion) (run : Run)
    (current : run.current = true) : (run.handleWorker completion).current = true := by
  unfold Run.handleWorker
  cases completion <;> try dsimp only
  case abandoned ticket =>
    split
    all_goals simp_all only [Machine.current, Session.failed_id, Session.failed_recognition]
  case loaded ok =>
    cases ok <;> try dsimp only
    all_goals repeat' (split <;> try dsimp only)
    all_goals first
      | exact current_none _ (fail_session _ _)
      | simp_all only [Machine.current, Session.notTranscribed_id,
          Session.notTranscribed_recognition, Bool.and_eq_true, beq_iff_eq, and_self]
  case transcribed ticket transcript =>
    cases transcript <;> try dsimp only
    · have revived : ({ run with worker := .unavailable } : Run).revive.2.current = true :=
        current_frame _ _ (revive_session _) (revive_latest _) current
      repeat' (split <;> try dsimp only)
      all_goals first
        | exact current_none _ (fail_session _ _)
        | simp_all only [Machine.current, Session.failed_id, Session.failed_recognition,
            Session.notTranscribed_id, Session.notTranscribed_recognition, Bool.and_eq_true,
            beq_iff_eq, and_self]
    · rename_i transcript
      split
      · rename_i session live
        have heard : Machine.current
            { run with worker := Worker.ready,
                       session := some (session.heard ticket transcript).2 } = true := by
          simp_all only [Machine.current, Session.heard_id, Session.heard_recognition,
            Bool.and_eq_true, beq_iff_eq, and_self]
        split
        · exact insertTranscript_current _ heard
        · exact heard
      · exact current_frame _ _ rfl rfl current

/-- A screen event keeps the invariant. -/
theorem handleShow_current (id : Nat) (screening : Screening) (run : Run)
    (current : run.current = true) : (run.handleShow id screening).2.current = true := by
  unfold Run.handleShow
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals simp_all only [Machine.current, warn_session, warn_latest, retireRecorder_session,
    retireRecorder_latest, fail_session, cue_session, discard_session, Session.screenRecorded_id,
    Session.screenRecorded_recognition]

/-- A delivery result keeps the invariant. -/
theorem deliveredBy_current (delivered : Delivered) (run : Run)
    (current : run.current = true) : (run.deliveredBy delivered).current = true := by
  unfold Run.deliveredBy
  cases delivered
  case reopened recognized =>
    cases recognized
    · exact current_none _ (endSession_session _ _)
    · exact current_map _ _ (Session.reopened_id _) (Session.reopened_recognition _) current
  all_goals exact current_none _ (endSession_session _ _)

/-- The live session's delivery result keeps the invariant. -/
theorem handleDelivered_current (delivered : Delivered) (run : Run)
    (current : run.current = true) : (run.handleDelivered delivered).current = true := by
  unfold Run.handleDelivered
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals first
    | exact deliveredBy_current _ _ current
    | exact current

/-- A control command keeps the invariant. -/
theorem control_current (now : Nat) (command : Command) (run : Run)
    (current : run.current = true) : (run.control now command).current = true := by
  unfold Run.control
  try dsimp only
  apply current_frame _ _ (respond_session _ _) (respond_latest _ _)
  cases command <;> try dsimp only
  all_goals repeat' (split <;> try dsimp only)
  all_goals first
    | exact current
    | exact handleAction_current _ _ (current_frame _ _ rfl rfl current)
    | exact pasteLast_current _ current

/-- A preset keeps the invariant. -/
theorem adoptPreset_current (reloads : Settings → Settings → Bool) (config : Settings)
    (session : Nat) (changed : Bool) (run : Run) (current : run.current = true) :
    (run.adoptPreset reloads config session changed).2.current = true := by
  unfold Run.adoptPreset
  try dsimp only
  repeat' (split <;> try dsimp only)
  all_goals first
    | exact current
    | exact current_none _ (applySettings_session _ _ _)

/-- Input keeps the invariant. -/
theorem handleInput_current (now : Nat) (input : Input) (run : Run)
    (current : run.current = true) : (run.handleInput now input).2.current = true := by
  unfold Run.handleInput
  try dsimp only
  cases input <;> dsimp only
  case startShow =>
    split
    · exact startShow_current _ (handleAction_current _ _
        (current_frame _ _ rfl rfl (endPractice_current _ current)))
    · exact current
  case makeShow => exact startShow_current _ current
  case interrupt =>
    split
    · exact current_none _ (discard_session _)
    · exact current_frame _ _ rfl rfl current
  all_goals repeat' split
  all_goals first
    | exact current
    | exact current_frame _ _ rfl rfl current
    | exact pasteLast_current _ current

/-- Every event keeps the invariant. -/
theorem handle_current (reloads : Settings → Settings → Bool) (now : Nat) (event : Event)
    (run : Run) (current : run.current = true) :
    (run.handle reloads now event).2.current = true := by
  unfold Run.handle
  try dsimp only
  split
  · exact current
  · exact current_none _ (fail_session _ _)
  · exact current_none _ (fail_session _ _)
  · exact handleAction_current _ _ (handleInput_current _ _ _ current)
  · exact current_frame _ _ (report_session _ _) (report_latest _ _) current
  · exact configure_current _ _ _
  · exact control_current _ _ _ current
  · exact current_frame _ _ (respond_session _ _) (respond_latest _ _)
      (adoptPreset_current _ _ _ _ _ current)
  · exact handleCapture_current _ _ _ current
  · exact handleAction_current _ _ (handleShow_current _ _ _ current)
  · exact handleWorker_current _ _ current
  · exact handleDelivered_current _ _ current
  · exact current_frame _ _ (handleAbandoned_session _ _ _) (handleAbandoned_latest _ _ _)
      current
  · split
    · exact current_frame _ _ (setDevice_session _ _ _) (setDevice_latest _ _ _) current
    · exact current
  · exact startRecovery_current _ current
  · exact armPractice_current _ current
  · exact endPractice_current _ current
  · exact handleAction_current _ _ current

/-- Every step keeps the invariant: the live session is always the latest, and its recognition
belongs to it. -/
theorem step_current (reloads : Settings → Settings → Bool) (now : Nat) (event : Event)
    (run : Run) (current : run.current = true) :
    (run.step reloads now event).2.current = true := by
  have handled : (run.handle reloads now event).2.current = true :=
    handle_current reloads now event run current
  unfold Run.step
  try dsimp only
  split
  · rename_i after running
    rw [running] at handled
    exact current_frame _ _ (holdCues_session _) (holdCues_latest _) (advance_current _ handled)
  · exact handled

/-- Shutting down leaves no live session. -/
theorem shutDown_session (run : Run) : run.shutDown.session = none :=
  abandon_session _

end Run

/-- The machine starts current. -/
theorem Machine.start_current (timing : Timing) (config : Settings) (desktopReady : Bool) :
    (Machine.start timing config desktopReady).current = true := rfl

/-- From the start, every sequence of events under every environment script reaches only states
where the live session is the latest and owns its recognition. -/
theorem Run.played_current (reloads : Settings → Settings → Bool) (timing : Timing)
    (config : Settings) (desktopReady : Bool) (env : Env) (events : List (Nat × Event)) :
    (events.foldl (fun (run : Run) (event : Nat × Event) => (run.step reloads event.1 event.2).2)
      { toMachine := Machine.start timing config desktopReady, env }).current = true := by
  have reached : ∀ (run : Run), run.current = true →
      (events.foldl (fun (run : Run) (event : Nat × Event) =>
        (run.step reloads event.1 event.2).2) run).current = true := by
    induction events with
    | nil => exact fun (run : Run) (current : run.current = true) => current
    | cons event rest more =>
      exact fun (run : Run) (current : run.current = true) =>
        more _ (Run.step_current reloads event.1 event.2 run current)
  exact reached _ (Machine.start_current timing config desktopReady)

end Speakeasy

end
