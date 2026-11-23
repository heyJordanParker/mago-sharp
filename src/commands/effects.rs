//! Effects command implementation.
//!
//! `mago effects <REPORT>` checks the `uses` of each `extern` line, spec section 29, against the report a test run
//! wrote under the engine's `sharp.effect_report` setting: for each plain PHP method an `extern` line declares, how
//! many calls of it ran and each built-in they ran. A built-in's effect is the one the standard library's `extern`
//! line of it gives.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::ColorChoice;
use clap::Parser;

use mago_allocator::LocalArena;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::r#extern::ExternMetadata;
use mago_codex::scanner::scan_program;
use mago_database::DatabaseReader;
use mago_names::resolver::NameResolver;
use mago_names::short_name;
use mago_syntax::parser::parse_file_with_settings;
use mago_word::Word;
use mago_word::empty_word;
use mago_word::word;

use crate::config::Configuration;
use crate::error::Error;
use crate::utils::create_orchestrator;

/// Check each `extern` line's `uses` against what a test run's calls ran.
#[derive(Parser, Debug)]
#[command(
    name = "effects",
    about = "Check each `extern` line's `uses` against what a test run's calls ran.",
    long_about = "Check each `extern` line's `uses` against what a test run's calls ran.\n\n\
                  Reads the report the engine writes under `-d sharp.effect_report=<file>`. A line whose calls ran a \
                  built-in with an effect it doesn't list is an error, and a `uses any` line gets the `uses` its \
                  calls needed. Exits non-zero on an error."
)]
pub struct EffectsCommand {
    /// The report the engine wrote under `sharp.effect_report`.
    report: PathBuf,
}

/// What a test run's calls of one plain PHP method ran: how many calls, and how many times they ran each built-in,
/// by its name, as `PDO::query` or `file_get_contents`.
#[derive(Default)]
struct Run {
    calls: u64,
    ran: BTreeMap<String, u64>,
}

impl EffectsCommand {
    /// Prints a line for each `extern` line the report shows wrong, and one for each `uses any` line it ran, with
    /// the `uses` its calls needed.
    pub fn execute(self, configuration: Configuration, color_choice: ColorChoice) -> Result<ExitCode, Error> {
        let named = self.report.display();
        let report = std::fs::read_to_string(&self.report)
            .map_err(|error| Error::InvalidArgument(format!("Can't read the effect report `{named}`: {error}.")))?;
        let runs =
            runs(&report).ok_or_else(|| Error::InvalidArgument(format!("The effect report `{named}` is damaged.")))?;

        let orchestrator = create_orchestrator(&configuration, color_choice, false, false, false);
        let database = orchestrator.load_database(&configuration.source.workspace, true, None, None)?;
        let mut codebase = CodebaseMetadata::default();
        for file in database.files().filter(|file| file.name.ends_with(b".sharp")) {
            let arena = LocalArena::new();
            let program = parse_file_with_settings(&arena, &file, configuration.parser.to_settings());
            let resolved = NameResolver::new(&arena).resolve(program);
            codebase.extend(scan_program(&arena, &file, program, &resolved, configuration.php_version));
        }

        let mut failed = false;
        for (method, run) in &runs {
            let Some((class, member)) = method.split_once("::") else {
                continue;
            };
            let Some(declaration) = codebase.extern_of_method(word(class), word(member)) else {
                continue;
            };
            let Ok(file) = database.get_ref(&declaration.span.file_id) else {
                continue;
            };
            let written = String::from_utf8_lossy(
                &file.contents[declaration.span.start.offset as usize..declaration.span.end.offset as usize],
            );
            let target = written
                .trim_start_matches("extern")
                .trim_start()
                .trim_start_matches("event ")
                .split(|character: char| character.is_whitespace() || character == '(')
                .next()
                .unwrap_or_default();
            let uses = written.rfind("uses").map_or("", |at| written[at..].trim_end_matches(';').trim_end());

            let used = used_effects(&codebase, run);
            if declaration.uses_any {
                let named = used.keys().map(short_name).collect::<Vec<_>>().join(", ");
                let (ran, needs) =
                    if named.is_empty() { ("nothing", "none") } else { (named.as_str(), named.as_str()) };
                let calls = if run.calls == 1 { "1 call".to_owned() } else { format!("{} calls", run.calls) };
                println!(
                    "help: `{target}` is declared `uses any`, and used {ran} in {calls}: declare it `uses {needs}`."
                );
                continue;
            }

            for (effect, built_in) in used {
                if declaration.effects.iter().any(|declared| covers(&codebase, *declared, effect)) {
                    continue;
                }

                failed = true;
                println!(
                    "error: `{target}` is declared `{uses}` ({}:{}), but it ran {built_in}, which has the effect {}.",
                    declaration.file,
                    file.line_number(declaration.span.start.offset) + 1,
                    short_name(effect),
                );
            }
        }

        Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
    }
}

/// The runs of the report, each request's line of it summed by method. `None` when a line is no object of runs.
fn runs(report: &str) -> Option<BTreeMap<String, Run>> {
    let mut runs = BTreeMap::<String, Run>::new();
    for line in report.lines().filter(|line| !line.trim().is_empty()) {
        let request = serde_json::from_str::<serde_json::Value>(line).ok()?;
        for (method, recorded) in request.as_object()? {
            let run = runs.entry(method.clone()).or_default();
            run.calls += recorded.get("calls")?.as_u64()?;
            for (built_in, times) in recorded.get("ran")?.as_object()? {
                *run.ran.entry(built_in.clone()).or_default() += times.as_u64()?;
            }
        }
    }

    Some(runs)
}

/// Each effect the built-ins of `run` have, as the standard library's `extern` line of each gives it, with the first
/// built-in by name that has it.
fn used_effects<'run>(codebase: &CodebaseMetadata, run: &'run Run) -> BTreeMap<Word, &'run str> {
    let mut used = BTreeMap::new();
    for built_in in run.ran.keys() {
        let declaration: Option<&ExternMetadata> = match built_in.split_once("::") {
            Some((class, member)) => codebase.extern_of_method(word(class), word(member)),
            None => codebase.extern_of(empty_word(), word(built_in.to_ascii_lowercase())),
        };
        let Some(declaration) = declaration.filter(|declaration| declaration.is_standard_library) else {
            continue;
        };
        for effect in &declaration.effects {
            used.entry(*effect).or_insert(built_in.as_str());
        }
    }

    used
}

/// Whether the effect `declared` a line lists covers the effect `used`: it is `used`, or a class `used` extends.
fn covers(codebase: &CodebaseMetadata, declared: Word, used: Word) -> bool {
    let mut effect = Some(used);
    while let Some(class) = effect {
        if class.as_bytes().eq_ignore_ascii_case(declared.as_bytes()) {
            return true;
        }
        effect = codebase.get_class_like(class).and_then(|metadata| metadata.direct_parent_class);
    }

    false
}
