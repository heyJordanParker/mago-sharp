/-!
PHP# `string`: the bytes it holds.

A PHP# string holds bytes and compares them byte by byte, and Lean's `String` holds only valid UTF-8, so a law proved
over `String` would skip strings PHP# code can pass. A byte list also reduces under `decide`.
-/

namespace Sharp

/-- PHP# `string`. -/
abbrev string := List UInt8

end Sharp
