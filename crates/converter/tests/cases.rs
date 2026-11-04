//! Runs each fixture pair of `tests/cases/<construct>/` through the converter, against a project of the case's own
//! `.php` files, analyzed as `mago convert` analyzes a project. `input.php` is the file it converts, and every other
//! `.php` file is a class it reaches. The converted text must equal `output.sharp` byte for byte, or the refusals,
//! each `line:column: reason`, the lines of `refusal.txt`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unwrap_in_result,
    clippy::missing_panics_doc,
    clippy::print_stderr
)]

use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;

use libtest_mimic::Arguments;
use libtest_mimic::Failed;
use libtest_mimic::Trial;

use mago_allocator::LocalArena;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings as AnalyzerSettings;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::reference::SymbolReferences;
use mago_converter::Input;
use mago_converter::convert;
use mago_database::Database;
use mago_database::DatabaseConfiguration;
use mago_database::DatabaseReader;
use mago_database::file::File;
use mago_database::file::FileType;
use mago_names::resolver::NameResolver;
use mago_prelude::Prelude;
use mago_server::Server;
use mago_server::Settings;
use mago_syntax::parser::parse_file;
use mago_syntax::settings::ParserSettings;

static PRELUDE: LazyLock<Prelude> = LazyLock::new(Prelude::build);

fn prelude() -> (CodebaseMetadata, SymbolReferences) {
    let Prelude { metadata, symbol_references, .. } = PRELUDE.clone();

    (metadata, symbol_references)
}

/// Each `.php` file under `folder`, its subfolders included.
fn php_files(folder: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            php_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "php") {
            files.push(path);
        }
    }
}

/// What the converter gives for the case at `folder`: the PHP# text, or each refusal as `line:column: reason`.
fn converted(folder: &Path) -> Result<String, String> {
    let mut database = Database::new(DatabaseConfiguration::new(folder, vec![], vec![], vec![], vec![]));
    let mut paths = Vec::new();
    php_files(folder, &mut paths);
    paths.sort();
    let mut input = None;
    for path in paths {
        let name = path.strip_prefix(folder).unwrap().to_string_lossy().into_owned();
        let contents = std::fs::read(&path).unwrap();
        let file = File::new(Cow::Owned(name.clone().into_bytes()), FileType::Host, Some(path), Cow::Owned(contents));
        let id = database.add(file);
        if name == "input.php" {
            input = Some(id);
        }
    }
    let input = input.unwrap_or_else(|| panic!("{} holds no input.php", folder.display()));

    let settings = Settings {
        parser: ParserSettings::default(),
        analyzer: AnalyzerSettings::default(),
        plugin_registry: Arc::new(PluginRegistry::with_library_providers()),
        use_progress_bars: false,
    };
    let mut server = Server::new(database.into_static(), prelude, settings);
    server.analyze().expect("the analysis runs");
    let file = server.database().get(&input).unwrap();
    let (_, artifacts) = server.analyze_file_with_artifacts(input).expect("the analysis holds input.php");
    let arena = LocalArena::new();
    let program = parse_file(&arena, &file);
    let names = NameResolver::new(&arena).resolve(program);
    let input = Input { file: &file, program, names: &names, codebase: server.codebase(), artifacts: &artifacts };

    convert(&input).map(|text| String::from_utf8(text).unwrap()).map_err(|refusals| {
        refusals
            .iter()
            .map(|refusal| {
                let line = file.line_number(refusal.span.start.offset) + 1;
                let column = file.column_number(refusal.span.start.offset) + 1;
                format!("{line}:{column}: {}\n", refusal.reason)
            })
            .collect::<Vec<_>>()
            .concat()
    })
}

fn run(folder: &Path) -> Result<(), Failed> {
    let (expected_file, expected_ok) = match (folder.join("output.sharp"), folder.join("refusal.txt")) {
        (output, refusal) if output.exists() && !refusal.exists() => (output, true),
        (output, refusal) if refusal.exists() && !output.exists() => (refusal, false),
        _ => panic!("{} holds one of output.sharp and refusal.txt", folder.display()),
    };
    let expected = std::fs::read_to_string(&expected_file).unwrap();
    let (actual, actual_ok) = match converted(folder) {
        Ok(text) => (text, true),
        Err(refusals) => (refusals, false),
    };
    if actual_ok == expected_ok && actual == expected {
        return Ok(());
    }

    let kind = if actual_ok { "converted to" } else { "refused with" };
    Err(format!("{} {kind}:\n{actual}\n--- expected, from {}:\n{expected}", folder.display(), expected_file.display())
        .into())
}

fn main() {
    let arguments = Arguments::from_args();
    let cases = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("cases");
    let mut folders: Vec<_> =
        std::fs::read_dir(&cases).unwrap().map(|entry| entry.unwrap().path()).filter(|path| path.is_dir()).collect();
    folders.sort();

    let trials = folders
        .into_iter()
        .map(|folder| {
            let name = folder.file_name().unwrap().to_string_lossy().into_owned();
            Trial::test(name, move || run(&folder))
        })
        .collect();

    libtest_mimic::run(&arguments, trials).exit();
}
