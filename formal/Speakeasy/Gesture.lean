module

public import Speakeasy.Spec.Gesture

/-!
# Gesture contracts

The dictation gestures the app promises: hold to talk, double tap for hands-free, the hard
recording limit, and a Show that Escape leaves alone unless the key is held.
-/

@[expose] public section

namespace Speakeasy

namespace Gesture

/-- Pressing the key from idle starts a held recording. -/
theorem press_starts_hold (gesture : Gesture) (now : Nat) (idle : gesture.phase = .idle)
    (up : gesture.down = false) :
    gesture.press now = (some .start, { gesture with phase := .held now, down := true }) := by
  obtain ⟨stage, key, timing⟩ := gesture
  dsimp only at idle up
  subst idle up
  rfl

/-- Releasing a key held longer than a tap finishes the recording. -/
theorem release_finishes_hold (gesture : Gesture) (started now : Nat)
    (held : gesture.phase = .held started) (long : started + gesture.timing.tap < now) :
    (gesture.release now).1 = some .finish := by
  obtain ⟨stage, key, timing⟩ := gesture
  dsimp only at held long
  subst held
  unfold Gesture.release Gesture.tick Gesture.deadline Gesture.finish
  dsimp only
  by_cases expired : started + timing.limit ≤ now
  · rw [ite_eq_left expired]
  · have slow : ¬now - started ≤ timing.tap := by omega
    rw [ite_eq_right expired]
    dsimp only
    rw [ite_eq_right slow]

/-- Releasing a tap within the limit keeps recording and waits for a second press. -/
theorem release_after_tap_waits (gesture : Gesture) (started now : Nat)
    (held : gesture.phase = .held started) (tap : now ≤ started + gesture.timing.tap)
    (early : now < started + gesture.timing.limit) :
    gesture.release now =
      (none, { gesture with phase := .pendingTap started now, down := false }) := by
  obtain ⟨stage, key, timing⟩ := gesture
  dsimp only at held tap early
  subst held
  have running : ¬started + timing.limit ≤ now := by omega
  have quick : now - started ≤ timing.tap := by omega
  unfold Gesture.release Gesture.tick Gesture.deadline
  dsimp only
  rw [ite_eq_right running]
  dsimp only
  rw [ite_eq_left quick]

/-- A second press within the double-tap window makes the recording hands-free. -/
theorem second_press_goes_hands_free (gesture : Gesture) (started released now : Nat)
    (pending : gesture.phase = .pendingTap started released) (up : gesture.down = false)
    (early : now < started + gesture.timing.limit)
    (quick : now < released + gesture.timing.doubleTap) :
    gesture.press now = (none, { gesture with phase := .handsFree started, down := true }) := by
  obtain ⟨stage, key, timing⟩ := gesture
  dsimp only at pending up early quick
  subst pending up
  have waiting : ¬min (started + timing.limit) (released + timing.doubleTap) ≤ now := by omega
  unfold Gesture.press Gesture.tick Gesture.deadline
  dsimp only
  rw [ite_eq_right waiting]
  rfl

/-- Every recording finishes once the limit has passed since it started. -/
theorem tick_enforces_limit (gesture : Gesture) (started now : Nat)
    (recording : gesture.phase = .held started ∨ gesture.phase = .handsFree started ∨
      gesture.phase = .showing started ∨ gesture.phase = .showArmed started ∨
      ∃ (released : Nat), gesture.phase = .pendingTap started released)
    (late : started + gesture.timing.limit ≤ now) :
    (gesture.tick now).1 = some .finish := by
  obtain ⟨stage, key, timing⟩ := gesture
  dsimp only at recording late
  unfold Gesture.tick Gesture.deadline Gesture.finish
  rcases recording with same | same | same | same | ⟨released, same⟩
  all_goals subst same
  all_goals dsimp only
  case inr.inr.inr.inr =>
    rw [ite_eq_left (Nat.le_trans (Nat.min_le_left _ _) late)]
  all_goals rw [ite_eq_left late]

/-- A tick either changes nothing or finishes the gesture. -/
theorem tick_cases (gesture : Gesture) (now : Nat) :
    gesture.tick now = (none, gesture) ∨ gesture.tick now = gesture.finish := by
  unfold Gesture.tick
  split
  · split
    · exact Or.inr rfl
    · exact Or.inl rfl
  · exact Or.inl rfl

/-- No gesture step restarts a recording's clock. From a recording, every step either ends it
or keeps the moment it started, so `tick_enforces_limit` counts from the first press. -/
theorem steps_keep_start (gesture : Gesture) (now started : Nat)
    (recording : gesture.phase.started = some started) :
    let kept : Gesture → Prop := fun (after : Gesture) =>
      after.phase.started = none ∨ after.phase.started = some started
    kept (gesture.press now).2 ∧ kept (gesture.release now).2 ∧ kept (gesture.tick now).2 ∧
      kept gesture.lock ∧ kept gesture.present ∧ kept (gesture.toggle now).2 ∧
      kept gesture.finish.2 ∧ kept gesture.abandon.2 ∧ kept gesture.cancel.2 ∧
      kept gesture.escape.2 ∧ kept gesture.interrupt.2 ∧ kept gesture.deliver.2 ∧
      kept gesture.save.2 ∧ kept gesture.complete := by
  unfold Gesture.press Gesture.release
  rcases tick_cases gesture now with ticked | ticked
  all_goals rw [ticked]
  all_goals obtain ⟨stage, key, timing⟩ := gesture
  all_goals cases stage
  all_goals dsimp only [GesturePhase.started] at recording
  all_goals cases recording
  all_goals unfold Gesture.lock Gesture.present Gesture.toggle Gesture.finish
  all_goals unfold Gesture.cancel Gesture.escape Gesture.interrupt Gesture.abandon
  all_goals unfold Gesture.deliver Gesture.save Gesture.complete
  all_goals dsimp only
  all_goals refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
  all_goals repeat' (split <;> try dsimp only)
  all_goals first
    | exact Or.inl rfl
    | exact Or.inr rfl

/-- Escape leaves a Show alone while the key is up. -/
theorem escape_spares_show (gesture : Gesture) (started : Nat)
    (showing : gesture.phase = .showing started ∨ gesture.phase = .showArmed started)
    (up : gesture.down = false) : gesture.escape = (none, gesture) := by
  obtain ⟨stage, key, timing⟩ := gesture
  dsimp only at showing up
  subst up
  rcases showing with same | same
  all_goals subst same
  all_goals rfl

/-- A saving Show cannot be cancelled or escaped. -/
theorem saving_ignores_cancel (gesture : Gesture) (saving : gesture.phase = .saving) :
    gesture.cancel = (none, gesture) ∧ gesture.escape = (none, gesture) := by
  obtain ⟨stage, key, timing⟩ := gesture
  dsimp only at saving
  subst saving
  exact ⟨rfl, rfl⟩

end Gesture

end Speakeasy

end
