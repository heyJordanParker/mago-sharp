/-
`sharp-lean`: the program Mago runs to prove PHP# laws.

Mago writes one `Check` request to standard input and reads the answer from standard output. The request holds the
source of each generated module its checks reach, and checks, each the generated modules some laws of one `.sharp`
file reach and the theorems of those laws, after the theorems of each law they call. The runner reads no file of the
project. It imports the runtime library once, then answers the checks on as many threads as the request asks: for each
check it elaborates the generated modules the check reaches, then each theorem: with its fixed steps when it has them,
and with Lean's search of `Sharp.Lean.Proof` when it has none or they leave it unproven and the runner imported that
search. It answers for each law: proven, each case and condition its proof left open with the goal, the law not proven
it holds through, Lean's error, or that its theorem ran out of the time budget the request gives each one. A generated
module Lean rejects refuses its check, with the line of its first error. This elaboration is the only one a generated
module gets: nothing builds the package.

One runner answers every check of a request, so each check shares the one environment the runner imports: the modules
of the runtime library its modules and checks name, which is all of Lean only when a check names `Sharp.Lean.Proof`. Each generated module is elaborated once per runner, whichever check reaches it first,
and a check that reaches it later waits for it. Generated modules are elaborated rather than imported. Each is
elaborated on the runtime library with exactly the generated modules it imports, transitively, as Lean compiles a module
from its imports alone, so whether Lean accepts it does not depend on the other modules of its check or of the request.
A check's theorems are elaborated with exactly the modules the check names, and what they import. A theorem is
elaborated once per runner when it can be: a later check that calls it gets the declaration Lean added for it, when the
check's environment holds every constant that declaration uses.

The runner writes each check's answer as soon as it has it, with the check's index, and says on standard error what each
check starts: `module <check> <module>` before it elaborates a generated module for it, and `theorems <check>` before it
elaborates its theorems. So when Mago stops a runner at its deadline, it keeps the answers written, and knows where each
check that has none stopped. A runner that answers on one thread stopped in a generated module refuses that check, and
the next runner gets the module among the request's stopped ones, which refuse each check that reaches them without
elaborating them again.
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
def major : UInt16 := 6
def checkRequest : UInt16 := 1
def checkedResponse : UInt16 := 2

/-- The kind of the request. -/
def kind : Decode UInt16 := do
  unless (← read).extract 0 4 == lawMagic do throw "the request does not start with MLAW"
  set 4
  let requested ← u16
  let minor ← u16
  unless requested == major do throw s!"unsupported law protocol version {requested}.{minor}"
  let kind ← u16
  let _reserved ← u16
  return kind

def header (kind : UInt16) : ByteArray :=
  lawMagic |>.push 0 |>.push major.toUInt8 |>.push 0 |>.push 0 |>.push (kind >>> 8).toUInt8 |>.push kind.toUInt8
    |>.push 0 |>.push 0

/-! The `Check` request and its answer. -/

/-- One theorem of a request: the statement of the law it proves, its Lean text, which Lean's search proves, the text of
its fixed steps, empty when it has none, whether the answer judges it, and the text of its law's search for the small
values that break it, empty when it has none. -/
structure Theorem where
  statement : Name
  text : String
  steps : String
  judged : Bool
  search : String

/-- One check of a request: the modules its theorems import, and the theorems, each after every theorem it uses. -/
structure Check where
  modules : Array Name
  theorems : Array Theorem

/-- A generated module of the request: its name, as Mago wrote it, and its Lean source. -/
structure Source where
  name : String
  text : String

/-- The name Mago wrote as `written`, a module or a law's statement, as Lean source names it: `Code.«App».«match»` names
`Code.App.match`. -/
def nameOf (written : String) : Name :=
  (written.splitOn ".").foldl (init := .anonymous) fun name part =>
    name.str (if part.startsWith "«" && part.endsWith "»" then String.ofList (part.toList.drop 1).dropLast else part)

#guard nameOf "Code.«App».«match».«Score»" == `Code.App.match.Score
#guard nameOf "«App».«Repro».«until»" == `App.Repro.«until»
#guard nameOf "Sharp.Lean.Proof" == `Sharp.Lean.Proof

def theorem_ : Decode Theorem := do
  return { statement := nameOf (← string), text := ← string, steps := ← string, judged := (← u8) != 0,
           search := ← string }

def check : Decode Check := do
  return { modules := (← many string).map nameOf, theorems := ← many theorem_ }

def source : Decode Source := do
  return { name := ← string, text := ← string }

/-- The axioms Lean's own `#print axioms` treats as standard. -/
def standardAxioms : Array Name := #[``propext, ``Classical.choice, ``Quot.sound]

/-- Whether a theorem that rests on `axioms` has a gap: an axiom outside the standard set, `sorryAx` among them. -/
def hasGap (axioms : Array Name) : Bool := axioms.any (!standardAxioms.contains ·)

#guard !hasGap #[``propext, ``Classical.choice, ``Quot.sound]
#guard hasGap #[``sorryAx]
#guard hasGap #[`Sharp.unproven]

/-- The options every generated module and theorem elaborates with: Lean's linters off, since no warning reaches an
answer. -/
def unlinted : Options := ({} : Options).setBool `linter.all false

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

/-- Whether `message` is the error Lean logs when a command runs out of its heartbeats. -/
def isOutOfHeartbeats (message : String) : Bool :=
  (message.splitOn "maximum number of heartbeats").length > 1

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

/-- What elaborating one theorem gave: each step whose goal stayed open, as its kind, its index and its goal, whether it
ran out of the heartbeats of its theorem outside those steps, the first other error, whether Lean declared the theorem,
and the axioms it rests on. With them, the declaration Lean added for it and each constant that declaration uses, when
they were all there before it, so no other declaration its elaboration added is part of it. -/
structure Elaborated where
  steps : Array (UInt8 × Nat × String)
  outOfTime : Bool
  rejected : Option String
  declared : Bool
  axioms : Array Name
  declaration : Option (Declaration × NameSet)

/-- Whether `env` holds each constant of `names`. -/
def holdsAll (env : Environment) (names : NameSet) : Bool := Id.run do
  for name in names do
    unless env.contains name do return false
  return true

/-- Elaborates the theorem `text` of the law `statement` in `env`, within `heartbeats` thousand heartbeats, and gives
the environment with its theorem and what elaborating it gave. The steps `Sharp/Lean/Proof.lean` runs share that budget,
so a theorem that runs out of it fails alone, and the other checks of the runner go on. -/
def elaborate (env : Environment) (statement : Name) (text : String) (heartbeats : Nat) :
    IO (Environment × Elaborated) := do
  let input := Parser.mkInputContext s!"open Sharp\n{text}" "<law>"
  let state ← IO.processCommands input {} (Command.mkState env {} (maxHeartbeats.set unlinted heartbeats))
  let elaborated := state.commandState.env
  let name := statement ++ `proof
  let mut steps := #[]
  let mut outOfTime := false
  let mut rejected := none
  for message in state.commandState.messages.toList do
    if message.severity != .error then continue
    let text ← message.data.toString
    match openStep text with
    | some step => steps := steps.push step
    | none =>
      if isOutOfHeartbeats text then outOfTime := true
      else if rejected.isNone then rejected := some text
  let declared := elaborated.contains name
  let axioms ← if declared then runCore elaborated (collectAxioms name) else pure #[]
  let declaration := match elaborated.find? name with
    | some (.thmInfo val) =>
      let uses := (ConstantInfo.thmInfo val).getUsedConstantsAsSet
      if holdsAll env uses then some (.thmDecl val, uses) else none
    | _ => none
  return (elaborated, { steps, outOfTime, rejected, declared, axioms, declaration })

/-- Whether elaborating a theorem proved it: Lean declared it, with no step left open, no error and no gap. -/
def Elaborated.proved (elaborated : Elaborated) : Bool :=
  elaborated.declared && elaborated.steps.isEmpty && !elaborated.outOfTime && elaborated.rejected.isNone
    && !hasGap elaborated.axioms

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
  deriving Inhabited

/-- What every check of a runner shares: the request's generated modules, the runtime library, whether it holds Lean's
search, the modules that stopped an earlier runner of the compile, the generated modules whose imports lead back to
them, the time budget of each theorem in thousands of heartbeats, and what the checks elaborated so far. -/
structure Shared where
  sources : NameMap Source
  library : Environment
  searches : Bool
  stopped : NameSet
  cyclic : NameSet
  heartbeats : Nat
  /-- Each generated module a check started to elaborate: the environment of it elaborated on the runtime library with
  exactly the generated modules it imports, transitively, and those modules with it, or why it has none. A check that
  reaches it later waits for it. -/
  modules : IO.Ref (NameMap (Task (Except Refusal (Environment × NameSet))))
  /-- What elaborating each theorem gave, by its statement. -/
  theorems : IO.Ref (NameMap Elaborated)

abbrev ModulesM := ExceptT Refusal (ReaderT Shared IO)

/-- The name Mago wrote for the generated `module`, which the answer and the progress name it by. -/
def written (sources : NameMap Source) (module : Name) : String :=
  (sources.find? module).map (·.name) |>.getD (module.toString (escape := false))

/-- Writes `line` to standard error at once and whole, so Mago has it even when it ends the runner right after, and the
lines of the checks that run together never mix. -/
def progress (line : String) : IO Unit := do
  let stderr ← IO.getStderr
  stderr.putStr s!"{line}\n"
  stderr.flush

/-- `env` with the generated `module` elaborated from its parsed source as the main module, with the linters off and no
other option, as Lean elaborates a module it compiles. -/
def elaborateIn (env : Environment) (module : Name)
    (parsed : Parser.InputContext × Parser.ModuleParserState × MessageLog × Array Name) : ModulesM Environment := do
  let (input, parserState, messages, _) := parsed
  let main := env.mainModule
  let state ← IO.processCommands input parserState (Command.mkState (env.setMainModule module) messages unlinted)
  if let some error := state.commandState.messages.toList.find? (·.severity == .error) then
    throwThe Refusal (.rejected module error.pos.line)
  return state.commandState.env.setMainModule main

/-- `env`, which holds the generated modules `present`, with `module` and each generated module it imports,
transitively, elaborated in it, each after the modules it imports, and the modules it then holds. Each of them was
elaborated before with exactly the modules it imports, so this elaboration builds an environment and judges nothing. -/
partial def extend (check : Nat) (module : Name) (env : Environment) (present : NameSet) :
    ModulesM (Environment × NameSet) := do
  if present.contains module then return (env, present)
  let sources := (← read).sources
  let some source := sources.find? module | throwThe Refusal (.rejected module 0)
  let parsed ← parse source
  let mut (env, present) := (env, present)
  for imported in parsed.2.2.2.filter isGenerated do
    (env, present) ← extend check imported env present
  progress s!"module {check} {written sources module}"
  return (← elaborateIn env module parsed, present.insert module)

mutual

/-- The environment of the generated `module` elaborated on the runtime library with exactly the generated modules it
imports, transitively, and those modules with it, as Lean compiles a module from its imports alone. Each module is
elaborated, or refused, once per runner: the first check that reaches it elaborates it, and every other waits for that
one. A module whose imports lead back to it is refused before any check starts, so no check waits for itself. -/
partial def elaborated (check : Nat) (module : Name) : ModulesM (Environment × NameSet) := do
  let shared ← read
  let promise ← IO.Promise.new
  let started ← shared.modules.modifyGet fun known => match known.find? module with
    | some task => (some task, known)
    | none => (none, known.insert module promise.result!)
  if let some task := started then
    return ← MonadExcept.ofExcept (← IO.wait task)
  let result : Except Refusal (Environment × NameSet) ←
    tryCatchThe Refusal (.ok <$> elaborateFirst check module) (pure ∘ .error)
  promise.resolve result
  MonadExcept.ofExcept result

/-- What [`elaborated`] gives for `module` in the check that elaborates it first. -/
partial def elaborateFirst (check : Nat) (module : Name) : ModulesM (Environment × NameSet) := do
  let shared ← read
  if shared.stopped.contains module then throwThe Refusal (.stopped module)
  if shared.cyclic.contains module then throwThe Refusal (.rejected module 0)
  let some source := shared.sources.find? module | throwThe Refusal (.rejected module 0)
  let parsed ← parse source
  let imports := parsed.2.2.2
  for imported in imports do
    let available :=
      if isGenerated imported then shared.sources.contains imported
      else (shared.library.getModuleIdx? imported).isSome
    unless available do throwThe Refusal (.rejected module 0)
  let (base, present) ← withImports check (imports.filter isGenerated)
  progress s!"module {check} {written shared.sources module}"
  return (← elaborateIn base module parsed, present.insert module)

/-- An environment with exactly the generated `modules`, and the generated modules they import, transitively, on the
runtime library, and those modules: the environment of the one of `modules` that holds the most, with each module it
lacks elaborated in it. -/
partial def withImports (check : Nat) (modules : Array Name) : ModulesM (Environment × NameSet) := do
  let mut largest : Option (Environment × NameSet) := none
  for module in modules do
    let result ← elaborated check module
    if largest.all (Std.TreeSet.size ·.2 < Std.TreeSet.size result.2) then largest := some result
  let some (env, present) := largest | return ((← read).library, {})
  let mut (env, present) := (env, present)
  for module in modules do
    (env, present) ← extend check module env present
  return (env, present)

end

/-- `env` with `item`'s theorem elaborated in it, and what that gave: with its fixed steps when it has them, and with
Lean's search when it has none or they leave it unproven and the runner holds that search. -/
def proved (env : Environment) (item : Theorem) (shared : Shared) : IO (Environment × Elaborated) := do
  unless item.steps.isEmpty do
    let fixed ← elaborate env item.statement item.steps shared.heartbeats
    if fixed.2.proved || !shared.searches then return fixed
  elaborate env item.statement item.text shared.heartbeats

/-- `env` with `item`'s theorem, and what elaborating it gave: what a check of the runner gave before, when `env` holds
each constant its declaration uses, so the runner adds the declaration Lean already checked, and else elaborated in
`env`, as [`proved`] elaborates it. -/
def withTheorem (env : Environment) (item : Theorem) (shared : Shared) : IO (Environment × Elaborated) := do
  if let some elaborated := (← shared.theorems.get).find? item.statement then
    if let some (declaration, uses) := elaborated.declaration then
      if holdsAll env uses then
        if let .ok added := env.addDeclCore 0 0 declaration none (doCheck := false) then
          return (added, elaborated)
  let (env, elaborated) ← proved env item shared
  shared.theorems.modify (·.insert item.statement elaborated)
  return (env, elaborated)

/-! The search for the small values that break a law whose proof stays open, spec section 28: `<statement>.«$breaking»`,
`Sharp.Law.breaking` of the law's claim on them, whose text the request holds beside the law's theorem, so only a law
whose proof stays open elaborates and compiles it. -/

/-- What the search for the small values that break a law gave. -/
inductive Search where
  /-- No search ran: the generated modules hold none for the law, or it reaches an `opaque` constant outside Lean's own
  library, whose value its compiled code does not hold, so a run of it would not be a run of the law's code. -/
  | skipped
  /-- No small value breaks the law. -/
  | unbroken
  /-- The first small values that break the law, as PHP# writes them. -/
  | broken (values : String)

/-- Whether `name`'s declaration reaches an `opaque` constant outside Lean's own library, through the constants it uses:
an interface's values, `Sharp.upcast`, or a reader each implementation of a class gives. -/
def reachesOpaque (env : Environment) (name : Name) : Bool := Id.run do
  let mut seen : NameSet := {}
  let mut pending := #[name]
  repeat
    let some next := pending.back? | break
    pending := pending.pop
    if seen.contains next then continue
    seen := seen.insert next
    if let some index := env.getModuleIdxFor? next then
      if let some module := env.allImportedModuleNames[index.toNat]? then
        if [`Init, `Std, `Lean].contains module.getRoot then continue
    match env.find? next with
    | some (.opaqueInfo _) => return true
    | some info => pending := pending ++ info.getUsedConstantsAsSet.toArray
    | none => pure ()
  return false

unsafe def evalBreaking (env : Environment) (name : Name) : Except String (Option String) :=
  env.evalConst (Option String) {} name (checkMeta := false)

/-- Runs the search for the small values that break the law `statement`, from `text`, its claim and its `«$breaking»`,
which it elaborates in `env`, when Lean elaborates it and it runs only code Lean compiled. -/
def search (env : Environment) (statement : Name) (text : String) : IO Search := do
  if text.isEmpty then return .skipped
  let input := Parser.mkInputContext s!"open Sharp\n{text}" "<search>"
  let state ← IO.processCommands input {} (Command.mkState env {} unlinted)
  if state.commandState.messages.hasErrors then return .skipped
  let env := state.commandState.env
  let name := statement.str "$breaking"
  if !env.contains name || reachesOpaque env name then return .skipped
  return match unsafe evalBreaking env name with
    | .ok (some values) => .broken values
    | .ok none => .unbroken
    | .error _ => .skipped

/-- Takes each theorem in order, each with the theorems before it in scope, as [`withTheorem`] gives it, and appends to
`b` the count of judged ones and the answer for each: `0` when it is proven; `1`, each step whose goal stays open, as
its kind, its index and its goal, then what the search for the small values that break the law gave: `0` when none ran,
`1` when it found none, or `2` and the values it found; `2` and the statement of the law not proven that it holds
through; `3` and Lean's first error, when Lean rejected the theorem, or the axioms it rests on, when it has a gap no law
it holds through explains; or `4` when it ran out of its heartbeats outside the steps of its proof. Each answer ends with
the milliseconds the theorem took, from the start of its elaboration to the end of its judgment. -/
def answer (env : Environment) (request : Check) (b : ByteArray) (shared : Shared) : IO ByteArray := do
  let mut env := env
  let mut gaps : Array Name := #[]
  let mut b := putU32 b (request.theorems.filter (·.judged)).size.toUInt32
  for item in request.theorems do
    let started ← IO.monoNanosNow
    let (next, elaborated) ← withTheorem env item shared
    env := next
    let name := item.statement ++ `proof
    let gap := hasGap elaborated.axioms
    if gap then gaps := gaps.push name
    unless item.judged do continue
    if elaborated.outOfTime then
      b := putU8 b 4
    else if let some message := elaborated.rejected then
      b := putString (putU8 b 3) message
    else if !elaborated.declared then
      b := putString (putU8 b 3) s!"Lean declared no {name}"
    else if !elaborated.steps.isEmpty then
      b := putU32 (putU8 b 1) elaborated.steps.size.toUInt32
      for (kind, index, goal) in elaborated.steps do
        b := putString (putU32 (putU8 b kind) index.toUInt32) goal
      b := match ← search env item.statement item.search with
        | .skipped => putU8 b 0
        | .unbroken => putU8 b 1
        | .broken values => putString (putU8 b 2) values
    else if gap then
      b := match reachedOf env name gaps with
        | some law => putString (putU8 b 2) law.getPrefix.toString
        | none => putString (putU8 b 3) s!"{name} rests on the axioms {elaborated.axioms}"
    else
      b := putU8 b 0
    b := putU32 b (((← IO.monoNanosNow) - started) / 1000000).toUInt32
  return b

/-- The answer of the check of index `index`: its index, then `0` and the judgments of its theorems, `1`, a generated
module Lean rejected and the line of its first error, or `2` and a stopped module the check reaches. -/
def answerCheck (shared : Shared) (index : Nat) (request : Check) : IO ByteArray := do
  let b := putU32 .empty index.toUInt32
  match ← ((withImports index (request.modules.filter isGenerated)).run.run shared) with
  | .ok (checked, _) =>
    progress s!"theorems {index}"
    let code := checked.constants.foldStage2 (fun code name _ => code.insert name) ({} : NameSet)
    answer (Proof.generated.setState checked code) request (putU8 b 0) shared
  | .error (.rejected module line) =>
    return putU32 (putString (putU8 b 1) (written shared.sources module)) line.toUInt32
  | .error (.stopped module) => return putString (putU8 b 2) (written shared.sources module)

/-- The generated modules of `imports` whose imports lead back to them, through the generated modules `imports` maps
each to. -/
def cyclicModules (imports : NameMap (Array Name)) : NameSet := Id.run do
  let mut cyclic : NameSet := {}
  for (module, direct) in imports do
    let mut seen : NameSet := {}
    let mut pending := direct
    repeat
      let some next := pending.back? | break
      pending := pending.pop
      if next == module then
        cyclic := cyclic.insert module
        break
      if seen.contains next then continue
      seen := seen.insert next
      pending := pending ++ (imports.find? next).getD #[]
  return cyclic

partial def readAll (stream : IO.FS.Stream) (bytes : ByteArray := .empty) : IO ByteArray := do
  let chunk ← stream.read 65536
  if chunk.isEmpty then return bytes else readAll stream (bytes ++ chunk)

/-- Whether the search path holds a build of `module`. -/
def found (module : Name) : IO Bool := do
  return (← (← searchPathRef.get).findModuleWithExt "olean" module).isSome

/-- Reads the request to the end of standard input, imports the runtime library its modules and checks import, and
answers its checks on the threads it asks for, each taking the next check no thread took. Each answer is written to
standard output as soon as its check has it, in the order the checks finish. Any other failure ends the runner with its
message on standard error. -/
def respond : IO Unit := do
  let bytes ← readAll (← IO.getStdin)
  let decode {α : Type} (body : Decode α) : IO α :=
    IO.ofExcept <| (((do let _ ← kind; body).run' 0).run bytes).mapError IO.userError
  let requested ← decode kind
  unless requested == checkRequest do throw <| IO.userError s!"unknown law request kind {requested}"
  let (threads, heartbeats, stopped, sources, checks) ←
    decode (do return ((← u32).toNat, (← u32).toNat, ← many string, ← many source, ← many check))
  -- The runtime library: each module outside `Code` that a generated module or a check names, and the search path
  -- holds. A generated module that names one it does not hold is rejected alone.
  let mut wanted : Array Name := #[]
  let mut imports : NameMap (Array Name) := {}
  for source in sources do
    let imported := (← parse source).2.2.2
    imports := imports.insert (nameOf source.name) (imported.filter isGenerated)
    for module in imported do
      unless isGenerated module || wanted.contains module do wanted := wanted.push module
  for request in checks do
    for module in request.modules do
      unless isGenerated module || wanted.contains module do wanted := wanted.push module
  let library ← wanted.filterM found
  let env ← importModules (library.map ({ module := · })) {} (loadExts := true)
  let shared : Shared := {
    sources := sources.foldl (fun sources source => sources.insert (nameOf source.name) source) {}
    library := env
    searches := library.contains `Sharp.Lean.Proof
    stopped := stopped.foldl (fun stopped module => stopped.insert (nameOf module)) {}
    cyclic := cyclicModules imports
    heartbeats
    modules := ← IO.mkRef {}
    theorems := ← IO.mkRef {}
  }
  let stdout ← IO.getStdout
  stdout.write (putU32 (header checkedResponse) checks.size.toUInt32)
  stdout.flush
  -- Each check's answer, or the error that ended the thread that answered it.
  let answers ← checks.mapM fun _ => (IO.Promise.new : BaseIO (IO.Promise (Except String ByteArray)))
  let next ← IO.mkRef 0
  for _ in [0:max threads 1] do
    let _ ← IO.asTask (prio := .dedicated) do
      repeat
        let index ← next.modifyGet fun index => (index, index + 1)
        let (some request, some promise) := (checks[index]?, answers[index]?) | break
        let answered ← (answerCheck shared index request).toBaseIO
        promise.resolve (answered.mapError toString)
  let mut pending := answers.toList.map (·.result!)
  repeat
    let first :: others := pending | break
    let (answered, rest) ← IO.waitAny' (first :: others)
    stdout.write (← IO.ofExcept (answered.mapError IO.userError))
    stdout.flush
    pending := rest

end Sharp.Runner

unsafe def main : IO Unit := do
  initSearchPath (← findSysroot)
  enableInitializersExecution
  Sharp.Runner.respond
