import Sharp.Lean.Int

/-!
The statement of a PHP# law.

A law holds when its expression never finishes with `false`. A throw or a loop that never ends proves nothing about
it, so it does not break the law: `a.add(b)` throws `ArithmeticError` on overflow, and `addKeepsCurrency` still holds.

The law's claim and its condition compute ints exactly (decision 057).

A law's statement is the one definition of the project's code whose type is a `Prop`, so this module imports nothing of
Lean's own beyond `Init`, and a runner that checks laws imports no more of Lean than they reach.
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

end Sharp

deriving instance DecidableEq for Except
