/-!
A PHP# interface: the type of the objects that implement it.

A law that reaches an interface holds for every class that implements it, today's and every later one, so Lean knows
nothing about the interface's values. Each method of the interface a law calls is an `opaque` function of them, as
`App.Shop.Payment.amount : App.Shop.Payment → M int`, so a proof holds whatever each implementation returns.
-/

namespace Sharp

/-- A type, with the instances a translated class derives for each of its properties. -/
structure Values where
  type : Type
  [inhabited : Inhabited type]
  [decidableEq : DecidableEq type]
  [repr : Repr type]

instance : Inhabited Values := ⟨⟨Unit⟩⟩

/-- The values of the interface whose Lean name is `name`. Lean's kernel never unfolds an `opaque`, so no proof learns
what they are, and two interfaces' values are never the same type. -/
opaque values (name : String) : Values

/-- The PHP# interface whose Lean name is `name`. -/
abbrev Interface (name : String) : Type := (values name).type

instance : Inhabited (Interface name) := (values name).inhabited
instance : DecidableEq (Interface name) := (values name).decidableEq
instance : Repr (Interface name) := (values name).repr

end Sharp
