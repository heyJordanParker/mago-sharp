//! The `sharp-lean` runner, which reads one request in Mago's payload encoding from standard input and writes its
//! answer to standard output: a law file's proofs, or the facts that break the structure rules.

use std::io;
use std::io::Write;
use std::path::Path;
use std::process::Output;
use std::process::Stdio;

use mago_extension::PayloadReader;
use mago_extension::PayloadWriter;

use crate::facts::Facts;
use crate::facts::Kind;
use crate::library;
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

/// One law a request asks about.
pub(crate) struct Question<'question> {
    /// The Lean name of the law's statement.
    pub(crate) statement: &'question str,
    /// Whether to look for a proof when the proof module has none.
    pub(crate) propose: bool,
    /// The definitions a proof step unfolds besides the law.
    pub(crate) unfold: Vec<&'question str>,
}

/// What a proof trusts beyond Lean's kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Proved,
    Gap,
    NativeDecide,
    Axiom(String),
}

/// A theorem whose type is a law's statement.
#[derive(Debug, Clone)]
pub(crate) struct Proof {
    pub(crate) line: u32,
    pub(crate) verdict: Verdict,
}

/// The answer for one law: its proofs, and the proof step that proves it when it had none and one was asked for.
#[derive(Debug, Clone)]
pub(crate) struct Answer {
    pub(crate) proofs: Vec<Proof>,
    pub(crate) proposal: Option<String>,
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
    /// The runner ran past `library::processor_seconds()` while it judged the rule.
    Stopped,
}

/// Runs the runner of `library` on the modules Lake built in `package`, which imports `module` and answers each
/// question, counting only the theorems of `proof_module`, or none when it is empty.
pub(crate) fn check(
    library: &Path,
    package: &Package,
    module: &str,
    proof_module: &str,
    questions: &[Question<'_>],
) -> io::Result<Vec<Answer>> {
    let output = run(library, package, &encode(module, proof_module, questions)?, library::processor_seconds())?;

    decode(&answer(output)?, questions.len()).map_err(io::Error::other)
}

/// Runs the runner of `library` on the modules Lake built in `package`, which imports every rules module, each named
/// with the namespace its rules constrain, and answers with each rule and the facts of `files` that break it, or why it
/// did not run the rule. `rules` is at least the number of rules the modules hold.
///
/// The runner names each rule on its standard error before it judges it, writes each judged rule as soon as it has one,
/// and gives each rule `library::processor_seconds()` of its own: it watches the processor time each rule uses, and
/// exits as `SIGXCPU` would once a rule passes the limit. That rule is refused as `Refusal::Stopped`, and a new runner
/// judges the rules no runner judged, so every other rule still runs. The runner's own processor-time limit is the limit
/// of every rule and one more, so a runner whose watch failed still ends.
pub(crate) fn rules(
    library: &Path,
    package: &Package,
    modules: &[(&str, &str)],
    files: &[(&str, &Facts)],
    rules: usize,
) -> io::Result<Vec<Broken>> {
    let seconds = library::processor_seconds();
    let limit = u32::try_from(seconds.saturating_mul(1000)).unwrap_or(u32::MAX);
    let backstop = seconds.saturating_mul(u64::try_from(rules).unwrap_or(u64::MAX).saturating_add(1));
    let mut judged: Vec<Broken> = Vec::new();
    let mut stopped: Vec<String> = Vec::new();
    loop {
        let output = run(library, package, &encode_rules(modules, files, &judged, &stopped, limit)?, backstop)?;
        if library::stopped(output.status)
            && let Some(rule) = judging(&output.stderr)
        {
            judged.extend(decode_broken(&output.stdout, modules.len(), files).map_err(io::Error::other)?);
            stopped.push(rule);
            continue;
        }

        judged.extend(decode_broken(&answer(output)?, modules.len(), files).map_err(io::Error::other)?);
        return Ok(judged);
    }
}

/// The rule the runner judged last, from the last `judging <rule>` line of its standard error.
fn judging(stderr: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stderr).lines().rev().find_map(|line| line.strip_prefix(JUDGING)).map(str::to_owned)
}

/// The answer the runner wrote, or an error with its exit status and the rest of its standard error.
fn answer(output: Output) -> io::Result<Vec<u8>> {
    if output.status.success() {
        return Ok(output.stdout);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let message: Vec<&str> = stderr.lines().filter(|line| !line.starts_with(JUDGING)).collect();
    let failure = if library::stopped(output.status) {
        format!("ran past {} processor seconds, the limit Mago sets on each Lean process", library::processor_seconds())
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

fn encode(module: &str, proof_module: &str, questions: &[Question<'_>]) -> io::Result<Vec<u8>> {
    let mut writer = header(CHECK_REQUEST);
    writer.write_string(module).map_err(io::Error::other)?;
    writer.write_string(proof_module).map_err(io::Error::other)?;
    writer.write_length(questions.len()).map_err(io::Error::other)?;
    for question in questions {
        writer.write_string(question.statement).map_err(io::Error::other)?;
        writer.write_u8(u8::from(question.propose));
        writer.write_length(question.unfold.len()).map_err(io::Error::other)?;
        for name in &question.unfold {
            writer.write_string(name).map_err(io::Error::other)?;
        }
    }

    Ok(writer.finish())
}

fn decode(payload: &[u8], questions: usize) -> Result<Vec<Answer>, String> {
    let mut reader = PayloadReader::new(payload);
    let error = |error: mago_extension::PayloadError| error.to_string();
    read_header(&mut reader, CHECKED_RESPONSE)?;

    let count = reader.read_count("answer count", reader.remaining()).map_err(error)?;
    if count != questions {
        return Err(format!("the runner answered {count} laws of {questions}"));
    }
    let mut answers = Vec::with_capacity(count);
    for _ in 0..count {
        let proof_count = reader.read_count("proof count", reader.remaining()).map_err(error)?;
        let mut proofs = Vec::with_capacity(proof_count);
        for _ in 0..proof_count {
            let line = reader.read_u32("proof line").map_err(error)?;
            let code = reader.read_u8("proof verdict").map_err(error)?;
            let axiom = reader.read_string("proof axiom").map_err(error)?;
            let verdict = match code {
                0 => Verdict::Proved,
                1 => Verdict::Gap,
                2 => Verdict::NativeDecide,
                3 => Verdict::Axiom(axiom),
                code => return Err(format!("the runner answered with the unknown verdict {code}")),
            };
            proofs.push(Proof { line, verdict });
        }
        let proposal = reader.read_string("proposal").map_err(error)?;
        answers.push(Answer { proofs, proposal: (!proposal.is_empty()).then_some(proposal) });
    }
    reader.finish().map_err(error)?;

    Ok(answers)
}

/// A `Rules` request: each module and its namespace, then each file's path and facts, then each rule an earlier runner
/// judged, which the runner skips, then each rule it refuses as `Refusal::Stopped` without judging it, then the
/// processor milliseconds each rule may use.
fn encode_rules(
    modules: &[(&str, &str)],
    files: &[(&str, &Facts)],
    judged: &[Broken],
    stopped: &[String],
    limit: u32,
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
    for rule in stopped {
        writer.write_string(rule).map_err(io::Error::other)?;
    }
    writer.write_u32(limit);

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
                7 => Refusal::Stopped,
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
        writer.write_u8(8);
        writer.write_string("App.Shared.Ledgr")?;

        assert_eq!(
            decode_broken(&writer.finish(), 1, &[]).err().as_deref(),
            Some("the runner answered with the unknown rule refusal 8")
        );

        Ok(())
    }
}
