//! Runs the front end on each file named on the command line, so `/usr/bin/time -l` counts only its instructions.
//!
//! A `.sharp` file goes through the parser, the binder, the semantic checks and the analysis with the prelude, then
//! through `check` and `lower`, as `mago` compiles it for the engine. Any other file goes through the parser, the
//! binder and the semantic checks in the dialect its name selects, as the checker reads it. `--until parse`,
//! `--until names` or `--until checks` stops after that pass, so the difference between two counts is one pass's cost.
//! A refusal, a parse error or a semantic issue panics, so a count never measures a front end that failed.
//! `bench.sh` beside this crate generates the classes and runs this example on them.

#![allow(clippy::print_stdout, clippy::expect_used)]

use std::borrow::Cow;
use std::env;
use std::fs;
use std::path::Path;

use foldhash::HashSet;

use mago_allocator::LocalArena;
use mago_analyzer::Analyzer;
use mago_analyzer::analysis_result::AnalysisResult;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings;
use mago_codex::populator::populate_codebase;
use mago_codex::scanner::scan_program;
use mago_database::file::File;
use mago_names::resolver::NameResolver;
use mago_php_version::PHPVersion;
use mago_prelude::Prelude;
use mago_semantics::SemanticsChecker;
use mago_sharp_bridge::InlineForms;
use mago_sharp_bridge::check;
use mago_sharp_bridge::lower;
use mago_syntax::parser::parse_file;
use mago_word::WordSet;

fn main() {
    let mut arguments = env::args().skip(1).peekable();
    let until = if arguments.next_if_eq("--until").is_some() { arguments.next() } else { None };

    for path in arguments {
        let source = fs::read(&path).expect("the file is readable");
        let sharp = until.is_none() && Path::new(&path).extension().is_some_and(|extension| extension == "sharp");
        let file = File::ephemeral(Cow::Owned(path.clone().into_bytes()), Cow::Owned(source));
        let arena = LocalArena::new();
        let program = parse_file(&arena, &file);
        assert!(program.errors.is_empty(), "{path} parses without errors: {:?}", program.errors);
        if until.as_deref() == Some("parse") {
            println!("{path}: parsed");
            continue;
        }

        let names = NameResolver::new(&arena).resolve(program);
        if until.as_deref() == Some("names") {
            println!("{path}: {} names", names.len());
            continue;
        }

        let issues = SemanticsChecker::new(PHPVersion::PHP85).check(&file, program, &names);
        if !sharp {
            let messages = issues.iter().map(|issue| &issue.message).collect::<Vec<_>>();
            assert!(messages.is_empty(), "{path} checks without issues: {messages:?}");

            println!("{path}: {} names", names.len());
            continue;
        }

        let Prelude { mut metadata, mut symbol_references, .. } = Prelude::build();
        let settings = Settings::default();
        metadata.extend(scan_program(&arena, &file, program, &names, settings.version));
        populate_codebase(&mut metadata, &mut symbol_references, WordSet::default(), HashSet::default());
        let mut result = AnalysisResult::new(symbol_references);
        let registry = PluginRegistry::with_library_providers();
        let artifacts = Analyzer::new(&arena, &file, &names, &metadata, &registry, settings)
            .analyze_with_artifacts(program, &mut result)
            .expect("the analysis runs");
        let issues: Vec<_> = issues.into_iter().chain(result.issues).collect();

        let forms = InlineForms::default();
        let checked = check(&file, program, names, &artifacts, &metadata, &forms, &issues)
            .unwrap_or_else(|refusal| panic!("{path} is accepted: {refusal:?}"));
        println!("{path}: {} nodes", lower(&checked).nodes().len());
    }
}
