//! Runs each fixture pair of `tests/cases/<construct>/` through the converter, against a project of the case's own
//! `.php` files, analyzed as `mago convert` analyzes a project. `input.php` is the file it converts, and every other
//! `.php` file, or `.sharp` file but `output.sharp`, is a class it reaches. A case is an application, as
//! `tests/cases/composer.json` says, unless a `composer.json` of its own says otherwise. As `mago convert` does, the
//! case analyzes the converted text as its package's, with the `extern namespace` lines the conversion adds, and an
//! issue the command keeps the file plain PHP for refuses it. The converted text must equal `output.sharp` byte for
//! byte, or the refusals the lines of `refusal.txt`: the converter's, each `line:column: reason`, or else the
//! analysis's, each as the command reports it, `at line <n> of its PHP#: <level> <code>: <message>`. The
//! `extern namespace` lines of a conversion must equal the lines of `extern-namespaces.txt`, and the plain PHP it reaches
//! that no `extern` line declares, each class or function by its dotted name, the lines of `undeclared.txt`, or be none
//! when the case has no such file. The case's analysis holds the `uses any` line `mago convert` writes for each.

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
use std::cell::RefCell;
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
use mago_analyzer::too_wide_members;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::reference::SymbolReferences;
use mago_converter::Converted;
use mago_converter::ExternNamespace;
use mago_converter::Input;
use mago_converter::Library;
use mago_converter::Undeclared;
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
use mago_sharp_bridge::InlineForms;
use mago_syntax::cst::Statement;
use mago_syntax::parser::parse_file;
use mago_syntax::settings::ParserSettings;

#[allow(clippy::large_include_file)]
static PRELUDE: LazyLock<Prelude> = LazyLock::new(|| {
    Prelude::decode(include_bytes!(concat!(env!("OUT_DIR"), "/prelude.bin")))
        .expect("build.rs writes a prelude that decodes")
});

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
            Statement::Class(class) => Some(class.name.value),
            Statement::Interface(interface) => Some(interface.name.value),
            Statement::Enum(enumeration) => Some(enumeration.name.value),
            Statement::Trait(r#trait) => Some(r#trait.name.value),
            _ => None,
        })
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .unwrap_or_else(|| panic!("{} declares no type", folder.display()))
}

/// A worker thread's analysis of the PHP# standard library, which every case's project holds, with the library's inline
/// forms. A case adds its own files to it and takes them out again, so the binary analyzes the library once per worker
/// thread, where a fresh analysis per project analyzed it twice for each case. The orchestrator's tests hold an
/// incremental analysis to what a full one gives.
struct Shared {
    server: Server,
    forms: InlineForms,
    /// The files of the last case, which the next case takes out first, also after a case that stopped part way.
    added: Vec<FileId>,
}

thread_local! {
    static SHARED: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

impl Shared {
    /// The standard library, analyzed. It gives `List`, `Map` and the rest their members, and its files are the
    /// project's own, as `mago compile` loads a package's PHP# files, so its forms come from the files the compile
    /// accepts.
    fn new() -> Self {
        let library = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../composer/library").canonicalize().unwrap();
        let mut database = Database::new(DatabaseConfiguration::new(&library, vec![], vec![], vec![], vec![]));
        let mut sources = Vec::new();
        files_of(&library, "sharp", &mut sources);
        for path in sources {
            database.add(sharp_file(&path, &library));
        }
        let settings = Settings {
            parser: ParserSettings::default(),
            analyzer: AnalyzerSettings::default(),
            plugin_registry: Arc::new(PluginRegistry::with_library_providers()),
            use_progress_bars: false,
            autoloader: None,
        };
        let mut server = Server::new(database.into_static(), prelude, settings);
        server.analyze().expect("the analysis runs");
        let forms = server.standard_library_forms(|issues| issues).expect("the standard library lowers");

        Self { server, forms, added: Vec::new() }
    }

    /// The project of the case at `folder`, analyzed: the library, the case's `.php` and `.sharp` files, with
    /// `converted`'s text in place of `input.php`, under its class's `.sharp` name, and a file of its `extern namespace`
    /// and `uses any` lines, when it is given.
    fn analyze(&mut self, folder: &Path, converted: Option<&Converted>) {
        let mut changed = std::mem::take(&mut self.added);
        for id in &changed {
            self.server.database_mut().delete(*id);
        }
        for file in case_files(folder, converted) {
            let id = self.server.database_mut().add(file);
            changed.push(id);
            self.added.push(id);
        }
        self.server.analyze_incremental(&changed).expect("the analysis runs");
    }
}

/// The PHP# file at `path`, marked as the loader marks a file it reads.
fn sharp_file(path: &Path, folder: &Path) -> File {
    let name = Cow::Owned(path.to_string_lossy().into_owned().into_bytes());
    let contents = Cow::Owned(std::fs::read(path).unwrap());
    let mut file = File::new(name, FileType::Host, Some(path.to_path_buf()), contents);
    mark_sharp_file(&mut file, folder);

    file
}

/// The files of the case at `folder`: its own PHP# classes, which a plain PHP project holds beside its PHP, as a
/// generic one, and its `.php` files, with `converted`'s text in place of `input.php`, and the file of its lines.
fn case_files(folder: &Path, converted: Option<&Converted>) -> Vec<File> {
    let mut sources = Vec::new();
    files_of(folder, "sharp", &mut sources);
    sources.retain(|path| path != &folder.join("output.sharp"));
    let mut files: Vec<File> = sources.iter().map(|path| sharp_file(path, folder)).collect();
    // The lines `mago convert` writes into the project's declarations file.
    if let Some(converted) =
        converted.filter(|converted| !converted.extern_namespaces.is_empty() || !converted.undeclared.is_empty())
    {
        let short = |name: &str| name.rsplit('\\').next().unwrap_or(name).to_owned();
        let (imports, externs): (Vec<String>, Vec<String>) = converted
            .undeclared
            .iter()
            .map(|undeclared| match undeclared {
                Undeclared::Class(class) => {
                    (format!("import {};", class.replace('\\', ".")), format!("extern {} uses any;", short(class)))
                }
                Undeclared::Function(namespace, function) => (
                    format!("import {};", namespace.replace('\\', ".")),
                    format!("extern {}.{function} uses any;", short(namespace)),
                ),
            })
            .unzip();
        let lines: Vec<String> = imports
            .into_iter()
            .chain(converted.extern_namespaces.iter().map(ExternNamespace::line))
            .chain(externs)
            .collect();
        let path = folder.join("AddedExterns.sharp");
        let contents = format!("namespace App;\n\n{}\n", lines.join("\n")).into_bytes();
        let mut file =
            File::new(Cow::Borrowed(b"AddedExterns.sharp"), FileType::Host, Some(path), Cow::Owned(contents));
        mark_sharp_file(&mut file, folder);
        files.push(file);
    }

    let mut paths = Vec::new();
    files_of(folder, "php", &mut paths);
    paths.sort();
    for path in paths {
        let name = path.strip_prefix(folder).unwrap().to_string_lossy().into_owned();
        let contents = std::fs::read(&path).unwrap();
        files.push(match (name.as_str(), converted) {
            ("input.php", Some(converted)) => {
                let name = format!("{}.sharp", declared(folder));
                let path = folder.join(&name);
                let name = Cow::Owned(name.into_bytes());
                let mut file = File::new(name, FileType::Host, Some(path), Cow::Owned(converted.text.clone()));
                mark_sharp_file(&mut file, folder);
                file
            }
            _ => File::new(Cow::Owned(name.into_bytes()), FileType::Host, Some(path), Cow::Owned(contents)),
        });
    }

    files
}

/// Each issue the analysis finds in `converted`, the conversion of the case at `folder`, that keeps the file plain PHP,
/// as `mago convert` reports it: an error or a warning, but the warnings decision 9 expects, each once. The project's
/// `too-wide-member` pass, which runs once the whole project is analyzed, counts its issues in the file too.
fn checked(shared: &mut Shared, folder: &Path, converted: &Converted) -> String {
    shared.analyze(folder, Some(converted));
    let server = &shared.server;
    let input = FileId::new(format!("{}.sharp", declared(folder)).as_bytes());
    let file = server.database().get(&input).unwrap();
    let (mut issues, _) = server.analyze_file_with_artifacts(input).expect("the analysis holds the converted file");
    let too_wide = too_wide_members(server.codebase(), server.symbol_references());
    issues.extend(too_wide.into_iter().filter(|issue| issue.primary_span().is_some_and(|span| span.file_id == input)));
    let mut lines: Vec<String> = Vec::new();
    for issue in issues.iter().filter(|issue| issue.level >= Level::Warning) {
        let code = issue.code.as_deref().unwrap_or_default();
        let line = issue.primary_span().map_or(0, |span| file.line_number(span.start.offset) + 1);
        let shown = format!("at line {line} of its PHP#: {} {code}: {}\n", issue.level, issue.message);
        if code != "redundant-docblock-type" && !lines.contains(&shown) {
            lines.push(shown);
        }
    }

    lines.concat()
}

/// What the converter gives for the case at `folder`: the PHP# text and its `extern namespace` lines, or each refusal
/// as `line:column: reason`.
fn converted(shared: &mut Shared, folder: &Path) -> Result<Converted, String> {
    shared.analyze(folder, None);
    let server = &shared.server;
    let library = Library::new(&shared.forms, server.codebase(), folder);
    let input = FileId::new(b"input.php");
    assert!(server.database().get(&input).is_ok(), "{} holds no input.php", folder.display());
    let file = server.database().get(&input).unwrap();
    let (_, artifacts) = server.analyze_file_with_artifacts(input).expect("the analysis holds input.php");
    let arena = LocalArena::new();
    let program = parse_file(&arena, &file);
    let names = NameResolver::new(&arena).resolve(program);
    let effects = server.effects();
    let input = Input {
        file: &file,
        program,
        names: &names,
        codebase: server.codebase(),
        artifacts: &artifacts,
        effects: &effects,
        references: server.symbol_references(),
        library: &library,
    };

    convert(&input).map_err(|refusals| {
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
    SHARED.with_borrow_mut(|shared| run_on(shared.get_or_insert_with(Shared::new), folder))
}

fn run_on(shared: &mut Shared, folder: &Path) -> Result<(), Failed> {
    let (expected_file, expected_ok) = match (folder.join("output.sharp"), folder.join("refusal.txt")) {
        (output, refusal) if output.exists() && !refusal.exists() => (output, true),
        (output, refusal) if refusal.exists() && !output.exists() => (refusal, false),
        _ => panic!("{} holds one of output.sharp and refusal.txt", folder.display()),
    };
    let expected = std::fs::read_to_string(&expected_file).unwrap();
    // The text the analysis refuses follows a failure, since its refusals name lines of it.
    let (actual, actual_ok, refused_text) = match converted(shared, folder) {
        Ok(converted) => {
            let text = String::from_utf8(converted.text.clone()).unwrap();
            match checked(shared, folder, &converted) {
                issues if issues.is_empty() => {
                    let lines = converted.extern_namespaces.iter().map(ExternNamespace::line).collect::<Vec<_>>();
                    let undeclared = converted
                        .undeclared
                        .iter()
                        .map(|undeclared| match undeclared {
                            Undeclared::Class(class) => class.replace('\\', "."),
                            Undeclared::Function(namespace, function) => {
                                format!("{}.{function}", namespace.replace('\\', "."))
                            }
                        })
                        .collect::<Vec<_>>();
                    for (kind, file, lines) in [
                        ("extern namespace lines", "extern-namespaces.txt", lines),
                        ("undeclared plain PHP", "undeclared.txt", undeclared),
                    ] {
                        let expected_lines = std::fs::read_to_string(folder.join(file)).unwrap_or_default();
                        let (lines, expected_lines) = (lines.join("\n"), expected_lines.trim_end());
                        if lines != expected_lines {
                            return Err(format!(
                                "{} adds the {kind}:\n{lines}\n--- expected, from {file}:\n{expected_lines}",
                                folder.display()
                            )
                            .into());
                        }
                    }

                    (text, true, String::new())
                }
                issues => (issues, false, format!("\n--- the conversion the analysis refuses:\n{text}")),
            }
        }
        Err(refusals) => (refusals, false, String::new()),
    };
    if actual_ok == expected_ok && actual == expected {
        return Ok(());
    }

    let kind = if actual_ok { "converted to" } else { "refused with" };
    let (folder, expected_file) = (folder.display(), expected_file.display());
    Err(format!("{folder} {kind}:\n{actual}\n--- expected, from {expected_file}:\n{expected}{refused_text}").into())
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
