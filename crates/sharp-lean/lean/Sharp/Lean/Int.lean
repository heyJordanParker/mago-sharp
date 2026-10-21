/-!
PHP# `int`, and the monad translated code runs in.

Translated code runs in `M := ExceptT Throwable Option`: `.error` is an engine error the code throws, and `none` is
a loop that never ends, as Aeneas's `div` is.

`int` is core `Int64`, so every value already has PHP's range. Each operation computes the exact `Int` result and
throws `ArithmeticError` when it leaves `[PHP_INT_MIN, PHP_INT_MAX]`, as PHP# throws on overflow. This is Aeneas's
`IScalar.tryMk` shape on Lean core types. Core `Int64` already owns `+ - *` with wrapping semantics, so translated
code calls `int.add`, `int.sub` and `int.mul` by name instead of using the operators.
-/

namespace Sharp

/-- The engine errors pure PHP# code can throw. Each case is named after the PHP class the engine throws. -/
inductive Throwable where
  /-- An `int` result left `[PHP_INT_MIN, PHP_INT_MAX]`. -/
  | ArithmeticError
  /-- `x / 0` and `x % 0`. -/
  | DivisionByZeroError
  /-- A `match` with no arm for its value. -/
  | UnhandledMatchError
  deriving Repr, DecidableEq

/-- The monad translated code runs in. -/
abbrev M := ExceptT Throwable Option

/-- PHP# `int`, a 64-bit signed integer. -/
abbrev int := Int64

namespace int

/-- The exact `Int` as an `int`, or `ArithmeticError` when it is outside the `int` range. -/
def ofInt (i : Int) : M int :=
  if h : Int64.minValue.toInt ≤ i ∧ i ≤ Int64.maxValue.toInt then
    pure (Int64.ofIntLE i h.1 h.2)
  else
    throw .ArithmeticError

def add (x y : int) : M int := ofInt (x.toInt + y.toInt)

def sub (x y : int) : M int := ofInt (x.toInt - y.toInt)

def mul (x y : int) : M int := ofInt (x.toInt * y.toInt)

def neg (x : int) : M int := ofInt (-x.toInt)

/-- PHP# `/` on two ints: truncates toward zero, as `Int.tdiv` does. -/
def div (x y : int) : M int :=
  if y = 0 then throw .DivisionByZeroError else ofInt (x.toInt.tdiv y.toInt)

/-- PHP's `%`: the remainder of the division toward zero, with the sign of `x`. -/
def mod (x y : int) : M int :=
  if y = 0 then throw .DivisionByZeroError else ofInt (x.toInt.tmod y.toInt)

/-! The bounds as numerals, so `omega` can read them. -/

@[simp] theorem minValue_toInt : Int64.minValue.toInt = -9223372036854775808 := rfl
@[simp] theorem maxValue_toInt : Int64.maxValue.toInt = 9223372036854775807 := rfl

/-- Every `int` lies in the `int` range. `omega` and `grind` do not know this by themselves. The `grind_pattern`
makes `grind` add it for every `x.toInt` in a goal. -/
theorem toInt_bounds (x : int) :
    -9223372036854775808 ≤ x.toInt ∧ x.toInt ≤ 9223372036854775807 :=
  ⟨x.minValue_le_toInt, x.toInt_le⟩

grind_pattern toInt_bounds => x.toInt

/-! A law's automatic steps read an `int` result as the exact `Int` it holds, so `simp` and `omega` reason about it, and
two `int`s as equal when their `Int`s are. -/

private theorem pure_ne_throw (x : int) (e : Throwable) : (pure x : M int) ≠ throw e := by
  intro h
  have := congrArg ExceptT.run h
  simp [ExceptT.run_pure, ExceptT.run_throw] at this

@[simp] theorem ofInt_eq_pure {i : Int} {x : int} :
    ofInt i = pure x ↔ (-9223372036854775808 ≤ i ∧ i ≤ 9223372036854775807) ∧ x.toInt = i := by
  unfold ofInt
  split
  · rename_i h
    simp only [minValue_toInt, maxValue_toInt] at h
    constructor
    · intro e
      have := congrArg ExceptT.run e
      simp [ExceptT.run_pure] at this
      subst this
      exact ⟨h, by simp⟩
    · rintro ⟨_, hx⟩
      congr 1
      apply Int64.toInt_inj.mp
      simp [hx]
  · rename_i h
    simp only [minValue_toInt, maxValue_toInt] at h
    constructor
    · intro e
      exact absurd e.symm (pure_ne_throw _ _)
    · rintro ⟨inside, _⟩
      exact absurd inside h

@[simp] theorem add_eq_pure {x y z : int} :
    add x y = pure z ↔ (-9223372036854775808 ≤ x.toInt + y.toInt ∧ x.toInt + y.toInt ≤ 9223372036854775807) ∧
      z.toInt = x.toInt + y.toInt := by
  simp [add]

@[simp] theorem sub_eq_pure {x y z : int} :
    sub x y = pure z ↔ (-9223372036854775808 ≤ x.toInt - y.toInt ∧ x.toInt - y.toInt ≤ 9223372036854775807) ∧
      z.toInt = x.toInt - y.toInt := by
  simp [sub]

@[simp] theorem mul_eq_pure {x y z : int} :
    mul x y = pure z ↔ (-9223372036854775808 ≤ x.toInt * y.toInt ∧ x.toInt * y.toInt ≤ 9223372036854775807) ∧
      z.toInt = x.toInt * y.toInt := by
  simp [mul]

@[simp] theorem neg_eq_pure {x z : int} :
    neg x = pure z ↔ (-9223372036854775808 ≤ -x.toInt ∧ -x.toInt ≤ 9223372036854775807) ∧ z.toInt = -x.toInt := by
  simp [neg]

attribute [simp] Int64.le_iff_toInt_le Int64.lt_iff_toInt_lt
attribute [simp ←] Int64.toInt_inj

/-! A method that calls itself on `n - 1` where a condition has shown `n` above a bound, or on `n + 1` where one has
shown it below, passes that step as `Int64`'s own `-` or `+`, which cannot leave the `int` range there, so Lean sees the
argument it recurses on. -/

theorem toInt_sub_one {x : int} (h : -9223372036854775808 < x.toInt) : (x - 1).toInt = x.toInt - 1 := by
  have := toInt_bounds x
  rw [Int64.toInt_sub, Int.bmod_eq_of_le] <;> simp <;> omega

theorem toInt_add_one {x : int} (h : x.toInt < 9223372036854775807) : (x + 1).toInt = x.toInt + 1 := by
  have := toInt_bounds x
  rw [Int64.toInt_add, Int.bmod_eq_of_le] <;> simp <;> omega

/-- A List's `count()`. No list PHP holds has more elements than an `int` counts, so a longer Lean list counts
`PHP_INT_MAX`, and every count is a value that can be read anywhere, in a lambda as in a condition. -/
def count {α : Type} (l : List α) : int := Int64.ofNat (min l.length 9223372036854775807)

@[simp] theorem toInt_count {α : Type} (l : List α) :
    (count l).toInt = min (l.length : Int) 9223372036854775807 := by
  unfold count
  rw [Int64.toInt_ofNat_of_lt (by omega)]
  omega

/-! A count is the list's length wherever the length is known to fit an `int`: for a list the law writes out, and
under a condition that bounds it. -/

attribute [simp] Int.le_min Int.min_eq_left

/-! The standard library's `Math.max` and `Math.min` are Lean's `max` and `min` on `int`, which never leave the `int`
range. A law's automatic steps read each as `max` and `min` of the `Int`s, which `omega` reasons about. -/

@[simp] theorem toInt_max (x y : int) : (max x y).toInt = max x.toInt y.toInt := by
  show (if x ≤ y then y else x).toInt = _
  rw [Int.max_def]
  by_cases h : x ≤ y
  · rw [ite_eq_left h, ite_eq_left (Int64.le_iff_toInt_le.mp h)]
  · rw [ite_eq_right h, ite_eq_right (fun e => h (Int64.le_iff_toInt_le.mpr e))]

@[simp] theorem toInt_min (x y : int) : (min x y).toInt = min x.toInt y.toInt := by
  show (if x ≤ y then x else y).toInt = _
  rw [Int.min_def]
  by_cases h : x ≤ y
  · rw [ite_eq_left h, ite_eq_left (Int64.le_iff_toInt_le.mp h)]
  · rw [ite_eq_right h, ite_eq_right (fun e => h (Int64.le_iff_toInt_le.mpr e))]

end int

/-- Proves a recursive call's argument smaller than the value it replaces, in the measure `termination_by` names: an
`int` one step towards a bound that a condition on the call's path tested, or the rest of a list pattern. -/
macro "smaller_value" : tactic => `(tactic| (
  simp_all only [decide_eq_true_eq, Bool.not_eq_true, decide_eq_false_iff_not, Bool.and_eq_true,
    Bool.or_eq_false_iff, Bool.or_eq_true, beq_iff_eq, Int64.le_iff_toInt_le, Int64.lt_iff_toInt_lt, List.length_cons,
    List.length_take]
  first
    | omega
    | (rw [Sharp.int.toInt_sub_one (by grind)]; grind)
    | (rw [Sharp.int.toInt_add_one (by grind)]; grind)))

end Sharp
