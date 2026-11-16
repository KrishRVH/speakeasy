module

public import Speakeasy.Spec.Machine
import Speakeasy.Current

/-!
# Safety of the session machine

Theorems about the model in `Speakeasy.Spec.Machine`. Each one holds for every state and every
answer the environment can give; a theorem that assumes `run.current` holds in every state the
machine reaches from its start, by `Run.played_current`.
-/

@[expose] public section

namespace Speakeasy

/-- While the machine is current, the live session is the latest and owns its recognition. -/
theorem Run.live_latest (run : Run) (session : Session) (live : run.session = some session)
    (current : run.current = true) :
    session.id = run.latest.id ∧ session.recognition.id = session.id := by
  unfold Machine.current at current
  rw [live] at current
  have both : (session.id == run.latest.id) = true ∧
      (session.recognition.id == session.id) = true := (Bool.and_eq_true _ _).mp current
  exact ⟨of_decide_eq_true both.1, of_decide_eq_true both.2⟩

/-- While the machine is current, the live session answers to no stale id. -/
theorem Run.live_rejects (run : Run) (session : Session) (id : Nat)
    (live : run.session = some session) (current : run.current = true)
    (stale : id ≠ run.latest.id) : (session.id == id) = false :=
  decide_eq_false (fun (same : session.id = id) =>
    stale (same.symm.trans (Run.live_latest run session live current).1))

/-- Holding cues sets `cuesHeld` to whether a session runs with sounds on. -/
theorem Run.holdCues_cuesHeld (run : Run) :
    run.holdCues.cuesHeld = (run.config.playSounds && run.session.isSome) := by
  unfold Run.holdCues
  dsimp only
  generalize (run.config.playSounds && run.session.isSome) = held
  split
  · rfl
  · rename_i same
    revert same
    cases held <;> cases run.cuesHeld <;> decide

/-- Holding cues changes no setting and no session. -/
theorem Run.holdCues_frame (run : Run) :
    run.holdCues.config = run.config ∧ run.holdCues.session = run.session := by
  unfold Run.holdCues
  dsimp only
  split <;> exact ⟨rfl, rfl⟩

/-- After every step the app survives, other apps' cues are held exactly while a session runs
with sounds on. -/
theorem Run.step_holds_cues (reloads : Settings → Settings → Bool) (now : Nat) (event : Event)
    (run : Run) (running : (run.step reloads now event).1 = .running) :
    (run.step reloads now event).2.cuesHeld =
      ((run.step reloads now event).2.config.playSounds &&
        (run.step reloads now event).2.session.isSome) := by
  unfold Run.step at running ⊢
  generalize run.handle reloads now event = handled at running ⊢
  obtain ⟨flow, after⟩ := handled
  cases flow
  · exact Run.holdCues_cuesHeld after.advance ▸ (Run.holdCues_frame after.advance).1 ▸
      (Run.holdCues_frame after.advance).2 ▸ rfl
  · nomatch running
  · nomatch running

/-- While the machine is current, a microphone event for any session but the latest changes
nothing. -/
theorem Run.handle_stale_capture (reloads : Settings → Settings → Bool) (now : Nat) (id : Nat)
    (capture : Capture) (run : Run) (current : run.current = true) (stale : id ≠ run.latest.id) :
    run.handle reloads now (.capture id capture) = (.running, run) := by
  unfold Run.handle Run.handleCapture
  dsimp only
  split
  · rfl
  · rename_i session live
    rw [Run.live_rejects run session id live current stale]
    rfl

/-- While the machine is current, a screen event for any session but the latest changes
nothing. -/
theorem Run.handle_stale_screening (reloads : Settings → Settings → Bool) (now : Nat) (id : Nat)
    (screening : Screening) (run : Run) (current : run.current = true)
    (stale : id ≠ run.latest.id) :
    run.handle reloads now (.screening id screening) = (.running, run) := by
  unfold Run.handle Run.handleShow
  dsimp only
  split
  · rfl
  · rename_i session live
    rw [Run.live_rejects run session id live current stale]
    rfl

/-- While the machine is current, a transcript for any session but the latest only frees the
worker: it cannot be inserted, and it leaves the live session as it was. -/
theorem Run.handle_stale_transcript (reloads : Settings → Settings → Bool) (now : Nat)
    (ticket : Ticket) (transcript : Transcript) (run : Run) (current : run.current = true)
    (stale : ticket.session ≠ run.latest.id) :
    run.handle reloads now (.worker (.transcribed ticket (some transcript))) =
      (.running, { run with worker := .ready }) := by
  unfold Run.handle Run.handleWorker
  dsimp only
  split
  · rename_i session live
    have owned : session.id = run.latest.id ∧ session.recognition.id = session.id :=
      Run.live_latest run session live current
    have different : (ticket.session == session.recognition.id) = false :=
      decide_eq_false (fun (same : ticket.session = session.recognition.id) =>
        stale (same.trans (owned.2.trans owned.1)))
    have refused : session.recognition.accepts ticket = false := by
      unfold Recognition.accepts
      rw [different]
      rfl
    unfold Session.heard Session.accepts
    rw [refused, Bool.and_false]
    show (Flow.running, ({ run with worker := .ready, session := some session } : Run)) = _
    rw [← live]
  · rfl

/-- A delivery result while the live session awaits none changes nothing. A delivery result
names no session, so the owner keeps one delivery at a time and reports a retired session's
result as `abandoned` instead. -/
theorem Run.handle_unawaited_delivery (reloads : Settings → Settings → Bool) (now : Nat)
    (delivered : Delivered) (run : Run)
    (unawaited : ∀ (session : Session), run.session = some session →
      session.awaitsDelivery = false) :
    run.handle reloads now (.delivered delivered) = (.running, run) := by
  unfold Run.handle Run.handleDelivered
  dsimp only
  split
  · rename_i session live
    rw [unawaited session live]
    rfl
  · rfl

/-- A result for a session that is not retiring changes nothing. -/
theorem Run.handle_unknown_abandoned (reloads : Settings → Settings → Bool) (now : Nat)
    (id : Nat) (delivered : Delivered) (run : Run) (unknown : run.retiring.contains id = false) :
    run.handle reloads now (.abandoned id delivered) = (.running, run) := by
  unfold Run.handle Run.handleAbandoned
  dsimp only
  rw [unknown]
  rfl

/-- A recorder that is not retiring ignores a retirement report. -/
theorem Run.handle_unretiring (reloads : Settings → Settings → Bool) (now : Nat)
    (recorder : Recorder) (run : Run) (busy : run.device recorder ≠ .retiring) :
    run.handle reloads now (.retired recorder) = (.running, run) := by
  have idle : (run.device recorder == .retiring) = false := decide_eq_false busy
  unfold Run.handle
  dsimp only
  rw [idle]
  rfl

/-- A retired session's delivery never touches the live session or the gesture. -/
theorem Run.handle_abandoned_frame (reloads : Settings → Settings → Bool) (now : Nat)
    (id : Nat) (delivered : Delivered) (run : Run) :
    (run.handle reloads now (.abandoned id delivered)).2.session = run.session ∧
      (run.handle reloads now (.abandoned id delivered)).2.gesture = run.gesture := by
  refine ⟨Run.handleAbandoned_session id delivered run, ?_⟩
  unfold Run.handle Run.handleAbandoned
  dsimp only
  repeat' (split <;> try dsimp only)
  all_goals rfl

/-- Cleanup keeps a Show's bundle only when the Show finished; an unfinished or forfeited
Show is discarded. -/
theorem Session.releaseOf_keeps_only_finished (stage : Stage) (disposition : Disposition)
    (narration : Option Bool) (kept : Session.releaseOf stage disposition = .keep narration) :
    disposition = .finished := by
  unfold Session.releaseOf at kept
  split at kept
  · rfl
  all_goals cases kept

/-- Cleanup keeps narration only when it was kept whole or recovered incomplete. -/
theorem Session.releaseOf_narration (stage : Stage) (disposition : Disposition)
    (incomplete : Bool) (kept : Session.releaseOf stage disposition = .keep (some incomplete)) :
    stage = .awaitingFootage (if incomplete then some .incomplete else none) := by
  unfold Session.releaseOf at kept
  split at kept
  · split at kept
    all_goals cases kept
    all_goals rfl
  all_goals cases kept

end Speakeasy

end
