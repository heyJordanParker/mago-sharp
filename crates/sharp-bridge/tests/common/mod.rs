//! Runs a PHP# file through the checker as `mago` runs it, then hands the checked program to the test.

// Each test file uses only some of the library declarations. A library file that fails to parse or an analysis that
// fails is a broken test, so it panics instead of joining the checker's refusal.
#![allow(dead_code, clippy::panic_in_result_fn, clippy::unwrap_in_result)]

use std::borrow::Cow;
use std::sync::LazyLock;

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
use mago_reporting::Issue;
use mago_semantics::SemanticsChecker;
use mago_sharp_bridge::CheckedProgram;
use mago_sharp_bridge::InlineForms;
use mago_sharp_bridge::Refusal;
use mago_sharp_bridge::check;
use mago_sharp_bridge::inline_forms;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::dialect::Dialect;
use mago_syntax::parser::parse_file;
use mago_syntax::parser::parse_file_with_dialect;
use mago_syntax::settings::ParserSettings;
use mago_word::WordSet;

/// `Lib\Model`, a plain PHP parent whose accessors read and write an attribute by name, as Eloquent's do.
pub const MODEL: (&str, &str) = (
    "src/Lib/Model.php",
    "<?php namespace Lib; class Model { public function getAttribute(string $key): \\Address { return new \\Address(); } public function setAttribute(string $key, mixed $value): void {} }",
);

/// `Lib\Field`, an attribute for any declaration with an optional value, label and limit.
pub const FIELD: (&str, &str) = (
    "src/Lib/Field.php",
    "<?php namespace Lib; #[\\Attribute(\\Attribute::TARGET_ALL)] final class Field { public function __construct(mixed $value = null, ?string $label = null, int $limit = 0) {} }",
);

/// `Lib\Entity`, a repeatable attribute for any declaration with an optional label and order.
pub const ENTITY: (&str, &str) = (
    "src/Lib/Entity.php",
    "<?php namespace Lib; #[\\Attribute(\\Attribute::TARGET_ALL | \\Attribute::IS_REPEATABLE)] final class Entity { public function __construct(mixed $label = null, mixed $order = null) {} }",
);

/// `App\Tenant\Searchable`, an attribute without arguments.
pub const SEARCHABLE: (&str, &str) =
    ("src/App/Tenant/Searchable.php", "<?php namespace App\\Tenant; #[\\Attribute] final class Searchable {}");

/// `Lib\HasLabel`, an interface without methods.
pub const HAS_LABEL: (&str, &str) = ("src/Lib/HasLabel.php", "<?php namespace Lib; interface HasLabel {}");

/// `App\Tenant\Missing` and `App\Tenant\Broken`, two exceptions.
pub const FAILURES: (&str, &str) = (
    "src/App/Tenant/Failures.php",
    "<?php namespace App\\Tenant; final class Missing extends \\Exception {} final class Broken extends \\Exception {}",
);

/// The standard library's `Sharp\Int`, with the signatures of its `parse` and `tryParse`.
pub const INT: (&str, &str) = (
    "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Int.sharp",
    "namespace Sharp;\n\npublic static class Int\n{\n    public static int parse(Any? value) => 0;\n\n    public static int? tryParse(Any? value) => null;\n}\n",
);

/// The standard library's `Sharp\Float`, with the signatures of its `parse` and `tryParse`.
pub const FLOAT: (&str, &str) = (
    "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Float.sharp",
    "namespace Sharp;\n\npublic static class Float\n{\n    public static float parse(Any? value) => 0.0;\n\n    public static float? tryParse(Any? value) => null;\n}\n",
);

/// The standard library's package in `vendor/`. A checked file under it is the standard library's, as the loader marks
/// a file whose nearest `composer.json` names the package.
const STANDARD_LIBRARY: &str = "vendor/heyjordanparker/php-sharp-composer/";

static PRELUDE: LazyLock<Prelude> = LazyLock::new(Prelude::build);
static PLUGIN_REGISTRY: LazyLock<PluginRegistry> = LazyLock::new(PluginRegistry::with_library_providers);

/// Parses `code` as the PHP# file at `path`, runs the semantic checks, scans it with the prelude and the `library`
/// files, each a path and its code, analyzes it and passes the result to `check`. Returns what `then` returns for the
/// checked program, or the refusal as `line:column parse error: message` or `line:column compile error: message` lines.
pub fn checked<R>(
    path: &str,
    code: &str,
    library: &[(&str, &str)],
    then: impl FnOnce(&CheckedProgram<'_>) -> R,
) -> Result<R, Vec<String>> {
    checked_inlining(path, code, library, &InlineForms::default(), then)
}

/// The inline forms of the standard library files in `library`, each checked alone first, as the orchestrator lowers
/// the library before the files that call it.
pub fn library_forms(library: &[(&str, &str)]) -> InlineForms {
    library
        .iter()
        .flat_map(|(path, code)| {
            checked(path, code, &[], inline_forms).unwrap_or_else(|refusal| panic!("{path} is checked: {refusal:?}"))
        })
        .collect()
}

/// [`checked`], with the inline forms the lowering of `code` may inline.
pub fn checked_inlining<R>(
    path: &str,
    code: &str,
    library: &[(&str, &str)],
    forms: &InlineForms,
    then: impl FnOnce(&CheckedProgram<'_>) -> R,
) -> Result<R, Vec<String>> {
    let mut file = File::ephemeral(Cow::Owned(path.as_bytes().to_vec()), Cow::Owned(code.as_bytes().to_vec()));
    file.is_standard_library = path.starts_with(STANDARD_LIBRARY);

    checked_file(&file, library, forms, then)
}

/// [`checked_inlining`] of `file` as the test builds it, such as a file a workspace root holds.
pub fn checked_file<R>(
    file: &File,
    library: &[(&str, &str)],
    forms: &InlineForms,
    then: impl FnOnce(&CheckedProgram<'_>) -> R,
) -> Result<R, Vec<String>> {
    let Prelude { mut metadata, mut symbol_references, .. } = PRELUDE.clone();
    let settings = Settings::default();
    let arena = LocalArena::new();
    for (path, code) in library {
        let file = File::ephemeral(Cow::Owned(path.as_bytes().to_vec()), Cow::Owned(code.as_bytes().to_vec()));
        let program = parse_file(&arena, &file);
        assert!(program.errors.is_empty(), "{path} parses: {:?}", program.errors);
        let names = NameResolver::new(&arena).resolve(program);
        metadata.extend(scan_program(&arena, &file, program, &names, settings.version));
    }

    let code = String::from_utf8_lossy(&file.contents);
    let program = parse_file_with_dialect(&arena, file, Dialect::Sharp, ParserSettings::default());
    let names = NameResolver::new(&arena).resolve(program);
    let semantic_issues = SemanticsChecker::new(PHPVersion::PHP85).check(file, program, &names);

    metadata.extend(scan_program(&arena, file, program, &names, settings.version));
    populate_codebase(&mut metadata, &mut symbol_references, WordSet::default(), HashSet::default());
    let mut result = AnalysisResult::new(symbol_references);
    let artifacts = Analyzer::new(&arena, file, &names, &metadata, &PLUGIN_REGISTRY, settings)
        .analyze_with_artifacts(program, &mut result)
        .expect("the analysis runs");
    let issues: Vec<Issue> = semantic_issues.into_iter().chain(result.issues).collect();

    match check(file, program, names, &artifacts, &metadata, forms, &issues) {
        Ok(checked) => Ok(then(&checked)),
        Err(Refusal::Parse(errors)) => {
            Err(errors.iter().map(|error| diagnostic(&code, Some(error.span()), "parse", &error.to_string())).collect())
        }
        Err(Refusal::Compile(errors)) => {
            Err(errors.iter().map(|issue| diagnostic(&code, issue.primary_span(), "compile", &issue.message)).collect())
        }
    }
}

/// `line:column severity error: message`, with a 1-based line and byte column, or `0:0` without a span. A line ends at
/// `\n`, `\r\n` or a lone `\r`, as the Zend scanner counts lines.
fn diagnostic(code: &str, span: Option<Span>, severity: &str, message: &str) -> String {
    let (line, column) = span.map_or((0, 0), |span| {
        let offset = span.start.offset as usize;
        let before = &code.as_bytes()[..offset];
        let mut line = 1;
        let mut start = 0;
        for (index, &byte) in before.iter().enumerate() {
            if byte == b'\n' || (byte == b'\r' && code.as_bytes().get(index + 1) != Some(&b'\n')) {
                line += 1;
                start = index + 1;
            }
        }

        (line, offset - start + 1)
    });

    format!("{line}:{column} {severity} error: {message}")
}
