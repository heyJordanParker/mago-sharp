import Sharp.Lean.Int
import Sharp.Lean.Steps

/-!
The statement of a PHP# law.

A law holds when its expression never finishes with `false`. A throw or a loop that never ends proves nothing about
it, so it does not break the law: `a.add(b)` throws `ArithmeticError` on overflow, and `addKeepsCurrency` still holds.

The law's claim and its condition compute ints exactly (decision 057).

A law's statement is the one definition of the project's code whose type is a `Prop`. This module imports Lean's own
modules beyond `Init` only for `Sharp/Lean/Steps.lean`: its simp sets, which this module's end tags the fixed steps'
lemmas with, and `sharp_simp_all`, which the fixed steps run.
-/

namespace Sharp

/-- A law's expression holds unless it finishes with `false`. -/
def holds (x : M Bool) : Prop :=
  match x.run with
  | some (.ok b) => b = true
  | _ => True

instance (x : M Bool) : Decidable (holds x) := by
  unfold holds; split <;> infer_instance

@[simp] theorem pure_inj {α : Type} (a b : α) : (pure a : M α) = pure b ↔ a = b := by
  constructor
  · intro h
    have := congrArg ExceptT.run h
    simp [ExceptT.run_pure] at this
    exact this
  · intro h
    rw [h]

@[simp] theorem holds_pure (b : Bool) : holds (pure b) ↔ b = true := Iff.rfl

@[simp] theorem holds_throw (e : Throwable) : holds (throw e : M Bool) := trivial

private theorem eq_pure {α : Type} (x : M α) (a : α) : x = pure a ↔ x.run = some (.ok a) :=
  ⟨fun h => h ▸ rfl, fun h => (ExceptT.ext h : x = ExceptT.mk (some (.ok a)))⟩

@[simp] theorem holds_bind {α : Type} (x : M α) (f : α → M Bool) :
    holds (x >>= f) ↔ ∀ a, x = pure a → holds (f a) := by
  simp only [eq_pure, holds, ExceptT.run_bind]
  cases hx : x.run with
  | none => simp
  | some r => cases r <;> simp

/-- `simp` rewrites `x >>= fun a => pure (f a)` to `f <$> x` first, so `holds_bind` alone leaves it. -/
@[simp] theorem holds_map {α : Type} (x : M α) (f : α → Bool) :
    holds (f <$> x) ↔ ∀ a, x = pure a → f a = true := by
  simp only [eq_pure, holds, ExceptT.run_map]
  cases hx : x.run with
  | none => simp
  | some r => cases r <;> simp [Except.map]

/-- A fact about the values code computes, as a law's `when` and a body's facts state, reads them one by one, as
`holds_bind` reads a law's claim. -/
@[simp] theorem bind_eq_pure {α β : Type} (x : M α) (f : α → M β) (b : β) :
    x >>= f = pure b ↔ ∃ a, x = pure a ∧ f a = pure b := by
  simp only [eq_pure, ExceptT.run_bind]
  cases hx : x.run with
  | none => simp
  | some r => cases r <;> simp

/-- A step that maps what it reads, as a template of an element a list read gives, gives a value only when the read
does. `grind` reads it too, in each case of an `if` it splits in a fact. -/
@[simp, grind =] theorem map_eq_pure {α β : Type} (f : α → β) (x : M α) (b : β) :
    f <$> x = pure b ↔ ∃ a, x = pure a ∧ f a = b := by
  rw [← bind_pure_comp, bind_eq_pure]
  simp

@[simp] theorem throw_eq_pure {α : Type} (e : Throwable) (b : α) : (throw e : M α) = pure b ↔ False := by
  constructor
  · intro h
    have := congrArg ExceptT.run h
    simp [ExceptT.run_pure] at this
  · intro h
    exact h.elim

/-- A value one of two branches gives, as `? :` gives it, is the one each branch gives where its condition holds. The
value comes first, so a step that binds it reads as that value in each case. -/
@[simp] theorem ite_eq_pure {α : Type} (c : Prop) [Decidable c] (x y : M α) (b : α) :
    (if c then x else y) = pure b ↔ (x = pure b ∧ c) ∨ (y = pure b ∧ ¬c) := by
  by_cases h : c <;> simp [h]

/-- A claim that chooses, as `||` and `? :` do, holds when each choice holds where it is made. -/
@[simp] theorem holds_ite (c : Prop) [Decidable c] (x y : M Bool) :
    holds (if c then x else y) ↔ (c → holds x) ∧ (¬c → holds y) := by
  by_cases h : c <;> simp [h]

/-! A counting loop is `List.countP` of its list, so a law over it reads that it counts no more than the list holds. -/

attribute [simp] List.countP_le_length

/-! `isEmpty()` of a `List` and of a `Map` reads as the count a law's claim computes exactly, `count() == 0`, and a list
`filter` leaves empty as one none of whose values the lambda keeps. -/

@[grind =] theorem isEmpty_eq_length {α : Type} (l : List α) : l.isEmpty = decide ((l.length : Int) = 0) := by
  cases l <;> simp <;> omega

attribute [grind =] List.filter_eq_nil_iff

/-!
The search for the moves that break an invariant of a PHP# machine (spec section 20.2), which the proof of the invariant
runs. It evaluates the machine's code on small values, so what it finds is a run of the code, and it proves nothing.
-/

namespace Machine

/-- The values `steps` give after `trace`, each with its trace: `trace`, then the step as written and the value it gives
as `shown` writes it, as `pay(cents: 0) → Paid(cents: 0)`. A step that throws gives none. -/
def next {S : Type} (shown : S → String) (trace : List String) (steps : List (String × M S)) :
    List (List String × S) :=
  steps.filterMap fun (written, step) => match step.run with
    | some (.ok value) => some (trace ++ [s!"{written} → {shown value}"], value)
    | _ => none

/-- Whether `claim` finishes with `false`. -/
def breaks (claim : M Bool) : Bool :=
  match claim.run with
  | some (.ok false) => true
  | _ => false

/-- A PHP# `string` as PHP# writes it, `"a"`. -/
def text (bytes : List UInt8) : String :=
  s!"\"{(String.fromUTF8? ⟨bytes.toArray⟩).getD "?"}\""

/-- The trace of the first of `reached` that `broken` holds of, else of the first value the steps `moves` gives each
value make, breadth-first, at most `depth` steps on. -/
def search {S : Type} (moves : S → List (String × M S)) (shown : S → String) (broken : S → Bool) :
    Nat → List (List String × S) → Option (List String)
  | 0, reached => (reached.find? (broken ·.2)).map (·.1)
  | depth + 1, reached => match reached.find? (broken ·.2) with
    | some (trace, _) => some trace
    | none => search moves shown broken depth (reached.flatMap fun (trace, value) => next shown trace (moves value))

end Machine

/-!
The search for the small values that break a law whose proof stays open (spec section 28), which the law checker's
runner evaluates. It runs the law's claim on them, so what it finds is a run of the code, and it proves nothing.
-/

namespace Law

/-- The first value of the parameter `name` among `values`, each as PHP# writes it with the computation that gives it,
for which `rest` finds values of the later parameters that break the law, as written: `name = value`, then those, as
`a = new Account(0), amount = -1`. A value whose computation throws is left out, and the search stops at the first. -/
def first {α : Type} (name : String) (values : List (String × M α)) (rest : α → Option String) : Option String :=
  values.findSome? fun (written, value) => match value.run with
    | some (.ok held) => (rest held).map fun later =>
      if later.isEmpty then s!"{name} = {written}" else s!"{name} = {written}, {later}"
    | _ => none

/-- `""` when `claim`, the law's claim on the values its parameters hold, finishes with `false`, and else none. -/
def broken (claim : M Bool) : Option String :=
  if Machine.breaks claim then some "" else none

end Law

/-! The lemmas the fixed steps rewrite with, each in the simp set of each pass that rewrites with it, as `src/lean.rs`
names the sets. -/

attribute [sharp_step, sharp_full] bind_assoc pure_bind holds_bind holds_pure holds_ite pure_inj
  bind_eq_pure map_eq_pure ite_eq_pure throw_eq_pure bne_iff_ne ne_eq beq_iff_eq beq_self_eq_true decide_eq_true_eq
  Bool.and_eq_true Option.isSome_some Option.getD_some

attribute [sharp_step ←, sharp_full ←] Int64.not_lt Int.not_lt

attribute [sharp_step, sharp_full] or_imp forall_and forall_exists_index and_imp forall_eq' implies_true false_implies

attribute [sharp_full] int.add_eq_pure int.sub_eq_pure int.toInt_max Int64.le_iff_toInt_le
  Int64.lt_iff_toInt_lt Int64.toInt_zero

end Sharp

deriving instance DecidableEq for Except
