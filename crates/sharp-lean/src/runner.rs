//! The `sharp-lean` runner, which reads one request in Mago's payload encoding from standard input and writes its
//! answer to standard output: how it judged the theorems of a file's laws, or the facts that break the structure rules.

use std::io;
use std::io::Write;
use std::path::Path;
use std::process::Output;
use std::process::Stdio;
use std::time::Duration;

use mago_extension::PayloadReader;
use mago_extension::PayloadWriter;

use crate::facts::Facts;
use crate::facts::Kind;
use crate::library;
use crate::library::Stop;
use crate::package::Package;

const LAW_PROTOCOL_MAGIC: [u8; 4] = *b"MLAW";
const LAW_PROTOCOL_MAJOR: u16 = 1;
const LAW_PROTOCOL_MINOR: u16 = 0;
const CHECK_REQUEST: u16 = 1;
const CHECKED_RESPONSE: u16 = 2;
const RULES_REQUEST: u16 = 3;
const BROKEN_RESPONSE: u16 = 4;

/// What the runner writes on its standard error before the name of each rule it judges.
const JUDGING: &str = "judging ";

/// How the runner exits when a rule runs past `library::processor_seconds()`: `pastLimit` in `Runner.lean`.
const PAST_LIMIT: i32 = 124;

/// How the runner exits when a rule waits until `library::deadline()`: `pastDeadline` in `Runner.lean`.
const PAST_DEADLINE: i32 = 125;

/// One theorem a request elaborates.
pub(crate) struct Theorem<'theorem> {
    /// The Lean name of the statement of the law it proves.
    pub(crate) statement: &'theorem str,
    /// Its Lean text.
    pub(crate) text: &'theorem str,
    /// Whether the answer judges it: a theorem of the file's own laws, after the theorems of the laws they call.
    pub(crate) judged: bool,
}

/// How the runner judged one law's theorem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Judgment {
    Proved,
    /// Each case and condition of the proof whose goal stayed open.
    Open(Vec<Open>),
    /// The proof holds through the theorem of the law of this statement, which is not proven.
    Through(String),
    /// Lean rejected the theorem, with its first error: a bug of the checker's translation.
    Rejected(String),
}

/// A step of a proof whose goal stayed open: the case or the call of its index, and the goal as the tree
/// `Sharp/Lean/Proof.lean` writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Open {
    pub(crate) condition: bool,
    pub(crate) index: usize,
    pub(crate) goal: String,
}

/// A structure rule the runner judged: the facts that break it, or why it did not run.
#[derive(Debug)]
pub(crate) struct Broken {
    /// The index of the rule's module in the request.
    pub(crate) module: usize,
    /// The rule's Lean name.
    pub(crate) rule: String,
    /// Each fact that breaks it: the index of its file in the request, and its index in that file's facts.
    pub(crate) facts: Vec<(usize, usize)>,
    pub(crate) refused: Option<Refused>,
}

/// Why the runner did not run a rule, and where its name is in its `Module.lean`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refused {
    pub(crate) refusal: Refusal,
    /// 1-based.
    pub(crate) line: u32,
    /// 0-based, counting characters.
    pub(crate) column: u32,
}

/// What a rule reached that the runner does not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `sorry`.
    Gap,
    /// An axiom beyond Lean's standard ones.
    Axiom(String),
    /// An `unsafe` constant.
    Unsafe(String),
    /// A workspace constant whose compiled code is not its definition: `implemented_by` or `extern`.
    ImplementedBy(String),
    /// `Sharp.Namespace.mk` or `Sharp.Rule.breaks`, which build a namespace from a string.
    Reserved(String),
    /// Lean's message when it could not evaluate the rule.
    Unevaluable(String),
    /// The rule ran past `library::processor_seconds()`, or waited until `library::deadline()`.
    Stopped(Stop),
}

/// Runs the runner of `library` on the modules Lake built in `package`, which imports `modules`, elaborates each of
/// `theorems` in order, and judges each judged one.
pub(crate) fn check(
    library: &Path,
    package: &Package,
    modules: &[&str],
    theorems: &[Theorem<'_>],
) -> io::Result<Vec<Judgment>> {
    let output = run(library, package, &encode(modules, theorems)?, library::processor_seconds())?;

    decode(&answer(output)?, theorems.iter().filter(|theorem| theorem.judged).count()).map_err(io::Error::other)
}

/// Runs the runner of `library` on the modules Lake built in `package`, which imports every rules module, each named
/// with the namespace its rules constrain, and answers with each rule and the facts of `files` that break it, or why it
/// did not run the rule.
///
/// The runner names each rule on its standard error before it judges it, writes each judged rule as soon as it has one,
/// and watches each rule: once a rule runs past `library::processor_seconds()` or waits until `library::deadline()`,
/// the runner exits as `PAST_LIMIT` or `PAST_DEADLINE`. That rule is refused as `Refusal::Stopped`, and a new runner
/// judges the rules no runner judged, so every other rule still runs. The runner's own processor-time limit is twice
/// the limit, so a runner whose watch failed still ends: `SIGXCPU` then starts a new runner from the rule it stopped,
/// which is refused only when it was the first its runner judged, since it then had that limit to itself.
pub(crate) fn rules(
    library: &Path,
    package: &Package,
    modules: &[(&str, &str)],
    files: &[(&str, &Facts)],
) -> io::Result<Vec<Broken>> {
    let seconds = library::processor_seconds();
    let milliseconds = |duration: Duration| u32::try_from(duration.as_millis()).unwrap_or(u32::MAX);
    let limits = (milliseconds(Duration::from_secs(seconds)), milliseconds(library::deadline()));
    let mut judged: Vec<Broken> = Vec::new();
    let mut stopped: Vec<(String, Stop)> = Vec::new();
    loop {
        let request = encode_rules(modules, files, &judged, &stopped, limits)?;
        let output = run(library, package, &request, seconds.saturating_mul(2))?;
        let started = judging(&output.stderr);
        let stop = match output.status.code() {
            Some(PAST_LIMIT) => Some(Stop::Processor),
            Some(PAST_DEADLINE) => Some(Stop::Waited),
            _ if library::stopped(output.status) && started.len() == 1 => Some(Stop::Processor),
            _ => None,
        };
        if let Some(rule) = started.last()
            && (stop.is_some() || library::stopped(output.status))
        {
            judged.extend(decode_broken(&output.stdout, modules.len(), files).map_err(io::Error::other)?);
            stopped.extend(stop.map(|stop| (rule.clone(), stop)));
            continue;
        }

        judged.extend(decode_broken(&answer(output)?, modules.len(), files).map_err(io::Error::other)?);
        return Ok(judged);
    }
}

/// The rules the runner started to judge, in order, from the `judging <rule>` lines of its standard error.
fn judging(stderr: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stderr).lines().filter_map(|line| line.strip_prefix(JUDGING)).map(str::to_owned).collect()
}

/// The answer the runner wrote, or an error with its exit status and the rest of its standard error.
fn answer(output: Output) -> io::Result<Vec<u8>> {
    if output.status.success() {
        return Ok(output.stdout);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let message: Vec<&str> = stderr.lines().filter(|line| !line.starts_with(JUDGING)).collect();
    let failure = if library::stopped(output.status) {
        format!(
            "ran past {} processor seconds, the limit the checker sets on each Lean process",
            library::processor_seconds()
        )
    } else {
        format!("failed with {}", output.status)
    };

    Err(io::Error::other(format!("the Lean runner {failure}: {}", message.join("\n").trim())))
}

/// Runs the runner on one request, under a limit of `seconds` of processor time, and returns how it exited and what it
/// wrote.
///
/// Each call runs its own runner, since Lean cannot free an imported environment in a process that elaborated with
/// it. `lake env` gives the runner the package's search path, and on Windows the folder of Lean's shared libraries,
/// which an executable that interprets Lean links to there.
fn run(library: &Path, package: &Package, request: &[u8], seconds: u64) -> io::Result<Output> {
    let mut runner = package
        .lake
        .command_within(seconds)
        .arg("env")
        .arg(library::runner(library))
        .current_dir(&package.directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // The runner reads the whole request before it writes, so writing it all first cannot fill a pipe both wait on. A
    // runner that fails before it reads the request is reported by its exit status and standard error.
    let mut input = runner.stdin.take().ok_or_else(|| io::Error::other("the Lean runner has no standard input"))?;
    let written = input.write_all(request);
    drop(input);

    let output = runner.wait_with_output()?;
    if output.status.success() {
        written?;
    }

    Ok(output)
}

fn header(kind: u16) -> PayloadWriter {
    let mut writer = PayloadWriter::new();
    writer.write_raw(&LAW_PROTOCOL_MAGIC);
    writer.write_u16(LAW_PROTOCOL_MAJOR);
    writer.write_u16(LAW_PROTOCOL_MINOR);
    writer.write_u16(kind);
    writer.write_u16(0);

    writer
}

/// Reads the header of an answer of `kind`.
fn read_header(reader: &mut PayloadReader<'_>, kind: u16) -> Result<(), String> {
    let error = |error: mago_extension::PayloadError| error.to_string();
    if reader.read_array::<4>("law message magic").map_err(error)? != LAW_PROTOCOL_MAGIC {
        return Err("the runner's answer does not start with MLAW".to_owned());
    }
    let major = reader.read_u16("law protocol major version").map_err(error)?;
    let minor = reader.read_u16("law protocol minor version").map_err(error)?;
    if major != LAW_PROTOCOL_MAJOR {
        return Err(format!("the runner speaks law protocol {major}.{minor}"));
    }
    let answered = reader.read_u16("law message kind").map_err(error)?;
    if answered != kind {
        return Err(format!("the runner answered with the unknown law message kind {answered}"));
    }
    reader.read_u16("law message reserved header").map_err(error)?;

    Ok(())
}

/// A `Check` request: each module to import, then each theorem's statement, text, and whether the answer judges it.
fn encode(modules: &[&str], theorems: &[Theorem<'_>]) -> io::Result<Vec<u8>> {
    let mut writer = header(CHECK_REQUEST);
    writer.write_length(modules.len()).map_err(io::Error::other)?;
    for module in modules {
        writer.write_string(module).map_err(io::Error::other)?;
    }
    writer.write_length(theorems.len()).map_err(io::Error::other)?;
    for theorem in theorems {
        writer.write_string(theorem.statement).map_err(io::Error::other)?;
        writer.write_string(theorem.text).map_err(io::Error::other)?;
        writer.write_u8(u8::from(theorem.judged));
    }

    Ok(writer.finish())
}

/// A `Checked` answer: for each of the `judged` theorems, its judgment.
fn decode(payload: &[u8], judged: usize) -> Result<Vec<Judgment>, String> {
    let mut reader = PayloadReader::new(payload);
    let error = |error: mago_extension::PayloadError| error.to_string();
    read_header(&mut reader, CHECKED_RESPONSE)?;

    let count = reader.read_count("judgment count", reader.remaining()).map_err(error)?;
    if count != judged {
        return Err(format!("the runner judged {count} laws of {judged}"));
    }
    let mut judgments = Vec::with_capacity(count);
    for _ in 0..count {
        let judgment = match reader.read_u8("judgment").map_err(error)? {
            0 => Judgment::Proved,
            1 => {
                let steps = reader.read_count("open step count", reader.remaining()).map_err(error)?;
                let mut open = Vec::with_capacity(steps);
                for _ in 0..steps {
                    let condition = match reader.read_u8("open step kind").map_err(error)? {
                        0 => false,
                        1 => true,
                        kind => return Err(format!("the runner answered with the unknown open step {kind}")),
                    };
                    let index = reader.read_u32("open step index").map_err(error)? as usize;
                    let goal = reader.read_string("open step goal").map_err(error)?;
                    open.push(Open { condition, index, goal });
                }
                Judgment::Open(open)
            }
            2 => Judgment::Through(reader.read_string("unproven law").map_err(error)?),
            3 => Judgment::Rejected(reader.read_string("Lean error").map_err(error)?),
            code => return Err(format!("the runner answered with the unknown judgment {code}")),
        };
        judgments.push(judgment);
    }
    reader.finish().map_err(error)?;

    Ok(judgments)
}

/// A `Rules` request: each module and its namespace, then each file's path and facts, then each rule an earlier runner
/// judged, which the runner skips, then each rule it refuses as `Refusal::Stopped` without judging it, with the code of
/// its refusal, then the processor milliseconds and the milliseconds each rule may use.
fn encode_rules(
    modules: &[(&str, &str)],
    files: &[(&str, &Facts)],
    judged: &[Broken],
    stopped: &[(String, Stop)],
    (limit, deadline): (u32, u32),
) -> io::Result<Vec<u8>> {
    let mut writer = header(RULES_REQUEST);
    writer.write_length(modules.len()).map_err(io::Error::other)?;
    for (module, namespace) in modules {
        writer.write_string(module).map_err(io::Error::other)?;
        writer.write_string(namespace).map_err(io::Error::other)?;
    }
    writer.write_length(files.len()).map_err(io::Error::other)?;
    for (path, facts) in files {
        writer.write_string(path).map_err(io::Error::other)?;
        writer.write_length(facts.list.len()).map_err(io::Error::other)?;
        for fact in &facts.list {
            match &fact.kind {
                Kind::Declaration(kind) => {
                    writer.write_u8(0);
                    writer.write_string(&fact.name).map_err(io::Error::other)?;
                    writer.write_u8(*kind as u8);
                }
                Kind::Import(namespace) => {
                    writer.write_u8(1);
                    writer.write_string(namespace).map_err(io::Error::other)?;
                    writer.write_string(&fact.name).map_err(io::Error::other)?;
                }
                Kind::Reference(namespace) => {
                    writer.write_u8(2);
                    writer.write_string(namespace).map_err(io::Error::other)?;
                    writer.write_string(&fact.name).map_err(io::Error::other)?;
                }
            }
            writer.write_u32(fact.location.start_line);
            writer.write_u32(fact.location.start_column);
            writer.write_u32(fact.location.end_line);
            writer.write_u32(fact.location.end_column);
        }
    }
    writer.write_length(judged.len()).map_err(io::Error::other)?;
    for broken in judged {
        writer.write_string(&broken.rule).map_err(io::Error::other)?;
    }
    writer.write_length(stopped.len()).map_err(io::Error::other)?;
    for (rule, stop) in stopped {
        writer.write_string(rule).map_err(io::Error::other)?;
        writer.write_u8(match stop {
            Stop::Processor => 7,
            Stop::Waited => 8,
        });
    }
    writer.write_u32(limit);
    writer.write_u32(deadline);

    Ok(writer.finish())
}

/// A `Broken` answer, the rules it holds up to its end, each index checked against the `modules` and `files` of its
/// request. A runner writes each rule whole as soon as it has judged it, so the answer of a stopped runner holds every
/// rule it judged.
fn decode_broken(payload: &[u8], modules: usize, files: &[(&str, &Facts)]) -> Result<Vec<Broken>, String> {
    let mut reader = PayloadReader::new(payload);
    let error = |error: mago_extension::PayloadError| error.to_string();
    read_header(&mut reader, BROKEN_RESPONSE)?;

    let mut rules = Vec::new();
    while reader.remaining() > 0 {
        let module = reader.read_u32("rule module").map_err(error)? as usize;
        if module >= modules {
            return Err(format!("the runner answered with the rules module {module} of {modules}"));
        }
        let rule = reader.read_string("rule name").map_err(error)?;
        let code = reader.read_u8("rule refusal").map_err(error)?;
        if code != 0 {
            let detail = reader.read_string("rule refusal detail").map_err(error)?;
            let refusal = match code {
                1 => Refusal::Gap,
                2 => Refusal::Axiom(detail),
                3 => Refusal::Unsafe(detail),
                4 => Refusal::ImplementedBy(detail),
                5 => Refusal::Reserved(detail),
                6 => Refusal::Unevaluable(detail),
                7 => Refusal::Stopped(Stop::Processor),
                8 => Refusal::Stopped(Stop::Waited),
                code => return Err(format!("the runner answered with the unknown rule refusal {code}")),
            };
            let line = reader.read_u32("rule line").map_err(error)?;
            let column = reader.read_u32("rule column").map_err(error)?;
            rules.push(Broken { module, rule, facts: Vec::new(), refused: Some(Refused { refusal, line, column }) });
            continue;
        }
        let fact_count = reader.read_count("broken fact count", reader.remaining()).map_err(error)?;
        let mut facts = Vec::with_capacity(fact_count);
        for _ in 0..fact_count {
            let file = reader.read_u32("broken fact file").map_err(error)? as usize;
            let fact = reader.read_u32("broken fact").map_err(error)? as usize;
            if files.get(file).is_none_or(|(_, facts)| fact >= facts.list.len()) {
                return Err(format!(
                    "the runner answered with the fact {fact} of file {file}, which the request has not"
                ));
            }
            facts.push((file, fact));
        }
        rules.push(Broken { module, rule, facts, refused: None });
    }
    reader.finish().map_err(error)?;

    Ok(rules)
}

#[cfg(test)]
#[allow(clippy::panic_in_result_fn)]
mod tests {
    use mago_extension::PayloadError;

    use super::*;

    /// A refusal this Mago does not know, from a runner of another build, is an error of the Lean step, never a rule
    /// that ran and broke nothing.
    #[test]
    fn an_answer_with_an_unknown_refusal_is_an_error() -> Result<(), PayloadError> {
        let mut writer = header(BROKEN_RESPONSE);
        writer.write_u32(0);
        writer.write_string("App.Shared.Module.neverReachesTenant")?;
        writer.write_u8(9);
        writer.write_string("App.Shared.Ledgr")?;

        assert_eq!(
            decode_broken(&writer.finish(), 1, &[]).err().as_deref(),
            Some("the runner answered with the unknown rule refusal 9")
        );

        Ok(())
    }
}
