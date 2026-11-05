/-
`sharp-lean`: the program Mago runs to prove PHP# laws.

Mago writes one `Check` request to standard input and reads the answer from standard output. The request holds a check
for each `.sharp` file whose laws one `mago compile` proves: the generated modules of its code and the theorems of its
laws, after the theorems of each law they call. The runner imports the runtime library once, then for each check
elaborates its generated modules from their sources in the package on top of that one import, elaborates each theorem
with the steps of `Sharp.Lean.Proof`, and answers for each law of the file: proven, each case and condition its proof
left open with the goal, the law not proven it holds through, or Lean's error. A generated module Lean rejects refuses
its check, with the line of its first error. This elaboration is the only one a generated module gets: nothing builds
the package.

One runner answers every check of a compile because Lean cannot free an imported environment while the tasks that
elaborated a theorem may still hold parts of it, so a runner imports once. A check's modules are elaborated rather than
imported, so each check sees its own modules alone, as it would have imported them, and drops them when it ends.

The runner writes each check's answer as soon as it has it, and says on standard error what it starts: `module <check>
<module>` before it elaborates a generated module, and `theorems <check>` before it elaborates a check's theorems. So
when Mago stops a runner at its processor-time limit or its deadline, it keeps the answers written, and knows the module
that stopped it. Mago then runs a runner for the checks after it, with that module among the request's stopped ones,
which refuse each check that reaches them without elaborating them again.
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
  unless major == 3 do throw s!"unsupported law protocol version {major}.{minor}"
  let kind ← u16
  let _reserved ← u16
  return kind

def header (kind : UInt16) : ByteArray :=
  lawMagic |>.push 0 |>.push 3 |>.push 0 |>.push 0 |>.push (kind >>> 8).toUInt8 |>.push kind.toUInt8
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

def theorem_ : Decode Theorem := do
  return { statement := (← string).toName, text := ← string, judged := (← u8) != 0 }

def check : Decode Check := do
  return { modules := (← many string).map String.toName, theorems := ← many theorem_ }

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
`1` for a call's condition, its index, and its goal as Mago's tree. -/
def openStep (message : String) : Option (UInt8 × Nat × String) := do
  let kind ← match message.splitOn " " with
    | "sharp-case" :: _ => some (0 : UInt8)
    | "sharp-condition" :: _ => some 1
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
rejected the theorem, or the axioms it rests on, when it has a gap no law it holds through explains. -/
def answer (env : Environment) (request : Check) (b : ByteArray) : IO ByteArray := do
  let mut env := env
  let mut gaps : Array Name := #[]
  let mut b := putU32 b (request.theorems.filter (·.judged)).size.toUInt32
  for item in request.theorems do
    let input := Parser.mkInputContext s!"open Sharp\n{item.text}" "<law>"
    let state ← IO.processCommands input {} (Command.mkState env {} {})
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

/-! The generated modules, under the root `Code`, which a check elaborates from their sources in the package, and the
runtime library they import, which the runner imports once. -/

def isGenerated (module : Name) : Bool := module.getRoot == `Code

/-- The source of the generated `module` and its header, as Lake reads them in the package: `Code/App/Money.lean` of
`Code.App.Money`. -/
def source (module : Name) : IO (Parser.InputContext × Elab.HeaderSyntax × Parser.ModuleParserState × MessageLog) := do
  let path := System.mkFilePath (module.components.map (·.toString (escape := false))) |>.addExtension "lean"
  let input := Parser.mkInputContext (← IO.FS.readFile path) path.toString
  let (header, parserState, messages) ← Parser.parseHeader input
  return (input, header, parserState, messages)

/-- Each module of the runtime library that `modules`, or a generated module they import, imports: the modules every
check imports, so the runner imports them once. -/
partial def libraryOf (modules : Array Name) (seen : NameSet := {}) (library : Array Name := #[]) :
    IO (NameSet × Array Name) := do
  let mut (seen, library) := (seen, library)
  for module in modules do
    if seen.contains module then continue
    seen := seen.insert module
    if !isGenerated module then
      library := library.push module
      continue
    let (_, header, _, _) ← source module
    (seen, library) ← libraryOf ((Elab.headerToImports header (includeInit := false)).map (·.module)) seen library
  return (seen, library)

/-- Why a check's generated module gave it no environment. -/
inductive Refusal where
  /-- Lean rejected the module, at the line of its first error. -/
  | rejected (module : Name) (line : Nat)
  /-- The module stopped an earlier runner of the compile, so it would only stop this one again. -/
  | stopped (module : Name)

/-- Writes `line` to standard error at once, so Mago has it even when it ends the runner right after. -/
def progress (line : String) : IO Unit := do
  let stderr ← IO.getStderr
  stderr.putStrLn line
  stderr.flush

/-- `env` with the generated `module` elaborated from its source, after each generated module it imports: as the main
module, with no options, as Lean elaborates a module it compiles. `done` holds the modules `env` already has, and
`stopped` the modules no runner of the compile may elaborate again. -/
partial def elaborate (stopped : NameSet) (check : Nat) (module : Name) (env : Environment) (done : NameSet) :
    ExceptT Refusal IO (Environment × NameSet) := do
  if done.contains module then return (env, done)
  if stopped.contains module then throwThe Refusal (.stopped module)
  let (input, header, parserState, messages) ← source module
  let mut (env, done) := (env, done.insert module)
  for imported in Elab.headerToImports header (includeInit := false) do
    if isGenerated imported.module then
      (env, done) ← elaborate stopped check imported.module env done
  progress s!"module {check} {module.toString (escape := false)}"
  let main := env.mainModule
  let state ← IO.processCommands input parserState (Command.mkState (env.setMainModule module) messages {})
  if let some error := state.commandState.messages.toList.find? (·.severity == .error) then
    throwThe Refusal (.rejected module error.pos.line)
  return (state.commandState.env.setMainModule main, done)

/-- Reads the request to the end of standard input, imports the runtime library its checks import, and writes the
answer of each check, in order, to standard output, each as soon as it has it: `0` and the judgments of its theorems,
`1`, a generated module Lean rejected and the line of its first error, or `2` and a stopped module the check reaches.
Any other failure ends the runner with its message on standard error. -/
def respond : IO Unit := do
  let bytes ← readAll (← IO.getStdin)
  let decode {α : Type} (body : Decode α) : IO α :=
    IO.ofExcept <| (((do let _ ← kind; body).run' 0).run bytes).mapError IO.userError
  let requested ← decode kind
  unless requested == checkRequest do throw <| IO.userError s!"unknown law request kind {requested}"
  let (stopped, checks) ← decode (do return (← many string, ← many check))
  let stopped := stopped.foldl (fun stopped module => stopped.insert module.toName) ({} : NameSet)
  let (_, library) ← libraryOf (checks.flatMap (·.modules))
  let env ← importModules (library.map ({ module := · })) {} (loadExts := true)
  let stdout ← IO.getStdout
  stdout.write (putU32 (header checkedResponse) checks.size.toUInt32)
  stdout.flush
  for request in checks, index in [0:checks.size] do
    let elaborated ← (request.modules.filter isGenerated).foldlM
      (fun (checked, done) module => elaborate stopped index module checked done) (env, ({} : NameSet)) |>.run
    let b ← match elaborated with
      | .ok (checked, _) => do
        progress s!"theorems {index}"
        answer checked request (putU8 .empty 0)
      | .error (.rejected module line) =>
        pure <| putU32 (putString (putU8 .empty 1) (module.toString (escape := false))) line.toUInt32
      | .error (.stopped module) => pure <| putString (putU8 .empty 2) (module.toString (escape := false))
    stdout.write b
    stdout.flush

end Sharp.Runner

unsafe def main : IO Unit := do
  initSearchPath (← findSysroot)
  enableInitializersExecution
  Sharp.Runner.respond
