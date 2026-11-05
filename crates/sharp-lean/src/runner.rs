//! The `sharp-lean` runner, which reads one request in Mago's payload encoding from standard input and writes its
//! answer to standard output: how it judged the theorems of the laws of each file a compile checks, or why a generated
//! module the file's check reaches refused it.

use std::io;
use std::path::Path;
use std::process::Output;

use mago_extension::PayloadReader;
use mago_extension::PayloadWriter;

use crate::library;
use crate::library::Stop;
use crate::package::Package;
use crate::package::Refusal;

const LAW_PROTOCOL_MAGIC: [u8; 4] = *b"MLAW";
const LAW_PROTOCOL_MAJOR: u16 = 3;
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

/// The runner's answer to one check.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    /// The judgment of each judged theorem, in order.
    Judged(Vec<Judgment>),
    /// The generated module of this name, which the check reaches, did not elaborate.
    Refused(String, Refusal),
}

/// Runs runners of `library` in `package` until each of `checks` has its answer. A runner answers the checks it gets in
/// turn: it elaborates the generated modules a check reaches, which refuse it when Lean rejects one, then each of its
/// theorems in order, and judges each judged one. Mago stops a runner at its processor-time limit or its deadline. One
/// stopped while it elaborates a generated module refuses that module's check with how it stopped, and keeps the answers
/// it wrote before; the next runner takes the checks after it, and refuses each one that reaches a stopped module
/// without elaborating it again. A runner stopped in a theorem, or one that fails, fails the compile. Returns the answer
/// of each check, in order.
pub(crate) fn check(library: &Path, package: &Package, checks: &[Check<'_>]) -> io::Result<Vec<Answer>> {
    let mut stopped: Vec<(String, Stop)> = Vec::new();
    let mut answers: Vec<Answer> = Vec::with_capacity(checks.len());
    while answers.len() < checks.len() {
        let left = &checks[answers.len()..];
        let (output, waited) = run(library, package, left.len(), &encode(&stopped, left)?)?;
        if !waited && output.status.success() {
            answers.extend(decode(&output.stdout, left, left.len(), &stopped).map_err(io::Error::other)?);
            continue;
        }

        let elaborating = elaborating(&output.stderr)
            .filter(|(index, _)| *index < left.len() && (waited || library::stopped(output.status)));
        let Some((index, module)) = elaborating else {
            return Err(failure(&output, waited, left.len()));
        };
        answers.extend(decode(&output.stdout, left, index, &stopped).map_err(io::Error::other)?);
        let stop = if waited { Stop::Waited } else { Stop::Processor };
        answers.push(Answer::Refused(module.clone(), Refusal::Stopped(stop)));
        stopped.push((module, stop));
    }

    Ok(answers)
}

/// The processor seconds the runner of `checks` checks may use: `library::processor_seconds()` for each, as each
/// check had a runner of its own before one runner answered them all.
fn processor_seconds(checks: usize) -> u64 {
    library::processor_seconds().saturating_mul(u64::try_from(checks).unwrap_or(u64::MAX))
}

/// The check and the generated module the runner was elaborating when it ended: the last line of its progress on
/// standard error, unless that line started the check's theorems.
fn elaborating(stderr: &[u8]) -> Option<(usize, String)> {
    let stderr = String::from_utf8_lossy(stderr);
    let last = stderr.lines().rev().find(|line| line.starts_with("module ") || line.starts_with("theorems "))?;
    let (index, module) = last.strip_prefix("module ")?.split_once(' ')?;

    Some((index.parse().ok()?, module.to_owned()))
}

/// The error of the runner of `checks` checks that failed, or that Mago stopped where it elaborated no generated
/// module. The debug log holds its standard error.
fn failure(output: &Output, waited: bool, checks: usize) -> io::Error {
    tracing::debug!(
        "The law checker's runner exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let failure = if waited {
        format!("did not finish within {} seconds", library::deadline().as_secs())
    } else if library::stopped(output.status) {
        format!(
            "ran past {} processor seconds, the {} the checker sets on each check, for its {checks} checks",
            processor_seconds(checks),
            library::processor_seconds(),
        )
    } else {
        format!("failed with {}", output.status)
    };

    io::Error::other(format!("the law checker {failure}. Run vendor/bin/mago compile with MAGO_LOG=debug to see why"))
}

/// Runs the runner on one request of `checks` checks, under `processor_seconds(checks)` of processor time, and returns
/// how it exited, what it wrote, and whether Mago ended it at `library::deadline()`, which counts from the last thing
/// it wrote.
///
/// One runner answers every check of a compile, since Lean cannot free an imported environment in a process that
/// elaborated with it, so each runner imports Lean and the runtime library once. `lake env` gives the runner the
/// package's search path, and on Windows the folder of Lean's shared libraries, which an executable that interprets
/// Lean links to there.
fn run(library: &Path, package: &Package, checks: usize, request: &[u8]) -> io::Result<(Output, bool)> {
    library::output_before_deadline(
        package
            .lake
            .command_within(processor_seconds(checks))
            .arg("env")
            .arg(library::runner(library))
            .current_dir(&package.directory),
        request,
    )
}

/// A `Check` request: each module an earlier runner of the compile stopped in, then for each check, each module to
/// import, then each theorem's statement, text, and whether the answer judges it.
fn encode(stopped: &[(String, Stop)], checks: &[Check<'_>]) -> io::Result<Vec<u8>> {
    let mut writer = PayloadWriter::new();
    writer.write_raw(&LAW_PROTOCOL_MAGIC);
    writer.write_u16(LAW_PROTOCOL_MAJOR);
    writer.write_u16(LAW_PROTOCOL_MINOR);
    writer.write_u16(CHECK_REQUEST);
    writer.write_u16(0);
    writer.write_length(stopped.len()).map_err(io::Error::other)?;
    for (module, _) in stopped {
        writer.write_string(module).map_err(io::Error::other)?;
    }
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

/// The answers of the first `answered` of `checks` in a `Checked` answer. It holds the answer of every check when
/// `answered` is all of them, and when Mago stopped its runner, it may end anywhere after those. `stopped` holds how
/// each module the request named stopped.
fn decode(
    payload: &[u8],
    checks: &[Check<'_>],
    answered: usize,
    stopped: &[(String, Stop)],
) -> Result<Vec<Answer>, String> {
    if answered == 0 {
        return Ok(Vec::new());
    }
    let Some(answered_checks) = checks.get(..answered) else {
        return Err(format!("the runner was stopped in check {answered} of {}", checks.len()));
    };

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
    let kind = reader.read_u16("law message kind").map_err(error)?;
    if kind != CHECKED_RESPONSE {
        return Err(format!("the runner answered with the unknown law message kind {kind}"));
    }
    reader.read_u16("law message reserved header").map_err(error)?;

    // A stopped runner's answer may end before its count of checks, so the count is bounded by the checks asked.
    let count = reader.read_count("check count", checks.len()).map_err(error)?;
    if count != checks.len() {
        return Err(format!("the runner answered {count} checks of {}", checks.len()));
    }
    let mut answers = Vec::with_capacity(answered);
    for check in answered_checks {
        let answer = match reader.read_u8("check answer").map_err(error)? {
            0 => {
                let judged = check.theorems.iter().filter(|theorem| theorem.judged).count();
                let count = reader.read_count("judgment count", reader.remaining()).map_err(error)?;
                if count != judged {
                    return Err(format!("the runner judged {count} laws of {judged}"));
                }
                Answer::Judged(judgments(&mut reader, count)?)
            }
            1 => {
                let module = reader.read_string("rejected module").map_err(error)?;
                let line = reader.read_u32("rejected line").map_err(error)?;
                Answer::Refused(module, Refusal::Rejected(line))
            }
            2 => {
                let module = reader.read_string("stopped module").map_err(error)?;
                let Some((_, stop)) = stopped.iter().find(|(known, _)| *known == module) else {
                    return Err(format!("the runner refused a check through {module}, which stopped no runner"));
                };
                Answer::Refused(module, Refusal::Stopped(*stop))
            }
            code => return Err(format!("the runner answered a check with the unknown answer {code}")),
        };
        answers.push(answer);
    }
    if answered == checks.len() {
        reader.finish().map_err(error)?;
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Checked` answer's header for `checks` checks, as the runner writes it before its first answer.
    fn answer_header(checks: usize) -> PayloadWriter {
        let mut writer = PayloadWriter::new();
        writer.write_raw(&LAW_PROTOCOL_MAGIC);
        writer.write_u16(LAW_PROTOCOL_MAJOR);
        writer.write_u16(LAW_PROTOCOL_MINOR);
        writer.write_u16(CHECKED_RESPONSE);
        writer.write_u16(0);
        let _ = writer.write_length(checks);
        writer
    }

    /// The runner's progress names the check and the generated module it was elaborating when Mago stopped it, and a
    /// runner stopped in a check's theorems, or before its progress began, was elaborating no module.
    #[test]
    fn a_stopped_runner_was_elaborating_the_module_its_last_progress_line_names() {
        let progress = b"module 0 Code.App.Fine\ntheorems 0\nmodule 1 Code.App.Shared\nmodule 1 Code.App.Store\n";

        assert_eq!(elaborating(progress), Some((1, "Code.App.Store".to_owned())));
        assert_eq!(elaborating(b"module 0 Code.App.Fine\ntheorems 0\n"), None);
        assert_eq!(elaborating(b"uncaught exception: the payload ends early\n"), None);
    }

    /// A runner Mago stopped in its third check leaves the answers of the two before it, which count, and whatever it
    /// wrote after them, which does not. A check refused through a module an earlier runner stopped in is refused with
    /// how that runner stopped, and one refused through a module Lean rejected with the line of its first error.
    #[test]
    fn a_stopped_runner_keeps_the_answers_it_wrote_before_the_check_it_stopped_in() {
        let check = || Check {
            modules: vec!["Code.App.Shared.Money".to_owned()],
            theorems: vec![Theorem { statement: "App.Shared.Money.addKeepsCurrency", text: "", judged: true }],
        };
        let checks = [check(), check(), check()];
        let stopped = [("Code.App.Tenant.Store.Module".to_owned(), Stop::Waited)];
        let mut writer = answer_header(3);
        writer.write_u8(2);
        let _ = writer.write_string("Code.App.Tenant.Store.Module");
        writer.write_u8(1);
        let _ = writer.write_string("Code.App.Shared.Money");
        writer.write_u32(26);
        writer.write_raw(b"1000000\n");

        assert_eq!(
            decode(&writer.finish(), &checks, 2, &stopped),
            Ok(vec![
                Answer::Refused("Code.App.Tenant.Store.Module".to_owned(), Refusal::Stopped(Stop::Waited)),
                Answer::Refused("Code.App.Shared.Money".to_owned(), Refusal::Rejected(26)),
            ])
        );
        assert_eq!(decode(b"1000000\n", &checks, 0, &stopped), Ok(Vec::new()));
    }
}
