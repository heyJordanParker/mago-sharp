/-
`sharp-lean`: the program Mago runs to prove PHP# laws.

Mago writes one `Check` request to standard input and reads the answer from standard output. The request holds the
source of every generated module of the project's code, and checks, each the generated modules some laws of one `.sharp`
file reach and the theorems of those laws, after the theorems of each law they call. The runner reads no file of the
project. It imports the runtime library once, then for each check elaborates the generated modules it reaches,
elaborates each theorem with the steps of `Sharp.Lean.Proof`, and answers for each law: proven, each case and condition
its proof left open with the goal, the law not proven it holds through, or Lean's error. A generated module Lean rejects
refuses its check, with the line of its first error. This elaboration is the only one a generated module gets: nothing
builds the package.

One runner answers the checks of a compile because Lean cannot free an imported environment while the tasks that
elaborated a theorem may still hold parts of it, so a runner imports once. Generated modules are elaborated rather than
imported. Each is elaborated on the runtime library with exactly the generated modules it imports, transitively, as Lean
compiles a module from its imports alone, so whether Lean accepts it does not depend on the other modules of its check or
of the request. A check's theorems are elaborated with exactly the modules the check names, and what they import.

The runner writes each check's answer as soon as it has it, and says on standard error what it starts: `module <check>
<module>` before it elaborates a generated module, and `theorems <check>` before it elaborates a check's theorems. So
when Mago stops a runner at its processor-time limit or its deadline, it keeps the answers written, and knows where the
runner stopped. A stop in a check after the runner's first starts the next runner at that check. A stop in a generated
module of the runner's first check refuses that check, and the next runner gets the module among the request's stopped
ones, which refuse each check that reaches them without elaborating them again.
-/
import Lean
import Sharp.Lean.Law
import Sharp.Lean.Proof

open Lean Elab

namespace Sharp.Runner

/-! Mago's payload primitives (`crates/extension/src/payload.rs`): integers in network byte order, strings prefixed by
their byte length as a `u32`. -/

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
def putString (b : ByteArray) (s : String) : ByteArray :=
  putU32 b s.utf8ByteSize.toUInt32 ++ s.toUTF8

/-! Every message starts as Mago's linter and analyzer messages do: a magic, the major and minor protocol version, the
message kind, and a reserved `u16`. -/

def lawMagic : ByteArray := "MLAW".toUTF8
def checkRequest : UInt16 := 1
def checkedResponse : UInt16 := 2

/-- The kind of the request. -/
def kind : Decode UInt16 := do
  unless (← read).extract 0 4 == lawMagic do throw "the request does not start with MLAW"
  set 4
  let major ← u16
  let minor ← u16
  unless major == 2 do throw s!"unsupported law protocol version {major}.{minor}"
  let kind ← u16
  let _reserved ← u16
  return kind

def header (kind : UInt16) : ByteArray :=
  lawMagic |>.push 0 |>.push 2 |>.push 0 |>.push 1 |>.push (kind >>> 8).toUInt8 |>.push kind.toUInt8
    |>.push 0 |>.push 0

/-! The `Check` request and its answer. -/

/-- One theorem of a request: the statement of the law it proves, its Lean text, and whether the answer judges it. -/
structure Theorem where
  statement : Name
  text : String
  judged : Bool

/-- One check of a request: the modules its theorems import, and the theorems, each after every theorem it uses. -/
structure Check where
  modules : Array Name
  theorems : Array Theorem

/-- A generated module of the request: its name, as Mago wrote it, and its Lean source. -/
structure Source where
  name : String
  text : String

/-- The module `name` names, as an `import` of it names it: `Code.App.«match».Score` names `Code.App.match.Score`. -/
def moduleName (name : String) : Name :=
  (name.splitOn ".").foldl (init := .anonymous) fun module part =>
    module.str (if part.startsWith "«" && part.endsWith "»" then String.ofList (part.toList.drop 1).dropLast else part)

#guard moduleName "Code.App.«match».Score" == `Code.App.match.Score
#guard moduleName "Sharp.Lean.Proof" == `Sharp.Lean.Proof

def theorem_ : Decode Theorem := do
  return { statement := (← string).toName, text := ← string, judged := (← u8) != 0 }

def check : Decode Check := do
  return { modules := (← many string).map moduleName, theorems := ← many theorem_ }

def source : Decode Source := do
  return { name := ← string, text := ← string }

/-- The axioms Lean's own `#print axioms` treats as standard. -/
def standardAxioms : Array Name := #[``propext, ``Classical.choice, ``Quot.sound]

/-- Whether a theorem that rests on `axioms` has a gap: an axiom outside the standard set, `sorryAx` among them. -/
def hasGap (axioms : Array Name) : Bool := axioms.any (!standardAxioms.contains ·)

#guard !hasGap #[``propext, ``Classical.choice, ``Quot.sound]
#guard hasGap #[``sorryAx]
#guard hasGap #[`Sharp.unproven]

def runCore (env : Environment) (action : CoreM α) : IO α := do
  let (value, _) ← action.toIO { fileName := "sharp-lean", fileMap := default, maxHeartbeats := 0 } { env }
  return value

/-- The step of a proof whose goal stays open, from the error `Sharp/Lean/Proof.lean` logs for it: `0` for a case and
`1` for a call's condition, `2` and `3` for one whose steps ran out of their time budget, its index, and its goal as
Mago's tree. -/
def openStep (message : String) : Option (UInt8 × Nat × String) := do
  let kind ← match message.splitOn " " with
    | "sharp-case" :: _ => some (0 : UInt8)
    | "sharp-condition" :: _ => some 1
    | "sharp-case-out-of-time" :: _ => some 2
    | "sharp-condition-out-of-time" :: _ => some 3
    | _ => none
  let parts := message.splitOn " "
  let index ← parts[1]? >>= String.toNat?
  return (kind, index, " ".intercalate (parts.drop 2))

/-- The first of `targets` that `name`'s declaration reaches through the constants it uses, `name` itself apart. -/
def reachedOf (env : Environment) (name : Name) (targets : Array Name) : Option Name := Id.run do
  let mut seen : NameSet := {}
  let mut pending := (env.find? name).map (·.getUsedConstantsAsSet.toArray) |>.getD #[]
  repeat
    let some next := pending.back? | break
    pending := pending.pop
    if seen.contains next || next == name then continue
    seen := seen.insert next
    if targets.contains next then return some next
    for used in (env.find? next).map (·.getUsedConstantsAsSet.toArray) |>.getD #[] do
      pending := pending.push used
  return none

/-- Elaborates each theorem in order, each with the theorems before it in scope, and appends to `b` the count of judged
ones and the answer for each: `0` when it is proven; `1` and each step whose goal stays open, as its kind, its index and
its goal; `2` and the statement of the law not proven that it holds through; or `3` and Lean's first error, when Lean
rejected the theorem, or the axioms it rests on, when it has a gap no law it holds through explains. A theorem has no
time budget of its own: each step `Sharp/Lean/Proof.lean` runs has its own, and Mago's limits on the runner bound the
rest. -/
def answer (env : Environment) (request : Check) (b : ByteArray) : IO ByteArray := do
  let mut env := env
  let mut gaps : Array Name := #[]
  let mut b := putU32 b (request.theorems.filter (·.judged)).size.toUInt32
  for item in request.theorems do
    let input := Parser.mkInputContext s!"open Sharp\n{item.text}" "<law>"
    let state ← IO.processCommands input {} (Command.mkState env {} (maxHeartbeats.set {} 0))
    env := state.commandState.env
    let name := item.statement ++ `proof
    let mut steps := #[]
    let mut rejected := none
    for message in state.commandState.messages.toList do
      if message.severity != .error then continue
      let text ← message.data.toString
      match openStep text with
      | some step => steps := steps.push step
      | none => if rejected.isNone then rejected := some text
    let declared := env.contains name
    let axioms ← if declared then runCore env (collectAxioms name) else pure #[]
    let gap := hasGap axioms
    if gap then gaps := gaps.push name
    unless item.judged do continue
    if let some message := rejected then
      b := putString (putU8 b 3) message
    else if !declared then
      b := putString (putU8 b 3) s!"Lean declared no {name}"
    else if !steps.isEmpty then
      b := putU32 (putU8 b 1) steps.size.toUInt32
      for (kind, index, goal) in steps do
        b := putString (putU32 (putU8 b kind) index.toUInt32) goal
    else if gap then
      b := match reachedOf env name gaps with
        | some law => putString (putU8 b 2) law.getPrefix.toString
        | none => putString (putU8 b 3) s!"{name} rests on the axioms {axioms}"
    else
      b := putU8 b 0
  return b

partial def readAll (stream : IO.FS.Stream) (bytes : ByteArray := .empty) : IO ByteArray := do
  let chunk ← stream.read 65536
  if chunk.isEmpty then return bytes else readAll stream (bytes ++ chunk)

/-! The generated modules, under the root `Code`, which the request holds and the runner elaborates, and the runtime
library they import, which the runner imports once. -/

def isGenerated (module : Name) : Bool := module.getRoot == `Code

/-- The header of the generated module `source`: its input, where its commands start, the messages of its header, and
the modules it imports. -/
def parse (source : Source) : IO (Parser.InputContext × Parser.ModuleParserState × MessageLog × Array Name) := do
  let input := Parser.mkInputContext source.text s!"{source.name}.lean"
  let (header, parserState, messages) ← Parser.parseHeader input
  return (input, parserState, messages, (Elab.headerToImports header (includeInit := false)).map (·.module))

/-- Why a check's generated module gave it no environment. -/
inductive Refusal where
  /-- Lean rejected the module, at the line of its first error, or at line 0 for an import of a module the runner has
  no source or build of, or that leads back to the module. -/
  | rejected (module : Name) (line : Nat)
  /-- The module stopped an earlier runner of the compile, so it would only stop this one again. -/
  | stopped (module : Name)

/-- What a runner knows of the generated modules of its request. -/
structure Modules where
  sources : NameMap Source
  /-- The runtime library, imported once. -/
  library : Environment
  /-- The modules that stopped an earlier runner of the compile. -/
  stopped : NameSet
  /-- Each module elaborated so far: the environment of it elaborated on the runtime library with exactly the
  generated modules it imports, transitively, and those modules with it. -/
  elaborated : NameMap (Environment × NameSet) := {}
  /-- Each module refused so far, and why. -/
  refused : NameMap Refusal := {}

abbrev ModulesM := ExceptT Refusal (StateT Modules IO)

/-- The name Mago wrote for the generated `module`, which the answer and the progress name it by. -/
def written (sources : NameMap Source) (module : Name) : String :=
  (sources.find? module).map (·.name) |>.getD (module.toString (escape := false))

/-- Writes `line` to standard error at once, so Mago has it even when it ends the runner right after. -/
def progress (line : String) : IO Unit := do
  let stderr ← IO.getStderr
  stderr.putStrLn line
  stderr.flush

/-- `env` with the generated `module` elaborated from its parsed source as the main module, with no options, as Lean
elaborates a module it compiles. -/
def elaborateIn (env : Environment) (module : Name)
    (parsed : Parser.InputContext × Parser.ModuleParserState × MessageLog × Array Name) : ModulesM Environment := do
  let (input, parserState, messages, _) := parsed
  let main := env.mainModule
  let state ← IO.processCommands input parserState (Command.mkState (env.setMainModule module) messages {})
  if let some error := state.commandState.messages.toList.find? (·.severity == .error) then
    throwThe Refusal (.rejected module error.pos.line)
  return state.commandState.env.setMainModule main

/-- `env`, which holds the generated modules `present`, with `module` and each generated module it imports,
transitively, elaborated in it, each after the modules it imports, and the modules it then holds. Each of them was
elaborated before with exactly the modules it imports, so this elaboration builds an environment and judges nothing. -/
partial def extend (check : Nat) (module : Name) (env : Environment) (present : NameSet) :
    ModulesM (Environment × NameSet) := do
  if present.contains module then return (env, present)
  let sources := (← get).sources
  let some source := sources.find? module | throwThe Refusal (.rejected module 0)
  let parsed ← parse source
  let mut (env, present) := (env, present)
  for imported in parsed.2.2.2.filter isGenerated do
    (env, present) ← extend check imported env present
  progress s!"module {check} {written sources module}"
  return (← elaborateIn env module parsed, present.insert module)

mutual

/-- The environment of the generated `module` elaborated on the runtime library with exactly the generated modules it
imports, transitively, and those modules with it, as Lean compiles a module from its imports alone. `visiting` holds
the modules whose imports led here, so an import cycle is rejected instead of followed. Each module is elaborated, or
refused, once per runner. -/
partial def elaborated (check : Nat) (visiting : NameSet) (module : Name) : ModulesM (Environment × NameSet) := do
  let known ← get
  if let some result := known.elaborated.find? module then return result
  if let some refusal := known.refused.find? module then throwThe Refusal refusal
  if known.stopped.contains module then throwThe Refusal (.stopped module)
  tryCatchThe Refusal
    (do
      let some source := known.sources.find? module | throwThe Refusal (.rejected module 0)
      let parsed ← parse source
      let imports := parsed.2.2.2
      for imported in imports do
        let available :=
          if isGenerated imported then known.sources.contains imported && !visiting.contains imported
          else (known.library.getModuleIdx? imported).isSome
        unless available do throwThe Refusal (.rejected module 0)
      let (base, present) ← withImports check (visiting.insert module) (imports.filter isGenerated)
      progress s!"module {check} {written known.sources module}"
      let result := (← elaborateIn base module parsed, present.insert module)
      modify fun known => { known with elaborated := known.elaborated.insert module result }
      return result)
    (fun refusal => do
      modify fun known => { known with refused := known.refused.insert module refusal }
      throwThe Refusal refusal)

/-- An environment with exactly the generated `modules`, and the generated modules they import, transitively, on the
runtime library, and those modules: the environment of the one of `modules` that holds the most, with each module it
lacks elaborated in it. -/
partial def withImports (check : Nat) (visiting : NameSet) (modules : Array Name) :
    ModulesM (Environment × NameSet) := do
  let mut largest : Option (Environment × NameSet) := none
  for module in modules do
    let result ← elaborated check visiting module
    if largest.all (Std.TreeSet.size ·.2 < Std.TreeSet.size result.2) then largest := some result
  let some (env, present) := largest | return ((← get).library, {})
  let mut (env, present) := (env, present)
  for module in modules do
    (env, present) ← extend check module env present
  return (env, present)

end

/-- Whether the search path holds a build of `module`. -/
def found (module : Name) : IO Bool := do
  return (← (← searchPathRef.get).findModuleWithExt "olean" module).isSome

/-- Reads the request to the end of standard input, imports the runtime library its modules and checks import, and
writes the answer of each check, in order, to standard output, each as soon as it has it: `0` and the judgments of its
theorems, `1`, a generated module Lean rejected and the line of its first error, or `2` and a stopped module the check
reaches. Any other failure ends the runner with its message on standard error. -/
def respond : IO Unit := do
  let bytes ← readAll (← IO.getStdin)
  let decode {α : Type} (body : Decode α) : IO α :=
    IO.ofExcept <| (((do let _ ← kind; body).run' 0).run bytes).mapError IO.userError
  let requested ← decode kind
  unless requested == checkRequest do throw <| IO.userError s!"unknown law request kind {requested}"
  let (stopped, sources, checks) ← decode (do return (← many string, ← many source, ← many check))
  -- The runtime library: each module outside `Code` that a generated module or a check names, and the search path
  -- holds. A generated module that names one it does not hold is rejected alone.
  let mut wanted : Array Name := #[]
  for source in sources do
    for imported in (← parse source).2.2.2 do
      unless isGenerated imported || wanted.contains imported do wanted := wanted.push imported
  for request in checks do
    for module in request.modules do
      unless isGenerated module || wanted.contains module do wanted := wanted.push module
  let library ← wanted.filterM found
  let env ← importModules (library.map ({ module := · })) {} (loadExts := true)
  let sources := sources.foldl (fun sources source => sources.insert (moduleName source.name) source)
    ({} : NameMap Source)
  let mut modules : Modules :=
    { sources, library := env,
      stopped := stopped.foldl (fun stopped module => stopped.insert (moduleName module)) ({} : NameSet) }
  let stdout ← IO.getStdout
  stdout.write (putU32 (header checkedResponse) checks.size.toUInt32)
  stdout.flush
  for request in checks, index in [0:checks.size] do
    let (elaborated, known) ← (withImports index {} (request.modules.filter isGenerated)).run.run modules
    modules := known
    let b ← match elaborated with
      | .ok (checked, _) => do
        progress s!"theorems {index}"
        let code := checked.constants.foldStage2 (fun code name _ => code.insert name) ({} : NameSet)
        answer (Proof.generated.setState checked code) request (putU8 .empty 0)
      | .error (.rejected module line) =>
        pure <| putU32 (putString (putU8 .empty 1) (written sources module)) line.toUInt32
      | .error (.stopped module) => pure <| putString (putU8 .empty 2) (written sources module)
    stdout.write b
    stdout.flush

end Sharp.Runner

unsafe def main : IO Unit := do
  initSearchPath (← findSysroot)
  enableInitializersExecution
  Sharp.Runner.respond
