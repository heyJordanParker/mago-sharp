//! Runs `mago` on a project that mixes PHP# and PHP files.

use std::borrow::Cow;
use std::path::Path;
use std::process::Command;
use std::process::Output;

use mago_database::ReadDatabase;
use mago_database::file::File;
use mago_linter::settings::Settings;
use mago_orchestrator::OrchestratorError;
use mago_orchestrator::service::lint::LintMode;
use mago_orchestrator::service::lint::LintService;
use mago_syntax::settings::ParserSettings;

const REPORT: &str = include_str!("fixtures/sharp/Report.sharp");

/// A PHP class whose property read on a possibly `null` value gets the analyzer's `?->` fix.
const BOX: &str = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Lib;\n\nfinal class Box\n{\n    public int $value = 0;\n\n    public static function maybe(): ?self\n    {\n        return null;\n    }\n}\n";

/// A PHP file the linter reports for its missing `declare(strict_types=1);` and the formatter rewrites.
const MESSY: &str = "<?php\n\nnamespace Lib;\n\nfunction  one( ): int {return 1;}\n";

fn valid_report() -> String {
    REPORT.replace("let label = \"one\";", "let label = 1;")
}

fn workspace(report: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src/Demo")).unwrap();
    std::fs::create_dir_all(directory.path().join("src/Lib")).unwrap();
    std::fs::write(directory.path().join("mago.toml"), include_str!("fixtures/sharp/mago.toml")).unwrap();
    std::fs::write(directory.path().join("src/Lib/Calc.php"), include_str!("fixtures/sharp/Calc.php")).unwrap();
    std::fs::write(directory.path().join("src/Demo/Report.sharp"), report).unwrap();
    directory
}

fn run(workspace: &Path, command: &str, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", command])
        .args(arguments)
        .env("MAGO_LOG", "info")
        .current_dir(workspace)
        .output()
        .unwrap()
}

fn report(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join("src/Demo/Report.sharp")).unwrap()
}

#[test]
fn analyze_reports_a_string_passed_to_a_php_int_parameter_at_the_sharp_position() {
    let directory = workspace(REPORT);

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!output.status.success(), "{stdout}");
    assert!(
        stdout.lines().any(|line| line.starts_with("src/Demo/Report.sharp:12:25:error - invalid-argument:")),
        "{stdout}"
    );
}

#[test]
fn analyze_finds_no_issues_once_the_argument_is_an_int() {
    let directory = workspace(&valid_report());

    let output = run(directory.path(), "analyze", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");
}

#[test]
fn analyze_reports_a_sharp_error_without_an_extensions_setting() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src/Demo")).unwrap();
    std::fs::create_dir_all(directory.path().join("src/Lib")).unwrap();
    std::fs::write(directory.path().join("src/Lib/Calc.php"), include_str!("fixtures/sharp/Calc.php")).unwrap();
    std::fs::write(
        directory.path().join("src/Demo/Report.sharp"),
        "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        return $extra + 1;\n    }\n}\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "analyze", "--reporting-format", "emacs"])
        .env("HOME", directory.path())
        .env("XDG_CONFIG_HOME", directory.path())
        .current_dir(directory.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.lines().any(|line| line.starts_with("src/Demo/Report.sharp:7:16:error - semantics:")), "{stdout}");
}

#[test]
fn analyze_reports_only_the_scope_error_for_a_local_used_after_its_block_closes() {
    let directory = workspace(
        "namespace Demo;\n\nclass Report\n{\n    public static int total()\n    {\n        {\n            let inner = 1;\n        }\n        return inner;\n    }\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let errors: Vec<&str> = stdout.lines().filter(|line| line.starts_with("src/Demo/Report.sharp:")).collect();
    assert_eq!(errors.len(), 1, "{stdout}");
    assert!(errors[0].starts_with("src/Demo/Report.sharp:10:16:error"), "{stdout}");
}

#[test]
fn analyze_reports_only_the_parse_error_for_php_syntax() {
    let directory = workspace(
        "namespace Demo;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        return this?->total(extra);\n    }\n\n    public int total(int extra)\n    {\n        return extra;\n    }\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    let errors: Vec<&str> = stdout.lines().filter(|line| line.starts_with("src/Demo/Report.sharp:")).collect();
    assert_eq!(errors.len(), 1, "{stdout}");
    assert!(errors[0].starts_with("src/Demo/Report.sharp:7:20:error - parse:"), "{stdout}");
}

/// The issues `mago analyze --reporting-format emacs` printed for `file`, as `line:column code`.
fn issues_in(stdout: &str, file: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| line.strip_prefix(file)?.strip_prefix(':'))
        .map(|line| {
            let mut parts = line.splitn(3, ':');
            let (row, column, rest) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
            let code = rest.split_once(" - ").map_or("", |(_, rest)| rest.split(':').next().unwrap());

            format!("{row}:{column} {code}")
        })
        .collect()
}

/// A semantic error stops a PHP# file from running, so the analyzer issues its refused code causes add nothing:
/// each refused line shows its refusal alone. The analyzer still refuses `exit` with a `string`, which the semantic
/// checks accept, and a PHP file keeps every issue.
#[test]
fn analyze_reports_only_the_refusal_on_each_refused_line() {
    let directory = workspace(
        "namespace Demo;\n\nclass Report\n{\n    public const int LIMIT = __Foo__.y;\n\n    public void host()\n    {\n        const host = _SERVER[\"HTTP_HOST\"];\n    }\n\n    public void count()\n    {\n        _GET[\"n\"]++;\n    }\n\n    public void fallback(string host = _SERVER[\"x\"])\n    {\n    }\n\n    public void code()\n    {\n        exit(_SERVER[\"code\"]);\n    }\n\n    public void file()\n    {\n        exit(__FILE__);\n    }\n\n    public void call()\n    {\n        _SERVER.read();\n    }\n\n    public void rows(int row)\n    {\n        const data = (array)row;\n    }\n\n    public void names()\n    {\n        const all = GLOBALS;\n        const env = _ENV;\n        const magic = __Something__;\n        const dollar = $_SERVER[\"HTTP_HOST\"];\n        const member = _SERVER.x;\n        __Foo__.bar();\n    }\n\n    public void reason(string reason)\n    {\n        exit(reason);\n    }\n\n    public void status(int code)\n    {\n        exit(code);\n    }\n\n    public void message()\n    {\n        exit(\"m\");\n    }\n\n    public void template()\n    {\n        exit(`m`);\n    }\n\n    public void parenthesized()\n    {\n        exit(((\"m\")));\n    }\n\n    public void wrapped()\n    {\n        exit(\n            _SERVER[\"code\"]\n        );\n    }\n}\n",
    );
    std::fs::write(
        directory.path().join("src/Demo/Twin.php"),
        "<?php\n\nnamespace Demo;\n\nclass Twin\n{\n    public const int LIMIT = __Foo__::y;\n\n    public function read(): void\n    {\n        $host = _SERVER[\"HTTP_HOST\"];\n        $all = GLOBALS;\n        $env = _ENV;\n        $magic = __Something__;\n        $dollar = $_SERVER[\"HTTP_HOST\"];\n        $member = _SERVER::x;\n        _SERVER::read();\n        __Foo__::bar();\n    }\n\n    public function reason(string $reason): void\n    {\n        exit($reason);\n    }\n}\n",
    )
    .unwrap();

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        issues_in(&stdout, "src/Demo/Report.sharp"),
        [
            "5:30 semantics",
            "9:22 semantics",
            "14:9 semantics",
            "17:40 semantics",
            "23:14 semantics",
            "28:14 semantics",
            "33:9 semantics",
            "38:22 semantics",
            "43:21 semantics",
            "44:21 semantics",
            "45:23 semantics",
            "46:24 semantics",
            "47:24 semantics",
            "48:9 semantics",
            "63:9 semantics",
            "68:9 semantics",
            "73:9 semantics",
            "79:13 semantics",
            "53:9 invalid-argument",
        ],
        "{stdout}"
    );
    assert_eq!(
        issues_in(&stdout, "src/Demo/Twin.php"),
        [
            "7:30 non-existent-class-like",
            "11:17 non-existent-constant",
            "11:9 mixed-assignment",
            "12:16 non-existent-constant",
            "12:9 mixed-assignment",
            "13:16 non-existent-constant",
            "13:9 mixed-assignment",
            "14:18 non-existent-constant",
            "14:9 mixed-assignment",
            "16:19 non-existent-class-like",
            "16:9 impossible-assignment",
            "17:18 non-existent-method",
            "18:18 non-existent-method",
        ],
        "{stdout}"
    );
}

/// A bare call is the global function's, so the analyzer names the member to write when no function of that name
/// exists. On a line that holds a refusal, the refusal shows alone.
#[test]
fn analyze_names_the_member_a_bare_call_meant_on_a_line_without_a_refusal() {
    let directory = workspace(
        "namespace Demo;\n\nclass Report\n{\n    public static int total() => 1;\n\n    public static int run() => total();\n\n    public static int line() => total(__LINE__);\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        issues_in(&stdout, "src/Demo/Report.sharp"),
        ["9:39 semantics", "7:32 non-existent-function", "7:32 mixed-return-statement"],
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "Write `Report.total()`: a static method reaches the members of its class through the class name."
        ),
        "{stdout}"
    );
}

/// A PHP sum and call chain 1,000 levels deep overflowed the stack of a debug `mago analyze`, and a PHP# sum 100,000
/// levels deep overflowed any build. Now the PHP file analyzes, and the PHP# file gets its nesting error.
#[test]
fn analyze_finishes_on_deeply_nested_files() {
    let directory = workspace(REPORT);
    let sum = |terms: usize| vec!["value"; terms];
    std::fs::write(
        directory.path().join("src/Lib/Deep.php"),
        format!(
            "<?php\n\nnamespace Lib;\n\nfinal class Deep\n{{\n    public function sum(int $value): int\n    {{\n        return ${};\n    }}\n\n    public function chain(): self\n    {{\n        return $this{};\n    }}\n}}\n",
            sum(1_000).join(" + $"),
            "->chain()".repeat(1_000)
        ),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("src/Demo/Deep.sharp"),
        format!(
            "namespace Demo;\n\nclass Deep\n{{\n    public int sum(int value)\n    {{\n        return {};\n    }}\n}}\n",
            sum(100_000).join(" + ")
        ),
    )
    .unwrap();

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stdout.lines().any(|line| line.starts_with("src/Demo/Deep.sharp:7:16:error - parse:")), "{stdout}{stderr}");
    assert!(!stdout.lines().any(|line| line.starts_with("src/Lib/Deep.php:")), "{stdout}{stderr}");
}

#[test]
fn analyze_fix_runs_on_php_files_beside_a_valid_sharp_file() {
    let directory = workspace(&valid_report());
    let reader = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Lib;\n\nfunction read(): ?int\n{\n    $box = Box::maybe();\n    return $box->value;\n}\n";
    std::fs::write(directory.path().join("src/Lib/Box.php"), BOX).unwrap();
    std::fs::write(directory.path().join("src/Lib/read.php"), reader).unwrap();

    let output = run(directory.path(), "analyze", &["--fix", "--dry-run"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!stderr.contains("does not support PHP# files yet"), "{stdout}{stderr}");
    assert!(stdout.contains("$box?->value"), "{stdout}{stderr}");
    assert_eq!(std::fs::read_to_string(directory.path().join("src/Lib/read.php")).unwrap(), reader);
}

#[test]
fn analyze_fix_refuses_a_fix_that_would_edit_a_sharp_file() {
    let source = "namespace Demo;\n\nimport Lib.Box;\n\nclass Report\n{\n    public static int total()\n    {\n        const box = Box.maybe();\n        return box.value;\n    }\n}\n";
    let directory = workspace(source);
    std::fs::write(directory.path().join("src/Lib/Box.php"), BOX).unwrap();

    let output = run(directory.path(), "analyze", &["--fix"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("`mago analyze --fix` does not support PHP# files yet: src/Demo/Report.sharp"), "{stderr}");
    assert_eq!(report(directory.path()), source);
}

/// The standard library's `Text`, with a native body, spec section 29, and the `@` the library writes before it throws
/// its own exception.
const TEXT: &str = "namespace Sharp.Text;\n\npublic static class Text\n{\n    public static extern string slug(string title);\n\n    public static string quiet(string title) => @trim(title);\n\n    public static string shout(string title) => strtoupper(title);\n}\n";

/// The standard library's `Int`, `Float` and `Bool`, spec section 24, with the library's signatures. Their names are
/// reserved, and only the standard library declares them.
const TYPE_CLASSES: [(&str, &str); 3] = [
    (
        "Int",
        "namespace Sharp;\n\nimport ValueError;\n\npublic static class Int\n{\n    public static int parse(Any? value)\n    {\n        const number = Int.tryParse(value);\n        if (number is int) {\n            return number;\n        }\n        throw new ValueError(\"Sharp\\\\Int::parse(): Argument #1 ($value) must hold an int\");\n    }\n\n    public static int? tryParse(Any? value)\n    {\n        return (value is string text) ? filter_var(text, FILTER_VALIDATE_INT, FILTER_NULL_ON_FAILURE) : null;\n    }\n}\n",
    ),
    (
        "Float",
        "namespace Sharp;\n\nimport ValueError;\n\npublic static class Float\n{\n    public static float parse(Any? value)\n    {\n        const number = Float.tryParse(value);\n        if (number is float) {\n            return number;\n        }\n        throw new ValueError(\"Sharp\\\\Float::parse(): Argument #1 ($value) must hold a float\");\n    }\n\n    public static float? tryParse(Any? value)\n    {\n        const number = (value is string text) && is_numeric(text) ? floatval(text) : null;\n        return (number is float) && is_finite(number) ? number : null;\n    }\n}\n",
    ),
    (
        "Bool",
        "namespace Sharp;\n\npublic static class Bool\n{\n    [Replaces(\"filter_var\")]\n    public static bool? tryParse(Any? value) => filter_var(value, FILTER_VALIDATE_BOOLEAN, FILTER_NULL_ON_FAILURE);\n}\n",
    ),
];

/// The standard library's `Replaces` attribute, decision 040, and the library classes whose methods wrap PHP functions:
/// `Date.format` wraps `date` and `gmdate`, `Regex.matches` and `Regex.match` both wrap `preg_match`, and
/// `Email.isValid`, `Ip.isValid` and `Url.isValid` wrap `filter_var`, as `Bool.tryParse` does.
const WRAPPERS: [(&str, &str); 6] = [
    (
        "Replaces",
        "namespace Sharp;\n\nimport Attribute;\n\n[Attribute(Attribute.TARGET_METHOD)]\npublic class Replaces\n{\n    public Replaces(string ...functions)\n    {\n    }\n}\n",
    ),
    (
        "Time/Date",
        "namespace Sharp.Time;\n\npublic static class Date\n{\n    [Replaces(\"date\", \"gmdate\")]\n    public static string format(int timestamp, string pattern) => date(pattern, timestamp);\n}\n",
    ),
    (
        "Text/Regex",
        "namespace Sharp.Text;\n\npublic static class Regex\n{\n    [Replaces(\"preg_match\")]\n    public static bool matches(string pattern, string text) => preg_match(pattern, text) is int found && found == 1;\n\n    [Replaces(\"preg_match\")]\n    public static bool match(string pattern, string text) => preg_match(pattern, text) is int found && found == 1;\n}\n",
    ),
    (
        "Net/Email",
        "namespace Sharp.Net;\n\npublic static class Email\n{\n    [Replaces(\"filter_var\")]\n    public static bool isValid(string address) => filter_var(address, FILTER_VALIDATE_EMAIL) is string;\n}\n",
    ),
    (
        "Net/Ip",
        "namespace Sharp.Net;\n\npublic static class Ip\n{\n    [Replaces(\"filter_var\")]\n    public static bool isValid(string address) => filter_var(address, FILTER_VALIDATE_IP) is string;\n}\n",
    ),
    (
        "Net/Url",
        "namespace Sharp.Net;\n\npublic static class Url\n{\n    [Replaces(\"filter_var\")]\n    public static bool isValid(string url) => filter_var(url, FILTER_VALIDATE_URL) is string;\n}\n",
    ),
];

/// The `composer.json` of the standard library's package.
const LIBRARY_PACKAGE: &str = "{\n    \"name\": \"heyjordanparker/php-sharp-composer\"\n}\n";

fn write(root: &Path, name: &str, contents: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// The standard library's package under `root`: its `composer.json`, its `Text`, its `Int`, `Float` and `Bool`, and
/// its `Replaces` with the classes that wrap PHP functions.
fn write_library(root: &Path) {
    write(root, "composer.json", LIBRARY_PACKAGE);
    write(root, "library/Sharp/Text/Text.sharp", TEXT);
    for (class, source) in TYPE_CLASSES.into_iter().chain(WRAPPERS) {
        write(root, &format!("library/Sharp/{class}.sharp"), source);
    }
}

/// A project whose `vendor/` holds the standard library's package, beside the project's `src/App/Page.sharp`.
fn library_workspace(page: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    write(
        directory.path(),
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"src\"]\nincludes = [\"vendor\"]\n",
    );
    write(directory.path(), "composer.json", "{\n    \"name\": \"acme/app\"\n}\n");
    write_library(&directory.path().join("vendor/heyjordanparker/php-sharp-composer"));
    write(directory.path(), "src/App/Page.sharp", page);
    directory
}

/// Every line `mago analyze` reports in the project's `src/App/Page.sharp`.
fn page_errors(page: &str) -> Vec<String> {
    let directory = library_workspace(page);
    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with("src/App/Page.sharp:"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn analyze_finds_no_issues_in_a_call_of_an_extern_method_of_the_standard_library() {
    let directory = library_workspace(
        "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page\n{\n    public string slug() => Text.slug(\"Hello\");\n}\n",
    );

    let output = run(directory.path(), "analyze", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");
}

/// `mago compile` checks the vendored library as a project file, and it stays the standard library, so its `extern`
/// method and its `@` compile.
#[test]
fn compile_accepts_extern_methods_and_silence_in_the_vendored_standard_library() {
    let directory = library_workspace(
        "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page\n{\n    public string slug() => Text.slug(\"Hello\");\n}\n",
    );

    let output = run(directory.path(), "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert!(output.status.success(), "{printed}");
    for compiled in ["src/App/Page.sharpc", "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text/Text.sharpc"]
    {
        assert!(directory.path().join(".sharp").join(compiled).is_file(), "{compiled}: {printed}");
    }
}

/// `mago compile` lowers the vendored library first, and a project file runs the library's one-call method as its
/// body: `Text.shout(name)` compiles to `strtoupper($name)`.
#[test]
fn compile_inlines_a_vendored_library_form_into_a_project_caller() {
    let directory = library_workspace(
        "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page\n{\n    public string title(string name) => Text.shout(name);\n}\n",
    );

    let output = run(directory.path(), "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert!(output.status.success(), "{printed}");
    let page = std::fs::read(directory.path().join(".sharp/src/App/Page.sharpc")).unwrap();
    assert!(page.windows(b"strtoupper".len()).any(|window| window == b"strtoupper"), "the form is inlined");
}

/// In the standard library's own repository, whose root `composer.json` names the package, `library/Sharp/` is the
/// library: its `extern` method and its `@` compile, and a file of another package in the same repository inlines its
/// one-call method. That package is still a project, so its own `extern` method under `Sharp` is refused.
#[test]
fn compile_treats_the_repository_of_the_standard_library_as_the_library() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n");
    write_library(root);
    write(root, "example/composer.json", "{\n    \"name\": \"acme/example\"\n}\n");
    write(
        root,
        "example/App/Title.sharp",
        "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Title\n{\n    public string of(string name) => Text.shout(name);\n}\n",
    );
    write(
        root,
        "example/App/Native.sharp",
        "namespace Sharp.Example;\n\npublic static class Native\n{\n    public static extern string run(string value);\n}\n",
    );

    let output = run(root, "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert_eq!(output.status.code(), Some(1), "{printed}");
    assert!(printed.contains("Compiled 11 PHP# files into .sharp/. 1 PHP# file was refused"), "{printed}");
    assert!(root.join(".sharp/library/Sharp/Text/Text.sharpc").is_file(), "{printed}");
    let title = std::fs::read(root.join(".sharp/example/App/Title.sharpc")).unwrap();
    assert!(title.windows(b"strtoupper".len()).any(|window| window == b"strtoupper"), "the form is inlined");
    assert!(!root.join(".sharp/example/App/Native.sharpc").exists(), "{printed}");
    assert!(printed.contains("native-body-outside-library"), "{printed}");
    assert!(!printed.contains("silence-outside-library"), "{printed}");
}

/// Without a `composer.json` that names the package, `library/Sharp/` is project code: its `extern` method and its `@`
/// are refused.
#[test]
fn compile_refuses_extern_and_silence_under_library_sharp_without_the_composer_json_of_the_library() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n");
    write(root, "library/Sharp/Text/Text.sharp", TEXT);

    let output = run(root, "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert_eq!(output.status.code(), Some(1), "{printed}");
    assert!(!root.join(".sharp/library/Sharp/Text/Text.sharpc").exists(), "{printed}");
    assert!(printed.contains("native-body-outside-library"), "{printed}");
    assert!(printed.contains("silence-outside-library"), "{printed}");
}

#[test]
fn analyze_reports_new_on_a_static_class() {
    assert_eq!(
        page_errors(
            "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page\n{\n    public Text make() => new Text();\n}\n"
        ),
        [
            "src/App/Page.sharp:7:31:error - abstract-instantiation: `Text` is a static class, so it has no instances: call its members on the class."
        ]
    );
}

#[test]
fn analyze_reports_a_class_that_extends_a_static_class() {
    assert_eq!(
        page_errors("namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page : Text\n{\n}\n"),
        ["src/App/Page.sharp:5:21:error - extend-final-class: `Text` is a static class, so no class can extend it."]
    );
}

/// Only the standard library declares native bodies, so a project file declares no `extern` method, whatever its
/// namespace.
#[test]
fn analyze_reports_an_extern_method_in_a_project_file() {
    for namespace in ["App", "Sharp.Mine"] {
        let page = format!(
            "namespace {namespace};\n\npublic static class Page\n{{\n    public static extern string slug(string title);\n}}\n"
        );

        assert_eq!(
            page_errors(&page),
            [
                "src/App/Page.sharp:5:33:error - native-body-outside-library: Only the standard library declares native bodies: give `slug` a body."
            ],
            "{namespace}"
        );
    }
}

/// A project file under `Sharp` passes the semantic checks, which know only the namespace, and the analyzer refuses
/// both forms only the standard library may write.
#[test]
fn analyze_reports_extern_and_silence_in_a_project_file_under_sharp() {
    assert_eq!(
        page_errors(
            "namespace Sharp.Mine;\n\npublic static class Page\n{\n    public static extern string slug(string title);\n\n    public static string quiet(string title) => @trim(title);\n}\n"
        ),
        [
            "src/App/Page.sharp:5:33:error - native-body-outside-library: Only the standard library declares native bodies: give `slug` a body.",
            "src/App/Page.sharp:7:49:error - silence-outside-library: `@` hides PHP's warnings, and only the standard library uses it: handle the failure where it happens.",
        ]
    );
}

/// A project calls the standard library's `Int`, `Float` and `Bool` by their bare names, with no import, spec
/// section 24. The checker reads them from the vendored package and gives each call the library's return type.
#[test]
fn analyze_types_the_parse_calls_of_the_standard_library_from_the_vendored_package() {
    let directory = library_workspace(
        "namespace App;\n\npublic class Page\n{\n    public int count() => Int.parse(\"1\");\n\n    public float? price() => Float.tryParse(\"x\");\n\n    public bool? flag() => Bool.tryParse(\"yes\");\n}\n",
    );

    let output = run(directory.path(), "analyze", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");

    assert_eq!(
        page_errors(
            "namespace App;\n\npublic class Page\n{\n    public string count() => Int.parse(\"1\");\n\n    public string price() => Float.tryParse(\"x\");\n\n    public string flag() => Bool.tryParse(\"yes\");\n}\n"
        ),
        [
            "src/App/Page.sharp:5:30:error - invalid-return-statement: Invalid return type for method `Page.count`: expected `string`, but found `int`.",
            "src/App/Page.sharp:7:30:error - nullable-return-statement: Method `Page.price` is declared to return `string` but possibly returns a nullable value (inferred as `float?`).",
            "src/App/Page.sharp:7:30:error - invalid-return-statement: Invalid return type for method `Page.price`: expected `string`, but found `float?`.",
            "src/App/Page.sharp:9:29:error - nullable-return-statement: Method `Page.flag` is declared to return `string` but possibly returns a nullable value (inferred as `bool?`).",
            "src/App/Page.sharp:9:29:error - invalid-return-statement: Invalid return type for method `Page.flag`: expected `string`, but found `bool?`.",
        ]
    );
}

/// The semantic checks let a file under the namespace `Sharp` declare the standard library's reserved type names,
/// and the analyzer refuses them in a project file.
#[test]
fn analyze_reports_a_type_class_of_the_standard_library_in_a_project_file() {
    assert_eq!(
        page_errors(
            "namespace Sharp;\n\npublic static class Bool\n{\n    public static bool? tryParse(Any? value) => null;\n}\n"
        ),
        [
            "src/App/Page.sharp:3:21:error - reserved-name-outside-library: Cannot use `Bool` as a class name: it is reserved."
        ]
    );
}

/// The standard library's own repository analyzes its sources as project code, and its root `composer.json` names
/// the package, so its `extern` methods, `@`, and its `Int`, `Float` and `Bool` are the library's.
#[test]
fn analyze_finds_no_issues_in_the_repository_of_the_standard_library() {
    let directory = tempfile::tempdir().unwrap();
    write(directory.path(), "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"library\"]\n");
    write_library(directory.path());

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");
}

/// `mago analyze` reports nothing in vendored files, so another package's `extern` method under `Sharp` gets no
/// report. The engine registers no native function for it, so a call of it fails when it runs.
#[test]
fn analyze_trusts_a_vendored_file_of_another_package_under_sharp() {
    let directory = library_workspace(
        "namespace App;\n\nimport Sharp.Tools.Tools;\n\npublic class Page\n{\n    public string slug() => Tools.slug(\"Hello\");\n}\n",
    );
    write(directory.path(), "vendor/acme/tools/composer.json", "{\n    \"name\": \"acme/tools\"\n}\n");
    write(
        directory.path(),
        "vendor/acme/tools/src/Sharp/Tools/Tools.sharp",
        "namespace Sharp.Tools;\n\npublic static class Tools\n{\n    public static extern string slug(string title);\n}\n",
    );

    let output = run(directory.path(), "analyze", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");
}

/// Decision 040: once the standard library wraps a PHP function, a project's call of it names the method to write.
#[test]
fn analyze_reports_a_call_of_a_wrapped_function_with_the_method_that_replaces_it() {
    assert_eq!(
        page_errors("namespace App;\n\npublic class Page\n{\n    public string year() => date(\"Y\");\n}\n"),
        [
            "src/App/Page.sharp:5:29:error - wrapped-function: `date` is wrapped by the standard library: write `Date.format(…)`. Add `import Sharp.Time.Date;` to the file."
        ]
    );
}

#[test]
fn analyze_names_both_methods_that_replace_a_function() {
    assert_eq!(
        page_errors(
            "namespace App;\n\npublic class Page\n{\n    public bool found() => preg_match(\"/a/\", \"a\") is int;\n}\n"
        ),
        [
            "src/App/Page.sharp:5:28:error - wrapped-function: `preg_match` is wrapped by the standard library: write `Regex.matches(…)` or `Regex.match(…)`. Add `import Sharp.Text.Regex;` to the file."
        ]
    );
}

/// Spec section 23: code names a class by the short name an import binds. A `.sharp` file that doesn't import the
/// wrapping class gets the import to add with the method to write. Plain PHP keeps upstream's text: it reports no call.
#[test]
fn analyze_names_the_import_a_wrapped_function_call_needs_and_reports_nothing_in_php() {
    let directory = library_workspace(
        "namespace App;\n\npublic class Page\n{\n    public bool found() => preg_match(\"/a/\", \"a\") is int;\n}\n",
    );
    write(
        directory.path(),
        "src/App/Finder.php",
        "<?php\n\ndeclare(strict_types=1);\n\nnamespace App;\n\nfinal class Finder\n{\n    public static function found(): bool\n    {\n        return preg_match('/a/', 'a') === 1;\n    }\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let page: Vec<&str> = stdout.lines().filter(|line| line.starts_with("src/App/Page.sharp:")).collect();

    assert_eq!(
        page,
        [
            "src/App/Page.sharp:5:28:error - wrapped-function: `preg_match` is wrapped by the standard library: write `Regex.matches(…)` or `Regex.match(…)`. Add `import Sharp.Text.Regex;` to the file."
        ],
        "{stdout}"
    );
    assert_eq!(issues_in(&stdout, "src/App/Finder.php"), Vec::<String>::new(), "{stdout}");
}

/// A file that already imports the wrapping class is told only the method to write.
#[test]
fn analyze_names_no_import_for_a_wrapping_class_the_file_imports() {
    assert_eq!(
        page_errors(
            "namespace App;\n\nimport Sharp.Text.Regex;\n\npublic class Page\n{\n    public bool found() => preg_match(\"/a/\", \"a\") is int;\n}\n"
        ),
        [
            "src/App/Page.sharp:7:28:error - wrapped-function: `preg_match` is wrapped by the standard library: write `Regex.matches(…)` or `Regex.match(…)`."
        ]
    );
}

/// Spec section 23: the code a message names to write uses the name the file imports the class as.
#[test]
fn analyze_names_the_methods_that_replace_a_function_by_the_name_the_file_imports_their_class_as() {
    assert_eq!(
        page_errors(
            "namespace App;\n\nimport Sharp.Text.Regex as Rx;\n\npublic class Page\n{\n    public bool found() => preg_match(\"/a/\", \"a\") is int;\n}\n"
        ),
        [
            "src/App/Page.sharp:7:28:error - wrapped-function: `preg_match` is wrapped by the standard library: write `Rx.matches(…)` or `Rx.match(…)`."
        ]
    );
}

/// Spec section 23's example, with a class for its struct and the project's own `Key` attribute for the standard one:
/// the renamed import leaves the bare `Key` to name another class.
#[test]
fn compile_accepts_a_class_that_renames_an_import_to_keep_the_short_name_free() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"src\"]\n");
    write(root, "src/Cache/Key.php", "<?php\n\nnamespace Cache;\n\nfinal class Key\n{\n}\n");
    write(
        root,
        "src/App/Key.php",
        "<?php\n\nnamespace App;\n\n#[\\Attribute]\nfinal class Key\n{\n    public function __construct(public string $name)\n    {\n    }\n}\n",
    );
    write(
        root,
        "src/App/Entry.sharp",
        "namespace App;\n\nimport Cache.Key as CacheKey;\n\npublic class Entry\n{\n    public Entry(\n        [Key(\"cache_key\")] public CacheKey key { get; },\n    ) { }\n}\n",
    );

    let output = run(root, "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert!(output.status.success(), "{printed}");
    assert!(root.join(".sharp/src/App/Entry.sharpc").is_file(), "{printed}");
}

/// The replacements come in the order the library declares them: the path order of its files, then source order.
#[test]
fn analyze_names_four_methods_that_replace_a_function_in_the_order_the_library_declares_them() {
    assert_eq!(
        page_errors(
            "namespace App;\n\npublic class Page\n{\n    public bool valid(string address) => filter_var(address, FILTER_VALIDATE_EMAIL) is string;\n}\n"
        ),
        [
            "src/App/Page.sharp:5:42:error - wrapped-function: `filter_var` is wrapped by the standard library: write `Bool.tryParse(…)`, `Email.isValid(…)`, `Ip.isValid(…)` or `Url.isValid(…)`. Add `import Sharp.Net.Email;`, `import Sharp.Net.Ip;` and `import Sharp.Net.Url;` to the file."
        ]
    );
}

/// Only the standard library decides which PHP functions are wrapped, as only it declares native bodies.
#[test]
fn analyze_reports_replaces_in_a_project_file() {
    assert_eq!(
        page_errors(
            "namespace App;\n\npublic static class Page\n{\n    [Replaces(\"date\")]\n    public static string year() => \"2026\";\n}\n"
        ),
        [
            "src/App/Page.sharp:5:6:error - replaces-outside-library: Only the standard library declares which PHP functions it wraps: remove `[Replaces]` from `year`."
        ]
    );
}

/// A library method names each function once, inside one `[Replaces]` or across two.
#[test]
fn analyze_reports_a_library_method_that_names_a_function_twice() {
    let directory = tempfile::tempdir().unwrap();
    write(directory.path(), "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"library\"]\n");
    write_library(directory.path());
    write(
        directory.path(),
        "library/Sharp/Time/Date.sharp",
        "namespace Sharp.Time;\n\npublic static class Date\n{\n    [Replaces(\"date\", \"gmdate\", \"date\")]\n    public static string format(int timestamp, string pattern) => date(pattern, timestamp);\n\n    [Replaces(\"gmdate\")]\n    [Replaces(\"gmdate\")]\n    public static string utc(int timestamp, string pattern) => gmdate(pattern, timestamp);\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut issues = issues_in(&stdout, "library/Sharp/Time/Date.sharp");
    issues.sort();

    assert_eq!(
        issues,
        ["5:33 duplicate-definition", "9:15 duplicate-definition", "9:6 attribute-not-repeatable"],
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "library/Sharp/Time/Date.sharp:5:33:error - duplicate-definition: `Date.format` names `date` twice: name each function once."
        ),
        "{stdout}"
    );
}

/// In the standard library's own repository, an edit to a library file can add a `[Replaces]` that refuses a call in
/// any `.sharp` file, so every library file is an input of every compiled file.
#[test]
fn compile_names_each_library_file_an_input_of_a_sharp_file_in_the_repository_of_the_standard_library() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n");
    write_library(root);
    write(root, "example/composer.json", "{\n    \"name\": \"acme/example\"\n}\n");
    write(
        root,
        "example/App/Clock.sharp",
        "namespace App;\n\npublic class Clock\n{\n    public string clean(string text) => trim(text);\n}\n",
    );

    let output = run(root, "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    let clock = std::fs::read(root.join(".sharp/example/App/Clock.sharpc")).expect(&printed);
    let date = b"library/Sharp/Time/Date.sharp";
    assert!(clock.windows(date.len()).any(|window| window == date), "{printed}");
}

/// A PHP# attribute class keeps the targets its `[Attribute(Attribute.TARGET_METHOD)]` reads, so `[Replaces]` on a
/// class is refused as PHP refuses it.
#[test]
fn analyze_reports_replaces_on_a_class() {
    let directory = tempfile::tempdir().unwrap();
    write(directory.path(), "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"library\"]\n");
    write_library(directory.path());
    write(
        directory.path(),
        "library/Sharp/Time/Date.sharp",
        "namespace Sharp.Time;\n\n[Replaces(\"date\")]\npublic static class Date\n{\n    public static string format(int timestamp, string pattern) => date(pattern, timestamp);\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(issues_in(&stdout, "library/Sharp/Time/Date.sharp"), ["3:2 invalid-attribute-target"], "{stdout}");
}

/// PHP's attribute classes keep the targets and the repeat flag `Attribute::TARGET_METHOD` gives them.
#[test]
fn analyze_reports_a_php_attribute_on_a_target_or_a_repeat_its_flags_leave_out() {
    let directory = tempfile::tempdir().unwrap();
    write(directory.path(), "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"src\"]\n");
    write(
        directory.path(),
        "src/App/Clock.php",
        "<?php\n\ndeclare(strict_types=1);\n\nnamespace App;\n\nuse Attribute;\n\n#[Attribute(Attribute::TARGET_METHOD)]\nfinal class Wraps\n{\n}\n\n#[Wraps]\nfinal class Clock\n{\n    #[Wraps]\n    #[Wraps]\n    public function year(): string\n    {\n        return '2026';\n    }\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        issues_in(&stdout, "src/App/Clock.php"),
        ["14:3 invalid-attribute-target", "18:7 attribute-not-repeatable"],
        "{stdout}"
    );
}

/// In the library's own repository, a library body calls the functions the library wraps, and a file of another
/// package beside it may not.
#[test]
fn analyze_accepts_a_wrapped_function_in_a_library_body_and_reports_it_in_another_package() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n");
    write_library(root);
    write(root, "example/composer.json", "{\n    \"name\": \"acme/example\"\n}\n");
    write(
        root,
        "example/App/Clock.sharp",
        "namespace App;\n\npublic class Clock\n{\n    public string year() => date(\"Y\");\n}\n",
    );

    let output = run(root, "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(issues_in(&stdout, "example/App/Clock.sharp"), ["5:29 wrapped-function"], "{stdout}");
    assert!(!stdout.lines().any(|line| line.starts_with("library/")), "{stdout}");
}

/// Plain PHP keeps every PHP function: only a `.sharp` file's call of a wrapped function is refused.
#[test]
fn analyze_reports_a_wrapped_function_in_a_sharp_file_and_not_in_a_php_file() {
    let directory =
        library_workspace("namespace App;\n\npublic class Page\n{\n    public string year() => date(\"Y\");\n}\n");
    write(
        directory.path(),
        "src/App/Clock.php",
        "<?php\n\ndeclare(strict_types=1);\n\nnamespace App;\n\nfinal class Clock\n{\n    public static function year(): string\n    {\n        return date('Y');\n    }\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(issues_in(&stdout, "src/App/Page.sharp"), ["5:29 wrapped-function"], "{stdout}");
    assert_eq!(issues_in(&stdout, "src/App/Clock.php"), Vec::<String>::new(), "{stdout}");
}

/// The PHP# forms that replace PHP functions, each as a method of `Page` beside the functions it replaces: `is`
/// tests a type (spec section 21), `(string)` converts a number and `/` divides two ints (section 24), a spread joins
/// two collections (section 12), and `for … of` reads one (section 17).
const SYNTAX_FORMS: [(&str, &str); 9] = [
    ("is_string", "public bool run(Any? value) => value is string;"),
    ("is_int", "public bool run(Any? value) => value is int;"),
    ("is_float", "public bool run(Any? value) => value is float;"),
    ("is_bool", "public bool run(Any? value) => value is bool;"),
    ("strval", "public string run(int number) => (string)number;"),
    ("intdiv", "public int run(int total, int count) => total / count;"),
    ("array_merge", "public List<int> run(List<int> first, List<int> second) => [...first, ...second];"),
    (
        "array_replace",
        "public Map<string, int> run(Map<string, int> first, Map<string, int> second) => [...first, ...second];",
    ),
    (
        "array_walk_recursive",
        "public int run(List<int> numbers)\n    {\n        let total = 0;\n        for (const number of numbers) {\n            total += number;\n        }\n        return total;\n    }",
    ),
];

/// Each form that replaces a PHP function is accepted today: the checker finds no error, and the file compiles.
#[test]
fn compile_accepts_each_form_that_replaces_a_php_function() {
    let refused: Vec<String> = SYNTAX_FORMS
        .into_iter()
        .filter_map(|(function, form)| {
            let directory = library_workspace(&format!("namespace App;\n\npublic class Page\n{{\n    {form}\n}}\n"));
            let output = run(directory.path(), "compile", &[]);
            let printed =
                format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

            (!output.status.success()).then(|| format!("{function}: {form}\n{printed}"))
        })
        .collect();

    assert!(refused.is_empty(), "{}", refused.join("\n"));
}

/// A project page that calls every PHP function PHP# replaces with its own syntax.
const REPLACED_CALLS: &str = "namespace App;\n\npublic class Page\n{\n    public bool text(Any? value) => is_string(value);\n    public bool whole(Any? value) => is_int(value);\n    public bool real(Any? value) => is_float(value);\n    public bool flag(Any? value) => is_bool(value);\n    public string label(int number) => strval(number);\n    public int half(int total, int count) => intdiv(total, count);\n    public List<int> join(List<int> first, List<int> second) => array_merge(first, second);\n    public Map<string, int> merge(Map<string, int> defaults, Map<string, int> overrides) => array_replace(defaults, overrides);\n    public bool walk(List<int> numbers) => array_walk_recursive(numbers, n => n);\n}\n";

/// A PHP# file that calls a PHP function PHP# replaces with syntax names the form to write, and a PHP file calls each
/// of them as before.
#[test]
fn analyze_reports_each_function_php_sharp_replaces_with_syntax_in_a_sharp_file_and_not_in_a_php_file() {
    let directory = library_workspace(REPLACED_CALLS);
    write(
        directory.path(),
        "src/App/Clock.php",
        "<?php\n\ndeclare(strict_types=1);\n\nnamespace App;\n\nfinal class Clock\n{\n    /**\n     * @param list<int> $first\n     * @param list<int> $second\n     * @param array<string, int> $defaults\n     * @param array<string, int> $overrides\n     *\n     * @return list<mixed>\n     */\n    public static function all(mixed $value, int $total, int $count, array $first, array $second, array $defaults, array $overrides): array\n    {\n        $numbers = $first;\n        array_walk_recursive($numbers, static fn(int $number): int => $number);\n\n        return [\n            is_string($value),\n            is_int($value),\n            is_float($value),\n            is_bool($value),\n            strval($total),\n            intdiv($total, $count),\n            array_merge($first, $second),\n            array_replace($defaults, $overrides),\n            $numbers,\n        ];\n    }\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let page: Vec<&str> = stdout.lines().filter(|line| line.starts_with("src/App/Page.sharp:")).collect();

    assert_eq!(
        page,
        [
            "src/App/Page.sharp:5:37:error - replaced-by-syntax: `is_string` is replaced by PHP# syntax: write `value is string`.",
            "src/App/Page.sharp:6:38:error - replaced-by-syntax: `is_int` is replaced by PHP# syntax: write `value is int`.",
            "src/App/Page.sharp:7:37:error - replaced-by-syntax: `is_float` is replaced by PHP# syntax: write `value is float`.",
            "src/App/Page.sharp:8:37:error - replaced-by-syntax: `is_bool` is replaced by PHP# syntax: write `value is bool`.",
            "src/App/Page.sharp:9:40:error - replaced-by-syntax: `strval` is replaced by PHP# syntax: write `(string)number`.",
            "src/App/Page.sharp:10:46:error - replaced-by-syntax: `intdiv` is replaced by PHP# syntax: write `total / count`.",
            "src/App/Page.sharp:11:65:error - replaced-by-syntax: `array_merge` is replaced by PHP# syntax: write `[...first, ...second]`.",
            "src/App/Page.sharp:12:93:error - replaced-by-syntax: `array_replace` is replaced by PHP# syntax: write `[...defaults, ...overrides]`.",
            "src/App/Page.sharp:13:44:error - replaced-by-syntax: `array_walk_recursive` is replaced by PHP# syntax: write a `for … of` loop.",
        ],
        "{stdout}"
    );
    assert_eq!(issues_in(&stdout, "src/App/Clock.php"), Vec::<String>::new(), "{stdout}");
}

/// The form names an argument that is one name or a member read as the call writes it, and any other argument by
/// the form's own placeholder.
#[test]
fn analyze_names_a_name_or_member_read_argument_in_the_form_and_a_placeholder_for_any_other() {
    assert_eq!(
        page_errors(
            "namespace App;\n\npublic class Page\n{\n    private Any? data = null;\n    public Any? read() => this.data;\n    public bool owned(Page other) => is_string(other.data);\n    public bool named() => is_string(this.read());\n    public int third(int total) => intdiv(total, 3);\n    public List<int> both(List<int> first) => array_merge(this.items(), first);\n    public List<int> all(List<int> first, List<int> second) => array_merge(first, this.items(), second);\n    public List<int> one(List<int> first) => array_merge(first);\n    public List<int> items() => [];\n}\n"
        ),
        [
            "src/App/Page.sharp:7:38:error - replaced-by-syntax: `is_string` is replaced by PHP# syntax: write `other.data is string`.",
            "src/App/Page.sharp:8:28:error - replaced-by-syntax: `is_string` is replaced by PHP# syntax: write `x is string`.",
            "src/App/Page.sharp:9:36:error - replaced-by-syntax: `intdiv` is replaced by PHP# syntax: write `total / b`.",
            "src/App/Page.sharp:10:47:error - replaced-by-syntax: `array_merge` is replaced by PHP# syntax: write `[...a, ...first]`.",
            "src/App/Page.sharp:11:64:error - replaced-by-syntax: `array_merge` is replaced by PHP# syntax: write `[...first, ...b, ...second]`.",
            "src/App/Page.sharp:12:46:error - replaced-by-syntax: `array_merge` is replaced by PHP# syntax: write `[...first]`.",
        ]
    );
}

/// In the library's own repository, a library body calls a PHP function PHP# replaces with syntax, and a file of
/// another package beside it may not.
#[test]
fn analyze_accepts_a_function_php_sharp_replaces_with_syntax_in_a_library_body_and_reports_it_in_another_package() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n");
    write_library(root);
    write(
        root,
        "library/Sharp/Text/Kind.sharp",
        "namespace Sharp.Text;\n\npublic static class Kind\n{\n    public static bool isText(Any? value) => is_string(value);\n}\n",
    );
    write(root, "example/composer.json", "{\n    \"name\": \"acme/example\"\n}\n");
    write(
        root,
        "example/App/Clock.sharp",
        "namespace App;\n\npublic class Clock\n{\n    public bool text(Any? value) => is_string(value);\n}\n",
    );

    let output = run(root, "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(issues_in(&stdout, "example/App/Clock.sharp"), ["5:37 replaced-by-syntax"], "{stdout}");
    assert!(!stdout.lines().any(|line| line.starts_with("library/")), "{stdout}");
}

/// `is`, `as` and `match` test a scalar type with the PHP function the table names, which no source the user wrote
/// calls, and a call the user writes as the value an `is` tests is still reported.
#[test]
fn analyze_reports_a_written_call_and_not_the_type_tests_of_is_as_and_match() {
    assert_eq!(
        page_errors(
            "namespace App;\n\npublic class Page\n{\n    public int? whole(Any? value) => value as int;\n    public string kind(Any? value) => match (value) {\n        string text => text,\n        default => \"\",\n    };\n    public bool both(Any? value) => is_string(value) is true;\n}\n"
        ),
        [
            "src/App/Page.sharp:10:37:error - replaced-by-syntax: `is_string` is replaced by PHP# syntax: write `value is string`."
        ]
    );
}

/// Once the standard library wraps a function PHP# also replaces with syntax, the call names the library's method
/// alone.
#[test]
fn analyze_reports_only_the_wrapping_method_for_a_function_the_library_wraps_and_syntax_replaces() {
    let directory = library_workspace(
        "namespace App;\n\npublic class Page\n{\n    public string label(int number) => strval(number);\n}\n",
    );
    write(
        directory.path(),
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text/Number.sharp",
        "namespace Sharp.Text;\n\npublic static class Number\n{\n    [Replaces(\"strval\")]\n    public static string text(int number) => (string)number;\n}\n",
    );

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let page: Vec<&str> = stdout.lines().filter(|line| line.starts_with("src/App/Page.sharp:")).collect();

    assert_eq!(
        page,
        [
            "src/App/Page.sharp:5:40:error - wrapped-function: `strval` is wrapped by the standard library: write `Number.text(…)`. Add `import Sharp.Text.Number;` to the file."
        ],
        "{stdout}"
    );
}

#[test]
fn linting_one_sharp_file_is_refused() {
    let service = LintService::new(ReadDatabase::empty(), Settings::default(), ParserSettings::default(), false);
    let file = File::ephemeral(Cow::Borrowed(b"src/Demo/Report.sharp"), Cow::Borrowed(REPORT.as_bytes()));

    let result = service.lint_file(&file, LintMode::Full, None, true);

    assert!(matches!(result, Err(OrchestratorError::SharpNotSupported { tool: "lint", .. })), "{result:?}");
}

fn messy_workspace() -> tempfile::TempDir {
    let directory = workspace(&valid_report());
    std::fs::write(directory.path().join("src/Lib/messy.php"), MESSY).unwrap();
    directory
}

/// Runs `mago` on a workspace holding `Report.sharp` and `messy.php`, checks that the PHP# file is unchanged,
/// and returns the output with the contents of `messy.php` afterwards.
fn run_beside_messy_php(command: &str, arguments: &[&str]) -> (Output, String) {
    run_in_messy_workspace(&messy_workspace(), command, arguments)
}

fn run_in_messy_workspace(directory: &tempfile::TempDir, command: &str, arguments: &[&str]) -> (Output, String) {
    let output = run(directory.path(), command, arguments);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!stderr.contains("panicked"), "{command} {arguments:?}: {stderr}");
    assert_eq!(report(directory.path()), valid_report(), "{command} {arguments:?} changed the PHP# file");

    (output, std::fs::read_to_string(directory.path().join("src/Lib/messy.php")).unwrap())
}

#[test]
fn lint_reports_the_php_file_and_skips_the_sharp_file() {
    let (output, _) = run_beside_messy_php("lint", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "{stdout}");
    assert!(stdout.lines().any(|line| line.starts_with("src/Lib/messy.php:1:1:warning - strict-types:")), "{stdout}");
}

#[test]
fn lint_fix_fixes_the_php_file_and_skips_the_sharp_file() {
    let (output, messy) = run_beside_messy_php("lint", &["--fix", "--potentially-unsafe"]);

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(messy.contains("declare(strict_types=1);"), "{messy}");
}

#[test]
fn format_formats_the_php_file_and_skips_the_sharp_file() {
    let (output, messy) = run_beside_messy_php("fmt", &[]);

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(messy.contains("function one(): int"), "{messy}");
}

#[test]
fn guard_checks_the_php_file_and_skips_the_sharp_file() {
    let (output, _) = run_beside_messy_php("guard", &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("No files found to check with guard."), "{stderr}");
}

#[test]
fn fix_formats_the_php_file_and_skips_the_sharp_file() {
    let (output, messy) = run_beside_messy_php("fix", &[]);

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(messy.contains("function one(): int"), "{messy}");
}

#[test]
fn lint_and_format_skip_a_staged_sharp_file() {
    let directory = messy_workspace();
    for arguments in [&["init", "--quiet"][..], &["add", "src"][..]] {
        assert!(Command::new("git").args(arguments).current_dir(directory.path()).status().unwrap().success());
    }

    let (output, _) = run_in_messy_workspace(&directory, "lint", &["--staged", "--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.lines().any(|line| line.starts_with("src/Lib/messy.php:1:1:warning - strict-types:")), "{stdout}");

    let (output, _) = run_in_messy_workspace(&directory, "fmt", &["--staged"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("Formatted and re-staged 1 file(s)."), "{stderr}");
}

#[cfg(unix)]
#[test]
fn lint_format_and_guard_never_read_a_discovered_sharp_file() {
    use std::os::unix::fs::PermissionsExt;

    let directory = workspace(&valid_report());
    let report = directory.path().join("src/Demo/Report.sharp");
    std::fs::set_permissions(&report, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&report).is_ok() {
        return;
    }

    for (command, arguments) in [("lint", &[][..]), ("fmt", &["--check"][..]), ("guard", &[][..])] {
        let output = run(directory.path(), command, arguments);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(output.status.success(), "{command}: {stderr}");
        assert!(!stderr.contains("Report.sharp"), "{command}: {stderr}");
        assert!(!stderr.contains("ERROR"), "{command}: {stderr}");
    }
}

#[test]
fn lint_format_and_guard_refuse_a_sharp_file_named_on_the_command_line() {
    let directory = workspace(REPORT);

    for (command, arguments, message) in [
        ("lint", &["src/Demo/Report.sharp"][..], "`mago lint` does not support PHP# files yet"),
        ("lint", &["--fix", "src/Demo/Report.sharp"][..], "`mago lint` does not support PHP# files yet"),
        ("fmt", &["src/Demo/Report.sharp"][..], "`mago format` does not support PHP# files yet"),
        ("guard", &["src/Demo/Report.sharp"][..], "`mago guard` does not support PHP# files yet"),
    ] {
        let output = run(directory.path(), command, arguments);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(!output.status.success(), "{command} {arguments:?} succeeded: {stderr}");
        assert!(stderr.contains(&format!("{message}: src/Demo/Report.sharp")), "{command} {arguments:?}: {stderr}");
        assert_eq!(report(directory.path()), REPORT, "{command} {arguments:?} changed the file");
    }
}

/// A PHP# class whose method returns a string where it declares `int`, with `pragma` on the line before.
fn broken_sharp(pragma: &str) -> String {
    format!(
        "namespace Demo;\n\npublic class Broken\n{{\n    public int total()\n    {{\n        {pragma}\n        return \"one\";\n    }}\n}}\n"
    )
}

/// The PHP twin of [`broken_sharp`].
fn broken_php(pragma: &str) -> String {
    format!(
        "<?php\n\nnamespace Demo;\n\nfinal class Broken\n{{\n    public function total(): int\n    {{\n        {pragma}\n        return 'one';\n    }}\n}}\n"
    )
}

const EXPECT_PRAGMA: &str = "// @mago-expect analysis:invalid-return-statement";

const IGNORE_ENTRY: &str = "ignore = [\"invalid-return-statement\"]";

/// A workspace with `src/Demo/{name}`, whose `mago.toml` holds `analyzer` under `[analyzer]`.
fn suppressed_workspace(name: &str, contents: &str, analyzer: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("src/Demo")).unwrap();
    std::fs::write(
        directory.path().join("mago.toml"),
        format!("php-version = \"8.4\"\n\n[source]\npaths = [\"src\"]\n\n[analyzer]\n{analyzer}\n"),
    )
    .unwrap();
    std::fs::write(directory.path().join("src/Demo").join(name), contents).unwrap();
    directory
}

/// A workspace holding [`broken_sharp`] and `src/Demo/Twin.php`, a PHP class with the same error.
fn broken_twins_workspace(analyzer: &str) -> tempfile::TempDir {
    let directory = suppressed_workspace("Broken.sharp", &broken_sharp(""), analyzer);
    std::fs::write(directory.path().join("src/Demo/Twin.php"), broken_php("").replace("class Broken", "class Twin"))
        .unwrap();
    directory
}

/// Writes a baseline of every issue `mago analyze` reports in `workspace` to `baseline.toml`.
fn generate_baseline(workspace: &Path) {
    let generated = run(workspace, "analyze", &["--generate-baseline", "--baseline", "baseline.toml"]);
    assert!(workspace.join("baseline.toml").is_file(), "{}", String::from_utf8_lossy(&generated.stderr));
}

/// A strict baseline entry for the error in [`broken_sharp`]. Mago never writes one, so a test writes it by hand.
const SHARP_ERROR_ENTRY: &str =
    "[[entries.\"src/Demo/Broken.sharp\".issues]]\ncode = \"invalid-return-statement\"\nstart_line = 7\nend_line = 7\n";

/// A strict baseline entry for the error in the `Twin.php` of [`broken_twins_workspace`].
const TWIN_ERROR_ENTRY: &str =
    "[[entries.\"src/Demo/Twin.php\".issues]]\ncode = \"invalid-return-statement\"\nstart_line = 9\nend_line = 9\n";

/// Writes a strict baseline holding `entries` to `baseline.toml` in `workspace`.
fn write_strict_baseline(workspace: &Path, entries: &[&str]) {
    std::fs::write(workspace.join("baseline.toml"), format!("variant = \"strict\"\n\n{}", entries.join("\n"))).unwrap();
}

/// Three workspaces holding `src/Demo/{name}`, each hiding the issues `code` names with one suppression: an
/// `@mago-expect` pragma that `contents` places, an analyzer `ignore` entry, and a baseline entry.
fn suppressed_workspaces(
    name: &str,
    contents: impl Fn(&str) -> String,
    code: &str,
) -> [(&'static str, tempfile::TempDir); 3] {
    let baselined = suppressed_workspace(name, &contents(""), "baseline = \"baseline.toml\"");
    generate_baseline(baselined.path());

    [
        ("a pragma", suppressed_workspace(name, &contents(&format!("// @mago-expect analysis:{code}")), "")),
        ("an ignore entry", suppressed_workspace(name, &contents(""), &format!("ignore = [\"{code}\"]"))),
        ("a baseline entry", baselined),
    ]
}

#[test]
fn analyze_reports_a_sharp_error_an_expect_pragma_targets_and_points_at_the_pragma() {
    let directory = suppressed_workspace("Broken.sharp", &broken_sharp(EXPECT_PRAGMA), "");

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("src/Demo/Broken.sharp:8:16:error - invalid-return-statement:"), "{stdout}");
    assert!(
        stdout.contains(
            "src/Demo/Broken.sharp:7:12:warning - unsuppressible-error: An error can't be suppressed in PHP#."
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("unfulfilled-expect"), "{stdout}");
}

#[test]
fn analyze_reports_a_sharp_error_an_ignore_entry_targets_and_points_at_the_error() {
    let directory = suppressed_workspace("Broken.sharp", &broken_sharp(""), IGNORE_ENTRY);

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rich = run(directory.path(), "analyze", &["--reporting-format", "rich"]);
    let rich = String::from_utf8_lossy(&rich.stdout);

    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("src/Demo/Broken.sharp:8:16:error - invalid-return-statement:"), "{stdout}");
    assert!(
        stdout.contains(
            "src/Demo/Broken.sharp:8:16:warning - unsuppressible-error: An error can't be suppressed in PHP#."
        ),
        "{stdout}"
    );
    assert!(rich.contains("An analyzer `ignore` entry in the configuration matches this error."), "{rich}");
}

#[test]
fn analyze_reports_a_sharp_error_a_baseline_entry_targets_and_points_at_the_error() {
    let directory = broken_twins_workspace("");
    write_strict_baseline(directory.path(), &[SHARP_ERROR_ENTRY, TWIN_ERROR_ENTRY]);

    let output = run(directory.path(), "analyze", &["--baseline", "baseline.toml", "--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let rich = run(directory.path(), "analyze", &["--baseline", "baseline.toml", "--reporting-format", "rich"]);
    let rich = String::from_utf8_lossy(&rich.stdout);

    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("src/Demo/Broken.sharp:8:16:error - invalid-return-statement:"), "{stdout}");
    assert!(
        stdout.contains(
            "src/Demo/Broken.sharp:8:16:warning - unsuppressible-error: An error can't be suppressed in PHP#."
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("src/Demo/Twin.php"), "{stdout}");
    assert!(rich.contains("A baseline entry matches this error."), "{rich}");
    assert!(stderr.contains("Filtered out 1 issues based on the baseline file."), "{stderr}");
}

#[test]
fn a_generated_baseline_leaves_out_a_sharp_error_and_keeps_a_php_error() {
    for variant in ["loose", "strict"] {
        let directory = broken_twins_workspace(&format!("baseline-variant = \"{variant}\""));
        generate_baseline(directory.path());
        let baseline = std::fs::read_to_string(directory.path().join("baseline.toml")).unwrap();

        let output = run(directory.path(), "analyze", &["--baseline", "baseline.toml", "--reporting-format", "emacs"]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(baseline.contains("src/Demo/Twin.php"), "{variant}: {baseline}");
        assert!(!baseline.contains("src/Demo/Broken.sharp"), "{variant}: {baseline}");
        assert!(!output.status.success(), "{variant}: {stdout}");
        assert!(stdout.contains("src/Demo/Broken.sharp:8:16:error - invalid-return-statement:"), "{variant}: {stdout}");
        assert!(!stdout.contains("unsuppressible-error"), "{variant}: {stdout}");
        assert!(!stdout.contains("src/Demo/Twin.php"), "{variant}: {stdout}");
        assert!(!stderr.contains("Your baseline file contains"), "{variant}: {stderr}");
        assert!(stderr.contains("Filtered out 1 issues based on the baseline file."), "{variant}: {stderr}");
    }
}

#[test]
fn a_pragma_an_ignore_entry_and_a_baseline_entry_still_hide_a_php_error() {
    for (suppression, directory) in suppressed_workspaces("Broken.php", broken_php, "invalid-return-statement") {
        let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success(), "{suppression}: {stdout}");
        assert!(!stdout.contains("invalid-return-statement"), "{suppression}: {stdout}");
        assert!(!stdout.contains("unsuppressible-error"), "{suppression}: {stdout}");
    }
}

#[test]
fn a_pragma_an_ignore_entry_and_a_baseline_entry_still_hide_a_sharp_warning() {
    let sure = |pragma: &str| {
        format!(
            "namespace Demo;\n\npublic class Sure\n{{\n    public int one()\n    {{\n        {pragma}\n        if (true) {{\n            return 1;\n        }}\n        return 2;\n    }}\n}}\n"
        )
    };

    for (suppression, directory) in suppressed_workspaces("Sure.sharp", sure, "redundant-condition") {
        let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(output.status.success(), "{suppression}: {stdout}");
        assert!(!stdout.contains("redundant-condition"), "{suppression}: {stdout}");
        assert!(!stdout.contains("unsuppressible-error"), "{suppression}: {stdout}");
    }
}

#[test]
fn compile_refuses_a_sharp_file_whose_error_a_pragma_an_ignore_entry_or_a_baseline_entry_targets() {
    let baselined = suppressed_workspace("Broken.sharp", &broken_sharp(""), "baseline = \"baseline.toml\"");
    write_strict_baseline(baselined.path(), &[SHARP_ERROR_ENTRY]);

    for (suppression, directory) in [
        ("a pragma", suppressed_workspace("Broken.sharp", &broken_sharp(EXPECT_PRAGMA), "")),
        ("an ignore entry", suppressed_workspace("Broken.sharp", &broken_sharp(""), IGNORE_ENTRY)),
        ("a baseline entry", baselined),
    ] {
        let output = run(directory.path(), "compile", &[]);
        let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

        assert_eq!(output.status.code(), Some(1), "{suppression}: {printed}");
        assert!(printed.contains("invalid-return-statement"), "{suppression}: {printed}");
        assert!(!directory.path().join(".sharp/src/Demo/Broken.sharpc").exists(), "{suppression}");
    }
}

/// Section 29's `app/Stubs/Stripe.sharp`, with `extra` after its declaration.
fn stripe_stub(extra: &str) -> String {
    format!("namespace App.Stubs;\n\nimport Stripe.StripeClient;\n\nextern StripeClient uses Http;\n{extra}")
}

/// A project with section 29's `extern` declaration file, a file with each other `extern` form, and the plain PHP
/// they declare.
fn extern_workspace(stripe: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\n");
    write(root, "app/Stubs/Stripe.sharp", stripe);
    write(
        root,
        "app/Stubs/Clock.sharp",
        "namespace App.Stubs;\n\nimport Carbon.Carbon;\nimport Brick.Math.BigDecimal;\n\nextern Carbon.now uses Clock;\nextern now uses Clock;\nextern BigDecimal;\n",
    );
    write(root, "app/Lib/StripeClient.php", "<?php\n\nnamespace Stripe;\n\nclass StripeClient\n{\n}\n");
    write(
        root,
        "app/Lib/Carbon.php",
        "<?php\n\nnamespace Carbon;\n\nclass Carbon\n{\n    public static function now(): self\n    {\n        return new self();\n    }\n}\n",
    );
    write(root, "app/Lib/BigDecimal.php", "<?php\n\nnamespace Brick\\Math;\n\nclass BigDecimal\n{\n}\n");
    write(root, "app/Lib/now.php", "<?php\n\nfunction now(): int\n{\n    return time();\n}\n");
    directory
}

#[test]
fn analyze_finds_no_issues_in_extern_declarations_of_plain_php() {
    let directory = extern_workspace(&stripe_stub(""));

    let output = run(directory.path(), "analyze", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");
}

#[test]
fn analyze_reports_only_the_second_extern_of_a_class() {
    let directory = extern_workspace(&stripe_stub("extern StripeClient;\n"));

    let output = run(directory.path(), "analyze", &["--reporting-format", "emacs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!output.status.success(), "{stdout}");
    assert_eq!(
        stdout.lines().filter(|line| line.contains(" - ")).collect::<Vec<_>>(),
        ["app/Stubs/Stripe.sharp:6:1:error - duplicate-extern: `StripeClient` already has an `extern` declaration."],
        "{stdout}"
    );
}

#[test]
fn compile_refuses_a_file_whose_getter_calls_plain_php_without_an_extern() {
    let directory = extern_workspace(&stripe_stub(""));
    write(
        directory.path(),
        "app/Shop/Label.sharp",
        "namespace App.Shop;\n\npublic class Label\n{\n    public Label(private string name) { }\n\n    public string text => trim(this.name);\n}\n",
    );

    let output = run(directory.path(), "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert_eq!(output.status.code(), Some(1), "{printed}");
    assert!(
        printed.contains("Getter `text` calls `trim`, which has no `extern` declaration. Getters must be pure."),
        "{printed}"
    );
    assert!(!directory.path().join(".sharp/app/Shop/Label.sharpc").exists());
    assert!(directory.path().join(".sharp/app/Stubs/Stripe.sharpc").exists(), "{printed}");
}

/// Spec section 28's `app/Shared/Money.sharp`, whose law holds over its pure `add`.
#[test]
fn analyze_finds_no_issues_in_a_class_with_a_law() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\n");
    write(
        root,
        "app/Shared/Money.sharp",
        "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n    public Money add(Money other) => new Money(this.amount + other.amount, this.currency);\n\n    law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n}\n",
    );

    let output = run(root, "analyze", &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stderr.contains("No issues found."), "{stdout}{stderr}");
}

#[test]
fn compile_refuses_a_file_whose_law_calls_plain_php_with_an_effect() {
    let directory = extern_workspace(&stripe_stub(""));
    let root = directory.path();
    write(
        root,
        "app/Lib/Gateway.php",
        "<?php\n\nnamespace Billing;\n\nfinal class Gateway\n{\n    public static function charge(int $amount): bool\n    {\n        return $amount > 0;\n    }\n}\n",
    );
    write(
        root,
        "app/Stubs/Billing.sharp",
        "namespace App.Stubs;\n\nimport Billing.Gateway;\n\nextern Gateway.charge uses Http;\n",
    );
    write(
        root,
        "app/Shop/Refund.sharp",
        "namespace App.Shop;\n\nimport Billing.Gateway;\n\npublic class Refund\n{\n    law refundAllowed(int amount) => Gateway.charge(amount);\n}\n",
    );

    let output = run(root, "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert_eq!(output.status.code(), Some(1), "{printed}");
    assert!(printed.contains("impure-law"), "{printed}");
    assert!(
        printed.contains(
            "Law `refundAllowed` calls `Gateway.charge`, which has the effect `Http`. Laws hold only over pure code."
        ),
        "{printed}"
    );
    assert!(!root.join(".sharp/app/Shop/Refund.sharpc").exists());
    assert!(root.join(".sharp/app/Stubs/Billing.sharpc").exists(), "{printed}");
}

/// A `set` that empties its `List` with `field = []` and adds to it again, as php-sharp's `Accessors.sharp` does,
/// compiles: the add is the `List` method the checker checked.
#[test]
fn compile_writes_a_list_property_whose_set_empties_and_refills_it() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\n");
    write(
        root,
        "app/Store/Product.sharp",
        "namespace Store;\n\nclass Product\n{\n    public List<string> tags\n    {\n        get => field;\n        set\n        {\n            field = [];\n            for (const tag of value) {\n                field.add(tag);\n            }\n        }\n    } = [];\n}\n",
    );

    let output = run(root, "compile", &[]);
    let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

    assert_eq!(output.status.code(), Some(0), "{printed}");
    assert!(root.join(".sharp/app/Store/Product.sharpc").is_file(), "{printed}");
}

#[test]
fn compile_and_analyze_print_json_under_mago_reporting_format_in_github_actions() {
    let directory = suppressed_workspace("Broken.sharp", &broken_sharp(""), "");

    for command in ["compile", "analyze"] {
        let output = Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", command])
            .env("GITHUB_ACTIONS", "true")
            .env("MAGO_REPORTING_FORMAT", "json")
            .current_dir(directory.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let report: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|error| panic!("{command}: {error}: {stdout}"));

        assert_eq!(report["issues"][0]["code"], "invalid-return-statement", "{command}: {stdout}");
        assert_eq!(
            report["issues"][0]["annotations"][0]["span"]["file_id"]["name"], "src/Demo/Broken.sharp",
            "{command}: {stdout}"
        );
    }
}

#[test]
fn a_reporting_format_flag_wins_over_mago_reporting_format() {
    let directory = suppressed_workspace("Broken.sharp", &broken_sharp(""), "");

    let output = Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "analyze", "--reporting-format", "emacs"])
        .env("MAGO_REPORTING_FORMAT", "json")
        .current_dir(directory.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("src/Demo/Broken.sharp:8:") && line.contains("invalid-return-statement")),
        "{stdout}"
    );
}

#[test]
fn compile_and_analyze_stop_on_a_mago_reporting_format_that_names_no_format() {
    let directory = suppressed_workspace("Broken.sharp", &broken_sharp(""), "");

    for command in ["compile", "analyze"] {
        let output = Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", command])
            .env("MAGO_REPORTING_FORMAT", "jsno")
            .current_dir(directory.path())
            .output()
            .unwrap();
        let printed = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));

        assert!(!output.status.success(), "{command}: {printed}");
        assert!(
            printed.contains("Invalid value for environment variable `MAGO_REPORTING_FORMAT`"),
            "{command}: {printed}"
        );
        assert!(!directory.path().join(".sharp").exists(), "{command}");
    }
}

#[test]
fn analyze_refuses_a_pin_to_the_next_major_and_names_the_pin_of_this_binary() {
    let directory = tempfile::tempdir().unwrap();
    let next_major = env!("CARGO_PKG_VERSION_MAJOR").parse::<u64>().unwrap() + 1;
    std::fs::write(directory.path().join("mago.toml"), format!("version = \"{next_major}\"\n")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--colors", "never", "analyze"])
        .env("MAGO_LOG", "info")
        .current_dir(directory.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let pin = concat!("version = \"", env!("CARGO_PKG_VERSION_MAJOR"), ".", env!("CARGO_PKG_VERSION_MINOR"), "\"");

    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains(pin), "{stderr}");
    assert!(!stderr.contains("--to-project-version"), "{stderr}");
}
