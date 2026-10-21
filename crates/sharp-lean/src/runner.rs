//! The `sharp-lean` runner, which reads one request in Mago's payload encoding from standard input and writes its
//! answer to standard output: a law file's proofs.

use std::io;
use std::io::Write;
use std::path::Path;
use std::process::Output;
use std::process::Stdio;

use mago_extension::PayloadReader;
use mago_extension::PayloadWriter;

use crate::library;
use crate::package::Package;

const LAW_PROTOCOL_MAGIC: [u8; 4] = *b"MLAW";
const LAW_PROTOCOL_MAJOR: u16 = 1;
const LAW_PROTOCOL_MINOR: u16 = 0;
const CHECK_REQUEST: u16 = 1;
const CHECKED_RESPONSE: u16 = 2;

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

/// Runs the runner of `library` on the modules Lake built in `package`, which imports `module` and answers each
/// question, counting only the theorems of `proof_module`, or none when it is empty.
pub(crate) fn check(
    library: &Path,
    package: &Package,
    module: &str,
    proof_module: &str,
    questions: &[Question<'_>],
) -> io::Result<Vec<Answer>> {
    let output = run(library, package, &encode(module, proof_module, questions)?)?;

    decode(&answer(output)?, questions.len()).map_err(io::Error::other)
}

/// The answer the runner wrote, or an error with its exit status and its standard error.
fn answer(output: Output) -> io::Result<Vec<u8>> {
    if output.status.success() {
        return Ok(output.stdout);
    }

    let failure = if library::stopped(output.status) {
        format!(
            "ran past {} processor seconds, the limit the checker sets on each Lean process",
            library::processor_seconds()
        )
    } else {
        format!("failed with {}", output.status)
    };

    Err(io::Error::other(format!("the Lean runner {failure}: {}", String::from_utf8_lossy(&output.stderr).trim())))
}

/// Runs the runner on one request, under `library::processor_seconds()` of processor time, and returns how it exited
/// and what it wrote.
///
/// Each call runs its own runner, since Lean cannot free an imported environment in a process that elaborated with
/// it. `lake env` gives the runner the package's search path, and on Windows the folder of Lean's shared libraries,
/// which an executable that interprets Lean links to there.
fn run(library: &Path, package: &Package, request: &[u8]) -> io::Result<Output> {
    let mut runner = package
        .lake
        .command()
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

fn encode(module: &str, proof_module: &str, questions: &[Question<'_>]) -> io::Result<Vec<u8>> {
    let mut writer = PayloadWriter::new();
    writer.write_raw(&LAW_PROTOCOL_MAGIC);
    writer.write_u16(LAW_PROTOCOL_MAJOR);
    writer.write_u16(LAW_PROTOCOL_MINOR);
    writer.write_u16(CHECK_REQUEST);
    writer.write_u16(0);
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
    if reader.read_array::<4>("law message magic").map_err(error)? != LAW_PROTOCOL_MAGIC {
        return Err("the runner's answer does not start with MLAW".to_owned());
    }
    let major = reader.read_u16("law protocol major version").map_err(error)?;
    let minor = reader.read_u16("law protocol minor version").map_err(error)?;
    if major != LAW_PROTOCOL_MAJOR {
        return Err(format!("the runner speaks law protocol {major}.{minor}"));
    }
    let answered = reader.read_u16("law message kind").map_err(error)?;
    if answered != CHECKED_RESPONSE {
        return Err(format!("the runner answered with the unknown law message kind {answered}"));
    }
    reader.read_u16("law message reserved header").map_err(error)?;

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
