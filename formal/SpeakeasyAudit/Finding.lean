import Lean

/-!
# Audit findings

A finding is one policy violation with the place it was found and how to fix it.
-/

namespace SpeakeasyAudit

/-- One policy violation. -/
structure Finding where
  /-- Where the violation is: a file and line, a declaration, or a manifest entry. -/
  location : String
  /-- What is wrong, phrased as the rule it breaks, then how to fix it. -/
  message : String

/-- Renders a finding as `location: message`. -/
def Finding.render (finding : Finding) : String :=
  s!"{finding.location}: {finding.message}"

/-- Prints every finding and returns the process exit code: 0 when there are none. -/
def report (check : String) (findings : Array Finding) : IO UInt32 := do
  if findings.isEmpty then
    IO.println s!"audit {check}: ok"
    pure 0
  else
    findings.forM (fun (finding : Finding) => IO.eprintln finding.render)
    IO.eprintln s!"audit {check}: {findings.size} finding(s)"
    pure 1

end SpeakeasyAudit
