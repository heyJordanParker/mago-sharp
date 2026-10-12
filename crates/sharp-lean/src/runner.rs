//! The `sharp-lean` runner, which reads one request in Mago's payload encoding from standard input and writes its
//! answer to standard output: a law file's proofs, or the facts that break the structure rules.

use std::io;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

use mago_extension::PayloadReader;
use mago_extension::PayloadWriter;

use crate::facts::Facts;
use crate::facts::Kind;
use crate::library;

const LAW_PROTOCOL_MAGIC: [u8; 4] = *b"MLAW";
const LAW_PROTOCOL_MAJOR: u16 = 1;
const LAW_PROTOCOL_MINOR: u16 = 0;
const CHECK_REQUEST: u16 = 1;
const CHECKED_RESPONSE: u16 = 2;
const RULES_REQUEST: u16 = 3;
const BROKEN_RESPONSE: u16 = 4;

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

/// A structure rule that facts break.
#[derive(Debug)]
pub(crate) struct Broken {
    /// The index of the rule's module in the request.
    pub(crate) module: usize,
    /// The rule's Lean name.
    pub(crate) rule: String,
    /// Each fact that breaks it: the index of its file in the request, and its index in that file's facts.
    pub(crate) facts: Vec<(usize, usize)>,
}

/// Runs the runner of `library` on the modules Lake built in `package`, which imports `module` and answers each
/// question, counting only the theorems of `proof_module`, or none when it is empty.
pub(crate) fn check(
    library: &Path,
    package: &Path,
    module: &str,
    proof_module: &str,
    questions: &[Question<'_>],
) -> io::Result<Vec<Answer>> {
    let output = run(library, package, &encode(module, proof_module, questions)?)?;

    decode(&output, questions.len()).map_err(io::Error::other)
}

/// Runs the runner of `library` on the modules Lake built in `package`, which imports every rules module, each named
/// with the namespace its rules constrain, and answers with each rule and the facts of `files` that break it.
pub(crate) fn rules(
    library: &Path,
    package: &Path,
    modules: &[(&str, &str)],
    files: &[(&str, &Facts)],
) -> io::Result<Vec<Broken>> {
    let output = run(library, package, &encode_rules(modules, files)?)?;

    decode_broken(&output, modules.len(), files).map_err(io::Error::other)
}

/// Runs the runner on one request and returns its answer.
///
/// Each call runs its own runner, since Lean cannot free an imported environment in a process that elaborated with
/// it. `lake env` gives the runner the package's search path, and on Windows the folder of Lean's shared libraries,
/// which an executable that interprets Lean links to there.
fn run(library: &Path, package: &Path, request: &[u8]) -> io::Result<Vec<u8>> {
    let mut runner = Command::new("lake")
        .arg("env")
        .arg(library::runner(library))
        .current_dir(package)
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
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "the Lean runner failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    written?;

    Ok(output.stdout)
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

/// A `Rules` request: each module and its namespace, then each file's path and facts.
fn encode_rules(modules: &[(&str, &str)], files: &[(&str, &Facts)]) -> io::Result<Vec<u8>> {
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

    Ok(writer.finish())
}

/// A `Broken` answer, each index checked against the `modules` and `files` of its request.
fn decode_broken(payload: &[u8], modules: usize, files: &[(&str, &Facts)]) -> Result<Vec<Broken>, String> {
    let mut reader = PayloadReader::new(payload);
    let error = |error: mago_extension::PayloadError| error.to_string();
    read_header(&mut reader, BROKEN_RESPONSE)?;

    let count = reader.read_count("rule count", reader.remaining()).map_err(error)?;
    let mut rules = Vec::with_capacity(count);
    for _ in 0..count {
        let module = reader.read_u32("rule module").map_err(error)? as usize;
        if module >= modules {
            return Err(format!("the runner answered with the rules module {module} of {modules}"));
        }
        let rule = reader.read_string("rule name").map_err(error)?;
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
        rules.push(Broken { module, rule, facts });
    }
    reader.finish().map_err(error)?;

    Ok(rules)
}
