import Lean
import Sharp.Lean.Law

/-!
The steps of the proof Mago writes for a PHP# law, from its body or, for a law without one, from its claim alone.

A law without a body is proven by `sharp_prove`, which unfolds every definition its claim reaches. Each case of a body
ends in `sharp_close`, which closes the case with Lean's automatic steps and the facts in scope, on the claim as it
stands or with each definition the claim calls unfolded once. The facts are the law calls of the body before it, each
`sharp_call%` of the theorem of the law it calls, which proves the called law's `when` condition first. A step that cannot close its goal reports the goal, as the
law reads it, in an error that starts with `sharp-case` or `sharp-condition` and its index, then admits the goal, so
every theorem is declared and each failure is reported once. The runner reads those errors, and Mago prints each goal
in PHP#.
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
`++` of lists is `List.append`, apart from `++` of strings. Anything else is `["?"]`. -/
partial def tree (e : Expr) : MetaM Json := do
  match ← instantiateMVars e with
  | .mdata _ e => tree e
  | .fvar id => return node "var" #[.str (written (← id.getDecl).userName)]
  | .lit (.natVal n) => return node "lit" #[.str (toString n)]
  | .lit (.strVal s) => return node "str" #[.str s]
  | .lam name type body _ =>
    withLocalDeclD name type fun x => do return node "lam" #[.str (written name), ← tree (body.instantiate1 x)]
  | .letE name type value body _ =>
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

/-- Whether a generated module of the project's code, under the root `Code`, declares `name`. -/
private def isCode (env : Environment) (name : Name) : Bool :=
  match env.getModuleIdxFor? name with
  | some index => env.header.moduleNames[index.toNat]!.getRoot == `Code
  | none => false

/-- Each definition of the project's code that `e` names, apart from a structure's fields and a law's statement. -/
private def definitionsIn (e : Expr) : MetaM (Array Name) := do
  let env ← getEnv
  let mut found := #[]
  for name in e.getUsedConstants do
    if isCode env name && (env.find? name).any (·.isDefinition) && !env.isProjectionFn name &&
        !lawAttribute.hasTag env name && !found.contains name then
      found := found.push name
  return found

/-- Each definition `e` names, and each one those definitions name, transitively. -/
private partial def reached (e : Expr) : MetaM (Array Name) := do
  let mut found := #[]
  let mut pending ← definitionsIn e
  while !pending.isEmpty do
    let name := pending.back!
    pending := pending.pop
    if found.contains name then continue
    found := found.push name
    if let some value := (← getEnv).find? name |>.bind (·.value?) then
      pending := pending ++ (← definitionsIn value)
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
on what is left, or `decide`. A case hypothesis keeps its `¬ x ≤ y`, so it still decides the `if` the definition
tests. -/
private def auto (definitions : Array Name) : TacticM Unit := do
  let kept : Array SimpItem := #[⟨(← `(Lean.Parser.Tactic.simpErase| -Int64.not_le)).raw⟩,
    ⟨(← `(Lean.Parser.Tactic.simpErase| -Int.not_le)).raw⟩]
  let items := (← definitions.mapM lemma) ++ kept
  evalTactic (← `(tactic| first
    | ((try simp_all [$items,*])
       first | done | omega | grind)
    | decide))

/-- Runs `step` on the proof state, and returns `true` when it closes every goal, or `false` with the state as it was. -/
private def closes (step : TacticM Unit) : TacticM Bool := do
  let saved ← saveState
  try
    step
    if (← getUnsolvedGoals).isEmpty then return true
    saved.restore
    return false
  catch _ =>
    saved.restore
    return false

/-- The main goal as the law reads it, as a tree: each definition the claim calls unfolded once, each of `conditions`
applied, and each value a step binds written where it is used. -/
private def presented (conditions : Array Ident) : TacticM Json := do
  let saved ← saveState
  for definition in ← definitionsIn (← getMainTarget) do unfoldOnce definition
  for condition in conditions do
    try evalTactic (← `(tactic| simp only [pure_inj, Bool.not_eq_true] at $condition:ident)) catch _ => pure ()
  let items := (← #[``bind_assoc, ``pure_bind, ``Bool.false_eq_true].mapM lemma) ++
    (← conditions.mapM fun condition => do
      let item : SimpItem := ⟨(← `(Lean.Parser.Tactic.simpLemma| $condition:ident)).raw⟩
      return item)
  try evalTactic (← `(tactic| simp only [↓reduceIte, $items,*])) catch _ => pure ()
  let goal ← getMainGoal
  let presented ← goal.withContext do tree (← goal.getType)
  saved.restore
  return presented

/-- Reports the main goal of the case `index`, with `conditions` applied, and admits it. -/
private def report (index : TSyntax `num) (conditions : Array Ident) : TacticM Unit := do
  let goal ← presented conditions
  logError m!"sharp-case {index.getNat} {goal.compress}"
  admitGoal (← getMainGoal)

/-- Proves a law without a body, whose case `index` is its whole claim: unfolds a recursive definition the claim reaches
once, and every other one as `simp` rewrites, then runs the automatic steps. -/
syntax "sharp_prove " num " [" ident,* "]" : tactic

elab_rules : tactic
  | `(tactic| sharp_prove $index [$conditions,*]) => do
    let definitions ← reached (← getMainTarget)
    let recursive ← definitions.filterM fun definition => isRecursiveDefinition definition
    let step : TacticM Unit := do
      for definition in recursive do unfoldOnce definition
      auto ((definitions.filter (!recursive.contains ·)) ++ (← lawsInScope))
    unless ← closes step do report index conditions.getElems

/-- Closes the case `index` of a body with the automatic steps and the laws of the facts in scope: on the claim as the
facts state it, else with each definition the claim calls unfolded once. -/
syntax "sharp_close " num " [" ident,* "]" : tactic

elab_rules : tactic
  | `(tactic| sharp_close $index [$conditions,*]) => do
    let unfolded : TacticM Unit := do
      for definition in ← definitionsIn (← getMainTarget) do unfoldOnce definition
      auto (← lawsInScope)
    unless ← closes (do auto (← lawsInScope)) <||> closes unfolded do report index conditions.getElems

/-- Proves the condition of the law the call `index` calls with the automatic steps. Each part of a condition joined by
`&&` that stays open is reported, as one goal of the call, and admitted. -/
syntax "sharp_condition " num : tactic

elab_rules : tactic
  | `(tactic| sharp_condition $index) => do
    if ← closes (do auto (← lawsInScope)) then return
    try evalTactic (← `(tactic| simp only [pure_inj, Bool.and_eq_true])) catch _ => pure ()
    try evalTactic (← `(tactic| repeat' apply And.intro)) catch _ => pure ()
    let mut open_ := #[]
    for goal in ← getUnsolvedGoals do
      setGoals [goal]
      if ← closes (do auto (← lawsInScope)) then continue
      open_ := open_.push (← goal.withContext do tree (← goal.getType))
      admitGoal goal
    logError m!"sharp-condition {index.getNat} {(Json.arr open_).compress}"

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
