import SpeakeasyAudit.Environment
import SpeakeasyAudit.Source

/-!
# Audit self-test

Proves every audit rule fires. Each case is a small specification module and a
library module that imports it, and together they break one rule; the audit
must report a finding containing the expected text. Control cases must produce
no finding at all, so the rules also prove they do not fire on compliant code.
Each case runs in its own process, since every import of a module in one
process keeps another copy of it in memory. Its modules are written to a
temporary directory, where the library module imports the specification
module's `.olean`, so the self-test needs none of the project's build output.

A probe also runs the `source` check on a module whose `#eval` would end the
process with a success code, which passes only if the check never runs it.
-/

namespace SpeakeasyAudit

open Lean System

/-- A pair of sources that must produce a finding containing `expected`, or none when
`expected` is empty. -/
structure SelfTestCase where
  /-- What the case breaks. -/
  name : String
  /-- The complete source of the specification module, `Speakeasy.Spec.SelfTest`. -/
  spec : String
  /-- The complete source of the library module, `Speakeasy.SelfTest`. -/
  source : String
  /-- Text the findings must contain; empty for a control. -/
  expected : String

/-- Joins source lines, each ending in a newline. -/
def lines (parts : List String) : String :=
  String.join (parts.map (fun (part : String) => part ++ "\n"))

/-- A compliant contract with its uniqueness theorem, reused by cases that break the library. -/
def cleanSpec : String :=
  lines
    ["/-! Clean specification. -/",
     "",
     "namespace SelfTest",
     "",
     "/-- `double` adds a count to itself. -/",
     "def DoubleSpec (double : Nat → Nat) : Prop :=",
     "  ∀ (count : Nat), double count = count + count",
     "",
     "/-- At most one function meets `DoubleSpec`. -/",
     "theorem DoubleSpec.unique (first : Nat → Nat) (second : Nat → Nat)",
     "    (hFirst : DoubleSpec first) (hSecond : DoubleSpec second) : first = second := by",
     "  funext (count : Nat)",
     "  exact (hFirst count).trans (hSecond count).symm",
     "",
     "end SelfTest"]

/-- A compliant definition that meets its contract through a `where` helper, which needs no
contract of its own, reused by cases that break one thing. -/
def cleanSource : String :=
  lines
    ["import Speakeasy.Spec.SelfTest",
     "",
     "/-! Clean. -/",
     "",
     "namespace SelfTest",
     "",
     "/-- Doubles. -/",
     "def double (count : Nat) : Nat :=",
     "  sum count count",
     "where",
     "  /-- Adds two counts. -/",
     "  sum (count : Nat) (other : Nat) : Nat :=",
     "    count + other",
     "",
     "/-- `double` meets its contract. -/",
     "theorem double_spec : DoubleSpec double := by",
     "  intro (count : Nat)",
     "  rfl",
     "",
     "end SelfTest"]

/-- Inserts a declaration into a clean source, before its namespace closes. -/
def withDecl (decl : String) (source : String := cleanSource) : String :=
  source.replace "end SelfTest\n" (decl ++ "\nend SelfTest\n")

/-- Cases that break only the library module, as name, source, and expected text. -/
def libraryCases : List (String × String × String) :=
  [⟨"clean control", cleanSource, ""⟩,
    ⟨"sorry", withDecl (lines
        ["/-- Unproved. -/",
         "theorem unproved : double 1 = 2 := by",
         "  sorry"]),
      "banned keyword `sorry`"⟩,
    ⟨"sorry axiom", withDecl (lines
        ["/-- Unproved. -/",
         "theorem unproved : double 1 = 2 := by",
         "  sorry"]),
      "depends on axioms [sorryAx]"⟩,
    ⟨"axiom", withDecl (lines
        ["/-- Assumed. -/",
         "axiom assumed : double 1 = 3"]), "declares an axiom"⟩,
    ⟨"native_decide", withDecl (lines
        ["/-- Native. -/",
         "theorem native : double 1 = 2 := by",
         "  native_decide"]),
      "native_decide.ax"⟩,
    ⟨"decide +native", withDecl (lines
        ["/-- Native. -/",
         "theorem native : double 1 = 2 := by",
         "  decide +native"]),
      "native evaluation bypasses the kernel"⟩,
    ⟨"Classical.choice", withDecl (lines
        ["/-- Classical. -/",
         "theorem classical (count : Nat) :",
         "    double count = 0 ∨ ¬double count = 0 :=",
         "  Classical.em (double count = 0)"]),
      "depends on axioms [Classical.choice"⟩,
    ⟨"partial", withDecl (lines
        ["/-- Loops. -/",
         "partial def spin (count : Nat) : Nat :=",
         "  spin count"]),
      "declares an opaque constant"⟩,
    ⟨"unsafe", withDecl (lines
        ["/-- Unsafe. -/",
         "unsafe def raw (count : Nat) : Nat :=",
         "  count"]),
      "is unsafe"⟩,
    ⟨"noncomputable", withDecl (lines
        ["/-- Chosen. -/",
         "noncomputable def chosen (count : Nat) : Nat :=",
         "  Classical.choose ⟨count, rfl⟩"]),
      "is noncomputable"⟩,
    ⟨"implemented_by", withDecl (lines
        ["/-- Replaced. -/",
         "@[implemented_by double]",
         "def slow (count : Nat) : Nat :=",
         "  2 * count"]),
      "has its code replaced"⟩,
    ⟨"extern", withDecl (lines
        ["/-- External. -/",
         "@[extern \"c_double\"]",
         "def external (count : Nat) : Nat :=",
         "  count"]),
      "has an external implementation"⟩,
    ⟨"missing contract", withDecl (lines
        ["/-- Triples. -/",
         "def triple (count : Nat) : Nat :=",
         "  count * 3",
         "",
         "/-- A statement that mentions `triple` but says nothing about it. -/",
         "theorem triple_self : triple 1 = triple 1 := by",
         "  rfl"]),
      "SelfTest.triple: meets no contract"⟩,
    ⟨"missing theorem docstring", withDecl (lines
        ["theorem double_zero : double 0 = 0 := by",
         "  rfl"]), "has no docstring"⟩,
    ⟨"simp without only", withDecl (lines
        ["/-- A fact. -/",
         "theorem double_zero : double 0 = 0 := by",
         "  simp [double]"]),
      "simplifier call without `only`"⟩,
    ⟨"simp?", withDecl (lines
        ["/-- A fact. -/",
         "theorem double_zero : double 0 = 0 := by",
         "  simp? [double]"]),
      "banned keyword `simp?`"⟩,
    ⟨"untyped binder", withDecl (lines
        ["/-- A fact. -/",
         "theorem double_comm (count) : double count = count + count := by",
         "  rfl"]),
      "binder without a type"⟩,
    ⟨"untyped fun", withDecl (lines
        ["/-- A fact. -/",
         "theorem double_fun : (fun count => double count) 1 = 2 := by",
         "  rfl"]),
      "fun binder without a type"⟩,
    ⟨"untyped let", withDecl (lines
        ["/-- Quadruples. -/",
         "def quadruple (count : Nat) : Nat :=",
         "  let twice := double count",
         "  double twice"]),
      "let or have without a type"⟩,
    ⟨"untyped intro", withDecl (lines
        ["/-- A fact. -/",
         "theorem double_all : ∀ (count : Nat), double count = count + count := by",
         "  intro count",
         "  rfl"]),
      "intro without a type"⟩,
    ⟨"missing result type", withDecl (lines
        ["/-- Quadruples. -/",
         "def quadruple (count : Nat) :=",
         "  double (double count)"]),
      "definition without a result type"⟩,
    ⟨"set_option", withDecl (lines
        ["set_option maxHeartbeats 400000 in",
         "/-- A fact. -/",
         "theorem double_zero : double 0 = 0 := by",
         "  rfl"]),
      "banned keyword `set_option`"⟩,
    ⟨"#eval", withDecl (lines
        ["#eval double 2"]), "banned keyword `#eval`"⟩,
    ⟨"# command", withDecl (lines
        ["#version"]), "banned keyword `#version`"⟩,
    ⟨"private definition control", withDecl (lines
        ["/-- Triples, for use inside this module only. -/",
         "private def triple (count : Nat) : Nat :=",
         "  count * 3"]), ""⟩,
    ⟨"scoped syntax control", withDecl (lines
        ["open BitVec",
         "",
         "example : (1#'(by decide) : BitVec 1).toNat = 1 := by",
         "  decide"]), ""⟩,
    ⟨"array literal control", withDecl (lines
        ["example : (#[1, 2] : Array Nat).size = 2 := by",
         "  decide"]), ""⟩,
    ⟨"run_tac", withDecl (lines
        ["/-- A fact. -/",
         "theorem double_zero : double 0 = 0 := by",
         "  run_tac pure ()",
         "  rfl"]),
      "banned keyword `run_tac`"⟩,
    ⟨"builtin_ command", withDecl (lines
        ["builtin_initialize started : Nat ← pure 0"]),
      "banned keyword `builtin_initialize`"⟩,
    ⟨"register_ command", withDecl (lines
        ["register_try?_tactic rfl"]),
      "banned keyword `register_try?_tactic`"⟩,
    ⟨"bv_decide", withDecl (lines
        ["/-- A fact. -/",
         "theorem bits : (1 : BitVec 2) + 1 = 2 := by",
         "  bv_decide"]),
      "banned keyword `bv_decide`"⟩,
    ⟨"exact?", withDecl (lines
        ["/-- A fact. -/",
         "theorem double_zero : double 0 = 0 := by",
         "  exact?"]),
      "banned keyword `exact?`"⟩,
    ⟨"macro", withDecl (lines
        ["/-- Shorthand. -/",
         "macro \"twice\" : term => `(2)"]),
      "banned keyword `macro`"⟩,
    ⟨"export", withDecl (lines
        ["/-- Exported. -/",
         "@[export project_double]",
         "def exported (count : Nat) : Nat :=",
         "  count"]),
      "banned attribute `export`"⟩,
    ⟨"init", withDecl (lines
        ["/-- Startup. -/",
         "def startup : IO Unit :=",
         "  pure ()",
         "",
         "/-- Initialized. -/",
         "@[init startup]",
         "def initialized : Nat :=",
         "  0"]),
      "banned attribute `init`"⟩,
    ⟨"builtin_ attribute", withDecl (lines
        ["/-- Startup. -/",
         "def startup : IO Unit :=",
         "  pure ()",
         "",
         "/-- Initialized. -/",
         "@[builtin_init startup]",
         "def initialized : Nat :=",
         "  0"]),
      "banned attribute `builtin_init`"⟩,
    ⟨"elaborator attribute", withDecl (lines
        ["/-- Elaborates. -/",
         "@[command_elab Lean.Parser.Command.check]",
         "def elaborate (count : Nat) : Nat :=",
         "  count"]),
      "banned attribute `command_elab`"⟩,
    ⟨"parser attribute", withDecl (lines
        ["/-- Parses. -/",
         "@[term_parser]",
         "def parse (count : Nat) : Nat :=",
         "  count"]),
      "banned attribute `term_parser`"⟩,
    ⟨"nolint", withDecl (lines
        ["/-- Exempted. -/",
         "@[nolint docBlame]",
         "def exempted (count : Nat) : Nat :=",
         "  count"]),
      "banned attribute `nolint`"⟩,
    ⟨"trailing whitespace", cleanSource.replace "namespace SelfTest\n" "namespace SelfTest \n",
      "trailing whitespace"⟩,
    ⟨"tab", cleanSource.replace "  count + other\n" "\tcount + other\n", "tab character"⟩,
    ⟨"carriage return", cleanSource.replace "namespace SelfTest\n" "namespace SelfTest\r\n",
      "carriage return"⟩,
    ⟨"bidirectional control", cleanSource.replace "/-- Doubles. -/"
      ("/-- Doubles." ++ (Char.ofNat 0x202E).toString ++ " -/"),
      "invisible or bidirectional control character"⟩,
    ⟨"long line", cleanSource.replace "/-- Doubles. -/"
      ("/-- " ++ "".pushn 'x' 100 ++ " -/"), "line longer than"⟩,
    ⟨"final newline", cleanSource ++ "\n", "file must end with exactly one newline"⟩]

/-- A contract and uniqueness theorem for `triple`, inserted into the clean specification. -/
def tripleSpec : String :=
  withDecl (lines
    ["/-- `triple` adds a count to itself twice. -/",
     "def TripleSpec (triple : Nat → Nat) : Prop :=",
     "  ∀ (count : Nat), triple count = count + count + count",
     "",
     "/-- At most one function meets `TripleSpec`. -/",
     "theorem TripleSpec.unique (first : Nat → Nat) (second : Nat → Nat)",
     "    (hFirst : TripleSpec first) (hSecond : TripleSpec second) : first = second := by",
     "  funext (count : Nat)",
     "  exact (hFirst count).trans (hSecond count).symm"]) cleanSpec

/-- Cases that need their own specification module. -/
def specCases : List SelfTestCase :=
  [⟨"hypothesis in a contract theorem", tripleSpec, withDecl (lines
      ["/-- Triples. -/",
       "def triple (count : Nat) : Nat :=",
       "  count * 3",
       "",
       "/-- Meets the contract only under an impossible hypothesis. -/",
       "theorem triple_spec (impossible : False) : TripleSpec triple :=",
       "  impossible.elim"]),
      "SelfTest.triple: meets no contract"⟩,
    ⟨"contract outside a specification module", cleanSpec, withDecl (lines
      ["/-- Triples. -/",
       "def triple (count : Nat) : Nat :=",
       "  count * 3",
       "",
       "/-- A contract the library states for itself. -/",
       "def TripleSpec (triple : Nat → Nat) : Prop :=",
       "  triple 0 = 0",
       "",
       "/-- `triple` meets its own contract. -/",
       "theorem triple_spec : TripleSpec triple := by",
       "  rfl"]),
      "SelfTest.triple: meets no contract"⟩,
    ⟨"weak uniqueness theorem", cleanSpec.replace "    (hFirst : DoubleSpec first)"
        "    (same : first 0 = second 0)\n    (hFirst : DoubleSpec first)", cleanSource,
      "SelfTest.DoubleSpec: contract has no uniqueness theorem"⟩,
    ⟨"specification imports the implementation",
      "import SpeakeasyAudit.Finding\n\n" ++ cleanSpec, cleanSource,
      "imports SpeakeasyAudit.Finding"⟩,
    ⟨"well-founded recursion control", withDecl (lines
      ["/-- `countdown` always reaches zero. -/",
       "def CountdownSpec (countdown : Nat → Nat) : Prop :=",
       "  ∀ (count : Nat), countdown count = 0",
       "",
       "/-- At most one function meets `CountdownSpec`. -/",
       "theorem CountdownSpec.unique (first : Nat → Nat) (second : Nat → Nat)",
       "    (hFirst : CountdownSpec first) (hSecond : CountdownSpec second) :",
       "    first = second := by",
       "  funext (count : Nat)",
       "  exact (hFirst count).trans (hSecond count).symm"]) cleanSpec, withDecl (lines
      ["/-- Counts down to zero. -/",
       "def countdown (count : Nat) : Nat :=",
       "  if count = 0 then 0 else countdown (count - 1)",
       "termination_by count",
       "decreasing_by omega",
       "",
       "/-- `countdown` meets its contract. -/",
       "theorem countdown_spec : CountdownSpec countdown := by",
       "  intro (count : Nat)",
       "  induction count with",
       "  | zero =>",
       "    rw [countdown]",
       "    rfl",
       "  | succ previous ih =>",
       "    rw [countdown]",
       "    simp only [Nat.add_one_ne_zero, if_false, Nat.add_sub_cancel]",
       "    exact ih"]),
      ""⟩]

/-- Every self-test case. -/
def selfTestCases : List SelfTestCase :=
  libraryCases.map (fun (entry : String × String × String) =>
    ⟨entry.1, cleanSpec, entry.2.1, entry.2.2⟩) ++ specCases

/-- The result of elaborating one file: its messages and the environment it produced. -/
structure Elaborated where
  /-- Every message elaboration logged. -/
  messages : MessageLog
  /-- The environment after the last command. -/
  env : Environment

/-- Elaborates one file as module `module`. `allowInitializers` must re-enable initializer
execution: each import consumes it. -/
def elaborateFile (allowInitializers : BaseIO Unit) (path : FilePath) (text : String)
    (module : Name) : IO Elaborated := do
  allowInitializers
  let inputCtx : Parser.InputContext := Parser.mkInputContext text path.toString
  let (header, parserState, messages) ← Parser.parseHeader inputCtx
  let (env, messages) ←
    Elab.processHeader header {} messages inputCtx (trustLevel := 1024) (mainModule := module)
  let state : Elab.Frontend.State ←
    Elab.IO.processCommands inputCtx parserState (Elab.Command.mkState env messages {})
  pure (Elaborated.mk state.commandState.messages state.commandState.env)

/-- Findings for the errors elaboration logged, so a control that fails to elaborate fails. -/
def elaborationFindings (path : FilePath) (elaborated : Elaborated) : IO (Array Finding) := do
  let mut findings : Array Finding := #[]
  for message in elaborated.messages.toList do
    if message.severity == .error then
      findings := findings.push
        ⟨s!"{path}:{message.pos.line}", s!"does not elaborate: {← message.data.toString}"⟩
  pure findings

/-- Every finding the audit reports for one case: the text and syntax rules on both modules,
and the environment rules on the library module. Both modules are written to a temporary
directory, where the library module imports the specification module's `.olean`. -/
def caseFindings (allowInitializers : BaseIO Unit) (testCase : SelfTestCase) :
    IO (Array Finding) := do
  let directory : FilePath ← IO.FS.createTempDir
  try
    let specPath : FilePath := directory / "Speakeasy" / "Spec" / "SelfTest.lean"
    let path : FilePath := directory / "Speakeasy" / "SelfTest.lean"
    IO.FS.createDirAll (directory / "Speakeasy" / "Spec")
    IO.FS.writeFile specPath testCase.spec
    IO.FS.writeFile path testCase.source
    let mut findings : Array Finding :=
      textFindings specPath testCase.spec ++ textFindings path testCase.source
    findings := findings ++ (← syntaxAudit allowInitializers #[(specPath, .spec), (path, .library)])
    let spec : Elaborated ←
      elaborateFile allowInitializers specPath testCase.spec `Speakeasy.Spec.SelfTest
    findings := findings ++ (← elaborationFindings specPath spec)
    writeModule spec.env (specPath.withExtension "olean")
    searchPathRef.modify (fun (paths : System.SearchPath) => directory :: paths)
    let library : Elaborated ←
      elaborateFile allowInitializers path testCase.source `Speakeasy.SelfTest
    findings := findings ++ (← elaborationFindings path library)
    pure (findings ++ (← declarationFindings library.env (ownedDecls library.env)))
  finally
    IO.FS.removeDirAll directory

/-- Runs the case at `index` and prints every finding it produces. -/
def selfTestCase (allowInitializers : BaseIO Unit) (index : Nat) : IO UInt32 := do
  match selfTestCases[index]? with
  | some testCase =>
    (← caseFindings allowInitializers testCase).forM (fun (finding : Finding) =>
      IO.println finding.render)
    pure 0
  | none =>
    IO.eprintln s!"no self-test case {index}"
    pure 2

/-- Runs the `source` check, in a child process, on a project whose only module has an `#eval`
that ends the process with a success code. The check passes only if it parses the module without
running it, and so reports the `#eval`. -/
def sourceProbe : IO (Array Finding) := do
  let directory : FilePath ← IO.FS.createTempDir
  try
    IO.FS.createDirAll (directory / "Speakeasy")
    IO.FS.writeFile (directory / "Speakeasy" / "Probe.lean")
      "#eval (IO.Process.exit 0 : IO Unit)\n"
    let output : IO.Process.Output ← IO.Process.output
      { cmd := (← IO.appPath).toString, args := #["source"], cwd := some directory }
    if output.exitCode == 1 && (output.stderr.splitOn "banned keyword `#eval`").length > 1 then
      pure #[]
    else
      pure #[⟨"self-test source probe", s!"`source` exited with {output.exitCode} without " ++
        "reporting the `#eval`, so it ran the module before checking it"⟩]
  finally
    IO.FS.removeDirAll directory

/-- Runs every case in a child process of this executable and reports each one whose findings
do not match its expectation, then runs the source probe. -/
def selfTest : IO (Array Finding) := do
  let mut failures : Array Finding ← sourceProbe
  for (testCase, index) in selfTestCases.zipIdx do
    let output : IO.Process.Output ← IO.Process.output
      { cmd := (← IO.appPath).toString, args := #["self-test-case", toString index] }
    let found : String := output.stdout
    let location : String := s!"self-test {testCase.name}"
    if output.exitCode != 0 then
      failures := failures.push
        ⟨location, s!"case exited with {output.exitCode}: {output.stderr.trimAscii}"⟩
    else if testCase.expected.isEmpty then
      for line in found.splitOn "\n" do
        if !line.isEmpty then
          failures := failures.push ⟨location, s!"control case reported {line}"⟩
    else if (found.splitOn testCase.expected).length == 1 then
      failures := failures.push ⟨location, s!"expected a finding with `{testCase.expected}`"⟩
  pure failures

end SpeakeasyAudit
