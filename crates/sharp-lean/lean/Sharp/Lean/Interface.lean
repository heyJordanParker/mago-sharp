/-!
A PHP# interface: the type of the objects that implement it.

A law that reaches an interface holds for every class that implements it, today's and every later one, so Lean knows
nothing about the interface's values. A law passes them along and passes a class's value where the interface is
expected, through `upcast`. It never calls their methods, since an implementation can have effects.
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
what they are, nor whether two interfaces' values are the same type. -/
opaque values (name : String) : Values

/-- The PHP# interface whose Lean name is `name`. -/
abbrev Interface (name : String) : Type := (values name).type

instance : Inhabited (Interface name) := (values name).inhabited
instance : DecidableEq (Interface name) := (values name).decidableEq
instance : Repr (Interface name) := (values name).repr

/-- A class's value as a value of an interface it implements, which each translated class declares as a `Coe`. Lean
knows nothing about the interface value it gives, as about every interface value. -/
opaque upcast {α β : Type} [Inhabited β] : α → β

end Sharp
