import Sharp.Lean.Int

/-!
PHP# `Map<TKey, TValue>` (spec section 12): its entries in the order their keys were first set, each key once.

A law reads a `Map` with `get`, `has`, `keys()`, `values()`, `count()`, `isEmpty()`, `isNotEmpty()`, `filter` and `any`
of a lambda, a read `map[key]`, `==` on two maps and a loop over its entries.
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

/-- `any(predicate)`: whether `predicate` holds of the value of an entry. -/
def any (predicate : ν → Bool) (map : Map κ ν) : Bool := map.entries.any (predicate ·.2)

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

/-- `==` on two `Map`s, spec section 19: they hold as many entries, and each key of `a`, in `a`'s order, is a key of `b`
whose value `equal`, the values' own `==`, takes as equal to its own, compared until one pair is not equal, as the
engine compares them. -/
def equalBy (equal : ν → ν → M Bool) (a b : Map κ ν) : M Bool :=
  if a.entries.length = b.entries.length then entries a.entries else pure false
where
  entries : List (κ × ν) → M Bool
    | [] => pure true
    | (key, value) :: rest => match b.get key with
      | some other => do if (← equal value other) then entries rest else pure false
      | none => pure false

/-- The keys of a map are distinct, so the key of each entry finds that entry's value. -/
theorem lookup_of_mem : ∀ {entries : List (κ × ν)} {key : κ} {value : ν},
    (entries.map Prod.fst).Nodup → (key, value) ∈ entries → entries.lookup key = some value
  | [], _, _, _, held => by simp at held
  | (k, v) :: rest, key, value, distinct, held => by
    simp only [List.map_cons, List.nodup_cons, List.mem_map] at distinct
    by_cases same : key = k
    · subst same
      simp only [List.mem_cons, Prod.mk.injEq, true_and] at held
      rcases held with rfl | held
      · simp [List.lookup]
      · exact absurd ⟨(key, value), held, rfl⟩ distinct.1
    · have other : (key == k) = false := by simp [same]
      simp only [List.mem_cons, Prod.mk.injEq, same, false_and, false_or] at held
      simp only [List.lookup, other]
      exact lookup_of_mem distinct.2 held

/-- A map equals itself entry by entry when each value equals itself, as `x == x` holds of every value of a type
whose `==` is its values' own. -/
theorem equalBy.entries_self {equal : ν → ν → M Bool} (h : ∀ x, equal x x = pure true) (map : Map κ ν) :
    ∀ (l : List (κ × ν)), l ⊆ map.entries → equalBy.entries equal map l = pure true
  | [], _ => rfl
  | (key, value) :: rest, within => by
    have found : map.get key = some value :=
      lookup_of_mem map.distinct (within (List.mem_cons_self ..))
    simp only [equalBy.entries, found, h, ite_true, pure_bind]
    exact equalBy.entries_self h map rest (fun _ held => within (List.mem_cons_of_mem _ held))

@[simp] theorem equalBy.entries_all {equal : ν → ν → M Bool} (h : ∀ x, equal x x = pure true) (map : Map κ ν) :
    equalBy.entries equal map map.entries = pure true :=
  equalBy.entries_self h map map.entries (fun _ held => held)

instance [DecidableEq ν] : DecidableEq (Map κ ν) := fun a b =>
  if h : a.entries = b.entries then
    isTrue (by cases a; cases b; simp_all)
  else
    isFalse (fun e => h (e ▸ rfl))

/-! The automatic steps read each member as the list function of the entries it is, and a map a law writes out as
its entries. -/

attribute [simp] get has keys values count isEmpty isNotEmpty filter any equalBy set ofList empty setIn

/-! A `Map` of nullable values reads a missing key and a stored null alike, as `Option.join` of what `get` finds, which
the automatic steps split into the two. -/

attribute [simp] Option.join_eq_none_iff
attribute [grind →] mem_of_lookup

end Map

end Sharp
