/-
`sharp-rules`: a Mago extension worker that checks structure rules over facts it holds as data.

It imports the rules modules named on its command line, as `lake exe runLinter` imports a module
and runs every `@[env_linter]`, and evaluates every `@[rule]` definition once. Then it reads Mago's
extension frames on stdin. Each request carries the facts of the files that changed and the paths
of the files removed since the generation the runner holds. The runner keeps one list of broken
facts per rule and file, re-checks only the files in the request, and re-checks every file for the
rules that read the namespace import graph only when the graph's edges change. It answers with all
broken facts of every file whose broken facts may have changed, so the answer grows with the change,
as the request does.
-/
import Lean
import Sharp.Facts

open Lean

namespace Sharp.Runner

/-! Mago's extension frame (`crates/extension/src/protocol.rs`) and payload primitives
(`crates/extension/src/payload.rs`): integers in network byte order, strings prefixed by their byte
length as a `u32`. -/

structure Frame where
  kind : UInt8
  flags : UInt8 := 0
  id : UInt64
  parentId : UInt64 := 0
  payload : ByteArray

def frameRequest : UInt8 := 1
def frameResponse : UInt8 := 2
def frameShutdown : UInt8 := 5
def flagError : UInt8 := 1

abbrev Decode := StateT Nat (ReaderT ByteArray (Except String))

def u8 : Decode UInt8 := do
  let bytes ← read
  let pos ← get
  unless pos < bytes.size do throw "the payload ends early"
  set (pos + 1)
  return bytes[pos]!

def u16 : Decode UInt16 := do
  return ((← u8).toUInt16 <<< 8) ||| (← u8).toUInt16

def u32 : Decode UInt32 := do
  let mut value : UInt32 := 0
  for _ in [0:4] do value := (value <<< 8) ||| (← u8).toUInt32
  return value

def u64 : Decode UInt64 := do
  let mut value : UInt64 := 0
  for _ in [0:8] do value := (value <<< 8) ||| (← u8).toUInt64
  return value

def count : Decode Nat := do
  let n := (← u32).toNat
  unless n ≤ (← read).size - (← get) do throw s!"a count of {n} exceeds the payload"
  return n

def string : Decode String := do
  let n ← count
  let pos ← get
  set (pos + n)
  match String.fromUTF8? ((← read).extract pos (pos + n)) with
  | some s => return s
  | none => throw "a string is not valid UTF-8"

def many (item : Decode α) : Decode (Array α) := do
  let n ← count
  let mut items := Array.mkEmpty n
  for _ in [0:n] do items := items.push (← item)
  return items

def putU8 (b : ByteArray) (v : UInt8) : ByteArray := b.push v
def putU32 (b : ByteArray) (v : UInt32) : ByteArray :=
  (((b.push (v >>> 24).toUInt8).push (v >>> 16).toUInt8).push (v >>> 8).toUInt8).push v.toUInt8
def putU64 (b : ByteArray) (v : UInt64) : ByteArray :=
  putU32 (putU32 b (v >>> 32).toUInt32) v.toUInt32
def putString (b : ByteArray) (s : String) : ByteArray :=
  putU32 b s.utf8ByteSize.toUInt32 ++ s.toUTF8

def magic : ByteArray := "MAGO".toUTF8

partial def readExact (stream : IO.FS.Stream) (n : Nat) (acc : ByteArray := .empty) : IO ByteArray := do
  if acc.size ≥ n then return acc
  let chunk ← stream.read (n - acc.size).toUSize
  if chunk.isEmpty then throw <| IO.userError s!"stdin ended {n - acc.size} bytes before the frame did"
  readExact stream n (acc ++ chunk)

/-- Reads one frame, `none` at a clean end of input between frames. -/
def readFrame (stream : IO.FS.Stream) : IO (Option Frame) := do
  let first ← stream.read 1
  if first.isEmpty then return none
  let header ← readExact stream 32 first
  let field {α : Type} (d : Decode α) (at_ : Nat) : IO α :=
    IO.ofExcept <| (d.run' at_).run header |>.mapError IO.userError
  unless header.extract 0 4 == magic do throw <| IO.userError "the frame does not start with MAGO"
  let major ← field u16 4
  unless major == 1 do throw <| IO.userError s!"unsupported extension protocol major version {major}"
  let length := (← field u32 28).toNat
  return some {
    kind := header[8]!, flags := header[9]!, id := ← field u64 12, parentId := ← field u64 20,
    payload := ← readExact stream length }

def writeFrame (stream : IO.FS.Stream) (frame : Frame) : IO Unit := do
  let header := putU32 (putU64 (putU64 (putU8 (putU8 (putU32 magic 0x00010000) frame.kind) frame.flags
    |>.push 0 |>.push 0) frame.id) frame.parentId) frame.payload.size.toUInt32
  stream.write (header ++ frame.payload)
  stream.flush

/-! The `Check` request and its answer. -/

def location (file : String) : Decode Location :=
  return ⟨file, (← u32).toNat, (← u32).toNat, (← u32).toNat, (← u32).toNat⟩

def declarationKind : Decode DeclarationKind := do
  match ← u8 with
  | 0 => return .class | 1 => return .interface | 2 => return .trait
  | 3 => return .enum | 4 => return .function | 5 => return .constant
  | k => throw s!"unknown declaration kind {k}"

def symbolKind : Decode SymbolKind := do
  match ← u8 with
  | 0 => return .classLike | 1 => return .function | 2 => return .constant
  | k => throw s!"unknown symbol kind {k}"

/-- One file's facts: its path, then its declarations, imports and references. -/
def file : Decode (String × Array Fact) := do
  let path ← string
  let declarations ← many do
    return Fact.declaration ⟨← string, ← declarationKind, ← location path⟩
  let imports ← many do
    return Fact.import ⟨← string, ← string, ← symbolKind, ← location path⟩
  let references ← many do
    return Fact.reference ⟨← string, ← string, ← location path⟩
  return (path, declarations ++ imports ++ references)

structure Check where
  previous : UInt64
  generation : UInt64
  removed : Array String
  changed : Array (String × Array Fact)

/-! Every structure message starts as Mago's linter and analyzer messages do: a magic, the major and
minor protocol version, the message kind, and a reserved `u16`. -/

def structureMagic : ByteArray := "MSTR".toUTF8
def checkRequest : UInt16 := 1
def checkedResponse : UInt16 := 2
def staleResponse : UInt16 := 3

def header (kind : UInt16) : ByteArray :=
  structureMagic |>.push 0 |>.push 1 |>.push 0 |>.push 0 |>.push (kind >>> 8).toUInt8 |>.push kind.toUInt8
    |>.push 0 |>.push 0

def check : Decode Check := do
  unless (← read).extract 0 4 == structureMagic do throw "the request does not start with MSTR"
  set 4
  let major ← u16
  let minor ← u16
  unless major == 1 do throw s!"unsupported structure protocol version {major}.{minor}"
  let kind ← u16
  unless kind == checkRequest do throw s!"unknown structure request kind {kind}"
  let _reserved ← u16
  return { previous := ← u64, generation := ← u64, removed := ← many string, changed := ← many file }

/-- A `@[rule]` definition and the facts that break it, per file. -/
structure LoadedRule where
  name : Name
  module : Name
  rule : Rule
  broken : Std.HashMap String (Array Fact) := {}

structure State where
  generation : UInt64 := 0
  files : Std.HashMap String (Array Fact) := {}
  /-- How many imports make each edge of the namespace import graph. -/
  edges : Std.HashMap (String × String) Nat := {}
  graph : Graph := {}
  rules : Array LoadedRule

def edgesOf (facts : Array Fact) : Array (String × String) :=
  facts.filterMap fun | .import i => i.edge | _ => none

/-- Adds or removes the facts' imports from the count of each edge. -/
def countEdges (edges : Std.HashMap (String × String) Nat) (facts : Array Fact) (add : Bool) :
    Std.HashMap (String × String) Nat :=
  (edgesOf facts).foldl (init := edges) fun edges edge =>
    match edges[edge]?, add with
    | none, true => edges.insert edge 1
    | some n, true => edges.insert edge (n + 1)
    | some 1, false => edges.erase edge
    | some n, false => edges.insert edge (n - 1)
    | none, false => edges

def brokenIn (breaks : Fact → Bool) (broken : Std.HashMap String (Array Fact)) (path : String)
    (facts : Array Fact) : Std.HashMap String (Array Fact) :=
  match facts.filter breaks with
  | #[] => broken.erase path
  | found => broken.insert path found

/-- The paths whose facts differ between two `broken` maps. -/
def differing (old new : Std.HashMap String (Array Fact)) : Array String :=
  let gone := old.fold (init := #[]) fun paths path facts =>
    if new[path]? == some facts then paths else paths.push path
  new.fold (init := gone) fun paths path _ => if old.contains path then paths else paths.push path

/-- Applies the request to the state and returns the paths whose broken facts may have changed, or
returns the generation the runner holds when the request was built on another one. The graph
changes only when an edge exists after the request and not before, or the reverse, so a file that
removes and adds back the same edges changes nothing for the rules that read the graph. -/
def State.apply (state : State) (request : Check) : Except UInt64 (State × Array String) := do
  if request.previous != 0 && request.previous != state.generation then throw state.generation
  let state := if request.previous == 0 then
    { state with files := {}, edges := {}, rules := state.rules.map ({ · with broken := {} }) }
  else state
  let touched := request.removed ++ request.changed.map (·.1)
  let mut files := state.files
  let mut edges := state.edges
  let mut candidates := #[]
  for path in touched do
    if let some old := files[path]? then
      edges := countEdges edges old false
      candidates := candidates ++ edgesOf old
      files := files.erase path
  for (path, facts) in request.changed do
    edges := countEdges edges facts true
    candidates := candidates ++ edgesOf facts
    files := files.insert path facts
  let graphChanged := request.previous == 0 ||
    candidates.any fun edge => state.edges.contains edge != edges.contains edge
  let graph := if graphChanged then Graph.ofEdges (edges.fold (fun es e _ => es.push e) #[]) else state.graph
  let recheckTouched (breaks : Fact → Bool) (broken : Std.HashMap String (Array Fact)) :=
    let broken := request.removed.foldl (·.erase ·) broken
    request.changed.foldl (fun broken (path, facts) => brokenIn breaks broken path facts) broken
  let recheckAll (breaks : Fact → Bool) :=
    files.fold (fun broken path facts => brokenIn breaks broken path facts) {}
  let mut dirty : Std.HashSet String := Std.HashSet.ofArray touched
  let mut rules := #[]
  for loaded in state.rules do
    let broken := match loaded.rule with
      | .each breaks => recheckTouched breaks loaded.broken
      | .eachIn breaks =>
        if graphChanged then recheckAll (breaks graph) else recheckTouched (breaks graph) loaded.broken
    if graphChanged then dirty := dirty.insertMany (differing loaded.broken broken)
    rules := rules.push { loaded with broken }
  return ({ generation := request.generation, files, edges, graph, rules }, dirty.toArray)

def putRange (b : ByteArray) (l : Location) : ByteArray :=
  putU32 (putU32 (putU32 (putU32 b l.startLine.toUInt32) l.startColumn.toUInt32) l.endLine.toUInt32)
    l.endColumn.toUInt32

/-- The generation, then every `dirty` file with all of its broken facts: each fact's rule, the
rule's module, its location and what it does. A file listed with no facts has none left. -/
def checked (state : State) (dirty : Array String) : ByteArray := Id.run do
  let mut b := putU32 (putU64 (header checkedResponse) state.generation) dirty.size.toUInt32
  for path in dirty do
    let broken := state.rules.foldl (init := #[]) fun acc loaded =>
      acc ++ (loaded.broken.getD path #[]).map (loaded, ·)
    b := putU32 (putString b path) broken.size.toUInt32
    for (loaded, fact) in broken do
      b := putString (putRange (putString (putString b loaded.name.toString) loaded.module.toString)
        fact.location) fact.describe
  return b

/-- The generation the runner holds. -/
def stale (generation : UInt64) : ByteArray :=
  putU64 (header staleResponse) generation

unsafe def loadRules (modules : Array Name) : IO (Array LoadedRule) := do
  initSearchPath (← findSysroot)
  enableInitializersExecution
  let env ← importModules (modules.map ({ module := · })) {} (trustLevel := 1024) (loadExts := true)
  modules.foldlM (init := #[]) fun rules module => do
    let some index := env.getModuleIdx? module | throw <| IO.userError s!"module {module} was not imported"
    (ruleAttribute.ext.getModuleEntries env index).foldlM (init := rules) fun rules name => do
      let rule ← IO.ofExcept (env.evalConst Rule {} name)
      return rules.push { name, module, rule }

partial def serve (stdin stdout : IO.FS.Stream) (state : State) : IO Unit := do
  let some frame ← readFrame stdin | return
  if frame.kind == frameShutdown then return
  unless frame.kind == frameRequest do return ← serve stdin stdout state
  match (check.run' 0).run frame.payload with
  | .error message =>
    writeFrame stdout { kind := frameResponse, flags := flagError, id := frame.id, payload := message.toUTF8 }
    serve stdin stdout state
  | .ok request =>
    match state.apply request with
    | .error held =>
      writeFrame stdout { kind := frameResponse, id := frame.id, payload := stale held }
      serve stdin stdout state
    | .ok (state, dirty) =>
      writeFrame stdout { kind := frameResponse, id := frame.id, payload := checked state dirty }
      serve stdin stdout state

end Sharp.Runner

open Sharp.Runner in
unsafe def main (modules : List String) : IO Unit := do
  let rules ← loadRules (modules.map String.toName).toArray
  serve (← IO.getStdin) (← IO.getStdout) { rules }
