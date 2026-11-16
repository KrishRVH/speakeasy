import SpeakeasyAudit.Environment
import SpeakeasyAudit.Manifest
import SpeakeasyAudit.SelfTest
import SpeakeasyAudit.Source

/-!
# Speakeasy audit

Lake's lint driver. Each subcommand prints its findings and exits nonzero if
there are any:

* `self-test`: proves every source and environment rule fires on modules that
  break it, that none fires on compliant ones, and that `source` reports a
  banned command without running it.
* `format`: text rules for every owned `.lean` file.
* `source`: ownership and syntax rules for every owned `.lean` file. The build
  runs it before compiling any owned module, so no banned command can run first.
* `manifest`: toolchain and dependency pinning.
* `environment`: kernel, axiom, contract, documentation, linter, and IR rules for
  the specifications, library, and executable.
* `test-environment`: the same rules for the test harness, which defines its own
  `main` and so needs its own environment.

Helpers for `scripts/check.sh` and the self-test print data rather than findings:

* `dependency-imports`: the dependency modules owned files import, which must be
  built before `source` can parse with their syntax.
* `library-declarations` and `executable-declarations`: the written
  declarations of that role, for the external kernel checker. The library's
  include its specifications.
* `self-test-case`: the findings for one self-test case.

Without a subcommand, every check except `source` runs, each in its own process:
importing an environment with extensions can be done once per process.
-/

open Lean SpeakeasyAudit

/-- Library and executable roots, audited in one environment. -/
def shippedRoots : Array Name := #[`Speakeasy, `SpeakeasyMain]

/-- Test roots, audited separately because they also define `main`. -/
def testRoots : Array Name := #[`SpeakeasyTests]

/-- Owned modules, by source path, whose role places them in the given roots' audit. -/
def modulesFor (roles : List Role) : IO (Array Name) := do
  let modules : Array Name := (← sourceFiles).map moduleOf
  pure (modules.filter (fun (module : Name) =>
    match roleOf module with
    | some role => roles.contains role
    | none => false))

/-- Runs one subcommand and returns its exit code. `allowInitializers` re-enables initializer
execution, which each environment import consumes. -/
def runCheck (allowInitializers : BaseIO Unit) (check : String) : IO UInt32 := do
  match check with
  | "self-test" => report check (← selfTest)
  | "format" => report check (← formatAudit)
  | "source" => report check (← sourceAudit allowInitializers)
  | "manifest" => report check (← manifestAudit)
  | "environment" =>
    report check (← environmentAudit shippedRoots (← modulesFor [.spec, .library, .executable]))
  | "test-environment" => report check (← environmentAudit testRoots (← modulesFor [.tests]))
  | "dependency-imports" =>
    (← dependencyImports).forM (fun (module : Name) => IO.println module)
    pure 0
  | "library-declarations" =>
    (← roleDeclarations shippedRoots [.spec, .library]).forM (fun (name : Name) => IO.println name)
    pure 0
  | "executable-declarations" =>
    (← roleDeclarations shippedRoots [.executable]).forM (fun (name : Name) => IO.println name)
    pure 0
  | other =>
    IO.eprintln s!"unknown audit check `{other}`"
    pure 2

/-- Every check the full audit runs, in order. `source` runs earlier, in the build. -/
def allChecks : List String :=
  ["self-test", "format", "manifest", "environment", "test-environment"]

/-- Runs one check in a child process of this executable and returns its exit code. -/
def runChild (check : String) : IO UInt32 := do
  let child : IO.Process.Child {} ←
    IO.Process.spawn { cmd := (← IO.appPath).toString, args := #[check] }
  child.wait

/-- Runs one requested check, or every check in its own process. -/
unsafe def main (args : List String) : IO UInt32 := do
  initSearchPath (← findSysroot)
  enableInitializersExecution
  match args with
  | [] =>
    let codes : List UInt32 ← allChecks.mapM runChild
    pure (codes.foldl max 0)
  | [check] => runCheck enableInitializersExecution check
  | ["self-test-case", index] =>
    match index.toNat? with
    | some number => selfTestCase enableInitializersExecution number
    | none =>
      IO.eprintln s!"self-test case `{index}` is not a number"
      pure 2
  | _ =>
    IO.eprintln "usage: speakeasy_audit [check]"
    pure 2
