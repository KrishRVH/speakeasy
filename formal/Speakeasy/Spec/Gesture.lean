module

/-!
# Gesture model

`speakeasy_core::gesture::Gesture`: hold-to-talk, double-tap hands-free, Shows, and the
recording limit. Moments are clock ticks; every threshold compares one moment's age with a
timing, so a moment older than the limit behaves like any other.
-/

@[expose] public section

namespace Speakeasy

/-- The gesture thresholds, in ticks. -/
structure Timing where
  /-- The longest press that counts as a tap. -/
  tap : Nat
  /-- How long after a tap a second press makes the session hands-free. -/
  doubleTap : Nat
  /-- The recording limit. -/
  limit : Nat

/-- Where the gesture is, with the moments its deadlines start from. -/
inductive GesturePhase where
  /-- No session. -/
  | idle
  /-- The key is held. -/
  | held (started : Nat)
  /-- A tap was released; a second press within the double-tap window goes hands-free. -/
  | pendingTap (started : Nat) (released : Nat)
  /-- Recording without the key held. -/
  | handsFree (started : Nat)
  /-- A Show is recording. -/
  | showing (started : Nat)
  /-- A Show is recording and the key is down, so its release finishes the Show. -/
  | showArmed (started : Nat)
  /-- A dictation is finishing. -/
  | processing
  /-- A Show is finishing. -/
  | saving

/-- When the recording a phase belongs to started, or `none` for a phase that records nothing. -/
def GesturePhase.started : GesturePhase → Option Nat
  | .held started | .pendingTap started _ | .handsFree started | .showing started
  | .showArmed started => some started
  | .idle | .processing | .saving => none

/-- The coarse gesture state other modules read, as `gesture::State`. -/
inductive GestureState where
  /-- No session. -/
  | idle
  /-- The key is held. -/
  | held
  /-- A tap is pending. -/
  | pendingTap
  /-- Recording without the key held, or a Show. -/
  | handsFree
  /-- Finishing. -/
  | processing
  deriving DecidableEq

/-- Decides whether two gesture state values are equal. -/
add_decl_doc instDecidableEqGestureState

/-- A gesture state value is the constructor its index names. -/
add_decl_doc GestureState.ofNat_ctorIdx

/-- What a gesture transition asks the session to do. -/
inductive Action where
  /-- Start a session. -/
  | start
  /-- Finish the session. -/
  | finish
  /-- Cancel the session. -/
  | cancel

/-- The gesture: its phase, whether the key is down, and its timing. -/
structure Gesture where
  /-- The phase. -/
  phase : GesturePhase
  /-- Whether the key is down. -/
  down : Bool
  /-- The thresholds. -/
  timing : Timing

namespace Gesture

/-- A fresh gesture with the given timing. -/
def new (timing : Timing) : Gesture :=
  { phase := .idle, down := false, timing }

/-- The coarse state. -/
def state (gesture : Gesture) : GestureState :=
  match gesture.phase with
  | .idle => .idle
  | .held _ => .held
  | .pendingTap _ _ => .pendingTap
  | .handsFree _ | .showing _ | .showArmed _ => .handsFree
  | .processing | .saving => .processing

/-- When the gesture finishes on its own, if it does. -/
def deadline (gesture : Gesture) : Option Nat :=
  match gesture.phase with
  | .idle | .processing | .saving => none
  | .held started | .handsFree started | .showing started | .showArmed started =>
    some (started + gesture.timing.limit)
  | .pendingTap started released =>
    some (min (started + gesture.timing.limit) (released + gesture.timing.doubleTap))

/-- Moves to processing a delivery when idle; reports whether it did. -/
def deliver (gesture : Gesture) : Bool × Gesture :=
  match gesture.phase with
  | .idle => (true, { gesture with phase := .processing })
  | _ => (false, gesture)

/-- Moves to saving a recovered Show when idle; reports whether it did. -/
def save (gesture : Gesture) : Bool × Gesture :=
  match gesture.phase with
  | .idle => (true, { gesture with phase := .saving })
  | _ => (false, gesture)

/-- Finishes a recording gesture. -/
def finish (gesture : Gesture) : Option Action × Gesture :=
  match gesture.phase with
  | .idle | .processing | .saving => (none, gesture)
  | .held _ | .pendingTap _ _ | .handsFree _ =>
    (some .finish, { gesture with phase := .processing })
  | .showing _ | .showArmed _ => (some .finish, { gesture with phase := .saving })

/-- Finishes the gesture once its deadline has passed. -/
def tick (now : Nat) (gesture : Gesture) : Option Action × Gesture :=
  match gesture.deadline with
  | some deadline => if deadline ≤ now then gesture.finish else (none, gesture)
  | none => (none, gesture)

/-- The key goes down. -/
def press (now : Nat) (gesture : Gesture) : Option Action × Gesture :=
  let expired : Option Action × Gesture := gesture.tick now
  let down : Gesture := { expired.2 with down := true }
  if gesture.down || expired.1.isSome then (expired.1, down)
  else
    match down.phase with
    | .idle => (some .start, { down with phase := .held now })
    | .pendingTap started _ => (none, { down with phase := .handsFree started })
    | .handsFree _ => down.finish
    | .showing started => (none, { down with phase := .showArmed started })
    | .held _ | .showArmed _ | .processing | .saving => (none, down)

/-- The key comes up. -/
def release (now : Nat) (gesture : Gesture) : Option Action × Gesture :=
  let expired : Option Action × Gesture := gesture.tick now
  let up : Gesture := { expired.2 with down := false }
  match up.phase with
  | .held started =>
    if now - started ≤ up.timing.tap then
      (none, { up with phase := .pendingTap started now })
    else up.finish
  | .showArmed _ => up.finish
  | .idle | .pendingTap _ _ | .handsFree _ | .showing _ | .processing | .saving =>
    (expired.1, up)

/-- Locks a held recording hands-free. -/
def lock (gesture : Gesture) : Gesture :=
  match gesture.phase with
  | .held started => { gesture with phase := .handsFree started }
  | _ => gesture

/-- Turns a recording gesture into a Show. -/
def present (gesture : Gesture) : Gesture :=
  match gesture.phase with
  | .held started | .handsFree started => { gesture with phase := .showing started }
  | _ => gesture

/-- Starts hands-free when idle, otherwise finishes. -/
def toggle (now : Nat) (gesture : Gesture) : Option Action × Gesture :=
  match gesture.phase with
  | .idle => (some .start, { gesture with phase := .handsFree now })
  | .held _ | .pendingTap _ _ | .handsFree _ | .showing _ | .showArmed _ => gesture.finish
  | .processing | .saving => (none, gesture)

/-- Ends the gesture whatever its phase. -/
def abandon (gesture : Gesture) : Option Action × Gesture :=
  match gesture.phase with
  | .idle => (none, gesture)
  | _ => (some .cancel, { gesture with phase := .idle })

/-- Cancels unless a Show is saving. -/
def cancel (gesture : Gesture) : Option Action × Gesture :=
  match gesture.phase with
  | .saving => (none, gesture)
  | _ => gesture.abandon

/-- Escape cancels, except a Show whose key is up. -/
def escape (gesture : Gesture) : Option Action × Gesture :=
  match gesture.phase with
  | .showing _ | .showArmed _ => if gesture.down then gesture.cancel else (none, gesture)
  | _ => gesture.cancel

/-- An interruption finishes a Show and cancels anything else. -/
def interrupt (gesture : Gesture) : Option Action × Gesture :=
  match gesture.phase with
  | .showing _ | .showArmed _ => gesture.finish
  | _ => gesture.cancel

/-- A finished session returns the gesture to idle. -/
def complete (gesture : Gesture) : Gesture :=
  match gesture.phase with
  | .processing | .saving => { gesture with phase := .idle }
  | _ => gesture

end Gesture

end Speakeasy

end
