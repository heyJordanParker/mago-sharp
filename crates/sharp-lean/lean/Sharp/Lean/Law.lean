import Lean.Attributes
import Sharp.Lean.Int

/-!
The statement of a PHP# law.

A law holds when its expression never finishes with `false`. A throw or a loop that never ends proves nothing about
it, so it does not break the law: `a.add(b)` throws `ArithmeticError` on overflow, and `addKeepsCurrency` still holds.
-/

namespace Sharp

/-- Marks the statement of a PHP# law. The runner finds every law by it. -/
initialize lawAttribute : Lean.TagAttribute ←
  Lean.registerTagAttribute `law "the statement of a PHP# law"

/-- A law's expression holds unless it finishes with `false`. -/
def holds (x : M Bool) : Prop :=
  match x.run with
  | some (.ok b) => b = true
  | _ => True

instance (x : M Bool) : Decidable (holds x) := by
  unfold holds; split <;> infer_instance

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

end Sharp

deriving instance DecidableEq for Except
