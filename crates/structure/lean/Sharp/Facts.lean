/-
The fixed library every rules file imports.

The checker collects the code's structure per PHP file: its declarations, `use` imports and name
references. A structure rule is a `@[rule]` definition of type `Rule`. The runner `sharp-rules`
loads the facts as data, runs every rule as compiled code, and reports each fact that breaks one.
A rule depends either on the fact alone (`Rule.each`) or on the fact and the namespace import graph
(`Rule.eachIn`), so after a change the runner re-checks only the changed files, and every file only
when the graph's edges change.
-/
import Lean.Attributes
import Std.Data.HashMap
import Std.Data.HashSet

namespace Sharp

/-- Source range of a fact, as CodeQL's `locations_default`: 1-based, inclusive, columns count bytes. -/
structure Location where
  file : String          -- workspace-relative path, e.g. "src/App/Shared/Money.php"
  startLine : Nat
  startColumn : Nat
  endLine : Nat
  endColumn : Nat
  deriving Repr, DecidableEq, Inhabited

inductive SymbolKind where
  | classLike | function | constant
  deriving Repr, DecidableEq, Inhabited

inductive DeclarationKind where
  | «class» | interface | «trait» | «enum» | function | constant
  deriving Repr, DecidableEq, Inhabited

structure Declaration where
  name : String          -- fully qualified, dot separated: "App.Shared.Money"
  kind : DeclarationKind
  location : Location    -- the declared name
  deriving Repr, DecidableEq, Inhabited

structure Import where
  namespace_ : String    -- namespace whose code holds the `use` statement; "" is the global namespace
  name : String          -- imported fully qualified name, dot separated
  kind : SymbolKind      -- `use`, `use function`, `use const`
  location : Location    -- the imported name inside the `use` item
  deriving Repr, DecidableEq, Inhabited

structure Reference where
  namespace_ : String    -- namespace whose code holds the reference
  name : String          -- referenced fully qualified name, dot separated
  location : Location
  deriving Repr, DecidableEq, Inhabited

/-- One fact of a PHP file. -/
inductive Fact where
  | declaration (d : Declaration)
  | «import» (i : Import)
  | reference (r : Reference)
  deriving Repr, DecidableEq, Inhabited

/-- The place in the PHP source a broken rule is reported on. -/
def Fact.location : Fact → Location
  | .declaration d => d.location
  | .import i => i.location
  | .reference r => r.location

/-- What the fact does, as the message on its PHP source. -/
def Fact.describe : Fact → String
  | .declaration d => s!"declares {d.name}"
  | .import i => s!"imports {i.name}"
  | .reference r => s!"references {r.name}"

/-- `name` is the namespace `ns` or sits inside it: `under "App.Shared.Money" "App.Shared"`. -/
def under (name ns : String) : Bool :=
  name == ns || name.startsWith (ns ++ ".")

/-- The namespace that holds `name`: "A.B.C" ↦ "A.B", "C" ↦ "" (the global namespace). -/
def namespaceOf (name : String) : String :=
  ".".intercalate (name.splitOn ".").dropLast

/-- The import's edge in the namespace import graph: from the importing namespace to the namespace
of the imported name. An import from the namespace's own code has no edge. -/
def Import.edge (i : Import) : Option (String × String) :=
  let target := namespaceOf i.name
  if i.namespace_ == target then none else some (i.namespace_, target)

/-- The namespace import graph, as the strongly connected component of every namespace on an edge. -/
structure Graph where
  component : Std.HashMap String Nat := {}
  deriving Inhabited

/-- The import's edge lies on a cycle: both its namespaces sit in one strongly connected component. -/
def Graph.onCycle (graph : Graph) (i : Import) : Bool :=
  match i.edge with
  | none => false
  | some (source, target) =>
    match graph.component[source]?, graph.component[target]? with
    | some a, some b => a == b
    | _, _ => false

private structure Tarjan where
  index : Std.HashMap String Nat := {}
  low : Std.HashMap String Nat := {}
  stack : Array String := #[]
  onStack : Std.HashSet String := {}
  component : Std.HashMap String Nat := {}
  components : Nat := 0

/-- Tarjan's strongly connected components over `successors`. -/
private partial def Tarjan.visit (successors : Std.HashMap String (Array String)) (v : String) :
    StateM Tarjan Unit := do
  let n := (← get).index.size
  modify fun s => { s with index := s.index.insert v n, low := s.low.insert v n,
                           stack := s.stack.push v, onStack := s.onStack.insert v }
  for w in successors.getD v #[] do
    if !(← get).index.contains w then
      visit successors w
      modify fun s => { s with low := s.low.insert v (min s.low[v]! s.low[w]!) }
    else if (← get).onStack.contains w then
      modify fun s => { s with low := s.low.insert v (min s.low[v]! s.index[w]!) }
  let s ← get
  if s.low[v]! == s.index[v]! then
    let id := s.components
    let rec pop (s : Tarjan) (fuel : Nat) : Tarjan :=
      match fuel, s.stack.back? with
      | fuel + 1, some w =>
        let s := { s with stack := s.stack.pop, onStack := s.onStack.erase w,
                          component := s.component.insert w id }
        if w == v then s else pop s fuel
      | _, _ => s
    set { pop s s.stack.size with components := id + 1 }

/-- The graph of the given distinct edges. -/
def Graph.ofEdges (edges : Array (String × String)) : Graph :=
  let successors := edges.foldl (init := ({} : Std.HashMap String (Array String)))
    fun m (source, target) => m.alter source fun ts => some ((ts.getD #[]).push target)
  let vertices := edges.foldl (init := (#[] : Array String)) fun vs (s, t) => (vs.push s).push t
  let run : StateM Tarjan Unit := vertices.forM fun v => do
    if !(← get).index.contains v then Tarjan.visit successors v
  { component := (run.run {}).2.component }

/-- A structure rule: the facts that break it. -/
inductive Rule where
  /-- Breaks every fact for which `breaks` is true. The answer depends on the fact alone. -/
  | each (breaks : Fact → Bool)
  /-- Breaks every fact for which `breaks graph` is true, where `graph` is the project's
  namespace import graph. -/
  | eachIn (breaks : Graph → Fact → Bool)
  deriving Inhabited

/-- Breaks every import for which `breaks` is true. -/
def Rule.imports (breaks : Import → Bool) : Rule :=
  .each fun | .import i => breaks i | _ => false

/-- Breaks every import for which `breaks graph` is true. -/
def Rule.importsIn (breaks : Graph → Import → Bool) : Rule :=
  .eachIn fun graph => fun | .import i => breaks graph i | _ => false

/-- Breaks every import of namespace `ns`, or of a namespace inside it, whose imported name sits
under `forbidden`. -/
def Rule.neverImports (ns forbidden : String) : Rule :=
  .imports fun i => under i.namespace_ ns && under i.name forbidden

/-- `Sharp.importsOf ns ⊆ allowed`, the rule form of the PHP# specification: code in namespace `ns`,
or in a namespace inside it, imports only from `ns` itself and from the namespaces in `allowed`.
Every other `⊆` keeps its meaning, `HasSubset.Subset`. -/
macro_rules
  | `(Sharp.importsOf $ns ⊆ $allowed) =>
    `(Sharp.Rule.imports fun i => Sharp.under i.namespace_ $ns && !Sharp.under i.name $ns &&
        !(($allowed : List String).any (Sharp.under i.name ·)))

/-- Breaks every import whose edge lies on a cycle of the namespace import graph. -/
def Rule.noImportCycles : Rule :=
  .importsIn Graph.onCycle

/-- Marks a definition of type `Sharp.Rule` as a structure rule the runner checks. -/
initialize ruleAttribute : Lean.TagAttribute ←
  Lean.registerTagAttribute `rule "a structure rule, checked over the code's facts by `sharp-rules`"
    fun declName => do
      unless (← Lean.getConstInfo declName).type == .const ``Rule [] do
        throwError "`@[rule]` needs a definition of type `Sharp.Rule`"

end Sharp
