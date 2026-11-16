module

/-!
# Service lifecycle model

`speakeasy_core::lifecycle::Lifecycle`: whether the dictation services are off, checking
settings, running, stopping, or quitting. Owners and restarts are the numbers that name them,
and the configuration epoch is a `UInt64` that wraps as the Rust counter does.
-/

@[expose] public section

namespace Speakeasy

/-- Where the services stand, with the owners and the restart each state holds. -/
inductive Service where
  /-- No owners and no settings check. -/
  | disabled
  /-- Settings are being checked for the epoch named. -/
  | validating (epoch : UInt64)
  /-- Owners run dictation. -/
  | running (owners : Nat)
  /-- Owners are stopping, with the restart to apply once they retire. -/
  | stopping (owners : Nat) (pending : Option Nat)
  /-- The app is quitting, with owners that have yet to retire. -/
  | quitting (owners : Option Nat)

/-- The service lifecycle, as `speakeasy_core::lifecycle::Lifecycle`. -/
structure Lifecycle where
  /-- Where the services stand. -/
  state : Service
  /-- The configuration epoch, which every settings change advances. -/
  epoch : UInt64
  /-- Whether the user paused dictation, so a save leaves it off. -/
  held : Bool

namespace Lifecycle

/-- Services start off, at epoch zero, not paused. -/
def initial : Lifecycle := { state := .disabled, epoch := 0, held := false }

/-- The owners the services hold, running or retiring. -/
def owners (lifecycle : Lifecycle) : Option Nat :=
  match lifecycle.state with
  | .running owners | .stopping owners _ | .quitting (some owners) => some owners
  | .disabled | .validating _ | .quitting none => none

/-- The restart queued for when stopping owners retire. -/
def pending (lifecycle : Lifecycle) : Option Nat :=
  match lifecycle.state with
  | .stopping _ pending => pending
  | .disabled | .validating _ | .running _ | .quitting _ => none

/-- Whether dictation counts as running, given whether running owners are live. -/
def isRunning (lifecycle : Lifecycle) (live : Bool) : Bool :=
  match lifecycle.state with
  | .running _ => live
  | .validating epoch => epoch == lifecycle.epoch
  | .disabled | .stopping _ _ | .quitting _ => false

/-- Whether retirement has a restart to apply. -/
def hasPending (lifecycle : Lifecycle) : Bool := lifecycle.pending.isSome

/-- Whether settings saved at epoch `saved` may enable dictation, given whether running owners
are live. -/
def saveMayEnable (lifecycle : Lifecycle) (saved : UInt64) (live : Bool) : Bool :=
  (!lifecycle.held && saved == lifecycle.epoch) || lifecycle.isRunning live ||
    lifecycle.hasPending

/-- Whether services may start: nothing runs, stops, or quits. -/
def mayStart (lifecycle : Lifecycle) : Bool :=
  match lifecycle.state with
  | .disabled | .validating _ => true
  | .running _ | .stopping _ _ | .quitting _ => false

/-- The running owners. -/
def active (lifecycle : Lifecycle) : Option Nat :=
  match lifecycle.state with
  | .running owners => some owners
  | .disabled | .validating _ | .stopping _ _ | .quitting _ => none

/-- The owners that are retiring. -/
def retiring (lifecycle : Lifecycle) : Option Nat :=
  match lifecycle.state with
  | .stopping owners _ | .quitting (some owners) => some owners
  | .disabled | .validating _ | .running _ | .quitting none => none

/-- Whether owners are stopping or the app is quitting. -/
def isPausing (lifecycle : Lifecycle) : Bool :=
  match lifecycle.state with
  | .stopping _ _ | .quitting _ => true
  | .disabled | .validating _ | .running _ => false

/-- Whether the app is quitting. -/
def isQuitting (lifecycle : Lifecycle) : Bool :=
  match lifecycle.state with
  | .quitting _ => true
  | .disabled | .validating _ | .running _ | .stopping _ _ => false

/-- Advance the epoch for a settings change, unless the app is quitting. -/
def reconfigure (lifecycle : Lifecycle) : Bool × Lifecycle :=
  if lifecycle.isQuitting then (false, lifecycle)
  else (true, { lifecycle with epoch := lifecycle.epoch + 1 })

/-- Begin a settings check at a new epoch when services may start, ending any pause. -/
def validate (lifecycle : Lifecycle) : Option UInt64 × Lifecycle :=
  if lifecycle.mayStart then
    (some (lifecycle.epoch + 1),
      { state := .validating (lifecycle.epoch + 1), epoch := lifecycle.epoch + 1, held := false })
  else (none, lifecycle)

/-- End the settings check for `epoch`, if it is the one in progress. -/
def validated (lifecycle : Lifecycle) (epoch : UInt64) : Bool × Lifecycle :=
  match lifecycle.state with
  | .validating current =>
    if current == epoch then (true, { lifecycle with state := .disabled }) else (false, lifecycle)
  | .disabled | .running _ | .stopping _ _ | .quitting _ => (false, lifecycle)

/-- Start owners when services may start. `launched` is what launching returns, `none` when it
fails; the result is whether owners started, or `none` when launching failed. -/
def start (lifecycle : Lifecycle) (launched : Option Nat) : Option Bool × Lifecycle :=
  if lifecycle.mayStart then
    match launched with
    | some owners => (some true, { lifecycle with state := .running owners })
    | none => (none, { lifecycle with state := .disabled })
  else (some false, lifecycle)

/-- Queue a restart for when stopping owners retire, ending any pause. -/
def queue (lifecycle : Lifecycle) (restart : Nat) : Bool × Lifecycle :=
  match lifecycle.state with
  | .stopping owners _ =>
    (true, { lifecycle with state := .stopping owners (some restart), held := false })
  | .disabled | .validating _ | .running _ | .quitting _ => (false, lifecycle)

/-- Pause dictation: stop running owners, drop a queued restart and a settings check, and hold
dictation off. The result is whether owners began stopping. -/
def pause (lifecycle : Lifecycle) : Bool × Lifecycle :=
  match lifecycle.state with
  | .running owners =>
    (true, { state := .stopping owners none, epoch := lifecycle.epoch + 1, held := true })
  | .stopping owners _ =>
    (false, { state := .stopping owners none, epoch := lifecycle.epoch + 1, held := true })
  | .disabled | .validating _ =>
    (false, { state := .disabled, epoch := lifecycle.epoch + 1, held := true })
  | .quitting _ => (false, lifecycle)

/-- Let the next save enable dictation, as on a new install. -/
def startFresh (lifecycle : Lifecycle) : Lifecycle := { lifecycle with held := false }

/-- Quit: keep owners until they retire and end any settings check. The result is whether the
app began quitting. -/
def quit (lifecycle : Lifecycle) : Bool × Lifecycle :=
  match lifecycle.state with
  | .running owners | .stopping owners _ =>
    (true, { lifecycle with state := .quitting (some owners), epoch := lifecycle.epoch + 1 })
  | .disabled | .validating _ =>
    (true, { lifecycle with state := .quitting none, epoch := lifecycle.epoch + 1 })
  | .quitting _ => (false, lifecycle)

/-- Retiring owners finished: return the queued restart, or forget the owners while quitting. -/
def retired (lifecycle : Lifecycle) : Option Nat × Lifecycle :=
  match lifecycle.state with
  | .stopping _ pending => (pending, { lifecycle with state := .disabled })
  | .quitting _ => (none, { lifecycle with state := .quitting none })
  | .disabled | .validating _ | .running _ => (none, lifecycle)

end Lifecycle

end Speakeasy
