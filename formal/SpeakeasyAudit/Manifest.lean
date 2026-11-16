import Lean
import SpeakeasyAudit.Finding
import SpeakeasyAudit.Policy

/-!
# Manifest audit

Checks that the build depends only on the pinned toolchain and the pinned Lake
manifest: the toolchain file names the running compiler, every dependency is an
allowed package pinned to a full commit, every checkout is at that commit with
no local edits, and no linter exceptions are recorded.
-/

namespace SpeakeasyAudit

open Lean System

/-- Whether a string is a full lowercase 40-digit hexadecimal commit hash. -/
def isCommitHash (value : String) : Bool :=
  value.length == 40 && value.all (fun (char : Char) => char.isDigit || ('a' ≤ char && char ≤ 'f'))

/-- Runs a command and returns its trimmed standard output, or `none` if it fails. -/
def commandOutput (command : String) (args : Array String) : IO (Option String) := do
  let output : IO.Process.Output ← IO.Process.output { cmd := command, args }
  pure (if output.exitCode == 0 then some output.stdout.trimAscii.toString else none)

/-- The checks for one manifest package entry and its checkout. -/
def packageFindings (entry : Json) : IO (Array Finding) := do
  let name : String := (entry.getObjValAs? String "name").toOption.getD "<unnamed>"
  let at_ : String := s!"lake-manifest.json:{name}"
  let field (key : String) : String := (entry.getObjValAs? String key).toOption.getD ""
  let mut findings : Array Finding := #[]
  if !allowedPackages.contains name then
    findings := findings.push ⟨at_, "package is not in the audit allowlist; remove the " ++
      "dependency, or add it to `allowedPackages` in Policy.lean"⟩
  if field "type" != "git" then
    findings := findings.push
      ⟨at_, "package is not a git dependency; require it from its git repository"⟩
  if !(field "url").startsWith "https://github.com/" then
    findings := findings.push
      ⟨at_, "package URL is not an https GitHub URL; require it from https://github.com/"⟩
  if !isCommitHash (field "rev") then
    findings := findings.push ⟨at_, "package rev is not a full commit hash; set its `rev` in " ++
      "lakefile.toml to a full commit and run `lake update`"⟩
  if field "inputRev" != field "rev" then
    findings := findings.push ⟨at_, "lakefile rev is not the pinned commit; set its `rev` in " ++
      "lakefile.toml to the full commit and run `lake update`"⟩
  let checkout : String := s!".lake/packages/{name}"
  match ← commandOutput "git" #["-C", checkout, "rev-parse", "HEAD"] with
  | some head =>
    if head != field "rev" then
      findings := findings.push ⟨at_, s!"checkout is at {head}, not the pinned commit; " ++
        s!"delete {checkout} and run `lake build` once online"⟩
  | none =>
    findings := findings.push ⟨at_, "checkout is missing; run `lake build` once online"⟩
  match ← commandOutput "git" #["-C", checkout, "status", "--porcelain"] with
  | some "" => pure ()
  | some _ =>
    findings := findings.push
      ⟨at_, s!"checkout has local changes; delete {checkout} and run `lake build` once online"⟩
  | none => pure ()
  pure findings

/-- Every manifest and toolchain rule. -/
def manifestAudit : IO (Array Finding) := do
  let mut findings : Array Finding := #[]
  let expected : String := s!"leanprover/lean4:v{Lean.versionString}\n"
  if (← IO.FS.readFile "lean-toolchain") != expected then
    findings := findings.push ⟨"lean-toolchain", s!"must be exactly {expected.trimAscii}"⟩
  if ← FilePath.pathExists "scripts/nolints.json" then
    findings := findings.push ⟨"scripts/nolints.json",
      "linter exceptions are not allowed; fix what the linters report and delete the file"⟩
  match Json.parse (← IO.FS.readFile "lake-manifest.json") with
  | .error message =>
    findings := findings.push
      ⟨"lake-manifest.json", s!"does not parse: {message}; regenerate it with `lake update`"⟩
  | .ok manifest =>
    let packages : Array Json := (manifest.getObjValAs? (Array Json) "packages").toOption.getD #[]
    let names : Array String := packages.map (fun (entry : Json) =>
      (entry.getObjValAs? String "name").toOption.getD "")
    for package in allowedPackages do
      if !names.contains package then
        findings := findings.push ⟨"lake-manifest.json",
          s!"allowed package {package} is unused; remove it from `allowedPackages` in Policy.lean"⟩
    for entry in packages do
      findings := findings ++ (← packageFindings entry)
  pure findings

end SpeakeasyAudit
