import Sharp.Lean.Int

/-!
PHP# `Map<TKey, TValue>` (spec section 12): its entries in the order their keys were first set, each key once.

A law reads a `Map` with `get`, `has`, `keys()`, `values()`, `count()`, `isEmpty()`, `isNotEmpty()`, `filter` of a
lambda, a read `map[key]` and a loop over its entries.
Each is a function of the entries, which the automatic steps rewrite to Lean's own list functions.
-/

namespace Sharp

/-- A PHP# `Map`: its entries in the order their keys were first set, whose keys are distinct, as the type carries. -/
structure Map (κ ν : Type) where
  entries : List (κ × ν)
  distinct : (entries.map Prod.fst).Nodup

namespace Map

variable {κ ν : Type} [DecidableEq κ]

/-- `entries` with `key` set to `value`: the value of an equal key replaced in its place, or the entry added last. -/
def setIn (key : κ) (value : ν) : List (κ × ν) → List (κ × ν)
  | [] => [(key, value)]
  | (k, v) :: rest => if k = key then (key, value) :: rest else (k, v) :: setIn key value rest

theorem keys_setIn (key : κ) (value : ν) (entries : List (κ × ν)) :
    (setIn key value entries).map Prod.fst =
      if key ∈ entries.map Prod.fst then entries.map Prod.fst else entries.map Prod.fst ++ [key] := by
  induction entries with
  | nil => simp [setIn]
  | cons entry rest ih =>
    obtain ⟨k, v⟩ := entry
    by_cases h : k = key
    · subst h
      simp [setIn]
    · have other : ¬key = k := fun e => h e.symm
      by_cases m : key ∈ rest.map Prod.fst <;> simp_all [setIn]

theorem distinct_setIn (key : κ) (value : ν) {entries : List (κ × ν)} (distinct : (entries.map Prod.fst).Nodup) :
    ((setIn key value entries).map Prod.fst).Nodup := by
  rw [keys_setIn]
  split
  · exact distinct
  · rename_i missing
    refine List.nodup_append.mpr ⟨distinct, by simp, fun k present other added => ?_⟩
    rw [List.mem_singleton] at added
    subst added
    intro same
    subst same
    exact missing present

/-- The empty `Map`, `[:]`. -/
def empty : Map κ ν := ⟨[], List.nodup_nil⟩

instance : Inhabited (Map κ ν) := ⟨empty⟩

/-- `map[key] = value`: the value of an equal key replaced in its place, or the entry added last. -/
def set (map : Map κ ν) (key : κ) (value : ν) : Map κ ν :=
  ⟨setIn key value map.entries, distinct_setIn key value map.distinct⟩

/-- The `Map` a literal writes, `[key: value, ...spread]`: each entry set in order, so a later key replaces an earlier
one's value. -/
def ofList (entries : List (κ × ν)) : Map κ ν :=
  entries.foldl (fun map (key, value) => map.set key value) empty

/-- `get(key)` and `map[key]`: the value of the equal key, or none. -/
def get (map : Map κ ν) (key : κ) : Option ν := map.entries.lookup key

/-- `has(key)`: whether an equal key is in the map. -/
def has (map : Map κ ν) (key : κ) : Bool := (map.get key).isSome

/-- `keys()`, in the map's order. -/
def keys (map : Map κ ν) : List κ := map.entries.map Prod.fst

/-- `values()`, in the map's order. -/
def values (map : Map κ ν) : List ν := map.entries.map Prod.snd

/-- `count()`, as a List's `count()` counts its entries. -/
def count (map : Map κ ν) : int := int.count map.entries

/-- `isEmpty()`: whether the map holds no entry. -/
def isEmpty (map : Map κ ν) : Bool := map.entries.isEmpty

/-- `isNotEmpty()`: whether the map holds an entry. -/
def isNotEmpty (map : Map κ ν) : Bool := !map.entries.isEmpty

/-- `filter(predicate)`: the entries whose values `predicate` keeps, in the map's order. -/
def filter (predicate : ν → Bool) (map : Map κ ν) : Map κ ν :=
  ⟨map.entries.filter (predicate ·.2), map.distinct.sublist (List.filter_sublist.map Prod.fst)⟩

/-- The value `get` finds is the value of an entry, so what holds of every entry, as what `filter` drops, holds of it. -/
theorem mem_of_lookup : ∀ {entries : List (κ × ν)} {key : κ} {value : ν},
    entries.lookup key = some value → (key, value) ∈ entries
  | [], _, _, found => by simp [List.lookup] at found
  | (k, v) :: rest, key, value, found => by
    by_cases same : key = k
    · subst same
      simp [List.lookup] at found
      simp [found]
    · have other : (key == k) = false := by simp [same]
      simp only [List.lookup, other] at found
      exact List.mem_cons_of_mem _ (mem_of_lookup found)

instance [DecidableEq ν] : DecidableEq (Map κ ν) := fun a b =>
  if h : a.entries = b.entries then
    isTrue (by cases a; cases b; simp_all)
  else
    isFalse (fun e => h (e ▸ rfl))

/-! The automatic steps read each member as the list function of the entries it is, and a map a law writes out as
its entries. -/

attribute [simp] get has keys values count isEmpty isNotEmpty filter set ofList empty setIn

/-! A `Map` of nullable values reads a missing key and a stored null alike, as `Option.join` of what `get` finds, which
the automatic steps split into the two. -/

attribute [simp] Option.join_eq_none_iff
attribute [grind →] mem_of_lookup

end Map

end Sharp
