import Batteries.Tactic.Lint
import SpeakeasyAudit.Finding
import SpeakeasyAudit.Policy

/-!
# Environment audit

Checks the compiled declarations of the owned modules, as the kernel accepted
them: the axioms each one depends on, escape hatches the source rules could miss,
contracts, documentation, every Batteries environment linter, and the compiled
IR. Policy exceptions must match a declaration and must still be needed.

A contract is a `Prop`-valued definition in a specification module whose explicit
arguments are the implementations it constrains; its implicit and instance
arguments are shared parameters. A library theorem such as
`theorem advance_spec : AdvanceSpec advance` shows an implementation meets it. A
uniqueness theorem in a specification module shows at most one implementation
can, so the contract fixes the implementation's behavior completely.
-/

namespace SpeakeasyAudit

open Lean Meta Batteries.Tactic.Lint

/-- A constant declared in an owned module. -/
structure OwnedDecl where
  /-- The constant's name in the environment; private names are mangled. -/
  name : Name
  /-- The name as written in source, which policy exceptions use. -/
  userName : Name
  /-- The declaring module. -/
  module : Name
  /-- The declaring module's role. -/
  role : Role
  /-- The kernel's view of the constant. -/
  info : ConstantInfo

/-- Renders an owned declaration for findings. -/
def OwnedDecl.label (decl : OwnedDecl) : String :=
  s!"{decl.module}:{decl.userName}"

/-- Adds a constant to `acc` if its module is owned. -/
def addOwned (acc : Array OwnedDecl) (module : Name) (name : Name) (info : ConstantInfo) :
    Array OwnedDecl :=
  match roleOf module with
  | some role =>
    acc.push { name, userName := (privateToUserName? name).getD name, module, role, info }
  | none => acc

/-- Every constant declared in an owned module, imported or elaborated in this environment,
sorted by name. -/
def ownedDecls (env : Environment) : Array OwnedDecl :=
  let imported : Array OwnedDecl := env.constants.map₁.fold
    (fun (acc : Array OwnedDecl) (name : Name) (info : ConstantInfo) =>
      let module? : Option Name := do
        let index : ModuleIdx ← env.getModuleIdxFor? name
        env.header.moduleNames[index.toNat]?
      match module? with
      | some module => addOwned acc module name info
      | none => acc)
    #[]
  let found : Array OwnedDecl := env.constants.map₂.foldl
    (fun (acc : Array OwnedDecl) (name : Name) (info : ConstantInfo) =>
      addOwned acc env.mainModule name info)
    imported
  found.qsort (fun (left : OwnedDecl) (right : OwnedDecl) => left.name.lt right.name)

/-- Whether a declaration is compiler or elaborator output rather than written source. -/
def isGenerated (env : Environment) (decl : OwnedDecl) : Bool :=
  decl.userName.isInternalDetail || env.isAutoDecl decl.name ||
    decl.info matches .ctorInfo _ || decl.info matches .recInfo _

/-- The exception entries for one declaration in a keyed exception list. -/
def exceptionFor (exceptions : List ((Name × Name) × List Name)) (decl : OwnedDecl) :
    List Name :=
  (exceptions.lookup (decl.module, decl.userName)).getD []

/-- Kernel-level escape hatches: axioms, opaque and unsafe constants, `partial` and
noncomputable definitions, and replaced or external code. -/
def kernelFindings (env : Environment) (decl : OwnedDecl) : Array Finding :=
  let rules : List (Bool × String) :=
    [(decl.info matches .axiomInfo _, "declares an axiom; state it as a theorem and prove it"),
      (decl.info matches .opaqueInfo _,
        "declares an opaque constant, as `partial` definitions do; write a total definition " ++
          "that recurses structurally or proves termination with `termination_by`"),
      (decl.info.isUnsafe, "is unsafe; write it in safe Lean"),
      (isNoncomputable env decl.name, "is noncomputable; give it a computable definition"),
      (isExtern env decl.name, "has an external implementation; define it in Lean"),
      ((Compiler.implementedByAttr.getParam? env decl.name).isSome,
        "has its code replaced; remove `implemented_by` so the kernel sees the code that runs")]
  (rules.filterMap (fun (rule : Bool × String) =>
    if rule.1 then some ⟨decl.label, rule.2⟩ else none)).toArray

/-- The axiom rule: a declaration depends on no axiom outside the global allowance and its own
exception, and its exception allows nothing it does not use. -/
def axiomFindings (decl : OwnedDecl) : CoreM (Array Finding) := do
  let used : Array Name ← collectAxioms decl.name
  let excepted : List Name := exceptionFor axiomExceptions decl
  let extra : Array Name := used.filter (fun (axiomName : Name) =>
    !allowedAxioms.contains axiomName && !excepted.contains axiomName)
  let unneeded : List Name := excepted.filter (fun (axiomName : Name) => !used.contains axiomName)
  let mut findings : Array Finding := #[]
  if !extra.isEmpty then
    findings := findings.push ⟨decl.label, s!"depends on axioms {extra.toList}; " ++
      "finish every proof, replace classical reasoning with decidable case splits, or add a " ++
      "Policy.lean exception if a dependency forces the axiom"⟩
  if !unneeded.isEmpty then
    findings := findings.push
      ⟨decl.label, s!"axiom exception {unneeded} is no longer needed; remove it from Policy.lean"⟩
  pure findings

/-- The compiled functions an owned declaration's code reaches before leaving compiler-generated
code: the declaration itself and every specialization or auxiliary function it calls, which have
no constant of their own. `fuel` bounds the walk by the number of compiled functions visited. -/
def generatedClosure (env : Environment) (pending : List Name) (seen : NameSet) : Nat → NameSet
  | 0 => seen
  | fuel + 1 =>
    match pending with
    | [] => seen
    | name :: rest =>
      if seen.contains name then generatedClosure env rest seen fuel
      else
        let callees : List Name := match IR.findEnvDecl env name with
          | some decl => (IR.collectUsedDecls env [decl]).toList.filter
              (fun (callee : Name) => !env.contains callee)
          | none => []
        generatedClosure env (callees ++ rest) (seen.insert name) fuel

/-- Compiled code: neither an owned function nor any compiler-generated function it reaches is
external, and none of them calls an unsafe function outside its exception, which must still be
needed. Calls into external runtime primitives such as `Nat.add` are expected; every compiled
program makes them. -/
def irFindings (env : Environment) (decl : OwnedDecl) : Array Finding := Id.run do
  let excepted : List Name := exceptionFor unsafeCallExceptions decl
  let reached : NameSet := generatedClosure env [decl.name] {} 100000
  let mut findings : Array Finding := #[]
  let mut unsafeCalls : NameSet := {}
  for function in reached.toArray do
    match IR.findEnvDecl env function with
    | none => pure ()
    | some compiled =>
      if compiled matches .extern .. then
        findings := findings.push
          ⟨decl.label, s!"compiles to external `{function}`; define it in Lean"⟩
      for callee in IR.collectUsedDecls env [compiled] do
        if (env.find? callee).any (fun (info : ConstantInfo) => info.isUnsafe) then
          unsafeCalls := unsafeCalls.insert callee
  for callee in unsafeCalls.toArray do
    if !excepted.contains callee then
      findings := findings.push ⟨decl.label, s!"compiled code calls unsafe `{callee}`; " ++
        "avoid the call, or add a Policy.lean exception if a dependency forces it"⟩
  let unneeded : List Name := excepted.filter (fun (callee : Name) => !unsafeCalls.contains callee)
  if !unneeded.isEmpty then
    findings := findings.push ⟨decl.label,
      s!"unsafe-call exception {unneeded} is no longer needed; remove it from Policy.lean"⟩
  pure findings

/-- Every exception for a module in the environment names a declaration of that module. -/
def staleExceptionFindings (env : Environment) (owned : Array OwnedDecl) : Array Finding :=
  let keys : List (Name × Name) :=
    axiomExceptions.map (fun (entry : (Name × Name) × List Name) => entry.1) ++
      unsafeCallExceptions.map (fun (entry : (Name × Name) × List Name) => entry.1) ++
      contractExemptions ++ uniquenessExemptions
  (keys.filterMap (fun (key : Name × Name) =>
    if !env.header.moduleNames.contains key.1 then none
    else if owned.any (fun (decl : OwnedDecl) => (decl.module, decl.userName) == key) then none
    else some ⟨s!"{key.1}:{key.2}",
      "exception names no declaration of its module; remove it from Policy.lean"⟩)).toArray

/-- Specification modules import no owned module outside the specifications, so no contract can
name, and so restate, the implementation it constrains. -/
def specImportFindings (env : Environment) : Array Finding := Id.run do
  let mut findings : Array Finding := #[]
  for (module, data) in env.header.moduleNames.zip env.header.moduleData do
    if roleOf module == some .spec then
      for imported in data.imports do
        if (roleOf imported.module).any (fun (role : Role) => role != .spec) then
          findings := findings.push ⟨s!"{module}", s!"imports {imported.module}; a " ++
            "specification may not depend on the implementation, so move what the contract " ++
            "needs into a specification module"⟩
  pure findings

/-- Which leading binders of a type are explicit. -/
def explicitBinders : Expr → List Bool
  | .forallE _ _ body info => info.isExplicit :: explicitBinders body
  | _ => []

/-- The contract a theorem shows definitions meet, and those definitions. Under its quantified
variables, the statement must be a contract applied to implementations, each an owned definition
applied to distinct variables alone, and every variable must be an argument of the contract or
of an implementation. A hypothesis is such a variable, so a theorem with one shows nothing.
`contracts` maps each contract to which of its arguments are implementations. -/
def boundDefinitions (contracts : NameMap (List Bool)) (implementations : NameSet)
    (type : Expr) : Option (Name × List Name) := Id.run do
  let mut body : Expr := type
  let mut variables : Nat := 0
  while body.isForall do
    body := body.bindingBody!
    variables := variables + 1
  body := body.consumeMData
  let some contract := body.getAppFn.constName? | return none
  let some explicit := contracts.find? contract | return none
  let args : Array Expr := body.getAppArgs
  if args.size != explicit.length then return none
  let mut bound : List Name := []
  let mut used : List Nat := []
  for (arg, isImplementation) in args.zip explicit.toArray do
    if isImplementation then
      let some name := arg.getAppFn.constName? | return none
      let indices : List Nat := arg.getAppArgs.toList.filterMap (fun (inner : Expr) =>
        match inner with
        | .bvar index => some index
        | _ => none)
      if !implementations.contains name || indices.length != arg.getAppNumArgs ||
          indices.eraseDups.length != indices.length then
        return none
      bound := name :: bound
      used := indices ++ used
    else if let .bvar index := arg then
      used := index :: used
  if bound.isEmpty || !(List.range variables).all (fun (index : Nat) => used.contains index) then
    none
  else some (contract, bound)

/-- The statement that a contract fixes its implementations: any two that meet it, with the same
parameters, are equal. -/
def uniquenessStatement (contract : ConstantInfo) : MetaM Expr := do
  let head : Expr := mkConst contract.name (contract.levelParams.map Level.param)
  forallTelescope contract.type (fun (args : Array Expr) (_ : Expr) => do
    let mut implementations : Array Expr := #[]
    for arg in args do
      if (← arg.fvarId!.getBinderInfo).isExplicit then
        implementations := implementations.push arg
    let others : Array (Name × (Array Expr → MetaM Expr)) ←
      implementations.mapM (fun (implementation : Expr) => do
        let type : Expr ← inferType implementation
        pure ((← implementation.fvarId!.getUserName).appendAfter "'",
          fun (_ : Array Expr) => pure type))
    withLocalDeclsD others (fun (copies : Array Expr) => do
      let swapped : Array Expr := args.map (fun (arg : Expr) =>
        match implementations.idxOf? arg with
        | some index => copies[index]!
        | none => arg)
      let equalities : List Expr ← (implementations.zip copies).toList.mapM
        (fun (pair : Expr × Expr) => mkEq pair.1 pair.2)
      let conclusion : Expr ←
        mkArrow (mkAppN head args) (← mkArrow (mkAppN head swapped) (mkAndN equalities))
      mkForallFVars (args ++ copies) conclusion))

/-- Whether an owned specification theorem states that a contract fixes its implementations. -/
def hasUniqueness (owned : Array OwnedDecl) (contract : ConstantInfo) (expected : Expr) :
    MetaM Bool :=
  owned.anyM (fun (decl : OwnedDecl) => do
    if decl.role != .spec || !decl.info.isTheorem ||
        decl.info.levelParams.length != contract.levelParams.length ||
        !decl.info.type.getUsedConstants.contains contract.name then
      return false
    let type : Expr :=
      decl.info.instantiateTypeLevelParams (contract.levelParams.map Level.param)
    withNewMCtxDepth (isDefEq expected type))

/-- Contract rules: every written public library or executable definition meets a contract, and
every contract an implementation meets has a uniqueness theorem. Private definitions, and helpers
nested under a definition's name as `where` and `let rec` declare them, are implementation
details: the contracts of the definitions that use them cover them. Exemptions must still be
needed. -/
def contractFindings (env : Environment) (owned : Array OwnedDecl) : MetaM (Array Finding) := do
  let contracts : NameMap (List Bool) := owned.foldl
    (fun (acc : NameMap (List Bool)) (decl : OwnedDecl) =>
      if decl.role == .spec && decl.info matches .defnInfo _ then
        acc.insert decl.name (explicitBinders decl.info.type)
      else acc)
    {}
  let implementations : NameSet := owned.foldl
    (fun (acc : NameSet) (decl : OwnedDecl) =>
      if (decl.role == .library || decl.role == .executable) && decl.info matches .defnInfo _ then
        acc.insert decl.name
      else acc)
    {}
  let needsContract (decl : OwnedDecl) : Bool :=
    implementations.contains decl.name && !isGenerated env decl && !isPrivateName decl.name &&
      !implementations.contains decl.name.getPrefix
  let mut bound : NameSet := {}
  let mut used : NameSet := {}
  for decl in owned do
    if decl.info.isTheorem && decl.role != .spec then
      if let some (contract, names) := boundDefinitions contracts implementations decl.info.type
      then
        bound := names.foldl (fun (acc : NameSet) (name : Name) => acc.insert name) bound
        used := used.insert contract
  let mut findings : Array Finding := #[]
  for decl in owned do
    let exempt : Bool := contractExemptions.contains (decl.module, decl.userName)
    if needsContract decl && !exempt && !bound.contains decl.name then
      let short : String := match decl.userName with
        | .str _ last => last
        | other => other.toString
      let contract : String := short.capitalize ++ "Spec"
      let remedy : String :=
        if decl.info.type.getForallBody.isProp then
          "a `Prop`-valued definition states requirements, so move it to a specification module"
        else
          s!"state its contract `{contract}` in a specification module, prove there that at " ++
            "most one function meets it, and prove " ++
            s!"`theorem {short}_spec : {contract} {short}` here"
      findings := findings.push ⟨decl.label, s!"meets no contract; {remedy}"⟩
    else if exempt && bound.contains decl.name then
      findings := findings.push
        ⟨decl.label, "contract exemption is no longer needed; remove it from Policy.lean"⟩
    if used.contains decl.name then
      let expected : Expr ← uniquenessStatement decl.info
      let unique : Bool ← hasUniqueness owned decl.info expected
      let exemptUnique : Bool := uniquenessExemptions.contains (decl.module, decl.userName)
      if !unique && !exemptUnique then
        let statement : String := (← ppExpr expected).pretty 1000
        findings := findings.push ⟨decl.label, "contract has no uniqueness theorem; prove " ++
          s!"`{statement}` in a specification module, or exempt the contract in Policy.lean " ++
          "if it deliberately admits several implementations"⟩
      else if unique && exemptUnique then
        findings := findings.push
          ⟨decl.label, "uniqueness exemption is no longer needed; remove it from Policy.lean"⟩
  pure findings

/-- Documentation: every written owned declaration, theorems included, has a docstring. -/
def docFindings (env : Environment) (owned : Array OwnedDecl) : IO (Array Finding) := do
  let mut findings : Array Finding := #[]
  for decl in owned do
    if !isGenerated env decl then
      if (← findDocString? env decl.name).isNone then
        findings := findings.push
          ⟨decl.label, "has no docstring; add a `/-- ... -/` comment that says what it is"⟩
  pure findings

/-- Every Batteries environment linter, including those disabled by default. -/
def lintFindings (declarations : Array Name) : CoreM (Array Finding) := do
  let everyLinter : List Name := (batteriesLinterExt.getState (← getEnv)).toList.map
    (fun (entry : Name × Name × Bool) => entry.1)
  let linters : Array NamedLinter ← getChecks (slow := true) (runOnly := none)
    (runAlways := some everyLinter)
  let results : Array (NamedLinter × Std.HashMap Name MessageData) ←
    lintCore declarations linters
  let mut findings : Array Finding := #[]
  for (linter, failures) in results do
    for (name, message) in failures.toList do
      findings := findings.push ⟨s!"{name}", s!"{linter.name}: {← message.toString}"⟩
  pure findings

/-- Every owned module must be imported by the audited roots, so none escapes the
environment rules, and must resolve to this project's build output, so a dependency module
of the same name cannot stand in for it. -/
def coverageFindings (env : Environment) (modules : Array Name) : IO (Array Finding) := do
  let buildDir : System.FilePath := (← IO.currentDir) / ".lake" / "build" / "lib" / "lean"
  let mut findings : Array Finding := #[]
  for module in modules do
    if !env.header.moduleNames.contains module then
      findings := findings.push ⟨s!"{module}",
        "owned module is not imported by the audited roots; import it from its root module"⟩
    let olean : System.FilePath ← findOLean module
    if !olean.toString.startsWith buildDir.toString then
      findings := findings.push ⟨s!"{module}",
        s!"resolves to {olean}, outside this project; rename the module"⟩
  pure findings

/-- The rules that need no Batteries linters or build output: kernel, IR, axioms, contracts,
specification imports, documentation, and stale exceptions. -/
def declarationFindings (env : Environment) (owned : Array OwnedDecl) : IO (Array Finding) := do
  let docs : Array Finding ← docFindings env owned
  let context : Core.Context := { fileName := "<audit>", fileMap := default }
  let (findings, _) ← (do
    let mut findings : Array Finding := staleExceptionFindings env owned ++ specImportFindings env
    for decl in owned do
      findings := findings ++ kernelFindings env decl ++ irFindings env decl
      findings := findings ++ (← axiomFindings decl)
    pure (findings ++ (← (contractFindings env owned).run')) : CoreM (Array Finding)).toIO
      context { env }
  pure (docs ++ findings)

/-- Runs every environment rule over the given root modules. `modules` lists the owned modules
the roots must cover. -/
def environmentAudit (roots : Array Name) (modules : Array Name) : IO (Array Finding) := do
  let imports : Array Import := (roots.push `Batteries.Tactic.Lint).map
    (fun (module : Name) => { module })
  let env : Environment ← importModules imports {} (trustLevel := 1024) (loadExts := true)
  let owned : Array OwnedDecl := ownedDecls env
  let coverage : Array Finding ← coverageFindings env modules
  let declarationRules : Array Finding ← declarationFindings env owned
  let declarations : Array Name := (owned.filter (fun (decl : OwnedDecl) =>
    !isGenerated env decl)).map (fun (decl : OwnedDecl) => decl.name)
  let context : Core.Context := { fileName := "<audit>", fileMap := default }
  let (lints, _) ← (lintFindings declarations).toIO context { env }
  pure (coverage ++ declarationRules ++ lints)

/-- The written declarations of the given roles under the given roots, for exporting to an
external kernel checker, which rechecks them with everything they depend on. -/
def roleDeclarations (roots : Array Name) (roles : List Role) : IO (Array Name) := do
  let imports : Array Import := roots.map (fun (module : Name) => { module })
  let env : Environment ← importModules imports {} (trustLevel := 1024)
  let written : Array OwnedDecl := (ownedDecls env).filter (fun (decl : OwnedDecl) =>
    roles.contains decl.role && !isGenerated env decl)
  pure (written.map (fun (decl : OwnedDecl) => decl.name))

end SpeakeasyAudit
