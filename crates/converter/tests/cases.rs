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
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_database::loader::mark_sharp_file;
use mago_names::resolver::NameResolver;
use mago_prelude::Prelude;
use mago_reporting::Level;
use mago_server::Server;
use mago_server::Settings;
use mago_syntax::cst::Statement;
use mago_syntax::parser::parse_file;
use mago_syntax::settings::ParserSettings;

static PRELUDE: LazyLock<Prelude> = LazyLock::new(Prelude::build);

fn prelude() -> (CodebaseMetadata, SymbolReferences) {
    let Prelude { metadata, symbol_references, .. } = PRELUDE.clone();

    (metadata, symbol_references)
}

/// Each file under `folder` with the extension `extension`, its subfolders included.
fn files_of(folder: &Path, extension: &str, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files_of(&path, extension, files);
        } else if path.extension().is_some_and(|found| found == extension) {
            files.push(path);
        }
    }
}

/// The name of the class `input.php` declares, which its `.sharp` file takes (spec section 27).
fn declared(folder: &Path) -> String {
    let path = folder.join("input.php");
    let file = File::new(Cow::Borrowed(b"input.php"), FileType::Host, None, Cow::Owned(std::fs::read(path).unwrap()));
    let arena = LocalArena::new();
    let program = parse_file(&arena, &file);
    let mut statements = program.statements.iter().flat_map(|statement| match statement {
        Statement::Namespace(namespace) => namespace.statements().iter().collect::<Vec<_>>(),
        statement => vec![statement],
    });

    statements
        .find_map(|statement| match statement {
            Statement::Class(class) => Some(String::from_utf8_lossy(class.name.value).into_owned()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{} declares no class", folder.display()))
}

/// The project of the case at `folder`, analyzed: its `.php` files, with `converted` in place of `input.php`, under its
/// class's `.sharp` name, when it is given.
fn analyzed(folder: &Path, converted: Option<&str>) -> Server {
    let mut database = Database::new(DatabaseConfiguration::new(folder, vec![], vec![], vec![], vec![]));
    // The PHP# standard library, as a project's `vendor/` holds it, gives `List`, `Map` and the rest their members.
    let library = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../composer/library").canonicalize().unwrap();
    let mut sources = Vec::new();
    files_of(&library, "sharp", &mut sources);
    for path in sources {
        let name = Cow::Owned(path.to_string_lossy().into_owned().into_bytes());
        let contents = Cow::Owned(std::fs::read(&path).unwrap());
        let mut file = File::new(name, FileType::Vendored, Some(path), contents);
        mark_sharp_file(&mut file, folder);
        database.add(file);
    }

    let mut paths = Vec::new();
    files_of(folder, "php", &mut paths);
    paths.sort();
    for path in paths {
        let name = path.strip_prefix(folder).unwrap().to_string_lossy().into_owned();
        let contents = std::fs::read(&path).unwrap();
        let file = match (name.as_str(), converted) {
            ("input.php", Some(text)) => {
                let name = format!("{}.sharp", declared(folder));
                let path = folder.join(&name);
                let name = Cow::Owned(name.into_bytes());
                let mut file = File::new(name, FileType::Host, Some(path), Cow::Owned(text.as_bytes().to_vec()));
                mark_sharp_file(&mut file, folder);
                file
            }
            _ => File::new(Cow::Owned(name.into_bytes()), FileType::Host, Some(path), Cow::Owned(contents)),
        };
        database.add(file);
    }

    let settings = Settings {
        parser: ParserSettings::default(),
        analyzer: AnalyzerSettings::default(),
        plugin_registry: Arc::new(PluginRegistry::with_library_providers()),
        use_progress_bars: false,
    };
    let mut server = Server::new(database.into_static(), prelude, settings);
    server.analyze().expect("the analysis runs");

    server
}

/// Each issue the PHP# checker finds in `text`, the conversion of the case at `folder`, as `line: level code:
/// message`, but the warnings decision 9 expects.
fn checked(folder: &Path, text: &str) -> String {
    let server = analyzed(folder, Some(text));
    let input = FileId::new(format!("{}.sharp", declared(folder)).as_bytes());
    let file = server.database().get(&input).unwrap();

    server
        .analyze_file_with_artifacts(input)
        .expect("the analysis holds the converted file")
        .0
        .iter()
        .filter(|issue| issue.level >= Level::Warning)
        .filter(|issue| !matches!(issue.code.as_deref(), Some("too-wide-member" | "redundant-docblock-type")))
        .map(|issue| {
            let line = issue.primary_span().map_or(0, |span| file.line_number(span.start.offset) + 1);
            format!("{line}: {} {}: {}\n", issue.level, issue.code.as_deref().unwrap_or_default(), issue.message)
        })
        .collect::<Vec<_>>()
        .concat()
}

/// What the converter gives for the case at `folder`: the PHP# text, or each refusal as `line:column: reason`.
fn converted(folder: &Path) -> Result<String, String> {
    let server = analyzed(folder, None);
    let input = FileId::new(b"input.php");
    assert!(server.database().get(&input).is_ok(), "{} holds no input.php", folder.display());
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
        // A pair holds the text the PHP# checker accepts, so a conversion that compares equal but fails it fails.
        let issues = if actual_ok { checked(folder, &actual) } else { String::new() };
        if issues.is_empty() {
            return Ok(());
        }

        return Err(format!("{} converts to text the PHP# checker refuses:\n{issues}", folder.display()).into());
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
