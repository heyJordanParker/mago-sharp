//! The `sharp-lean` runner, which reads one request in Mago's payload encoding from standard input and writes its
//! answer to standard output: how it judged the theorems of the laws of each check a compile sends, or why a generated
//! module the check reaches refused it.

use std::io;
use std::path::Path;
use std::process::Output;

use mago_extension::PayloadReader;
use mago_extension::PayloadWriter;

use crate::library;
use crate::library::Stop;
use crate::package::Module;
use crate::package::Package;
use crate::package::Refusal;

const LAW_PROTOCOL_MAGIC: [u8; 4] = *b"MLAW";
const LAW_PROTOCOL_MAJOR: u16 = 2;
const LAW_PROTOCOL_MINOR: u16 = 1;
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

/// A step of a proof whose goal stayed open: the case or the call of its index, whether its steps ran out of their
/// time budget, and the goal as the tree `Sharp/Lean/Proof.lean` writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Open {
    pub(crate) condition: bool,
    pub(crate) out_of_time: bool,
    pub(crate) index: usize,
    pub(crate) goal: String,
}

/// One check of a request: the theorems of laws of one file, and the modules they import, by name: generated modules
/// the request holds, and modules of the runtime library.
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

/// Runs runners of `library` in `package` until each of `checks` has its answer, with the generated `modules` in each
/// request. A runner answers the checks it gets in turn: it elaborates the generated modules a check reaches, which
/// refuse it when Lean rejects one, then each of its theorems in order, and judges each judged one. Mago stops a runner
/// at its processor-time limit or its deadline, which each runner has whole, as each file's runner had. A runner
/// stopped in a check after its first keeps the answers it wrote before that check, and the next runner starts at that
/// check, since the checks before it spent part of the limit. A runner stopped in its first check while it elaborates a
/// generated module refuses that check with the module and how it stopped, and the module joins `stopped`, which each
/// later runner refuses without elaborating it again. One stopped in its first check's theorems, or one that fails,
/// fails the compile. Returns the answer of each check, in order.
pub(crate) fn check(
    library: &Path,
    package: &Package,
    modules: &[Module],
    checks: &[Check<'_>],
    stopped: &mut Vec<(String, Stop)>,
) -> io::Result<Vec<Answer>> {
    let mut answers: Vec<Answer> = Vec::with_capacity(checks.len());
    while answers.len() < checks.len() {
        let left = &checks[answers.len()..];
        let (output, waited) = run(library, package, &encode(stopped, modules, left)?)?;
        if !waited && output.status.success() {
            answers.extend(decode(&output.stdout, left, left.len(), stopped).map_err(io::Error::other)?);
            continue;
        }

        let reached = progress(&output.stderr)
            .filter(|(index, _)| *index < left.len() && (waited || library::stopped(output.status)));
        let Some((index, module)) = reached else {
            return Err(failure(&output, waited));
        };
        answers.extend(decode(&output.stdout, left, index, stopped).map_err(io::Error::other)?);
        if index > 0 {
            continue;
        }
        let Some(module) = module else {
            return Err(failure(&output, waited));
        };
        let stop = if waited { Stop::Waited } else { Stop::Processor };
        answers.push(Answer::Refused(module.clone(), Refusal::Stopped(stop)));
        stopped.push((module, stop));
    }

    Ok(answers)
}

/// The check the runner was in when it ended, and the generated module it was elaborating there, if any: the last line
/// of its progress on standard error, `module <check> <module>` or `theorems <check>`.
fn progress(stderr: &[u8]) -> Option<(usize, Option<String>)> {
    let stderr = String::from_utf8_lossy(stderr);
    let last = stderr.lines().rev().find(|line| line.starts_with("module ") || line.starts_with("theorems "))?;
    if let Some(index) = last.strip_prefix("theorems ") {
        return Some((index.parse().ok()?, None));
    }
    let (index, module) = last.strip_prefix("module ")?.split_once(' ')?;

    Some((index.parse().ok()?, Some(module.to_owned())))
}

/// The error of a runner that failed, or that Mago stopped in its first check's theorems or before its progress began.
/// The debug log holds its standard error.
fn failure(output: &Output, waited: bool) -> io::Error {
    tracing::debug!(
        "The law checker's runner exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let failure = if waited {
        format!("did not finish within {} seconds", library::deadline().as_secs())
    } else if library::stopped(output.status) {
        format!("ran past {} processor seconds, the limit the checker sets on each check", library::processor_seconds())
    } else {
        format!("failed with {}", output.status)
    };

    io::Error::other(format!("the law checker {failure}. Run vendor/bin/mago compile with MAGO_LOG=debug to see why"))
}

/// Runs the runner on one request, under `library::processor_seconds()` of processor time, and returns how it exited,
/// what it wrote, and whether Mago ended it at `library::deadline()`, which counts from the last thing it wrote.
///
/// One runner answers the checks of a compile, since Lean cannot free an imported environment in a process that
/// elaborated with it, so each runner imports Lean and the runtime library once. `lake env` gives the runner the
/// package's search path, and on Windows the folder of Lean's shared libraries, which an executable that interprets
/// Lean links to there.
fn run(library: &Path, package: &Package, request: &[u8]) -> io::Result<(Output, bool)> {
    library::output_before_deadline(
        package.lake.command().arg("env").arg(library::runner(library)).current_dir(&package.directory),
        request,
    )
}

/// A `Check` request: each module an earlier runner of the compile stopped in, each generated module's name and
/// source, then for each check, each module to import, then each theorem's statement, text, and whether the answer
/// judges it.
fn encode(stopped: &[(String, Stop)], modules: &[Module], checks: &[Check<'_>]) -> io::Result<Vec<u8>> {
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
    writer.write_length(modules.len()).map_err(io::Error::other)?;
    for module in modules {
        writer.write_string(&module.name).map_err(io::Error::other)?;
        writer.write_string(&module.text).map_err(io::Error::other)?;
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
                    let (condition, out_of_time) = match reader.read_u8("open step kind").map_err(error)? {
                        0 => (false, false),
                        1 => (true, false),
                        2 => (false, true),
                        3 => (true, true),
                        kind => return Err(format!("the runner answered with the unknown open step {kind}")),
                    };
                    let index = reader.read_u32("open step index").map_err(error)? as usize;
                    let goal = reader.read_string("open step goal").map_err(error)?;
                    open.push(Open { condition, out_of_time, index, goal });
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
#[allow(clippy::panic_in_result_fn)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::library::Lake;

    /// The variable that names the library a test built to `stopping`, which that test runs.
    const LIBRARY: &str = "SHARP_LEAN_TEST_LIBRARY";

    /// The runtime library, built for these tests in a folder of the system's temporary folder, once per library source
    /// and Lean version as Mago builds it, with the `lake` on the `PATH`.
    fn library() -> io::Result<PathBuf> {
        library::build_in(&std::env::temp_dir().join("mago-sharp-lean-tests"), &Lake::on_path())
    }

    /// A package in a new folder under `root` whose runner uses `library`.
    fn package(root: &Path, library: &Path) -> io::Result<Package> {
        let package = Package::new(root);
        std::fs::create_dir_all(&package.directory)?;
        std::fs::write(package.directory.join("lean-toolchain"), library::TOOLCHAIN)?;
        package.write(library)?;

        Ok(package)
    }

    /// The generated module `name` with the source `text`.
    fn module(name: &str, text: &str) -> Module {
        Module { name: name.to_owned(), host: name.to_owned(), imports: Vec::new(), text: text.to_owned() }
    }

    /// A check of `modules` with no theorem, which only elaborates them.
    fn elaborating(modules: &[&str]) -> Check<'static> {
        Check { modules: modules.iter().map(|module| (*module).to_owned()).collect(), theorems: Vec::new() }
    }

    /// Lean compiles a module from its own imports alone, so a module that uses another without importing it is
    /// rejected, alone as beside the module it uses, and one that imports it is not.
    #[test]
    fn a_module_that_lost_an_import_is_rejected_alone_and_beside_the_module_it_needs() -> io::Result<()> {
        let library = library()?;
        let root = tempfile::tempdir()?;
        let package = package(root.path(), &library)?;
        let modules = [
            module("Code.App.Base", "import Sharp.Lean.Law\n\ndef App.Base.one : Nat := 1\n"),
            module("Code.App.Uses", "import Sharp.Lean.Law\n\ndef App.Uses.two : Nat := App.Base.one + 1\n"),
            module("Code.App.Imports", "import Code.App.Base\n\ndef App.Imports.two : Nat := App.Base.one + 1\n"),
        ];
        let rejected = || Answer::Refused("Code.App.Uses".to_owned(), Refusal::Rejected(3));

        let beside =
            check(&library, &package, &modules, &[elaborating(&["Code.App.Base", "Code.App.Uses"])], &mut Vec::new())?;
        let alone = check(&library, &package, &modules, &[elaborating(&["Code.App.Uses"])], &mut Vec::new())?;
        let imported = check(&library, &package, &modules, &[elaborating(&["Code.App.Imports"])], &mut Vec::new())?;

        assert_eq!(beside, [rejected()]);
        assert_eq!(alone, [rejected()]);
        assert_eq!(imported, [Answer::Judged(Vec::new())]);

        Ok(())
    }

    /// A generated module that imports a module the request holds no source of, nor the runtime library a build of, is
    /// rejected at line 0, and refuses only the check that reaches it: the runner answers the next check.
    #[test]
    fn an_import_of_no_module_refuses_only_the_check_that_reaches_it() -> io::Result<()> {
        let library = library()?;
        let root = tempfile::tempdir()?;
        let package = package(root.path(), &library)?;
        let modules = [
            module("Code.App.Base", "import Sharp.Lean.Law\n\ndef App.Base.one : Nat := 1\n"),
            module("Code.App.Lost", "import Code.App.Gone\n\ndef App.Lost.one : Nat := 1\n"),
            module("Code.App.Missing", "import Sharp.Lean.Gone\n\ndef App.Missing.one : Nat := 1\n"),
        ];
        let checks =
            [elaborating(&["Code.App.Lost"]), elaborating(&["Code.App.Missing"]), elaborating(&["Code.App.Base"])];

        let answers = check(&library, &package, &modules, &checks, &mut Vec::new())?;

        assert_eq!(
            answers,
            [
                Answer::Refused("Code.App.Lost".to_owned(), Refusal::Rejected(0)),
                Answer::Refused("Code.App.Missing".to_owned(), Refusal::Rejected(0)),
                Answer::Judged(Vec::new()),
            ]
        );

        Ok(())
    }

    /// The real runner, under a processor-time limit of a few seconds, proves the law of its first check, then runs
    /// forever in its second check's module, so Mago stops it there. That check's answer is not refused: the next
    /// runner starts at it with a limit of its own, stops in it again, now its first check, and refuses it through the
    /// module, which joins the stopped ones. The test runs `stopping` in a process of its own, since the limit is the
    /// one Mago runs under.
    #[cfg(unix)]
    #[test]
    fn the_real_runner_answers_the_check_before_a_stop_and_the_next_runner_refuses_the_module_that_stopped_it()
    -> io::Result<()> {
        use std::os::unix::process::CommandExt;

        let library = library()?;
        let mut stopping = std::process::Command::new(std::env::current_exe()?);
        stopping.args(["--exact", "runner::tests::stopping", "--ignored", "--test-threads=1"]).env(LIBRARY, &library);
        // SAFETY: `getrlimit` and `setrlimit` are async-signal-safe, and the closure allocates nothing.
        unsafe { stopping.pre_exec(|| library::limit_processor_time(4)) };

        let output = mago_database::process::output(&mut stopping)?;

        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "the_real_runner_answers_the_check_before_a_stop_and_the_next_runner_refuses_the_module_that_stopped_it runs it under a processor-time limit"]
    fn stopping() -> io::Result<()> {
        let library = PathBuf::from(std::env::var_os(LIBRARY).ok_or_else(|| io::Error::other("no library to use"))?);
        let root = tempfile::tempdir()?;
        let package = package(root.path(), &library)?;
        let modules = [
            module("Code.App.Fine", "import Sharp.Lean.Law\n\ndef App.Fine.one : Nat := 1\n"),
            module(
                "Code.App.Spin",
                "import Sharp.Lean.Law\n\npartial def App.Spin.spin (n : Nat) : Nat := App.Spin.spin (n + 1)\n\n#eval App.Spin.spin 0\n",
            ),
        ];
        let fine = Check {
            modules: vec!["Code.App.Fine".to_owned()],
            theorems: vec![Theorem {
                statement: "App.Fine.isOne",
                text: "theorem App.Fine.isOne.proof : App.Fine.one = 1 := rfl",
                judged: true,
            }],
        };
        let mut stopped = Vec::new();

        let answers = check(&library, &package, &modules, &[fine, elaborating(&["Code.App.Spin"])], &mut stopped)?;

        assert_eq!(answers.first(), Some(&Answer::Judged(vec![Judgment::Proved])));
        let Some(Answer::Refused(module, Refusal::Stopped(stop))) = answers.get(1) else {
            return Err(io::Error::other(format!("the second check was answered {answers:?}")));
        };
        assert_eq!(module, "Code.App.Spin");
        assert_eq!(stopped, [("Code.App.Spin".to_owned(), *stop)]);
        assert_eq!(answers.len(), 2);

        Ok(())
    }

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

    /// The runner's progress names the check it was in when Mago stopped it, and the generated module it was
    /// elaborating there, which is none in a check's theorems. A runner stopped before its progress began was in no
    /// check.
    #[test]
    fn a_stopped_runner_was_in_the_check_and_the_module_its_last_progress_line_names() {
        let lines = b"module 0 Code.App.Fine\ntheorems 0\nmodule 1 Code.App.Shared\nmodule 1 Code.App.Store\n";

        assert_eq!(progress(lines), Some((1, Some("Code.App.Store".to_owned()))));
        assert_eq!(progress(b"module 0 Code.App.Fine\ntheorems 0\n"), Some((0, None)));
        assert_eq!(progress(b"uncaught exception: the payload ends early\n"), None);
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
