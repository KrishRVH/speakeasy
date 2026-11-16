module

public import Speakeasy.Spec.Lifecycle

/-!
# Lifecycle contracts

The service lifecycle never drops owners before they retire, never starts owners over others,
names the check it begins, treats quitting as final, applies only the latest queued restart and
only once, and keeps a pause's flag until a resume, a queued restart, or a fresh start clears it.
-/

@[expose] public section

namespace Speakeasy

namespace Lifecycle

/-- Every operation except retirement keeps the owners the services hold, so owners are
released only after they acknowledge retiring. -/
theorem keeps_owners (lifecycle : Lifecycle) (owners : Nat) (epoch : UInt64)
    (launched : Option Nat) (restart : Nat) (holding : lifecycle.owners = some owners) :
    lifecycle.reconfigure.2.owners = some owners ∧ lifecycle.validate.2.owners = some owners ∧
      (lifecycle.validated epoch).2.owners = some owners ∧
      (lifecycle.start launched).2.owners = some owners ∧
      (lifecycle.queue restart).2.owners = some owners ∧
      lifecycle.pause.2.owners = some owners ∧ lifecycle.startFresh.owners = some owners ∧
      lifecycle.quit.2.owners = some owners := by
  obtain ⟨state, current, held⟩ := lifecycle
  cases state with
  | disabled => cases holding
  | validating _ => cases holding
  | running _ =>
    cases holding
    exact ⟨rfl, rfl, rfl, rfl, rfl, rfl, rfl, rfl⟩
  | stopping _ _ =>
    cases holding
    exact ⟨rfl, rfl, rfl, rfl, rfl, rfl, rfl, rfl⟩
  | quitting kept =>
    cases kept with
    | none => cases holding
    | some _ =>
      cases holding
      exact ⟨rfl, rfl, rfl, rfl, rfl, rfl, rfl, rfl⟩

/-- Starting while owners run or retire changes nothing, so owners never start over others. -/
theorem start_spares_owners (lifecycle : Lifecycle) (owners : Nat) (launched : Option Nat)
    (holding : lifecycle.owners = some owners) :
    lifecycle.start launched = (some false, lifecycle) := by
  obtain ⟨state, current, held⟩ := lifecycle
  cases state with
  | disabled => cases holding
  | validating _ => cases holding
  | running _ => rfl
  | stopping _ _ => rfl
  | quitting kept =>
    cases kept with
    | none => cases holding
    | some _ => rfl

/-- A settings check runs for the epoch `validate` returns, which is the new current epoch, so the
caller's later `validated` call names the check in progress. -/
theorem validate_names_check (lifecycle : Lifecycle) :
    (lifecycle.validate.1 = some lifecycle.validate.2.epoch ∧
      lifecycle.validate.2.state = .validating lifecycle.validate.2.epoch) ∨
      lifecycle.validate = (none, lifecycle) := by
  unfold validate
  split
  · exact Or.inl ⟨rfl, rfl⟩
  · exact Or.inr rfl

/-- Quitting is final: no operation leaves it, none reports a change it would make, and
retirement applies no restart. -/
theorem quit_is_final (lifecycle : Lifecycle) (epoch : UInt64) (launched : Option Nat)
    (restart : Nat) (quitting : lifecycle.isQuitting = true) :
    (lifecycle.reconfigure = (false, lifecycle) ∧ lifecycle.validate = (none, lifecycle) ∧
      lifecycle.validated epoch = (false, lifecycle) ∧
      lifecycle.start launched = (some false, lifecycle) ∧
      lifecycle.queue restart = (false, lifecycle) ∧ lifecycle.pause = (false, lifecycle) ∧
      lifecycle.quit = (false, lifecycle)) ∧
      lifecycle.startFresh.isQuitting = true ∧ lifecycle.retired.1 = none ∧
      lifecycle.retired.2.isQuitting = true := by
  obtain ⟨state, current, held⟩ := lifecycle
  cases state with
  | disabled => cases quitting
  | validating _ => cases quitting
  | running _ => cases quitting
  | stopping _ _ => cases quitting
  | quitting _ => exact ⟨⟨rfl, rfl, rfl, rfl, rfl, rfl, rfl⟩, rfl, rfl, rfl⟩

/-- Retirement applies the restart queued last. -/
theorem retired_applies_latest (lifecycle : Lifecycle) (restart : Nat)
    (queued : (lifecycle.queue restart).1 = true) :
    (lifecycle.queue restart).2.retired.1 = some restart := by
  obtain ⟨state, current, held⟩ := lifecycle
  cases state with
  | disabled => cases queued
  | validating _ => cases queued
  | running _ => cases queued
  | stopping _ _ => rfl
  | quitting _ => cases queued

/-- Retirement releases every retiring owner and applies a restart at most once. -/
theorem retired_once (lifecycle : Lifecycle) :
    lifecycle.retired.2.retiring = none ∧ lifecycle.retired.2.retired.1 = none := by
  obtain ⟨state, current, held⟩ := lifecycle
  cases state with
  | disabled => exact ⟨rfl, rfl⟩
  | validating _ => exact ⟨rfl, rfl⟩
  | running _ => exact ⟨rfl, rfl⟩
  | stopping _ _ => exact ⟨rfl, rfl⟩
  | quitting _ => exact ⟨rfl, rfl⟩

/-- Right after a pause, unless the app is quitting, no save enables dictation and no settings
check completes, whatever epoch the save or check began at. -/
theorem pause_holds_off (lifecycle : Lifecycle) (saved epoch : UInt64) (live : Bool)
    (staying : lifecycle.isQuitting = false) :
    lifecycle.pause.2.saveMayEnable saved live = false ∧
      (lifecycle.pause.2.validated epoch).1 = false := by
  obtain ⟨state, current, held⟩ := lifecycle
  cases state with
  | disabled => exact ⟨rfl, rfl⟩
  | validating _ => exact ⟨rfl, rfl⟩
  | running _ => exact ⟨rfl, rfl⟩
  | stopping _ _ => exact ⟨rfl, rfl⟩
  | quitting _ => cases staying

/-- Settings changes, check results, starts, quitting, and retirement all keep the pause flag set,
so only a resume, a queued restart, or a fresh start clears it. The flag stops a save from
enabling dictation; running owners count as running whatever it says. -/
theorem keeps_pause (lifecycle : Lifecycle) (epoch : UInt64) (launched : Option Nat)
    (held : lifecycle.held = true) :
    lifecycle.reconfigure.2.held = true ∧ (lifecycle.validated epoch).2.held = true ∧
      (lifecycle.start launched).2.held = true ∧ lifecycle.quit.2.held = true ∧
      lifecycle.retired.2.held = true := by
  obtain ⟨state, current, holding⟩ := lifecycle
  dsimp only at held
  subst held
  cases launched with
  | none =>
    cases state with
    | disabled => exact ⟨rfl, rfl, rfl, rfl, rfl⟩
    | validating checking =>
      refine ⟨rfl, ?_, rfl, rfl, rfl⟩
      unfold validated
      dsimp only
      split <;> rfl
    | running _ => exact ⟨rfl, rfl, rfl, rfl, rfl⟩
    | stopping _ _ => exact ⟨rfl, rfl, rfl, rfl, rfl⟩
    | quitting _ => exact ⟨rfl, rfl, rfl, rfl, rfl⟩
  | some _ =>
    cases state with
    | disabled => exact ⟨rfl, rfl, rfl, rfl, rfl⟩
    | validating checking =>
      refine ⟨rfl, ?_, rfl, rfl, rfl⟩
      unfold validated
      dsimp only
      split <;> rfl
    | running _ => exact ⟨rfl, rfl, rfl, rfl, rfl⟩
    | stopping _ _ => exact ⟨rfl, rfl, rfl, rfl, rfl⟩
    | quitting _ => exact ⟨rfl, rfl, rfl, rfl, rfl⟩

end Lifecycle

end Speakeasy
