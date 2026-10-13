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

end int

end Sharp
