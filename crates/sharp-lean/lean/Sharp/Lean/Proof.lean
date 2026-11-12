import Lean
import Sharp.Lean.Law

/-!
The steps of the proof Mago writes for a PHP# law, from its body or, for a law without one, from its claim alone.

A law without a body is proven by `sharp_prove`, which unfolds every definition its claim reaches. Each case of a body
ends in `sharp_close`, which closes the case with Lean's automatic steps and the facts in scope, on the claim as it
stands, with each definition the claim calls unfolded once, or with those definitions as `simp` rewrites, and then with
every definition they reach that does not recurse as one too. The facts
are the law calls of the body before it, each `sharp_call%` of the theorem of the law it calls, which proves the called
law's `when` condition first. A step that cannot close its goal reports the goal, as the law reads it, in an error that starts with `sharp-case` or `sharp-condition` and its index, then admits the goal, so
every theorem is declared and each failure is reported once. Each step has a time budget of its own, so a case whose
steps run out of theirs is reported as `sharp-case-out-of-time` or `sharp-condition-out-of-time`, and the cases after
it still run. A law without a body that proves case by case over an enum it takes reports each case left open with the
case it is. The runner reads those errors, and Mago prints each goal in PHP#.
-/

namespace Sharp

@[simp] theorem pure_inj {α : Type} (a b : α) : (pure a : M α) = pure b ↔ a = b := by
  constructor
  · intro h
    have := congrArg ExceptT.run h
    simp [ExceptT.run_pure] at this
    exact this
  · intro h
    rw [h]

@[simp] theorem throw_eq_pure {α : Type} (e : Throwable) (b : α) : (throw e : M α) = pure b ↔ False := by
  constructor
  · intro h
    have := congrArg ExceptT.run h
    simp [ExceptT.run_pure] at this
  · intro h
    exact h.elim

private theorem eq_pure {α : Type} (x : M α) (a : α) : x = pure a ↔ x.run = some (.ok a) :=
  ⟨fun h => h ▸ rfl, fun h => (ExceptT.ext h : x = ExceptT.mk (some (.ok a)))⟩

/-- A fact about the values a body computes reads them one by one, as `holds_bind` reads a law's. -/
@[simp] theorem bind_eq_pure {α β : Type} (x : M α) (f : α → M β) (b : β) :
    x >>= f = pure b ↔ ∃ a, x = pure a ∧ f a = pure b := by
  simp only [eq_pure, ExceptT.run_bind]
  cases hx : x.run with
  | none => simp
  | some r => cases r <;> simp

@[simp] theorem map_eq_pure {α β : Type} (f : α → β) (x : M α) (b : β) :
    f <$> x = pure b ↔ ∃ a, x = pure a ∧ f a = b := by
  rw [← bind_pure_comp, bind_eq_pure]
  simp

@[simp] theorem ite_eq_pure {α : Type} (c : Prop) [Decidable c] (x y : M α) (b : α) :
    (if c then x else y) = pure b ↔ (c ∧ x = pure b) ∨ (¬c ∧ y = pure b) := by
  by_cases h : c <;> simp [h]

/-- A claim that chooses, as `||` and `? :` do, holds when each choice holds where it is made. -/
@[simp] theorem holds_ite (c : Prop) [Decidable c] (x y : M Bool) :
    holds (if c then x else y) ↔ (c → holds x) ∧ (¬c → holds y) := by
  by_cases h : c <;> simp [h]

namespace Proof

open Lean Elab Tactic Meta

private def node (kind : String) (items : Array Json) : Json :=
  .arr (#[.str kind] ++ items)

/-- `name` as PHP# code may name it: without macro scopes and without the guillemets Lean escapes a name with. -/
private def written (name : Name) : String :=
  name.eraseMacroScopes.toString (escape := false)

/-- `e` as a tree Mago prints in PHP#: `["app", constant, argument…]` with only the explicit arguments, `["var", name]`,
`["lit", digits]`, `["str", text]`, `["field", name, value]`, `["lam", name, body]` and `["let", name, value, body]`.
`++` of lists is `List.append`, apart from `++` of strings. Anything else is `["?"]`. A `lam` or `let` names its value
apart from every name in scope, as `x_1` inside an `x`, so each `var` reads the value it names. -/
partial def tree (e : Expr) : MetaM Json := do
  match ← instantiateMVars e with
  | .mdata _ e => tree e
  | .fvar id => return node "var" #[.str (written (← id.getDecl).userName)]
  | .lit (.natVal n) => return node "lit" #[.str (toString n)]
  | .lit (.strVal s) => return node "str" #[.str s]
  | .lam name type body _ =>
    let name := (← getLCtx).getUnusedName name.eraseMacroScopes
    withLocalDeclD name type fun x => do return node "lam" #[.str (written name), ← tree (body.instantiate1 x)]
  | .letE name type value body _ =>
    let name := (← getLCtx).getUnusedName name.eraseMacroScopes
    withLetDecl name type value fun x => do
      return node "let" #[.str (written name), ← tree value, ← tree (body.instantiate1 x)]
  | .proj struct index value =>
    let fields := getStructureFields (← getEnv) struct
    return node "field" #[.str (fields[index]?.map written |>.getD "?"), ← tree value]
  | e@(.app ..) | e@(.const ..) =>
    let fn := e.getAppFn
    let args := e.getAppArgs
    let .const name _ := fn | return node "?" #[]
    if name == ``OfNat.ofNat && args.size == 3 then
      if let .lit (.natVal n) := args[1]! then return node "lit" #[.str (toString n)]
    if let some info ← getProjectionFnInfo? name then
      if let (false, some value) := (info.fromClass, args[info.numParams]?) then
        return node "field" #[.str (written (name.componentsRev.head?.getD name)), ← tree value]
    let info ← getFunInfoNArgs fn args.size
    let mut explicit := #[]
    for h : index in [0:args.size] do
      if info.paramInfo[index]?.all (·.binderInfo.isExplicit) then explicit := explicit.push (← tree args[index])
    let name := if name == ``HAppend.hAppend && (args[0]?.any (·.isAppOf ``List)) then ``List.append else name
    return node "app" (#[.str (written name)] ++ explicit)
  | _ => return node "?" #[]

/-- The constants `sharp-lean` declared when it elaborated the generated modules of the project's code, which it records
in each environment it elaborates a check's theorems in. -/
initialize generated : EnvExtension NameSet ← registerEnvExtension (pure {})

/-- Whether a generated module of the project's code declares `name`, as `generated` records: never one of the theorems
`sharp-lean` elaborates, nor a helper Lean declares for one, such as its `match_1`. -/
private def isCode (env : Environment) (name : Name) : Bool :=
  (generated.getState env).contains name

/-- Each definition of the project's code that `e` names, apart from a structure's fields and a law's statement. -/
private def definitionsIn (e : Expr) : MetaM (Array Name) := do
  let env ← getEnv
  let mut found := #[]
  for name in e.getUsedConstants do
    if isCode env name && (env.find? name).any (·.isDefinition) && !env.isProjectionFn name &&
        !lawAttribute.hasTag env name && !found.contains name then
      found := found.push name
  return found

/-- What `name` is defined as: the body its equation states for a recursive definition, which Lean compiles into a
term that names its recursion's helpers rather than the definitions it calls, and else its value. -/
private def definedAs (name : Name) : MetaM (Option Expr) := do
  if ← isRecursiveDefinition name then
    let some equation ← getUnfoldEqnFor? name (nonRec := true) | return none
    return some (← inferType (← mkConstWithLevelParams equation))
  return (← getEnv).find? name |>.bind (·.value?)

/-- Each definition `e` names, and each one those definitions name, transitively. -/
private partial def reached (e : Expr) : MetaM (Array Name) := do
  let mut found := #[]
  let mut pending ← definitionsIn e
  while !pending.isEmpty do
    let name := pending.back!
    pending := pending.pop
    if found.contains name then continue
    found := found.push name
    if let some body ← definedAs name then
      pending := pending ++ (← definitionsIn body)
  return found

/-- The statement of each law a fact in scope of the main goal is the theorem of. -/
private def lawsInScope : TacticM (Array Name) := do
  let env ← getEnv
  (← getMainGoal).withContext do
    let mut found := #[]
    for declaration in ← getLCtx do
      for name in (← instantiateMVars declaration.type).getUsedConstants do
        if lawAttribute.hasTag env name && !found.contains name then found := found.push name
    return found

private abbrev SimpItem := TSyntax [`Lean.Parser.Tactic.simpErase, `Lean.Parser.Tactic.simpLemma]

private def lemma (name : Name) : TacticM SimpItem :=
  return ⟨(← `(Lean.Parser.Tactic.simpLemma| $(mkIdent name):ident)).raw⟩

/-- Unfolds `definition` once, when the goal names it. -/
private def unfoldOnce (definition : Name) : TacticM Unit := do
  try evalTactic (← `(tactic| unfold $(mkIdent definition):ident)) catch _ => pure ()

/-- The automatic steps that close a goal: `simp_all` with `definitions` and every fact in scope, then `omega` or `grind`
on what is left, or `decide`. A case hypothesis keeps its `¬ x ≤ y` and its `¬ x < y`, so it still decides the `if`
the definition tests. -/
private def auto (definitions : Array Name) : TacticM Unit := do
  let kept : Array SimpItem := #[⟨(← `(Lean.Parser.Tactic.simpErase| -Int64.not_le)).raw⟩,
    ⟨(← `(Lean.Parser.Tactic.simpErase| -Int.not_le)).raw⟩,
    ⟨(← `(Lean.Parser.Tactic.simpErase| -Int64.not_lt)).raw⟩,
    ⟨(← `(Lean.Parser.Tactic.simpErase| -Int.not_lt)).raw⟩]
  let items := (← definitions.mapM lemma) ++ kept
  evalTactic (← `(tactic| first
    | ((try simp_all [$items,*])
       first | done | omega | grind)
    | decide))

/-- The time budget of each step, in thousands of heartbeats: Lean's default budget for a whole command, which a law's
whole proof had before each step had its own. The runner gives a law's theorem no budget of its own. -/
private def budget : Nat := 200000

/-- Runs `step` with a time budget of its own, so a step that runs out of it leaves each step after it its whole
budget. -/
private def budgeted (step : TacticM α) : TacticM α :=
  withCurrHeartbeats <| withTheReader Core.Context (fun context => { context with maxHeartbeats := budget * 1000 }) step

/-- How a step left the goal it ran on. -/
private inductive Outcome where
  /-- It closed every goal. -/
  | closed
  /-- It left a goal open. -/
  | unclosed
  /-- It ran out of its time budget before it finished. -/
  | outOfTime
  deriving BEq

/-- Runs `step` on the proof state with its own budget, and returns `.closed` when it closes every goal, or the state as
it was with `.outOfTime` when the step ran out of its budget and `.unclosed` otherwise. A step that reaches Lean's
recursion limit, as `simp` does on a definition that calls itself on any value, is `.unclosed`. -/
private def attempt (step : TacticM Unit) : TacticM Outcome := do
  let saved ← saveState
  tryCatchRuntimeEx
    (budgeted do
      step
      if (← getUnsolvedGoals).isEmpty then return .closed
      saved.restore
      return .unclosed)
    fun error => do
      saved.restore
      return if error.isMaxHeartbeat then .outOfTime else .unclosed

/-- Runs each of `steps` with `attempt` until one closes the goal: `.closed` when one does, and else `.outOfTime` when
one ran out of its budget, or `.unclosed`. -/
private def firstClosing (steps : List (TacticM Unit)) : TacticM Outcome := do
  let mut outcome := Outcome.unclosed
  for step in steps do
    match ← attempt step with
    | .closed => return .closed
    | .outOfTime => outcome := .outOfTime
    | .unclosed => pure ()
  return outcome

/-- The main goal as the law reads it, as a tree: each definition the claim calls unfolded once, each of `conditions`
applied, and each value a step binds written where it is used. The goal as it stands when that runs out of its
budget. -/
private def presented (conditions : Array Ident) : TacticM Json := do
  let saved ← saveState
  let goal ← getMainGoal
  let whole : TacticM Json := goal.withContext do tree (← goal.getType)
  let presented ← tryCatchRuntimeEx
    (budgeted do
      for definition in ← definitionsIn (← getMainTarget) do unfoldOnce definition
      for condition in conditions do
        try evalTactic (← `(tactic| simp only [pure_inj, Bool.not_eq_true] at $condition:ident)) catch _ => pure ()
      let items := (← #[``bind_assoc, ``pure_bind, ``Bool.false_eq_true].mapM lemma) ++
        (← conditions.mapM fun condition => do
          let item : SimpItem := ⟨(← `(Lean.Parser.Tactic.simpLemma| $condition:ident)).raw⟩
          return item)
      try evalTactic (← `(tactic| simp only [↓reduceIte, $items,*])) catch _ => pure ()
      let goal ← getMainGoal
      goal.withContext do tree (← goal.getType))
    fun _ => do
      saved.restore
      whole
  saved.restore
  return presented

/-- Reports the main goal of the case `index`, with `conditions` applied, as the `outcome` of its steps left it, and
admits it. The goal of one case of the enums the proof split on is `["split", [[value, case]…], goal]`, with the case of
each enum value `split` names. -/
private def report (index : TSyntax `num) (conditions : Array Ident) (outcome : Outcome)
    (split : Array (Name × Name) := #[]) : TacticM Unit := do
  let goal ← presented conditions
  let cases := split.map fun (value, case) => Json.arr #[.str (written value), .str (written case)]
  let goal := if split.isEmpty then goal else node "split" #[.arr cases, goal]
  let kind := if outcome == .outOfTime then "sharp-case-out-of-time" else "sharp-case"
  logError m!"{kind} {index.getNat} {goal.compress}"
  admitGoal (← getMainGoal)

/-- The name of each value of the main goal whose type is an enum, as a PHP# enum or `bool` translates. -/
private def enumsInScope : TacticM (Array Name) := do
  (← getMainGoal).withContext do
    let mut found := #[]
    for declaration in ← getLCtx do
      let .const type _ := declaration.type.getAppFn | continue
      if !declaration.isImplementationDetail && (← isEnumType type) then found := found.push declaration.userName
    return found

/-- The goal of each case of the enum values `enums` of the main goal, one for each way their cases combine, with the
case each value is in it. -/
private def enumCases (enums : Array Name) : TacticM (Array (MVarId × Array (Name × Name))) := do
  let mut split := #[(← getMainGoal, #[])]
  for enum in enums do
    let mut next := #[]
    for (goal, cases) in split do
      let value ← goal.withContext do return (← getLocalDeclFromUserName enum).fvarId
      for case in ← goal.cases value do
        -- Only a sparse split has a case of no constructor, its catch-all.
        let some constructor := case.ctorName | throwError "the split on {enum} has a case of no constructor"
        next := next.push (case.mvarId, cases.push (enum, constructor))
    split := next
  return split

/-- Proves a law without a body, whose case `index` is its whole claim: unfolds a recursive definition the claim reaches
once, and every other one as `simp` rewrites, then runs the automatic steps. A claim over values the law writes out,
as `Money.total([new Money(1, "EUR")])`, needs each recursion unfolded until it ends, so the steps then run with the
recursive definitions as `simp` rewrites too. A claim over an enum the law takes, as `retry.backoff()[index]` over
a `Retry`, reads what each case computes, so the steps then run on each case of each enum, as a body's `match` on each
case would, and each case they leave open is reported on its own, with the case it is. -/
syntax "sharp_prove " num " [" ident,* "]" : tactic

elab_rules : tactic
  | `(tactic| sharp_prove $index [$conditions,*]) => do
    let definitions ← reached (← getMainTarget)
    let recursive ← definitions.filterM fun definition => isRecursiveDefinition definition
    let laws ← lawsInScope
    let enums ← enumsInScope
    let once : TacticM Unit := do
      for definition in recursive do unfoldOnce definition
      auto ((definitions.filter (!recursive.contains ·)) ++ laws)
    let ended : TacticM Unit := auto (definitions ++ laws)
    let proven : TacticM Outcome := firstClosing (once :: if recursive.isEmpty then [] else [ended])
    let whole ← proven
    if whole == .closed then return
    if enums.isEmpty then
      report index conditions.getElems whole
      return
    for (case, split) in ← enumCases enums do
      setGoals [case]
      let outcome ← proven
      unless outcome == .closed do report index conditions.getElems outcome split

/-- Closes the case `index` of a body with the automatic steps and the laws of the facts in scope: on the claim as the
facts state it, else with each definition the claim calls unfolded once, else with those definitions as `simp` rewrites,
else with every definition they reach that does not recurse as a `simp` rewrite too, as a law without a body reads
them, so a case that knows `role = Owner` reads what `role.level()` computes through the methods it calls. A rewrite of
a recursive definition reads a call on a value its equation matches, as `total (first :: rest)`, and keeps a call on any
other value, as `total b`, as the facts state it. -/
syntax "sharp_close " num " [" ident,* "]" : tactic

elab_rules : tactic
  | `(tactic| sharp_close $index [$conditions,*]) => do
    let definitions ← definitionsIn (← getMainTarget)
    let reaching ← (← reached (← getMainTarget)).filterM fun definition => do
      return !definitions.contains definition && !(← isRecursiveDefinition definition)
    let laws ← lawsInScope
    let unfolded : TacticM Unit := do
      for definition in definitions do unfoldOnce definition
      auto laws
    let outcome ← firstClosing ([auto laws, unfolded, auto (definitions ++ laws)] ++
      if reaching.isEmpty then [] else [auto (definitions ++ reaching ++ laws)])
    unless outcome == .closed do report index conditions.getElems outcome

/-- Proves the condition of the law the call `index` calls with the automatic steps. Each part of a condition joined by
`&&` that stays open is reported, as one goal of the call, and admitted, and the call ran out of time when a part's
steps ran out of their budget. -/
syntax "sharp_condition " num : tactic

elab_rules : tactic
  | `(tactic| sharp_condition $index) => do
    if (← attempt (do auto (← lawsInScope))) == .closed then return
    try evalTactic (← `(tactic| simp only [pure_inj, Bool.and_eq_true])) catch _ => pure ()
    try evalTactic (← `(tactic| repeat' apply And.intro)) catch _ => pure ()
    let mut open_ := #[]
    let mut outOfTime := false
    for goal in ← getUnsolvedGoals do
      setGoals [goal]
      let outcome ← attempt (do auto (← lawsInScope))
      if outcome == .closed then continue
      outOfTime := outOfTime || outcome == .outOfTime
      open_ := open_.push (← goal.withContext do tree (← goal.getType))
      admitGoal goal
    let kind := if outOfTime then "sharp-condition-out-of-time" else "sharp-condition"
    unless open_.isEmpty do logError m!"{kind} {index.getNat} {(Json.arr open_).compress}"

/-- Runs `tactics` as one step with its own budget, and keeps what they did only when they close every goal, as
`try (tactics; done)` does for steps that fail. -/
syntax "sharp_attempt " "(" tacticSeq ")" : tactic

elab_rules : tactic
  | `(tactic| sharp_attempt ($tactics)) => discard <| attempt (evalTactic tactics)

/-- The call `index` of a law, `proof`, the law's theorem applied to the call's arguments: the fact the law states, after
its `when` condition is proven with `sharp_condition`. -/
syntax "sharp_call% " num term : term

elab_rules : term
  | `(sharp_call% $index $proof) => do
    let fact ← Term.elabTerm proof none
    Term.synthesizeSyntheticMVarsNoPostponing
    let .forallE _ condition _ _ ← whnf (← inferType fact) | return fact
    let shown ← Term.elabTerm (← `(by sharp_condition $index)) (some condition)
    return mkApp fact shown

end Proof

end Sharp
