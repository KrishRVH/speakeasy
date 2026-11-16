module

import Plausible.Testable
import Speakeasy.Spec.Machine

/-!
# Speakeasy model tests

Concrete runs of the session machine model. The `decide` examples evaluate them in the kernel
at build time. `main` runs the same checks and a seeded Plausible property against the native
binary, where compiler and runtime bugs would show. The exhaustive comparison with the Rust
machine is `speakeasy_model`'s job.
-/

open Speakeasy

/-- Timing for the scenarios, in ticks. -/
def timing : Timing := { tap := 1, doubleTap := 2, limit := 10 }

/-- Settings without sounds, so runs record fewer effects. -/
def quiet : Settings := { playSounds := false, recognition := .segmented, probe := false }

/-- Plays timed events from a fresh machine on a ready desktop, answering queries in order. -/
def play (answers : List Nat) (events : List (Nat × Event)) : Flow × Run :=
  events.foldl (fun (state : Flow × Run) (event : Nat × Event) =>
      match state.1 with
      | .running =>
        state.2.step (fun (_ : Settings) (_ : Settings) => false) event.1 event.2
      | .failed | .stopped => state)
    (.running, { toMachine := Machine.start timing quiet true, env := { answers, effects := [] } })

/-- A held dictation: the model loads, the key is held past a tap, and the text is typed. -/
def held : List (Nat × Event) :=
  [(0, .worker (.loaded true)), (0, .input .press), (0, .capture 1 .ready), (5, .input .release),
    (5, .capture 1 (.finished (.sealed (some none)))), (5, .retired .microphone),
    (5, .worker (.transcribed { session := 1, part := .final } (some .text))),
    (5, .delivered (.inserted .sent))]

/-- Whether the dictation ended done, typed its text once, and can be pasted again. -/
def dictationTyped : Bool :=
  let (flow, run) := play [0, 0, 0, 0] held
  let done : Bool := match run.outcome with
    | .done => true
    | _ => false
  let inserts : Nat := (run.env.effects.filter (fun (effect : Effect) => match effect with
    | .insert => true
    | _ => false)).length
  flow == .running && run.session.isNone && done && run.lastDictation && inserts == 1

/-- Whether a microphone report for the first session leaves the second one recording. -/
def staleIgnored : Bool :=
  let (flow, run) := play [0, 0, 0, 0, 0, 0, 0]
    (held ++ [(6, .input .press), (6, .capture 2 .ready),
      (7, .capture 1 (.finished (.sealed none)))])
  flow == .running && match run.session with
    | some session => session.id == 2 && session.isRecording
    | none => false

example : dictationTyped = true := by decide +kernel
example : staleIgnored = true := by decide +kernel

/-- Events a random run draws from, with session ids that are current, stale, and new. -/
def events : List Event :=
  [.input .press, .input .release, .input .lock, .input .makeShow, .input .escape,
    .input .toggle, .input .pasteLast, .input .startShow, .input .cancel, .input .interrupt,
    .worker (.loaded true), .worker (.loaded false), .capture 1 .ready, .capture 2 .ready,
    .capture 1 (.finished (.sealed (some none))), .capture 2 (.finished (.sealed (some none))),
    .capture 2 (.finished (.unheard false)),
    .worker (.transcribed { session := 1, part := .final } (some .text)),
    .worker (.transcribed { session := 2, part := .final } (some .text)),
    .worker (.transcribed { session := 2, part := .final } (some .words)),
    .worker (.transcribed { session := 2, part := .final } none),
    .screening 1 .finished, .screening 2 .finished, .screening 2 .unchosen,
    .delivered (.inserted .sent), .delivered (.published (.saved true)),
    .delivered (.reopened (some (.narration none))), .abandoned 1 (.abandoned true),
    .abandoned 2 (.published .recovered), .retired .microphone, .retired .screen, .timer,
    .recover, .practice, .practiceClosed, .control .start, .control (.stop (some 2)),
    .configure quiet]

/-- The events the choices pick, one per moment from `now`. -/
def chosen : Nat → List Nat → List (Nat × Event)
  | _, [] => []
  | now, choice :: rest =>
    (now, events.getD (choice % events.length) .timer) :: chosen (now + 1) rest

/-- Whether a run keeps the proved invariant and keeps the gesture idle exactly when no session
runs. -/
def sound (answers : List Nat) (choices : List Nat) : Bool :=
  let (_, run) := play answers (chosen 0 choices)
  run.current && (run.gesture.state == .idle) == run.session.isNone

/-- Fixed Plausible settings, so every run checks the same samples. -/
def propertyConfig : Plausible.Configuration :=
  { numInst := 2000, maxSize := 60, randomSeed := some 20261116, quiet := true }

/-- Reports one failed check and returns whether it passed. -/
def checkExample (name : String) (passed : Bool) : IO Bool := do
  if passed then
    pure true
  else
    IO.eprintln s!"{name}: failed"
    pure false

open Plausible.Decorations in
/-- Runs one property and returns whether Plausible found no counterexample. -/
def checkProperty (name : String) (property : Prop)
    (decorated : DecorationsOf property := by mk_decorations)
    [Plausible.Testable decorated] : IO Bool := do
  match ← Plausible.Testable.checkIO decorated propertyConfig with
  | .success _ => pure true
  | .gaveUp attempts =>
    IO.eprintln s!"{name}: gave up after {attempts} attempts"
    pure false
  | .failure _ counterexample _ =>
    IO.eprintln s!"{name}: counterexample {counterexample}"
    pure false

/-- Runs every example and property; exits nonzero if any fails. -/
public def main : IO UInt32 := do
  let results : List Bool := [
    ← checkExample "a held dictation types its text" dictationTyped,
    ← checkExample "a stale report leaves the newer session" staleIgnored,
    ← checkProperty "every random run stays sound"
      (∀ (answers : List Nat) (choices : List Nat), sound answers choices = true)
  ]
  pure (if results.all (fun (passed : Bool) => passed) then 0 else 1)
