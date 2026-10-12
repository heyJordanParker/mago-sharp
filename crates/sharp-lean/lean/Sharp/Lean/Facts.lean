import Lean.Attributes
import Lean.Elab.Command
import Lean.Elab.Term
import Lean.IdentifierSuggestion
import Std.Data.HashMap
import Std.Data.HashSet

/-!
The structure of PHP# code as data, and the structure rules that judge it.

`mago compile` collects the facts of every `.sharp` file: its declarations, its imports and its name references. A
structure rule is a `@[rule]` definition of type `Rule` in a `Module.lean`. It constrains the namespace of the
`Module.lean` that holds it: the runner passes it that namespace and only the facts under it, and reports each fact that
breaks it on the code that holds the fact.

A rule names a namespace by its full name, `App.Shared.Schema`, which Lean resolves like any name: `mago compile`
declares one `Namespace` constant per namespace of the codebase, so a misspelled namespace is Lean's unknown identifier.
The constructor of `Namespace` is private, so `⟨"App.Shared.Ledgr"⟩` is refused too.
-/

namespace Sharp

/-- One namespace of the codebase, by its full name. A rule gets one only from `namespace_decl`: the runner refuses a
rule that reaches the constructor or `Rule.breaks`. -/
structure Namespace where
  private mk ::
  /-- Dot separated: "App.Shared.Schema". -/
  name : String
  deriving Repr, DecidableEq

open Lean Elab Command in
/-- `namespace_decl App App.Shared` declares the namespace constants `App` and `App.Shared`, apart from a name Lean
already holds a constant of, such as `List`. It works only in the module `Code` that `mago compile` generates, so a rule
names only a namespace that a class-like of the codebase sits in. One command compiles every constant at once: on 2,648
namespaces, that takes half the time of one command each. -/
elab "namespace_decl " ids:ident+ : command => do
  unless (← getMainModule) == `Code do
    throwError "`namespace_decl` belongs in the module mago compile generates: a rule names only a namespace of the codebase"
  let mut declared := #[]
  for id in ids do
    let name := id.getId
    unless (← getEnv).contains name do
      liftCoreM <| addDecl <| .defnDecl {
        name, levelParams := [], type := mkConst ``Namespace,
        value := mkApp (mkConst ``Namespace.mk) (mkStrLit (name.toString (escape := false))),
        hints := .abbrev, safety := .safe }
      declared := declared.push name
  liftCoreM <| compileDecls declared

open Lean Elab Term in
/-- A name below a namespace that names nothing, such as `App.Shared.Ledgr` below `App.Shared`, is an unknown identifier,
as any name that names nothing is, and not a field Lean cannot find on `App.Shared`. -/
@[term_elab ident] def elabNamespaceName : TermElab := fun stx _ => do
  if (← resolveLocalName stx.getId).isSome then throwUnsupportedSyntax
  let [(name, field :: _)] ← resolveGlobalName stx.getId | throwUnsupportedSyntax
  unless (← getConstInfo name).type == .const ``Namespace [] && !(← getEnv).contains (``Namespace ++ .mkSimple field) do
    throwUnsupportedSyntax
  throwUnknownNameWithSuggestions stx.getId (ref? := stx)

/-- Where a fact sits in its `.sharp` file, as CodeQL's `locations_default`: 1-based, inclusive, columns count bytes. -/
structure Location where
  /-- Workspace-relative: "app/Shared/Ledger.sharp". -/
  file : String
  startLine : Nat
  startColumn : Nat
  endLine : Nat
  endColumn : Nat
  deriving Repr, DecidableEq, Inhabited

inductive DeclarationKind where
  | «class» | interface | «enum»
  deriving Repr, DecidableEq, Inhabited

structure Declaration where
  /-- Fully qualified, dot separated: "App.Shared.Money". -/
  name : String
  kind : DeclarationKind
  /-- The declared name. -/
  location : Location
  deriving Repr, DecidableEq, Inhabited

structure Import where
  /-- The namespace whose code holds the import. "" is the global namespace. -/
  namespace_ : String
  /-- The imported class-like, fully qualified and dot separated. -/
  name : String
  /-- The imported name inside the import. -/
  location : Location
  deriving Repr, DecidableEq, Inhabited

structure Reference where
  /-- The namespace whose code holds the reference. -/
  namespace_ : String
  /-- The referenced name, fully qualified and dot separated. -/
  name : String
  location : Location
  deriving Repr, DecidableEq, Inhabited

/-- One fact of a `.sharp` file. -/
inductive Fact where
  | declaration (d : Declaration)
  | «import» (i : Import)
  | reference (r : Reference)
  deriving Repr, DecidableEq, Inhabited

/-- Where a broken rule is reported. -/
def Fact.location : Fact → Location
  | .declaration d => d.location
  | .import i => i.location
  | .reference r => r.location

/-- `name` is the namespace `ns` or sits inside it: `under "App.Shared.Money" App.Shared`. -/
def under (name : String) (ns : Namespace) : Bool :=
  name == ns.name || name.startsWith (ns.name ++ ".")

/-- The namespace that holds `name`: "A.B.C" ↦ "A.B", "C" ↦ "" (the global namespace). -/
def namespaceOf (name : String) : String :=
  ".".intercalate (name.splitOn ".").dropLast

/-- The namespace whose code holds the fact. -/
def Fact.namespace_ : Fact → String
  | .declaration d => namespaceOf d.name
  | .import i => i.namespace_
  | .reference r => r.namespace_

/-- The import's edge in the namespace import graph: from the importing namespace to the namespace of the imported
name. An import from the namespace's own code has no edge. -/
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

/-- The graph of the given edges. -/
def Graph.ofEdges (edges : Array (String × String)) : Graph :=
  let successors := edges.foldl (init := ({} : Std.HashMap String (Array String)))
    fun m (source, target) => m.alter source fun ts => some ((ts.getD #[]).push target)
  let vertices := edges.foldl (init := (#[] : Array String)) fun vs (s, t) => (vs.push s).push t
  let run : StateM Tarjan Unit := vertices.forM fun v => do
    if !(← get).index.contains v then Tarjan.visit successors v
  { component := (run.run {}).2.component }

/-- A structure rule: the facts that break it. -/
inductive Rule where
  /-- `ns` is the namespace of the Module.lean that holds the rule. The runner passes it only the facts under `ns`. -/
  | each (breaks : (ns : Namespace) → Fact → Bool)
  /-- As `each`, with the namespace import graph of every `.sharp` file. -/
  | eachIn (breaks : (ns : Namespace) → Graph → Fact → Bool)
  deriving Inhabited

/-- Breaks every import for which `breaks` is true. -/
def Rule.imports (breaks : (ns : Namespace) → Import → Bool) : Rule :=
  .each fun ns => fun | .import i => breaks ns i | _ => false

/-- Breaks every import for which `breaks` is true in the namespace import graph. -/
def Rule.importsIn (breaks : (ns : Namespace) → Graph → Import → Bool) : Rule :=
  .eachIn fun ns graph => fun | .import i => breaks ns graph i | _ => false

/-- Breaks every import of a name under `forbidden`. -/
def Rule.neverImports (forbidden : Namespace) : Rule :=
  .imports fun _ i => under i.name forbidden

/-- Breaks every import of a name outside the rule's own namespace, outside every namespace in `allowed`, and outside
`Sharp`: the standard library is always in reach, as a language's standard library is. `neverImports` still forbids a
part of it. -/
def Rule.importsOnly (allowed : List Namespace) : Rule :=
  .imports fun ns i => !(ns :: ⟨"Sharp"⟩ :: allowed).any (under i.name ·)

/-- Breaks every import whose edge lies on a cycle of the namespace import graph. -/
def Rule.noImportCycles : Rule :=
  .importsIn fun _ => Graph.onCycle

/-- Whether `fact` breaks `rule`, which the Module.lean of the namespace `ns` holds: the fact sits under `ns` and the
rule breaks it there. The runner names `ns` as its request does, so this is the one place a namespace comes from a
string, and the runner refuses a rule that reaches it. -/
def Rule.breaks (rule : Rule) (ns : String) (graph : Graph) (fact : Fact) : Bool :=
  let ns : Namespace := ⟨ns⟩
  under fact.namespace_ ns && match rule with
    | .each breaks => breaks ns fact
    | .eachIn breaks => breaks ns graph fact

/-- Marks a definition of type `Sharp.Rule` in a `Module.lean` as a structure rule the runner checks. -/
initialize ruleAttribute : Lean.TagAttribute ←
  Lean.registerTagAttribute `rule "a structure rule, checked over the facts of the namespace of its Module.lean"
    fun declName => do
      unless (← Lean.getConstInfo declName).type == .const ``Rule [] do
        throwError "`@[rule]` needs a definition of type `Sharp.Rule`"
      unless (System.FilePath.mk (← Lean.getFileName)).fileName == some "Module.lean" do
        throwError "`@[rule]` belongs in a Module.lean: a structure rule constrains the namespace of the Module.lean that holds it"

end Sharp
