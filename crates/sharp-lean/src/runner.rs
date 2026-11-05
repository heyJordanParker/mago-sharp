//! The `sharp-lean` runner, which reads one request in Mago's payload encoding from standard input and writes its
//! answer to standard output: how it judged the theorems of the laws of each file a compile checks.

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
const LAW_PROTOCOL_MAJOR: u16 = 2;
const LAW_PROTOCOL_MINOR: u16 = 0;
const CHECK_REQUEST: u16 = 1;
const CHECKED_RESPONSE: u16 = 2;

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

/// One check of a request: the theorems of one file's laws, and the modules they import.
pub(crate) struct Check<'check> {
    pub(crate) modules: Vec<String>,
    /// In order, each after every theorem it uses.
    pub(crate) theorems: Vec<Theorem<'check>>,
}

/// Runs one runner of `library` on the modules Lake built in `package`, which answers every check of `checks` in turn:
/// it elaborates the modules the check imports and each of its theorems in order, and judges each judged one. Returns
/// the judgments of each check, in order.
pub(crate) fn check(library: &Path, package: &Package, checks: &[Check<'_>]) -> io::Result<Vec<Vec<Judgment>>> {
    let output = run(library, package, checks.len(), &encode(checks)?)?;

    decode(&answer(output, checks.len())?, checks).map_err(io::Error::other)
}

/// The processor seconds the runner of `checks` checks may use: `library::processor_seconds()` for each, as each
/// check had a runner of its own before one runner answered them all.
fn processor_seconds(checks: usize) -> u64 {
    library::processor_seconds().saturating_mul(u64::try_from(checks).unwrap_or(u64::MAX))
}

/// The answer the runner of `checks` checks wrote, or an error with its exit status. The debug log holds its standard
/// error.
fn answer(output: Output, checks: usize) -> io::Result<Vec<u8>> {
    if output.status.success() {
        return Ok(output.stdout);
    }

    tracing::debug!(
        "The law checker's runner exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let failure = if library::stopped(output.status) {
        format!(
            "ran past {} processor seconds, the {} the checker sets on each check, for its {checks} checks",
            processor_seconds(checks),
            library::processor_seconds(),
        )
    } else {
        format!("failed with {}", output.status)
    };

    Err(io::Error::other(format!(
        "the law checker {failure}. Run vendor/bin/mago compile with MAGO_LOG=debug to see why"
    )))
}

/// Runs the runner on one request of `checks` checks, under `processor_seconds(checks)` of processor time, and returns
/// how it exited and what it wrote.
///
/// One runner answers every check of a compile, since Lean cannot free an imported environment in a process that
/// elaborated with it, so each runner imports Lean and the runtime library once. `lake env` gives the runner the
/// package's search path, and on Windows the folder of Lean's shared libraries, which an executable that interprets
/// Lean links to there.
fn run(library: &Path, package: &Package, checks: usize, request: &[u8]) -> io::Result<Output> {
    let mut runner = package
        .lake
        .command_within(processor_seconds(checks))
        .arg("env")
        .arg(library::runner(library))
        .current_dir(&package.directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // The runner reads the whole request before it writes, so writing it all first cannot fill a pipe both wait on. A
    // runner that fails before it reads the request is reported by its exit status and standard error.
    let mut input = runner.stdin.take().ok_or_else(|| io::Error::other("the law checker has no standard input"))?;
    let written = input.write_all(request);
    drop(input);

    let output = runner.wait_with_output()?;
    if output.status.success() {
        written?;
    }

    Ok(output)
}

/// A `Check` request: for each check, each module to import, then each theorem's statement, text, and whether the
/// answer judges it.
fn encode(checks: &[Check<'_>]) -> io::Result<Vec<u8>> {
    let mut writer = PayloadWriter::new();
    writer.write_raw(&LAW_PROTOCOL_MAGIC);
    writer.write_u16(LAW_PROTOCOL_MAJOR);
    writer.write_u16(LAW_PROTOCOL_MINOR);
    writer.write_u16(CHECK_REQUEST);
    writer.write_u16(0);
    writer.write_length(checks.len()).map_err(io::Error::other)?;
    for check in checks {
        writer.write_length(check.modules.len()).map_err(io::Error::other)?;
        for module in &check.modules {
            writer.write_string(module).map_err(io::Error::other)?;
        }
        writer.write_length(check.theorems.len()).map_err(io::Error::other)?;
        for theorem in &check.theorems {
            writer.write_string(theorem.statement).map_err(io::Error::other)?;
            writer.write_string(theorem.text).map_err(io::Error::other)?;
            writer.write_u8(u8::from(theorem.judged));
        }
    }

    Ok(writer.finish())
}

/// A `Checked` answer: for each of `checks`, the judgment of each of its judged theorems.
fn decode(payload: &[u8], checks: &[Check<'_>]) -> Result<Vec<Vec<Judgment>>, String> {
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

    let answered = reader.read_count("check count", reader.remaining()).map_err(error)?;
    if answered != checks.len() {
        return Err(format!("the runner answered {answered} checks of {}", checks.len()));
    }
    let mut answers = Vec::with_capacity(answered);
    for check in checks {
        let judged = check.theorems.iter().filter(|theorem| theorem.judged).count();
        let count = reader.read_count("judgment count", reader.remaining()).map_err(error)?;
        if count != judged {
            return Err(format!("the runner judged {count} laws of {judged}"));
        }
        answers.push(judgments(&mut reader, count)?);
    }
    reader.finish().map_err(error)?;

    Ok(answers)
}

/// The next `count` judgments of `reader`.
fn judgments(reader: &mut PayloadReader<'_>, count: usize) -> Result<Vec<Judgment>, String> {
    let error = |error: mago_extension::PayloadError| error.to_string();
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

    Ok(judgments)
}
