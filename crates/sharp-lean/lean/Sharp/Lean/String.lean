import Sharp.Lean.Int

/-!
PHP# `string`: the bytes it holds.

A PHP# string holds bytes and compares them byte by byte, and Lean's `String` holds only valid UTF-8, so a law proved
over `String` would skip strings PHP# code can pass. A byte list also reduces under `decide`.
-/

namespace Sharp

/-- PHP# `string`. -/
abbrev string := List UInt8

/-- `(string)` of an `int`: its decimal digits, after `-` when it is negative, as PHP writes it. -/
def int.text (n : int) : string := (toString n.toInt).toUTF8.toList

#guard int.text 7 = [55]
#guard int.text (-42) = [45, 52, 50]

end Sharp
