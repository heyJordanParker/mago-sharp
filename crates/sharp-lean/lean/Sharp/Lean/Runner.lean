/-
`sharp-lean`: the program Mago runs to verify PHP# laws, propose the proofs Lean's own steps find, and check structure
rules.

Mago writes one request to standard input and reads the answer from standard output. A `Check` request names one law
file's module: the proof module when Lake built it, or else the generated module of its code. The runner imports it, as
`lake exe runLinter` imports a module, and answers for each law of the request: every theorem of the proof module whose
type is exactly the law's statement, with the axioms its proof uses, and, when the request asks for one, the first
automatic proof step that proves the law. A `Rules` request names every `Module.lean` that Lake built, with the facts of
every `.sharp` file. The runner imports them all, runs each `@[rule]` over the facts under its namespace, and answers
with every fact that breaks one. A rule that trusts more than Lean's kernel checks, or that builds a namespace from a
string, does not run, and the answer says what it reached.

One runner answers one request because Lean cannot free an imported environment while the tasks that elaborated a
proposal may still hold parts of it. Lean's own server likewise restarts a file's worker to import anew.
-/
import Lean
import Std.Async.Process
import Sharp.Lean.Law
import Sharp.Lean.Facts

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
def rulesRequest : UInt16 := 3
def brokenResponse : UInt16 := 4

/-- The kind of the request. -/
def kind : Decode UInt16 := do
  unless (← read).extract 0 4 == lawMagic do throw "the request does not start with MLAW"
  set 4
  let major ← u16
  let minor ← u16
  unless major == 1 do throw s!"unsupported law protocol version {major}.{minor}"
  let kind ← u16
  let _reserved ← u16
  return kind

def header (kind : UInt16) : ByteArray :=
  lawMagic |>.push 0 |>.push 1 |>.push 0 |>.push 0 |>.push (kind >>> 8).toUInt8 |>.push kind.toUInt8
    |>.push 0 |>.push 0

/-! The `Check` request and its answer. -/

/-- One law of a request: its statement, whether to look for a proof, and the definitions a proof unfolds. -/
structure Law where
  statement : Name
  propose : Bool
  unfold : Array Name

/-- A request: the module to import, the proof module whose theorems count, or `anonymous` for none, and the laws. -/
structure Check where
  module : Name
  proofModule : Name
  laws : Array Law

def law : Decode Law := do
  return { statement := (← string).toName, propose := (← u8) != 0, unfold := (← many string).map String.toName }

def check : Decode Check := do
  return { module := (← string).toName, proofModule := (← string).toName, laws := ← many law }

/-! Verifying a proof. -/

/-- The axioms Lean's own `#print axioms` treats as standard. -/
def standardAxioms : Array Name := #[``propext, ``Classical.choice, ``Quot.sound]

/-- What a proof trusts beyond Lean's kernel. -/
inductive Verdict where
  | proved
  | gap
  | nativeDecide
  | axiom_ (name : Name)

def Verdict.code : Verdict → UInt8
  | .proved => 0
  | .gap => 1
  | .nativeDecide => 2
  | .axiom_ _ => 3

def Verdict.axiomName : Verdict → String
  | .axiom_ name => name.toString
  | _ => ""

/-- `native_decide` adds one axiom per call, named `<theorem>._native.native_decide.ax_1_1`. -/
def verdict (axioms : Array Name) : Verdict :=
  if axioms.contains ``sorryAx then .gap
  else if axioms.any (·.components.contains `_native) then .nativeDecide
  else match axioms.find? (!standardAxioms.contains ·) with
    | some name => .axiom_ name
    | none => .proved

def runCore (env : Environment) (action : CoreM α) : IO α := do
  let (value, _) ← action.toIO { fileName := "sharp-lean", fileMap := default, maxHeartbeats := 0 } { env }
  return value

/-- Every theorem of `proofModule` whose type is exactly the constant `statement`. -/
def proofsOf (env : Environment) (proofModule : Name) (statement : Name) : Array Name := Id.run do
  let some index := env.getModuleIdx? proofModule | return #[]
  let mut proofs := #[]
  for name in env.header.moduleData[index.toNat]!.constNames do
    if let some (.thmInfo info) := env.find? name then
      if info.type == .const statement [] then proofs := proofs.push name
  return proofs

/-- The line a declaration starts on, or `0` when Lean recorded none. -/
def lineOf (env : Environment) (name : Name) : IO Nat := do
  let ranges ← runCore env (findDeclarationRanges? name)
  return ranges.map (·.range.pos.line) |>.getD 0

/-! Proposing a proof. -/

/-- Lean's own steps, tried in order: `simp` with the law and every definition it reaches, then `simp +contextual`
with the same names, then that `simp` followed by `omega`, then `decide` on the unfolded law. -/
def steps (law : Law) : Array String :=
  let names := ", ".intercalate (#[law.statement].append law.unfold |>.map toString).toList
  #[s!"simp [{names}]", s!"simp +contextual [{names}]", s!"simp [{names}] <;> omega",
    s!"unfold {law.statement}\n  decide"]

/-- Whether `step` proves the law in `env` with only the standard axioms. -/
def proves (env : Environment) (law : Law) (step : String) : IO Bool := do
  let text := s!"theorem «$proposal» : {law.statement} := by\n  {step}\n"
  let input := Parser.mkInputContext text "<proposal>"
  let state ← IO.processCommands input {} (Command.mkState env {} {})
  if state.commandState.messages.hasErrors then return false
  let env := state.commandState.env
  unless env.contains `«$proposal» do return false
  let axioms ← runCore env (collectAxioms `«$proposal»)
  return match verdict axioms with
    | .proved => true
    | _ => false

/-- The first step that proves the law, or `none`. -/
def propose (env : Environment) (law : Law) : IO (Option String) := do
  for step in steps law do
    if ← proves env law step then return some step
  return none

/-- The answer for each law: its proofs, each with its line, its verdict and the axiom that refused it, then the
proposed step or an empty string. -/
def answer (env : Environment) (request : Check) : IO ByteArray := do
  let mut b := putU32 (header checkedResponse) request.laws.size.toUInt32
  for law in request.laws do
    let proofs := proofsOf env request.proofModule law.statement
    b := putU32 b proofs.size.toUInt32
    for proof in proofs do
      let v := verdict (← runCore env (collectAxioms proof))
      b := putString (putU8 (putU32 b (← lineOf env proof).toUInt32) v.code) v.axiomName
    let proposal ← if law.propose && proofs.isEmpty then propose env law else pure none
    b := putString b (proposal.getD "")
  return b

/-! The `Rules` request and its answer. -/

/-- A `Module.lean` that Lake built, and the namespace its rules constrain. -/
structure RulesModule where
  module : Name
  /-- Dot separated: "App.Shared". -/
  namespace_ : String

def location (file : String) : Decode Location :=
  return ⟨file, (← u32).toNat, (← u32).toNat, (← u32).toNat, (← u32).toNat⟩

def declarationKind : Decode DeclarationKind := do
  match ← u8 with
  | 0 => return .class | 1 => return .interface | 2 => return .enum
  | k => throw s!"unknown declaration kind {k}"

def fact (path : String) : Decode Fact := do
  match ← u8 with
  | 0 => return .declaration ⟨← string, ← declarationKind, ← location path⟩
  | 1 => return .import ⟨← string, ← string, ← location path⟩
  | 2 => return .reference ⟨← string, ← string, ← location path⟩
  | k => throw s!"unknown fact {k}"

/-- One file's facts: its path, then each fact. -/
def file : Decode (Array Fact) := do
  let path ← string
  many (fact path)

/-- Why the runner does not run a rule: it trusts more than Lean's kernel checks, or it builds a namespace from a string
instead of naming one `Code` declares, so a misspelled name would never fire. -/
inductive Refusal where
  | gap
  | axiom_ (name : Name)
  | unsafe_ (name : Name)
  /-- A workspace constant whose compiled code is not its definition: `implemented_by` or `extern`. -/
  | implementedBy (name : Name)
  /-- `Sharp.Namespace.mk` or `Sharp.Rule.breaks`, which only the runtime library and the runner use. -/
  | reserved (name : Name)
  | unevaluable (message : String)
  /-- The rule ran past its processor-time limit. -/
  | stopped
  /-- The rule waited until its deadline. -/
  | waited

def Refusal.code : Refusal → UInt8
  | .gap => 1
  | .axiom_ _ => 2
  | .unsafe_ _ => 3
  | .implementedBy _ => 4
  | .reserved _ => 5
  | .unevaluable _ => 6
  | .stopped => 7
  | .waited => 8

/-- A request: the rules modules, the facts of every `.sharp` file, the rules an earlier runner judged, which this runner
skips, the rules an earlier runner stopped, which this runner refuses as it stopped them without judging them, and the
processor milliseconds and the milliseconds each rule may use. -/
structure Rules where
  modules : Array RulesModule
  files : Array (Array Fact)
  judged : Array String
  stopped : Array (String × Refusal)
  limit : Nat
  deadline : Nat

/-- A rule an earlier runner stopped, and the code of how. -/
def stoppedRule : Decode (String × Refusal) := do
  let name ← string
  match ← u8 with
  | 7 => return (name, .stopped)
  | 8 => return (name, .waited)
  | k => throw s!"unknown stop {k}"

def rules : Decode Rules := do
  let modules ← many do return { module := (← string).toName, namespace_ := ← string }
  let files ← many file
  return {
    modules, files, judged := ← many string, stopped := ← many stoppedRule, limit := (← u32).toNat,
    deadline := (← u32).toNat }

def Refusal.detail : Refusal → String
  | .gap | .stopped | .waited => ""
  | .axiom_ name | .unsafe_ name | .implementedBy name | .reserved name => name.toString
  | .unevaluable message => message

/-- The constants only the runtime library and the runner use to make a `Namespace`. -/
def reserved : Array Name := #[`Sharp.Namespace.mk, ``Rule.breaks]

/-- The roots of the module names Lean and Mago own: Lean's, Lake's, the generated modules' and the runtime library's.
`mago compile` links no PSR-4 folder under them, and `OWNED_ROOTS` in `src/package.rs` holds the same roots. -/
def ownedRoots : Array Name := #[`Init, `Lean, `Std, `Lake, `Code, `Sharp]

/-- Whether `name` belongs to a module under a root Lean or Mago owns, which the walk trusts and does not enter. -/
def trusted (env : Environment) (name : Name) : Bool :=
  match env.getModuleIdxFor? name with
  | some index => ownedRoots.contains env.header.moduleNames[index.toNat]!.getRoot
  | none => false

/-- The first constant `rule` reaches that a rule may not: a reserved constant, an `unsafe` one, or a workspace
constant whose compiled code is not its definition. The walk goes through the workspace's constants, as `collectAxioms`
goes through every constant, and stops at those it trusts. A safe constant cannot name an unsafe one, so an unsafe
constant of Lean is reached only through an unsafe rule, which is refused for the unsafe constant it names. -/
def reached (env : Environment) (rule : Name) : Option Refusal := Id.run do
  let mut seen : NameSet := {}
  let mut pending := #[rule]
  repeat
    let some name := pending.back? | break
    pending := pending.pop
    if seen.contains name then continue
    seen := seen.insert name
    if reserved.contains (privateToUserName name) then return some (.reserved (privateToUserName name))
    let some info := env.find? name | continue
    if info.isUnsafe && name != rule then return some (.unsafe_ name)
    if trusted env name then continue
    if (Compiler.getImplementedBy? env name).isSome || isExtern env name then return some (.implementedBy name)
    for used in info.getUsedConstantsAsSet do
      pending := pending.push used
  if (env.find? rule).any (·.isUnsafe) then return some (.unsafe_ rule)
  return none

/-- Why `rule` may not run, or `none`: it reaches a constant `reached` refuses, or it uses `sorry` or an axiom beyond
Lean's standard ones, as a law's proof may not. The walk comes first, so an unsafe rule is refused for the unsafe
constant it names rather than for the axiom Lean's compiler gives that constant. -/
def refusalOf (env : Environment) (rule : Name) : IO (Option Refusal) := do
  if let some refusal := reached env rule then return some refusal
  let axioms ← runCore env (collectAxioms rule)
  if axioms.contains ``sorryAx then return some .gap
  return axioms.find? (!standardAxioms.contains ·) |>.map .axiom_

/-- The line, counted from 1, and the column, counted from 0, of the name `rule` declares, or `0, 0` when Lean recorded
none. -/
def placeOf (env : Environment) (rule : Name) : IO (Nat × Nat) := do
  let ranges ← runCore env (findDeclarationRanges? rule)
  return ranges.map (fun ranges => (ranges.selectionRange.pos.line, ranges.selectionRange.pos.column)) |>.getD (0, 0)

/-- One judged rule of the answer: its module's index, its name, then either `0` and every fact under its namespace that
breaks it, as the file's index and the fact's index in that file, or the code of its refusal, its detail, and the line
and column of its name. -/
def entry (moduleIndex : UInt32) (name : Name) (judged : Except (Refusal × Nat × Nat) (Array (UInt32 × UInt32))) :
    ByteArray := Id.run do
  let mut b := putString (putU32 .empty moduleIndex) name.toString
  match judged with
  | .ok broken =>
    b := putU32 (putU8 b 0) broken.size.toUInt32
    for (fileIndex, factIndex) in broken do
      b := putU32 (putU32 b fileIndex) factIndex
  | .error (refusal, line, column) =>
    b := putU32 (putU32 (putString (putU8 b refusal.code) refusal.detail) line.toUInt32) column.toUInt32
  return b

/-- The processor milliseconds this process has used. -/
def processorTime : IO Int := do
  let usage ← Std.IO.Process.getResourceUsage
  return usage.cpuUserTime.toInt + usage.cpuSystemTime.toInt

/-- How the runner exits when a rule runs past its processor-time limit: `PAST_LIMIT` in `src/runner.rs`. It is not 152,
which only the kernel's `SIGXCPU` gives, once the runner itself runs past its own limit. -/
def pastLimit : UInt8 := 124

/-- How the runner exits when a rule waits until its deadline: `PAST_DEADLINE` in `src/runner.rs`. -/
def pastDeadline : UInt8 := 125

/-- Runs `act` on a thread of its own while this thread reads the time every 50 milliseconds, and ends the runner as
`pastLimit` once `act` has used `limit` processor milliseconds, or as `pastDeadline` once `deadline` milliseconds have
passed, as when it waits. Lean holds no global lock, so evaluated code never keeps this thread from reading. -/
def watched {α : Type} (limit deadline : Nat) (act : IO α) : IO α := do
  let started ← processorTime
  let startedAt ← IO.monoMsNow
  let task ← IO.asTask act (prio := .dedicated)
  while !(← IO.hasFinished task) do
    if (← processorTime) - started ≥ limit then IO.Process.exit pastLimit
    if (← IO.monoMsNow) - startedAt ≥ deadline then IO.Process.exit pastDeadline
    IO.sleep 50
  IO.ofExcept task.get

/-- Writes the answer's header, then the `entry` of each rule of each rules module an earlier runner did not judge, in
request order, each as soon as it is judged, so the rules it judged survive a stop. Before it judges a rule, it writes
`judging` and the rule's name on standard error, so Mago knows which rule ran when the runner stopped. Each rule gets
the request's `limit` and `deadline` of its own, which `watched` keeps. -/
unsafe def judge (env : Environment) (request : Rules) : IO Unit := do
  let stdout ← IO.getStdout
  let stderr ← IO.getStderr
  let edges := request.files.foldl (init := #[]) fun edges facts =>
    facts.foldl (init := edges) fun edges fact =>
      match fact with
      | .import i => match i.edge with | some edge => edges.push edge | none => edges
      | _ => edges
  let graph := Graph.ofEdges edges
  stdout.write (header brokenResponse)
  stdout.flush
  let mut index : UInt32 := 0
  for rulesModule in request.modules do
    let some moduleIndex := env.getModuleIdx? rulesModule.module
      | throw <| IO.userError s!"module {rulesModule.module} was not imported"
    for name in ruleAttribute.ext.getModuleEntries env moduleIndex do
      if request.judged.contains name.toString then continue
      let judged : Except (Refusal × Nat × Nat) (Array (UInt32 × UInt32)) ←
        if let some (_, refusal) := request.stopped.find? (·.1 == name.toString) then
          pure (.error (refusal, ← placeOf env name))
        else do
          stderr.putStrLn s!"judging {name}"
          stderr.flush
          watched request.limit request.deadline do
            let evaluated : Except Refusal Rule ← match ← refusalOf env name with
              | some refusal => pure (.error refusal)
              | none => pure <| (env.evalConst Rule {} name).mapError Refusal.unevaluable
            match evaluated with
            | .error refusal => pure (.error (refusal, ← placeOf env name))
            | .ok rule => do
              let mut broken := #[]
              let mut fileIndex : UInt32 := 0
              for facts in request.files do
                let mut factIndex : UInt32 := 0
                for fact in facts do
                  if rule.breaks rulesModule.namespace_ graph fact then
                    broken := broken.push (fileIndex, factIndex)
                  factIndex := factIndex + 1
                fileIndex := fileIndex + 1
              pure (.ok broken)
      stdout.write (entry index name judged)
      stdout.flush
    index := index + 1

partial def readAll (stream : IO.FS.Stream) (bytes : ByteArray := .empty) : IO ByteArray := do
  let chunk ← stream.read 65536
  if chunk.isEmpty then return bytes else readAll stream (bytes ++ chunk)

/-- Reads the request to the end of standard input and writes its answer to standard output. Any failure ends the
runner with its message on standard error. -/
unsafe def respond : IO Unit := do
  let bytes ← readAll (← IO.getStdin)
  let decode {α : Type} (body : Decode α) : IO α :=
    IO.ofExcept <| (((do let _ ← kind; body).run' 0).run bytes).mapError IO.userError
  let requested ← decode kind
  if requested == checkRequest then
    let request ← decode check
    let stdout ← IO.getStdout
    stdout.write (← answer (← importModules #[{ module := request.module }] {} (loadExts := true)) request)
    stdout.flush
  else if requested == rulesRequest then
    let request ← decode rules
    judge (← importModules (request.modules.map ({ module := ·.module })) {} (loadExts := true)) request
  else
    throw <| IO.userError s!"unknown law request kind {requested}"

end Sharp.Runner

unsafe def main : IO Unit := do
  initSearchPath (← findSysroot)
  enableInitializersExecution
  Sharp.Runner.respond
