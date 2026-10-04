/-
The fixed library every generated structure module imports.

The checker emits the code's structure as `Sharp.declarations`, `Sharp.imports` and
`Sharp.references` in a generated `Sharp.lean`. Architecture rules are theorems over those lists.

Every function here works on lists of UTF-8 bytes, never on `String.startsWith`,
`String.splitOn` or `String.toList`. In Lean 4.34 `decide` gets stuck on `startsWith`,
`decide +kernel` gets stuck on `splitOn`, and `toList` decodes UTF-8 by well-founded recursion,
which costs the kernel 15.5 s over 190 imports where the bytes cost 0.3 s. Byte lists reduce under
`decide`, `decide +kernel` and compiled code alike. When the kernel rejects a false rule,
`decide +kernel` reduces it again in the elaborator and reports "proved that the proposition is
false" only if that reduction finishes; when it hits a limit such as `maxRecDepth`, as on Laravel's
5,941 imports, Lean reports the kernel's own error, "(kernel) application type mismatch".
-/
import Lean.Elab.Tactic.Basic
import Lean.Meta.Eval

namespace Sharp

/-- Source range of a fact, as CodeQL's `locations_default`: 1-based, inclusive, columns count bytes. -/
structure Location where
  file : String          -- workspace-relative path, e.g. "src/App/Shared/Money.php"
  startLine : Nat
  startColumn : Nat
  endLine : Nat
  endColumn : Nat
  deriving Repr, DecidableEq

inductive SymbolKind where
  | classLike | function | constant
  deriving Repr, DecidableEq

inductive DeclarationKind where
  | «class» | interface | «trait» | «enum» | function | constant
  deriving Repr, DecidableEq

structure Declaration where
  name : String          -- fully qualified, dot separated: "App.Shared.Money"
  kind : DeclarationKind
  location : Location    -- the declared name
  deriving Repr, DecidableEq

structure Import where
  namespace_ : String    -- namespace whose code holds the `use` statement; "" is the global namespace
  name : String          -- imported fully qualified name, dot separated
  kind : SymbolKind      -- `use`, `use function`, `use const`
  location : Location    -- the imported name inside the `use` item
  deriving Repr, DecidableEq

structure Reference where
  namespace_ : String    -- namespace whose code holds the reference
  name : String          -- referenced fully qualified name, dot separated
  location : Location
  deriving Repr, DecidableEq

/-- The UTF-8 bytes of `s`. The kernel reads them straight off a string literal. -/
private def bytes (s : String) : List UInt8 :=
  s.toByteArray.data.toList

/-- `path` without its last segment and the `separator` before it. -/
private def parent [BEq α] (separator : α) (path : List α) : List α :=
  ((path.reverse.dropWhile (· != separator)).drop 1).reverse

/-- `name` is the namespace `ns` or sits inside it: `under "App.Shared.Money" "App.Shared"`.
It compares bytes: `ns ++ "."` ends on a whole character, so a byte prefix is a character prefix. -/
def under (name ns : String) : Bool :=
  name == ns || (bytes ns ++ [('.').toUInt8]).isPrefixOf (bytes name)

/-- The namespace that holds `name`: "A.B.C" ↦ "A.B", "C" ↦ "" (the global namespace).
It decodes UTF-8, which costs the kernel about 80 ms per name, so the graph below works on bytes. -/
def namespaceOf (name : String) : String :=
  String.ofList (parent '.' name.toList)

/-- The import's edge in the namespace import graph, as UTF-8 bytes: from the importing namespace
to the namespace of the imported name. An import from the namespace's own code has no edge. -/
private def edge (i : Import) : Option (List UInt8 × List UInt8) :=
  let source := bytes i.namespace_
  let target := parent ('.').toUInt8 (bytes i.name)
  if source == target then none else some (source, target)

/-- The namespace import graph: one edge per import, self edges dropped. Duplicate edges are merged,
which changes no path and shrinks every search below. -/
private def importGraph (imports : List Import) : List (List UInt8 × List UInt8) :=
  (imports.filterMap edge).eraseDups

/-- `graph` leads from a namespace in `frontier` to `goal`, by breadth-first search that expands
only the namespaces it has not `seen`. Every namespace after the first is the target of an edge
and enters `frontier` once, so `graph.length + 1` steps cover the whole search. -/
private def reaches (graph : List (List UInt8 × List UInt8)) (goal : List UInt8) :
    Nat → List (List UInt8) → List (List UInt8) → Bool
  | 0, _, _ => false
  | fuel + 1, frontier, seen =>
    frontier.contains goal ||
      let next := (graph.filterMap fun (source, target) =>
        if frontier.contains source && !seen.contains target then some target else none).eraseDups
      !next.isEmpty && reaches graph goal fuel next (seen ++ next)

/-- The import's edge lies on a cycle: the imported namespace reaches the importing namespace again. -/
def onCycle (imports : List Import) (i : Import) : Bool :=
  match edge i with
  | none => false
  | some (source, target) =>
    let graph := importGraph imports
    reaches graph source (graph.length + 1) [target] [target]

/-- The namespace import graph has no cycle, by Kahn's algorithm. -/
def acyclic (imports : List Import) : Bool :=
  let graph := importGraph imports
  go graph.length graph
where
  /-- Drop every edge whose source no edge enters, until no edge is left or none can be dropped.
  Each step drops at least one edge, so `graph.length` steps empty an acyclic graph. -/
  go : Nat → List (List UInt8 × List UInt8) → Bool
    | _, [] => true
    | 0, _ => false
    | fuel + 1, edges =>
      let entered := (edges.map Prod.snd).eraseDups
      let kept := edges.filter fun (source, _) => entered.contains source
      kept.length < edges.length && go fuel kept

/-- A fact with a place in the PHP source, which a failed rule reports. -/
class Located (α : Type) where
  location : α → Location
  describe : α → String

instance : Located Import where
  location i := i.location
  describe i := s!"imports {i.name}"

instance : Located Reference where
  location r := r.location
  describe r := s!"references {r.name}"

instance : Located Declaration where
  location d := d.location
  describe d := s!"declares {d.name}"

/-- One GNU diagnostic line per fact in `xs` that breaks `holds`, which the checker parses from
`lean --json` to place the error on the PHP line. -/
private def diagnostics [Located α] (theoremName : String) (xs : List α) (holds : α → Bool) :
    List String :=
  (xs.filter (! holds ·)).map fun x =>
    let l := Located.location x
    s!"{l.file}:{l.startLine}.{l.startColumn}-{l.endLine}.{l.endColumn}: {Located.describe x} breaks {theoremName}"

/-- Decides `∀ x ∈ xs, p x` by `List.all`, in place of core's `List.decidableBAll`. That instance
matches on the decision for the tail, so the kernel nests about four reductions per fact and fails
past `16 * maxRecDepth` of them: from 4,073 imports at the default `maxRecDepth` of 1000.
`List.all` returns the tail's value as it is, so the kernel's depth does not grow with the list:
59,410 imports check with `maxRecDepth` at 100. -/
instance {xs : List α} {p : α → Prop} [DecidablePred p] : Decidable (∀ x ∈ xs, p x) :=
  decidable_of_iff (xs.all (p ·)) (by simp only [List.all_eq_true, decide_eq_true_eq])

open Lean Meta Elab Tactic

private unsafe def evalStringsUnsafe (e : Expr) : MetaM (List String) :=
  evalExpr (List String) (mkApp (mkConst ``List [.zero]) (mkConst ``String)) e

/-- Runs `e : List String` as compiled code. -/
@[implemented_by evalStringsUnsafe]
private opaque evalStrings (e : Expr) : MetaM (List String)

/-- For a goal `∀ x ∈ xs, p x` with `xs : List α`, `[Located α]` and decidable `p`, the expression
of its diagnostics. `none` for every other goal. -/
private def diagnosticsOf? (goal : Expr) (theoremName : String) : MetaM (Option Expr) :=
  forallBoundedTelescope goal (some 2) fun args body => do
    let #[x, member] := args | return none
    if body.containsFVar member.fvarId! then return none
    let memberType ← inferType member
    unless memberType.isAppOfArity ``Membership.mem 5 do return none
    let xs := memberType.getArg! 3
    if xs.containsFVar x.fvarId! then return none
    unless (← whnfR (← inferType xs)).isAppOfArity ``List 1 do return none
    if (← synthInstance? (mkApp (mkConst ``Located) (← inferType x))).isNone then return none
    let some holds ← observing? (mkDecide body) | return none
    some <$> mkAppM ``diagnostics #[toExpr theoremName, xs, ← mkLambdaFVars #[x] holds]

/-- Proves the goal by `decide +kernel`. When a goal `∀ x ∈ xs, p x` over `Located` facts is false,
logs one error per fact that breaks `p`, in the GNU format the checker reads, then fails. Every
other failure is Lean's own `decide` error. -/
syntax (name := sharpDecide) "sharp_decide" : tactic

@[tactic sharpDecide] def evalSharpDecide : Tactic := fun _ => do
  let goal ← instantiateMVars (← getMainTarget)
  try
    evalTactic (← `(tactic| decide +kernel))
  catch decideError =>
    let theoremName := privateToUserName ((← Term.getDeclName?).getD .anonymous)
    let some diagnostics ← diagnosticsOf? goal theoremName.toString | throw decideError
    let lines ← evalStrings diagnostics
    let some last := lines.getLast? | throw decideError
    -- The last line is the tactic's own failure, so Lean adds no "unsolved goals" error.
    for line in lines.dropLast do logError line
    throwError last

end Sharp
