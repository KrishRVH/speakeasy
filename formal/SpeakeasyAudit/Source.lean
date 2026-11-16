import SpeakeasyAudit.Finding
import SpeakeasyAudit.Policy

/-!
# Source audit

Checks every owned `.lean` file: the text rules that stand in for a formatter,
and syntax rules the compiler cannot express. The syntax rules run before any
owned code is compiled, and they parse without elaborating: a banned command
such as `#eval` would otherwise run, inside the build or inside this audit,
before the rule that bans it. Each file is parsed with the syntax of its
dependency imports and the scope its `namespace`, `section`, and `open` commands
set, as the compiler parses it. Owned imports are skipped: owned modules may not
be built yet, and they cannot define syntax.
-/

namespace SpeakeasyAudit

open Lean System

/-- Keywords no owned file may contain, grouped by what each one does, each group with the
remedy its findings state. `bannedAtomRemedy?` also bans every `#` command, every `builtin_` and
`register_` command, and `unsafe` outside the audit tool. -/
def bannedAtoms : List (String × List String) :=
  [ -- Bypasses the kernel or hides code from it.
    ("finish the proof", ["sorry", "admit"]),
    ("use `decide` or another proof the kernel checks", ["native_decide", "bv_decide", "bv_check"]),
    ("state it as a theorem and prove it", ["axiom"]),
    ("write a total definition that recurses structurally or proves termination with " ++
      "`termination_by`", ["opaque", "partial"]),
    ("give it a computable definition", ["noncomputable"]),
    ("define it in Lean", ["extern", "test_extern", "init_quot"]),
    -- Changes options that belong in the Lake configuration.
    ("set options in `lakefile.toml`, and split a slow proof into lemmas",
      ["set_option", "unlock_limits"]),
    -- Runs code during elaboration, or defines syntax, macros, elaborators, simplification
    -- procedures, or initializers.
    ("write plain definitions and theorems; owned code defines no syntax and runs nothing " ++
      "while it compiles",
      ["run_cmd", "run_elab", "run_meta", "run_tac", "set_library_suggestions", "macro",
        "macro_rules", "syntax", "declare_syntax_cat", "elab", "elab_rules", "elab_stx_quot",
        "declare_config_elab", "declare_config_elab_legacy", "declare_command_config_elab",
        "declare_command_config_elab_legacy", "declare_core_config_elab",
        "declare_term_config_elab", "declare_simp_like_tactic", "binder_predicate", "notation",
        "infix", "infixl", "infixr", "prefix", "postfix", "initialize", "simproc",
        "simproc_decl", "dsimproc", "dsimproc_decl", "cbv_simproc", "cbv_simproc_decl",
        "simproc_pattern%", "cbv_simproc_pattern%", "grind_propagator", "declare_sint_simprocs",
        "declare_uint_simprocs"]),
    -- Leaves an unpinned search behind.
    ("run it in your editor, then write the call it suggests",
      ["simp?", "simp?!", "simp_all?", "simp_all?!", "dsimp?", "dsimp?!", "simpa?", "simpa?!",
        "exact?", "apply?", "rw?", "try?", "grind?", "bv_decide?", "mvcgen?"])]

/-- Whether a keyword is a `#` command, such as `#eval` or `#print`. The literal openers `#[`
and `#v[` are not commands. -/
def isHashCommand (value : String) : Bool :=
  match value.toList with
  | '#' :: first :: rest =>
    first.isAlpha &&
      rest.all (fun (char : Char) => char.isAlphanum || char == '_' || char == '!' || char == '?')
  | _ => false

/-- The remedy for a keyword an owned file of the given role may not contain, or `none` if it
may. `builtin_` commands belong to Lean itself, and every `register_` command declares an
initializer. The audit tool may use `unsafe`, because it must call Lean's importer. -/
def bannedAtomRemedy? (role : Role) (value : String) : Option String :=
  match bannedAtoms.find? (fun (group : String × List String) => group.2.contains value) with
  | some group => some group.1
  | none =>
    if isHashCommand value then
      some "check concrete cases with `example ... := by decide` or in the test executable"
    else if value.startsWith "builtin_" || value.startsWith "register_" then
      some "owned code runs nothing when it is imported"
    else if value == "unsafe" && role != .audit then some "write it in safe Lean"
    else none

/-- Attributes no owned declaration may carry, grouped by what each one does, each group with
the remedy its findings state. `bannedAttributeRemedy?` also bans every `builtin_` attribute and
every parser attribute. -/
def bannedAttributes : List (String × List Name) :=
  [("remove it, so the kernel checks the code that runs",
      [`implemented_by, `export, `extern, `never_extract, `csimp, `cpass]),
    -- The audit itself imports the module, which would run an initializer.
    ("remove it: owned code runs nothing when it is imported", [`init]),
    ("remove it: owned code registers nothing the elaborator, simplifier, or pretty printer runs",
      [`command_elab, `term_elab, `tactic, `macro, `doElem_elab, `doElem_control_info,
        `inductive_elab, `grind_tactic, `try_tactic, `try_suggestion, `library_suggestions,
        `sym_discharger, `sym_simproc, `sym_dsimproc, `frameproc, `simprocAttr, `sevalprocAttr,
        `cbvSimprocAttr, `simprocBuiltinAttr, `sevalprocBuiltinAttr, `cbvSimprocBuiltinAttr,
        `grindPropagatorBuiltinAttr, `delab, `app_unexpander, `formatter, `parenthesizer,
        `combinator_formatter, `combinator_parenthesizer, `category_parenthesizer,
        `quot_precheck, `run_parser_attribute_hooks, `run_builtin_parser_attribute_hooks,
        `doc_role, `doc_directive, `doc_code_block, `doc_command, `doc_block_md, `doc_inline_md,
        `doc_code_block_suggestions, `doc_code_suggestions, `deferred_doc_check]),
    -- The audit's own linter pass runs registered linters.
    ("remove it: owned code registers no linters",
      [`env_linter, `missing_docs_handler, `unused_variables_ignore_fn,
        `package_code_quality_check]),
    ("fix what the linter reports instead of exempting the declaration", [`nolint]),
    ("remove it: owned code registers nothing the language server runs",
      [`code_action_provider, `command_code_action, `hole_code_action, `server_rpc_method,
        `widget_module])]

/-- The remedy for an attribute no owned declaration may carry, or `none` if it may. `builtin_`
attributes belong to Lean itself, and every parser attribute registers code the parser runs. -/
def bannedAttributeRemedy? (name : Name) : Option String :=
  match bannedAttributes.find? (fun (group : String × List Name) => group.2.contains name) with
  | some group => some group.1
  | none =>
    if name.toString.startsWith "builtin_" || name.toString.endsWith "_parser" then
      some "remove it: owned code registers nothing the parser or elaborator runs"
    else none

/-- The attribute an attribute instance applies, named as Lean's elaborator names it: the
identifier of a simple attribute, otherwise the last component of its syntax kind. -/
def attributeName? (stx : Syntax) : Option Name :=
  if stx.isOfKind ``Lean.Parser.Term.attrInstance then
    let attr : Syntax := stx[1]
    if attr.isOfKind ``Lean.Parser.Attr.simple then some attr[0].getId.eraseMacroScopes
    else
      match attr.getKind with
      | .str _ last => some (Name.mkSimple last)
      | _ => none
  else none

/-- Simplifier tactics that must name their lemmas with `only`. -/
def onlyRequired : List SyntaxNodeKind :=
  [``Lean.Parser.Tactic.simp, ``Lean.Parser.Tactic.simpAll, ``Lean.Parser.Tactic.dsimp,
    ``Lean.Parser.Tactic.simpa, ``Lean.Parser.Tactic.simpaUsingBang,
    ``Lean.Parser.Tactic.simpArith, ``Lean.Parser.Tactic.simpAllArith,
    ``Lean.Parser.Tactic.simpAutoUnfold, ``Lean.Parser.Tactic.dsimpAutoUnfold]

/-- Characters that render invisibly or reorder text, so source can read differently from
what the compiler sees. -/
def invisibleChars : List Char :=
  [0x00A0, 0x00AD, 0x200B, 0x200C, 0x200D, 0x200E, 0x200F, 0x2028, 0x2029, 0x202A, 0x202B,
    0x202C, 0x202D, 0x202E, 0x2060, 0x2066, 0x2067, 0x2068, 0x2069, 0xFEFF].map Char.ofNat

/-- The module name a source path compiles to. -/
def moduleOf (path : FilePath) : Name :=
  (path.withExtension "").components.foldl
    (fun (name : Name) (component : String) =>
      if component == "." then name else Name.str name component)
    Name.anonymous

/-- Every `.lean` file under the project, skipping hidden directories such as `.lake`. -/
def sourceFiles : IO (Array FilePath) := do
  let files : Array FilePath ← FilePath.walkDir "."
    (fun (dir : FilePath) => pure !((dir.fileName.getD "").startsWith "."))
  pure ((files.filter (fun (file : FilePath) => file.extension == some "lean")).qsort
    (fun (left : FilePath) (right : FilePath) => left.toString < right.toString))

/-- Text rules: LF line endings, no tabs, no trailing whitespace, no invisible characters,
bounded line length, and exactly one final newline. -/
def textFindings (path : FilePath) (text : String) : Array Finding := Id.run do
  let mut findings : Array Finding := #[]
  let lines : List String := text.splitOn "\n"
  let mut number : Nat := 0
  for line in lines do
    number := number + 1
    let at_ : String := s!"{path}:{number}"
    if line.contains '\r' then
      findings := findings.push ⟨at_, "carriage return; use LF line endings"⟩
    if line.contains '\t' then
      findings := findings.push ⟨at_, "tab character; indent with spaces"⟩
    if line.endsWith " " then
      findings := findings.push ⟨at_, "trailing whitespace; delete it"⟩
    if line.any (fun (char : Char) => invisibleChars.contains char) then
      findings := findings.push
        ⟨at_, "invisible or bidirectional control character; delete it, or escape it in a string"⟩
    if line.length > maxLineLength then
      findings := findings.push ⟨at_, s!"line longer than {maxLineLength} characters; wrap it"⟩
  if !text.endsWith "\n" || text.endsWith "\n\n" then
    findings := findings.push ⟨s!"{path}", "file must end with exactly one newline"⟩
  pure findings

/-- Whether a simplifier call names its lemmas with `only`, searching its own arguments but
not nested tactic blocks. -/
def hasOnly (stx : Syntax) : Bool :=
  let level1 : Array Syntax := stx.getArgs
  let level2 : Array Syntax := level1.flatMap (fun (child : Syntax) => child.getArgs)
  let level3 : Array Syntax := level2.flatMap (fun (child : Syntax) => child.getArgs)
  (level1 ++ level2 ++ level3).any (fun (child : Syntax) => child.isToken "only")

/-- Whether a binder syntax carries its type. Bare names, holes, and anonymous constructor
patterns do not. -/
def isTypedBinder (binder : Syntax) : Bool :=
  binder.isOfKind ``Lean.Parser.Term.typeAscription ||
    binder.isOfKind ``Lean.Parser.Term.instBinder ||
    (binder.isOfKind ``Lean.Parser.Term.explicitBinder && binder[2].getNumArgs > 0) ||
    (binder.isOfKind ``Lean.Parser.Term.implicitBinder && binder[2].getNumArgs > 0) ||
    (binder.isOfKind ``Lean.Parser.Term.strictImplicitBinder && binder[2].getNumArgs > 0)

/-- The binder rule for one syntax node: every binder that can carry a type does. -/
def binderMessage (stx : Syntax) : Option String :=
  let kind : SyntaxNodeKind := stx.getKind
  if kind == ``Lean.Parser.Term.explicitBinder || kind == ``Lean.Parser.Term.implicitBinder ||
      kind == ``Lean.Parser.Term.strictImplicitBinder then
    if stx[2].getNumArgs == 0 then some "binder without a type; write `(name : Type)`" else none
  else if kind == ``Lean.Parser.Command.optDeclSig || kind == ``Lean.Parser.Command.declSig then
    if stx[0].getArgs.any (fun (binder : Syntax) => !isTypedBinder binder) then
      some "declaration binder without a type; write `(name : Type)`"
    else none
  else if kind == ``Lean.Parser.Command.definition || kind == ``Lean.Parser.Command.abbrev then
    if stx[2][1].getNumArgs == 0 then
      some "definition without a result type; write `: Type` after its binders"
    else none
  else if kind == ``Lean.Parser.Term.basicFun then
    let untyped : Bool := stx[0].getArgs.any (fun (binder : Syntax) => !isTypedBinder binder)
    if stx[1].getNumArgs == 0 && untyped then
      some "fun binder without a type; write `fun (name : Type) => ...`"
    else none
  else if kind == ``Lean.Parser.Term.forall then
    let untyped : Bool := stx[1].getArgs.any (fun (binder : Syntax) => !isTypedBinder binder)
    if stx[2].getNumArgs == 0 && untyped then
      some "∀ binder without a type; write `∀ (name : Type), ...`"
    else none
  else if kind == `Lean.unbracketedExplicitBinders then
    if stx[1].getNumArgs == 0 then some "∃ binder without a type; write `∃ (name : Type), ...`"
    else none
  else if kind == ``Lean.Parser.Term.letIdDecl then
    if stx[2].getNumArgs == 0 then
      some "let or have without a type; write `let name : Type := ...`"
    else none
  else if kind == ``Lean.Parser.Term.doIdDecl then
    if stx[1].getNumArgs == 0 then
      some "do binding without a type; write `let name : Type ← ...`"
    else none
  else if kind == ``Lean.Parser.Tactic.intro then
    if stx[1].getArgs.any (fun (binder : Syntax) => !isTypedBinder binder) then
      some "intro without a type; write `intro (name : Type)`"
    else none
  else none

/-- Syntax rules for one node, given the role of the file it is in. -/
def nodeMessages (role : Role) (stx : Syntax) : List String :=
  let atomRule : List String :=
    match stx with
    | .atom _ value =>
      (bannedAtomRemedy? role value).toList.map (fun (remedy : String) =>
        s!"banned keyword `{value}`; {remedy}")
    | _ => []
  let attributeRule : List String :=
    match attributeName? stx with
    | some name =>
      (bannedAttributeRemedy? name).toList.map (fun (remedy : String) =>
        s!"banned attribute `{name}`; {remedy}")
    | none => []
  let simpRule : List String :=
    if onlyRequired.contains stx.getKind && !hasOnly stx then
      ["simplifier call without `only`; list its lemmas as `simp only [...]`, which `simp?` " ++
        "prints when run in your editor"]
    else []
  let nativeRule : List String :=
    if (stx.isOfKind ``Lean.Parser.Tactic.posConfigItem ||
        stx.isOfKind ``Lean.Parser.Tactic.valConfigItem) && stx[1].getId == `native then
      ["native evaluation bypasses the kernel; use plain `decide`"]
    else []
  let binderRule : List String := (binderMessage stx).toList
  atomRule ++ attributeRule ++ simpRule ++ nativeRule ++ binderRule

/-- Findings for every node of one command. -/
def commandFindings (path : FilePath) (fileMap : FileMap) (role : Role) (command : Syntax) :
    Array Finding := Id.run do
  let mut findings : Array Finding := #[]
  for stx in command.topDown do
    for message in nodeMessages role stx do
      let line : Nat := match stx.getPos? with
        | some pos => (fileMap.toPosition pos).line
        | none => 0
      findings := findings.push ⟨s!"{path}:{line}", message⟩
  pure findings

/-- Commands the parser elaborates: they only set the scope later commands parse in, such as a
namespace whose scoped syntax a dependency defines. -/
def scopeCommands : List SyntaxNodeKind :=
  [``Lean.Parser.Command.namespace, ``Lean.Parser.Command.section, ``Lean.Parser.Command.end,
    ``Lean.Parser.Command.open]

/-- Parses every command after the header and returns its findings. A scope command that breaks
no rule is elaborated, and every other command only parsed. Each command consumes input, so the
input's length bounds the loop. -/
def parseCommands (path : FilePath) (role : Role) (fuel : Nat) :
    Elab.Frontend.FrontendM (Array Finding) := do
  let fileMap : FileMap := (← Elab.Frontend.getInputContext).fileMap
  let mut findings : Array Finding := #[]
  for _ in [0:fuel] do
    Elab.Frontend.updateCmdPos
    let commandState : Elab.Command.State ← Elab.Frontend.getCommandState
    let scope : Elab.Command.Scope := commandState.scopes.head!
    let context : Parser.ParserModuleContext :=
      { env := commandState.env, options := scope.opts, currNamespace := scope.currNamespace,
        openDecls := scope.openDecls }
    let (command, parserState, messages) := Parser.parseCommand
      (← Elab.Frontend.getInputContext) context (← Elab.Frontend.getParserState)
      commandState.messages
    Elab.Frontend.setParserState parserState
    Elab.Frontend.setMessages messages
    let commandRules : Array Finding := commandFindings path fileMap role command
    findings := findings ++ commandRules
    if scopeCommands.contains command.getKind && commandRules.isEmpty then
      Elab.Frontend.elabCommandAtFrontend command
      -- Only parse errors are findings; an `open` of an owned namespace fails to elaborate.
      Elab.Frontend.setMessages messages
    if Parser.isTerminalCommand command then
      break
  pure findings

/-- A file's header, parsed, and the imports from outside the project it parses with. -/
structure ParsedHeader where
  /-- The file's text and name. -/
  inputCtx : Parser.InputContext
  /-- The header syntax. -/
  header : Elab.HeaderSyntax
  /-- The parser state after the header. -/
  parserState : Parser.ModuleParserState
  /-- Messages from parsing the header. -/
  messages : MessageLog
  /-- The header's imports from outside the project, and whether the file is a module. Owned
  modules may not be built yet, and they cannot define syntax. -/
  key : Array Import × Bool

/-- Parses one file's header. -/
def parseFileHeader (path : FilePath) (text : String) : IO ParsedHeader := do
  let inputCtx : Parser.InputContext := Parser.mkInputContext text path.toString
  let (header, parserState, messages) ← Parser.parseHeader inputCtx
  let imports : Array Import := (Elab.HeaderSyntax.imports header).filter
    (fun (imported : Import) => (roleOf imported.module).isNone)
  pure ⟨inputCtx, header, parserState, messages, (imports, Elab.HeaderSyntax.isModule header)⟩

/-- Syntax findings for one file, parsed after its header in `env`, which holds the syntax of
its imports, or in an empty environment with a finding when those failed to import. -/
def syntaxFindings (path : FilePath) (role : Role) (parsed : ParsedHeader)
    (env : Except String Environment) : IO (Array Finding) := do
  let mut findings : Array Finding := commandFindings path parsed.inputCtx.fileMap role
    parsed.header.raw
  let base : Environment ← match env with
    | .ok imported => pure (imported.setMainModule (moduleOf path))
    | .error message =>
      findings := findings.push ⟨s!"{path}", s!"imports fail to load: {message}; build the " ++
        "dependency modules first with `scripts/check.sh build`"⟩
      mkEmptyEnvironment
  let state : Elab.Frontend.State :=
    { commandState := Elab.Command.mkState base parsed.messages {},
      parserState := parsed.parserState, cmdPos := parsed.parserState.pos }
  let fuel : Nat := parsed.inputCtx.inputString.length + 1
  let (commandRules, final) ←
    ((parseCommands path role fuel).run { inputCtx := parsed.inputCtx }).run state
  for message in final.commandState.messages.toList do
    if message.severity == .error then
      findings := findings.push ⟨s!"{path}:{message.pos.line}", s!"does not parse: " ++
        s!"{(← message.data.toString).trimAscii}; owned code may use only syntax that Lean " ++
        "and the project's dependencies define"⟩
  pure (findings ++ commandRules)

/-- Syntax findings for the given files, each with its role, parsed without running any of
them. Imports outside the project are loaded once per distinct set for their syntax;
`allowInitializers` must re-enable their initializers, since each import consumes it. -/
def syntaxAudit (allowInitializers : BaseIO Unit) (files : Array (FilePath × Role)) :
    IO (Array Finding) := do
  let mut environments : List ((Array Import × Bool) × Except String Environment) := []
  let mut findings : Array Finding := #[]
  for (path, role) in files do
    let parsed : ParsedHeader ← parseFileHeader path (← IO.FS.readFile path)
    let env : Except String Environment ← match environments.lookup parsed.key with
      | some env => pure env
      | none =>
        allowInitializers
        let level : OLeanLevel := if parsed.key.2 then .exported else .private
        try
          pure (.ok (← importModules parsed.key.1 {} (trustLevel := 1024) (loadExts := true)
            (level := level)))
        catch error => pure (.error (toString error))
    environments := (parsed.key, env) :: environments
    findings := findings ++ (← syntaxFindings path role parsed env)
  pure findings

/-- Formatting findings for every owned file: the text rules alone. -/
def formatAudit : IO (Array Finding) := do
  let mut findings : Array Finding := #[]
  for path in ← sourceFiles do
    findings := findings ++ textFindings path (← IO.FS.readFile path)
  pure findings

/-- Source findings for every owned file: ownership and syntax rules. -/
def sourceAudit (allowInitializers : BaseIO Unit) : IO (Array Finding) := do
  let mut findings : Array Finding := #[]
  let mut files : Array (FilePath × Role) := #[]
  if ← FilePath.pathExists "lakefile.lean" then
    findings := findings.push ⟨"lakefile.lean", "use lakefile.toml; it cannot run code"⟩
  for path in ← sourceFiles do
    match roleOf (moduleOf path) with
    | none =>
      findings := findings.push ⟨s!"{path}",
        "not part of an owned module root; move it under one or delete it"⟩
    | some role => files := files.push (path, role)
  pure (findings ++ (← syntaxAudit allowInitializers files))

/-- The modules owned files import from the project's dependencies, which the source audit
loads for their syntax and so must be built first. Toolchain modules are always built. -/
def dependencyImports : IO (Array Name) := do
  let toolchain : String := ((← findSysroot) / "lib" / "lean").toString
  let mut modules : Array Name := #[]
  for path in ← sourceFiles do
    if (roleOf (moduleOf path)).isSome then
      for imported in (← parseFileHeader path (← IO.FS.readFile path)).key.1 do
        if !modules.contains imported.module then
          let inToolchain : Bool ←
            try pure ((← findOLean imported.module).toString.startsWith toolchain)
            catch _ => pure false
          if !inToolchain then
            modules := modules.push imported.module
  pure modules

end SpeakeasyAudit
