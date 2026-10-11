#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;

use foldhash::HashSet;

use mago_allocator::LocalArena;
use mago_analyzer::Analyzer;
use mago_analyzer::analysis_result::AnalysisResult;
use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_analyzer::effects::EffectSummary;
use mago_analyzer::effects::Effects;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::plugin::context::HookContext;
use mago_analyzer::plugin::hook::ExpressionHook;
use mago_analyzer::plugin::hook::ExpressionHookResult;
use mago_analyzer::plugin::hook::HookResult;
use mago_analyzer::plugin::hook::StaticCall;
use mago_analyzer::plugin::hook::StaticMethodCallHook;
use mago_analyzer::plugin::provider::Provider;
use mago_analyzer::plugin::provider::ProviderMeta;
use mago_analyzer::settings::Settings;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::populator::populate_codebase;
use mago_codex::scanner::scan_program;
use mago_codex::ttype::TType;
use mago_database::DatabaseReader;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_names::CHANGING_COLLECTION_METHODS;
use mago_names::resolver::NameResolver;
use mago_prelude::Prelude;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::parser::parse_file;
use mago_word::WordSet;
use mago_word::ascii_lowercase_word;

static PRELUDE: LazyLock<Prelude> = LazyLock::new(Prelude::build);
static PLUGIN_REGISTRY: LazyLock<PluginRegistry> = LazyLock::new(PluginRegistry::with_library_providers);

const CALC: &str = "<?php\n\nnamespace Lib;\n\nfinal class Calc\n{\n    public static function make(): self\n    {\n        return new self();\n    }\n\n    public function add(int $a, int $b): int\n    {\n        return $a + $b;\n    }\n}\n";

/// The settings every test analyzes with, unless it names its own.
fn settings() -> Settings {
    Settings { find_unused_expressions: true, find_unused_definitions: true, check_throws: true, ..Default::default() }
}

/// Analyzes `analyzed` together with `others`, and returns its issues as `line:column code`.
fn issues(analyzed: (&'static str, &'static str), others: &[(&'static str, &'static str)]) -> Vec<String> {
    issues_with(settings(), analyzed, others)
}

/// Analyzes `analyzed` together with `others` under `settings`, and returns its issues as `line:column code`.
fn issues_with(
    settings: Settings,
    analyzed: (&'static str, &'static str),
    others: &[(&'static str, &'static str)],
) -> Vec<String> {
    issues_in(&PLUGIN_REGISTRY, settings, analyzed, others)
}

/// Analyzes `analyzed` together with `others` under `settings` and the plugins of `registry`, and returns its issues
/// as `line:column code`.
fn issues_in(
    registry: &PluginRegistry,
    settings: Settings,
    analyzed: (&'static str, &'static str),
    others: &[(&'static str, &'static str)],
) -> Vec<String> {
    analyze(registry, settings, analyzed, others).iter().map(|issue| located(analyzed.1, issue)).collect()
}

/// An issue of `code` as `line:column code`, at its primary span.
fn located(code: &str, issue: &Issue) -> String {
    let offset = issue.primary_span().expect("a primary span").start.offset as usize;
    let line = code[..offset].matches('\n').count() + 1;
    let column = offset - code[..offset].rfind('\n').map_or(0, |newline| newline + 1) + 1;

    format!("{line}:{column} {}", issue.code.as_deref().unwrap_or("none"))
}

/// Analyzes `analyzed` together with `others` under `settings` and the plugins of `registry`, and returns its issues.
fn analyze(
    registry: &PluginRegistry,
    settings: Settings,
    analyzed: (&'static str, &'static str),
    others: &[(&'static str, &'static str)],
) -> Vec<Issue> {
    analyze_with_artifacts(registry, settings, analyzed, others).0
}

/// Analyzes `analyzed` together with `others` under `settings` and the plugins of `registry`, and returns its issues
/// and the analysis artifacts.
fn analyze_with_artifacts(
    registry: &PluginRegistry,
    settings: Settings,
    analyzed: (&'static str, &'static str),
    others: &[(&'static str, &'static str)],
) -> (Vec<Issue>, AnalysisArtifacts) {
    let Prelude { mut database, mut metadata, mut symbol_references } = PRELUDE.clone();

    let file_ids: Vec<_> = std::iter::once(&analyzed)
        .chain(others)
        .map(|(name, code)| {
            (*name, database.add(File::ephemeral(Cow::Borrowed(name.as_bytes()), Cow::Borrowed(code.as_bytes()))))
        })
        .collect();

    let arena = LocalArena::new();
    let mut programs = Vec::new();
    for (name, file_id) in &file_ids {
        let file = database.get_ref(file_id).expect("file was just added");
        let program = parse_file(&arena, file);
        assert!(programs.is_empty() || !program.has_errors(), "{name} did not parse: {:?}", program.errors);

        let names = NameResolver::new(&arena).resolve(program);
        metadata.extend(scan_program(&arena, file, program, &names, settings.version));
        programs.push((file, program, names));
    }

    populate_codebase(&mut metadata, &mut symbol_references, WordSet::default(), HashSet::default());

    let (file, program, names) = &programs[0];
    let mut result = AnalysisResult::new(symbol_references);
    let artifacts = Analyzer::new(&arena, file, names, &metadata, registry, settings)
        .analyze_with_artifacts(program, &mut result)
        .expect("analysis succeeds");

    // The analyzed file's parse errors come first, as `mago analyze` reports them beside the analysis.
    (program.errors.iter().map(Issue::from).chain(result.issues).collect(), artifacts)
}

#[test]
fn adding_a_mixed_operand_reports_mixed_operand_as_in_php() {
    let source = "<?php\n\nnamespace Lib;\n\nfinal class Source\n{\n    public static function value(): mixed\n    {\n        return 1;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Source;\n\nclass Report\n{\n    public static int total()\n    {\n        return Source.value() + 1;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Source;\n\nclass Report\n{\n    public static function total(): int\n    {\n        return Source::value() + 1;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Source.php", source)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Source.php", source)]);

    assert!(codes(&sharp_issues).contains(&"mixed-operand"), "{sharp_issues:?}");
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn the_analyzer_reads_the_dialect_from_the_program_not_the_file_name() {
    const CODE: &str = "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        let label = \"one\";\n        return extra + label;\n    }\n}\n";
    let Prelude { mut metadata, mut symbol_references, .. } = PRELUDE.clone();
    let settings = Settings::default();
    let arena = LocalArena::new();
    let sharp_file = File::ephemeral(Cow::Borrowed(b"src/Demo/Report.sharp"), Cow::Borrowed(CODE.as_bytes()));
    let php_named_file = File::ephemeral(Cow::Borrowed(b"src/Demo/Report.php"), Cow::Borrowed(CODE.as_bytes()));
    let program = parse_file(&arena, &sharp_file);
    let names = NameResolver::new(&arena).resolve(program);
    metadata.extend(scan_program(&arena, &sharp_file, program, &names, settings.version));
    populate_codebase(&mut metadata, &mut symbol_references, WordSet::default(), HashSet::default());

    let mut result = AnalysisResult::new(symbol_references);
    Analyzer::new(&arena, &php_named_file, &names, &metadata, &PLUGIN_REGISTRY, settings)
        .analyze(program, &mut result)
        .expect("analysis succeeds");

    assert!(result.issues.iter().any(|issue| issue.code.as_deref() == Some("invalid-operand")), "{:#?}", result.issues);
}

/// The binder records a call of a changing collection method as a write to its local, so a lambda captures that
/// local by reference. It knows the methods only by name, so its list must name every collection method without
/// `@mutation-free`, which changes the collection.
#[test]
fn the_binder_knows_every_changing_collection_method() {
    for class in ["Sharp\\ListMethods", "Sharp\\MapMethods", "Sharp\\SetMethods"] {
        let metadata =
            PRELUDE.metadata.get_class_like(class.as_bytes()).expect("the collection stub is in the prelude");
        let mut changing: Vec<&str> = metadata
            .methods
            .iter()
            .filter(|method| {
                !PRELUDE
                    .metadata
                    .get_method(class.as_bytes(), method.as_bytes())
                    .expect("a method")
                    .flags
                    .is_mutation_free()
            })
            .map(|method| std::str::from_utf8(method.as_bytes()).expect("an ASCII name"))
            .collect();
        changing.sort_unstable();
        assert!(!changing.is_empty(), "{class} has no changing method, so the stub was not read");

        let known: Vec<&str> =
            changing.iter().copied().filter(|method| CHANGING_COLLECTION_METHODS.contains(method)).collect();
        assert_eq!(known, changing, "{class} changes the collection in a method CHANGING_COLLECTION_METHODS lacks");
    }
}

/// The issue codes alone, in order.
fn codes(issues: &[String]) -> Vec<&str> {
    issues.iter().map(|issue| issue.split_once(' ').map_or("", |(_, code)| code)).collect()
}

#[test]
fn passing_a_string_to_a_php_int_parameter_is_an_invalid_argument_at_the_sharp_position() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        let label = \"one\";\n        const base = 2;\n        const calc = Calc.make();\n        return calc.add(label, base * extra);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public static function total(int $extra): int\n    {\n        $label = \"one\";\n        $base = 2;\n        $calc = Calc::make();\n        return $calc->add($label, $base * $extra);\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Calc.php", CALC)]);

    assert_eq!(sharp_issues, ["12:25 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn correct_sharp_code_has_no_issues() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        let label = 1;\n        const base = 2;\n        const calc = Calc.make();\n        return calc.add(label, base * extra);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]), Vec::<String>::new());
}

/// A `let` without a written type takes its first value's type, as C#'s `var` and TypeScript's `let` do, so a value
/// of another type is an error where it is assigned. A plain PHP variable still takes any value.
#[test]
fn a_let_local_keeps_the_type_of_its_first_value_where_php_changes_it() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total()\n    {\n        let value = 1;\n        value = \"one\";\n        return value;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(): int\n    {\n        $value = 1;\n        $value = \"one\";\n        return $value;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["8:17 invalid-local-assignment-value"]);
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["11:16 invalid-return-statement"]);
}

/// The semantic checks refuse a `let` that starts as `null` without a written type, so a later value adds no second
/// error naming the `null` type.
#[test]
fn a_later_value_of_an_untyped_null_start_adds_no_issue() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total()\n    {\n        let value = null;\n        value = 5;\n        return value;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

/// A `let`'s first value fixes its general type: a literal widens to its scalar type, and a list or map literal to a
/// `List<T>` or `Map<TKey, TValue>` of any length, so the local takes any value of that type and a collection method
/// checks its arguments against it. A `List` literal assigned to the `Map` is refused as a literal of the other
/// collection too.
#[test]
fn a_let_local_takes_any_value_of_its_first_values_general_type() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    public int run()\n    {\n        let total = 0;\n        total = 5;\n        let sizes = [5];\n        sizes = [];\n        sizes.add(6);\n        sizes.add(\"six\");\n        let rows = [[1]];\n        rows = [[2, 3], []];\n        let prices = [\"a\": 1];\n        prices = [\"b\": 2, \"c\": 3];\n        prices = [1.5];\n        return total + count(sizes) + count(rows) + count(prices);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[]),
        ["12:19 invalid-argument", "17:18 invalid-array-element", "17:18 invalid-local-assignment-value"]
    );
}

#[test]
fn incrementing_and_decrementing_a_let_local_changes_its_type_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string total()\n    {\n        let up = 0;\n        let down = 0;\n        up++;\n        down--;\n        ++up;\n        --down;\n        return up * down;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(): string\n    {\n        $up = 0;\n        $down = 0;\n        $up++;\n        $down--;\n        ++$up;\n        --$down;\n        return $up * $down;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["13:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn this_calls_the_instance_method_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int total()\n    {\n        return this.label();\n    }\n\n    public string label()\n    {\n        return \"one\";\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public function total(): int\n    {\n        return $this->label();\n    }\n\n    public function label(): string\n    {\n        return \"one\";\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["7:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_class_in_the_same_namespace_takes_static_calls_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Helper\n{\n    public static int twice(int value)\n    {\n        return value * 2;\n    }\n}\n\nclass Report\n{\n    public static int total()\n    {\n        return Helper.twice(\"one\") - Helper.missing();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Helper\n{\n    public static function twice(int $value): int\n    {\n        return $value * 2;\n    }\n}\n\nclass Report\n{\n    public static function total(): int\n    {\n        return Helper::twice(\"one\") - Helper::missing();\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues[0], "15:29 invalid-argument");
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_local_reads_a_missing_property_as_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total()\n    {\n        const calc = Calc.make();\n        return calc.missing;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public static function total(): int\n    {\n        $calc = Calc::make();\n        return $calc->missing;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Calc.php", CALC)]);

    assert!(!sharp_issues.is_empty());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn bare_constants_resolve_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total()\n    {\n        return PHP_INT_MAX - MISSING;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(): int\n    {\n        return PHP_INT_MAX - MISSING;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues[0], "7:30 non-existent-constant");
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn php_calls_a_sharp_method_by_position_and_by_name() {
    let sharp =
        "namespace Demo;\n\nclass Report\n{\n    public int total(int extra)\n    {\n        return extra;\n    }\n}\n";
    let php = "<?php\n\nnamespace App;\n\nfunction run(\\Demo\\Report $report): int\n{\n    return $report->total(extra: 1) + $report->total(\"one\");\n}\n";

    let php_issues = issues(("src/App/run.php", php), &[("src/Demo/Report.sharp", sharp)]);

    assert_eq!(php_issues, ["7:54 invalid-argument"]);
}

#[test]
fn sharp_arguments_follow_strict_conversion_rules() {
    let money = "<?php\n\nnamespace Lib;\n\nfinal class Money\n{\n    public static function of(float $amount): self\n    {\n        return new self();\n    }\n\n    public static function named(string $name): self\n    {\n        return new self();\n    }\n\n    public function __toString(): string\n    {\n        return 'money';\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Money;\n\nclass Report\n{\n    public static Money total(int extra)\n    {\n        const money = Money.of(extra);\n        return Money.named(money);\n    }\n}\n";

    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Money;\n\nclass Report\n{\n    public static function total(int $extra): Money\n    {\n        $money = Money::of($extra);\n        return Money::named($money);\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Money.php", money)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Money.php", money)]);

    assert_eq!(sharp_issues, ["10:28 invalid-argument"]);
    // Mago applies strict conversion rules to every file, whether or not it declares `strict_types=1`.
    assert_eq!(php_issues, ["12:29 invalid-argument"]);
}

#[test]
fn a_numeric_string_passed_to_a_php_int_parameter_is_an_invalid_argument_as_under_strict_types() {
    let counter = "<?php\n\nnamespace Lib;\n\nfinal class Counter\n{\n    public static function take(int $value): int\n    {\n        return $value;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Counter;\n\nclass Report\n{\n    public static int total()\n    {\n        return Counter.take(\"5\");\n    }\n}\n";
    let php = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Demo;\n\nuse Lib\\Counter;\n\nclass Report\n{\n    public static function total(): int\n    {\n        return Counter::take('5');\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Counter.php", counter)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Counter.php", counter)]);

    assert_eq!(sharp_issues, ["9:29 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// Spec section 14.3: a method named without parentheses is a function value, typed as PHP types `$calc->add(...)`.
#[test]
fn a_method_named_without_a_call_is_a_closure_of_its_signature() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public int total()\n    {\n        const calc = Calc.make();\n        Function<int(int, int)> add = calc.add;\n        const Function<int()> again = this.total;\n        return add(1, 2) + again() + calc.add(\"x\", 1) + add(\"y\", 2);\n    }\n\n    private int hidden() => 1;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public function total(): int\n    {\n        $calc = Calc::make();\n        $add = $calc->add(...);\n        $again = $this->total(...);\n        return $add(1, 2) + $again() + $calc->add(\"x\", 1) + $add(\"y\", 2);\n    }\n\n    private function hidden(): int { return 1; }\n}\n";
    let others = [("src/Lib/Calc.php", CALC)];

    assert_eq!(
        codes(&issues(("src/Demo/Report.php", php), &others)),
        ["invalid-argument", "invalid-argument", "unused-method"]
    );
    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &others),
        ["12:47 invalid-argument", "12:61 invalid-argument", "15:17 unused-method"]
    );
}

/// `Class.name` with no constant, enum case or static property of that name is the static method as a value, typed as
/// PHP types `Calc::make(...)`. An instance method read through its class stays an error.
#[test]
fn a_static_method_named_through_its_class_without_a_call_is_a_closure_of_its_signature() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public int total()\n    {\n        const Function<Calc()> make = Calc.make;\n        const Function<int(int)> twice = Report.twice;\n        return make().add(twice(1), 2) + twice(\"x\") + Calc.add;\n    }\n\n    private static int twice(int n) => n * 2;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public function total(): int\n    {\n        $make = Calc::make(...);\n        $twice = Report::twice(...);\n        return $make()->add($twice(1), 2) + $twice(\"x\") + Calc::$add;\n    }\n\n    private static function twice(int $n): int { return $n * 2; }\n}\n";
    let others = [("src/Lib/Calc.php", CALC)];

    let php_issues = issues(("src/Demo/Report.php", php), &others);
    assert_eq!(
        codes(&php_issues),
        ["invalid-argument", "non-existent-property", "null-operand", "mixed-return-statement"],
        "{php_issues:?}"
    );
    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &others);
    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{sharp_issues:?}");
    assert_eq!(&sharp_issues[..2], ["11:48 invalid-argument", "11:60 non-existent-property"]);
}

/// A method read as a value obeys the method's visibility, as `$order->secret(...)` does in PHP.
#[test]
fn a_private_method_read_as_a_value_from_outside_its_class_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public Function<int()> run(Order order) => order.secret;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public function run(Order $order): \\Closure { return $order->secret(...); }\n}\n";
    let order = "<?php\n\nnamespace Demo;\n\nfinal class Order\n{\n    public function __construct() { $this->secret(); }\n\n    private function secret(): int { return 1; }\n}\n";
    let others = [("src/Demo/Order.php", order)];

    let php_codes =
        codes(&issues(("src/Demo/Report.php", php), &others)).into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(codes(&issues(("src/Demo/Report.sharp", sharp), &others)), php_codes);
    assert!(php_codes.iter().any(|code| code.contains("method")), "{php_codes:?}");
}

#[test]
fn a_local_passed_by_reference_to_a_php_method_changes_as_in_php() {
    let counter = "<?php\n\nnamespace Lib;\n\nfinal class Counter\n{\n    public static function make(): self\n    {\n        return new self();\n    }\n\n    public function fill(?string &$value): void\n    {\n        $value = 'filled';\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Counter;\n\nclass Report\n{\n    public static int total()\n    {\n        string? value = null;\n        const counter = Counter.make();\n        counter.fill(value);\n        return value;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Counter;\n\nclass Report\n{\n    public static function total(): int\n    {\n        $value = null;\n        $counter = Counter::make();\n        $counter->fill($value);\n        return $value;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Counter.php", counter)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Counter.php", counter)]);

    assert_eq!(sharp_issues, ["12:16 nullable-return-statement", "12:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn unused_parameters_are_found_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int used, int unused, int _skipped)\n    {\n        return used;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(int $used, int $unused, int $_skipped): int\n    {\n        return $used;\n    }\n}\n";
    let settings = || Settings { find_unused_parameters: true, ..settings() };

    let sharp_issues = issues_with(settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues_with(settings(), ("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["5:39 unused-parameter"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn unused_parameters_of_an_expression_body_are_found_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private int count = 0;\n\n    public Report(private int start, int unused) => this.count = start;\n\n    public int total(int used, int unused) => used;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    private int $count = 0;\n    public function __construct(private int $start, int $unused) { $this->count = $start; }\n\n    public function total(int $used, int $unused): int { return $used; }\n}\n";
    let settings = || Settings { find_unused_parameters: true, ..settings() };

    let sharp_issues = issues_with(settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues_with(settings(), ("src/Demo/Report.php", php), &[]);

    assert_eq!(
        sharp_issues,
        ["7:38 unused-parameter", "9:32 unused-parameter", "7:31 unused-property", "5:17 write-only-property"]
    );
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_for_counter_has_the_type_of_its_value_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string total(int extra)\n    {\n        let total = 0;\n        for (let step = 0; step < extra; step++) {\n            total += step;\n        }\n        return total;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(int $extra): string\n    {\n        $total = 0;\n        for ($step = 0; $step < $extra; $step++) {\n            $total += $step;\n        }\n        return $total;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["11:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_parameter_read_only_by_a_for_counter_is_used() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int start)\n    {\n        let total = 0;\n        for (let step = start; step < 3; step++) {\n            total += step;\n        }\n        return total;\n    }\n}\n";
    let settings = Settings { find_unused_parameters: true, ..settings() };

    assert_eq!(issues_with(settings, ("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn for_of_loop_variables_have_the_key_and_value_types_as_in_php() {
    let store = "<?php\n\nnamespace Demo;\n\nclass Store\n{\n    /** @return array<int, string> */\n    public static function names(): array\n    {\n        return ['a'];\n    }\n}\n";
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int last()\n    {\n        let found = 0;\n        for (const [key, name] of Store.names()) {\n            found = key;\n            return name;\n        }\n        return found;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function last(): int\n    {\n        $found = 0;\n        foreach (Store::names() as $key => $name) {\n            $found = $key;\n            return $name;\n        }\n        return $found;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Demo/Store.php", store)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Demo/Store.php", store)]);

    assert_eq!(sharp_issues, ["10:20 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_parameter_read_only_by_a_for_of_collection_is_used() {
    let store = "<?php\n\nnamespace Demo;\n\nclass Store\n{\n    /** @return list<int> */\n    public function values(): array\n    {\n        return [1];\n    }\n}\n";
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(Store source)\n    {\n        let total = 0;\n        for (const value of source.values()) {\n            total += value;\n        }\n        return total;\n    }\n}\n";
    let settings = Settings { find_unused_parameters: true, ..settings() };

    assert_eq!(
        issues_with(settings, ("src/Demo/Report.sharp", sharp), &[("src/Demo/Store.php", store)]),
        Vec::<String>::new()
    );
}

#[test]
fn null_coalescing_a_local_narrows_it_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? first, int? second)\n    {\n        if (first != null || second != null) {\n            return first ?? second;\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(?int $first, ?int $second): int\n    {\n        if ($first !== null || $second !== null) {\n            return $first ?? $second;\n        }\n        return 0;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn nullable_values_flow_in_and_out_with_no_issues() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static Calc? find(int? id, Calc? fallback = null)\n    {\n        if (id == null) {\n            return null;\n        }\n        return fallback;\n    }\n\n    public static int? total()\n    {\n        const found = Report.find(null);\n        if (found === null) {\n            return null;\n        }\n        return found.add(1, 2);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]), Vec::<String>::new());
}

#[test]
fn null_coalescing_assignment_makes_a_local_non_null_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? extra)\n    {\n        let total = extra;\n        total ??= 0;\n        extra ??= 1;\n        return total + extra;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(?int $extra): int\n    {\n        $total = $extra;\n        $total ??= 0;\n        $extra ??= 1;\n        return $total + $extra;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn null_safe_access_gives_a_nullable_value_as_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int? maybe(Calc? calc)\n    {\n        return calc?.add(1, 2);\n    }\n\n    public static int total(Calc? calc)\n    {\n        return calc?.add(1, 2);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public static function maybe(?Calc $calc): ?int\n    {\n        return $calc?->add(1, 2);\n    }\n\n    public static function total(?Calc $calc): int\n    {\n        return $calc?->add(1, 2);\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Calc.php", CALC)]);

    assert_eq!(sharp_issues, ["14:16 nullable-return-statement", "14:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_method_read_through_null_safe_access_is_a_closure_or_null() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total(Calc? calc)\n    {\n        Function<int(int, int)>? add = calc?.add;\n        const Function<int(int, int)> call = add ?? ((a, b) => 0);\n        return call(1, 2);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]), Vec::<String>::new());
}

#[test]
fn a_nullable_value_where_a_value_is_required_is_reported_as_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total(int? extra, Calc? calc)\n    {\n        const sum = calc.add(extra, 1);\n        return extra;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public static function total(?int $extra, ?Calc $calc): int\n    {\n        $sum = $calc->add($extra, 1);\n        return $extra;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Calc.php", CALC)]);

    assert_eq!(
        sharp_issues,
        [
            "9:21 possible-method-access-on-null",
            "9:30 possibly-null-argument",
            "10:16 nullable-return-statement",
            "10:16 invalid-return-statement",
        ]
    );
    // PHP reports storing the `mixed` result of the call, and PHP# stores it as `Any?` without a report.
    let php_codes: Vec<&str> = codes(&php_issues).into_iter().filter(|code| *code != "mixed-assignment").collect();
    assert_eq!(codes(&sharp_issues), php_codes);
}

#[test]
fn a_typed_local_takes_values_of_its_written_type_with_no_issues() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total(int? value, bool make)\n    {\n        Calc? found = null;\n        if (make) {\n            found = Calc.make();\n        }\n        int? first = null;\n        first ??= value;\n        const int base = 10;\n        int total = base;\n        total += first ?? 1;\n        return found?.add(total, 1) ?? total;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]), Vec::<String>::new());
}

#[test]
fn locals_of_sibling_blocks_keep_their_own_written_types() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static void total(bool flag)\n    {\n        if (flag) {\n            int value = 1;\n            value = 2;\n        } else {\n            string value = \"one\";\n            value = \"two\";\n        }\n        {\n            float value = 1.5;\n            value = 2.5;\n        }\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_value_that_is_not_the_written_type_of_a_local_is_reported() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? value)\n    {\n        int count = \"one\";\n        count = 2;\n        count = 2.5;\n        count = value;\n        const string label = 1;\n        return count;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        [
            "7:21 invalid-local-assignment-value",
            "9:17 invalid-local-assignment-value",
            "10:17 invalid-local-assignment-value",
            "11:30 invalid-local-assignment-value",
        ]
    );

    let first = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[]).remove(0);
    assert_eq!(first.message, "Invalid assignment to `count`: it is declared as `int`.");
}

#[test]
fn a_typed_for_counter_takes_only_values_of_its_written_type() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? extra)\n    {\n        for (int step = \"one\"; step < 3; step++) {\n        }\n        for (int? found = null; found == null; ) {\n            found = extra;\n        }\n        for (int count = 0; count < 3; count++) {\n            count = 1.5;\n        }\n        return 0;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        ["7:25 invalid-local-assignment-value", "13:21 invalid-local-assignment-value"]
    );
}

#[test]
fn a_by_reference_write_that_can_never_fit_the_written_type_of_a_local_is_reported() {
    let sharp = "namespace Demo;\n\nclass Response\n{\n    public static List<string> line()\n    {\n        string file = \"\";\n        List<string> line = [];\n        headers_sent(file, line);\n        return line;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Response\n{\n    /** @return list<string> */\n    public static function line(): array\n    {\n        $file = '';\n        $line = [];\n        headers_sent($file, $line);\n        return $line;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Response.sharp", sharp), &[]), ["9:28 invalid-local-assignment-value"]);
    assert_eq!(issues(("src/Demo/Response.php", php), &[]), ["13:16 invalid-return-statement"]);
}

#[test]
fn the_nullable_return_help_writes_the_nullable_type_as_the_file_does() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? extra)\n    {\n        return extra;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(?int $extra): int\n    {\n        return $extra;\n    }\n}\n";

    let help = |analyzed| {
        analyze(&PLUGIN_REGISTRY, settings(), analyzed, &[])
            .into_iter()
            .find(|issue| issue.code.as_deref() == Some("nullable-return-statement"))
            .and_then(|issue| issue.help)
            .expect("a nullable-return-statement help")
    };

    assert!(help(("src/Demo/Report.sharp", sharp)).contains("(e.g., 'int?')"));
    assert!(help(("src/Demo/Report.php", php)).contains("(e.g., '?int')"));
}

#[test]
fn assigning_a_property_of_one_local_keeps_the_memoized_calls_of_another_as_in_php() {
    let box_class = "<?php\n\nnamespace Lib;\n\nfinal class Box\n{\n    public int $value = 0;\n\n    /** @mutation-free */\n    public function count(): ?int\n    {\n        return null;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Box;\n\nclass Report\n{\n    public static int total(Box box, Box other)\n    {\n        if (other.count() != null) {\n            box.value = 1;\n            return other.count();\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Box;\n\nclass Report\n{\n    public static function total(Box $box, Box $other): int\n    {\n        if ($other->count() !== null) {\n            $box->value = 1;\n            return $other->count();\n        }\n        return 0;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Box.php", box_class)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Box.php", box_class)]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_sharp_import_passes_the_use_statement_and_casing_checks_as_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\nimport Lib.Money;\nimport Lib.missing;\n\nclass Report\n{\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\nuse Lib\\Money;\nuse Lib\\missing;\n\nclass Report\n{\n}\n";
    let money = "<?php\n\nnamespace Lib;\n\nfinal class money\n{\n}\n";
    let settings = || Settings { check_use_statements: true, check_name_casing: true, ..settings() };

    let others = [("src/Lib/Calc.php", CALC), ("src/Lib/money.php", money)];

    let sharp_issues = issues_with(settings(), ("src/Demo/Report.sharp", sharp), &others);
    let php_issues = issues_with(settings(), ("src/Demo/Report.php", php), &others);

    assert_eq!(sharp_issues, ["4:8 incorrect-class-like-casing", "5:8 non-existent-use-import"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// `Sharp\Text\Regex`, a plain PHP class with a static method.
const REGEX: (&str, &str) = (
    "src/Sharp/Text/Regex.php",
    "<?php\n\nnamespace Sharp\\Text;\n\nfinal class Regex\n{\n    public static function matches(string $pattern, string $text): bool\n    {\n        return preg_match($pattern, $text) === 1;\n    }\n}\n",
);

/// The issues of the PHP twin of a `.sharp` file that calls `Sharp\Text\Regex` through `import … as Rx`.
fn renamed_regex_call_issues() -> Vec<String> {
    let php = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Demo;\n\nuse Sharp\\Text\\Regex as Rx;\n\nclass Report\n{\n    public static function run(string $text): bool\n    {\n        return Rx::matches('/a/', $text) && Rx::matches(1, $text);\n    }\n}\n";

    issues(("src/Demo/Report.php", php), &[REGEX])
}

#[test]
fn a_php_call_through_a_use_renamed_with_as_is_checked_as_the_imported_class() {
    assert_eq!(codes(&renamed_regex_call_issues()), ["invalid-argument"]);
}

/// Spec section 23: `Rx.matches(…)` calls `Sharp\Text\Regex::matches` when the file imports the class as `Rx`, so its
/// arguments are checked against that method, as in PHP.
#[test]
fn a_call_through_a_renamed_import_is_checked_as_the_imported_class_as_in_php() {
    let sharp = "namespace Demo;\n\nimport Sharp.Text.Regex as Rx;\n\nclass Report\n{\n    public static bool run(string text) => Rx.matches(\"/a/\", text) && Rx.matches(1, text);\n}\n";

    let sharp_issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[REGEX]);
    let located_issues: Vec<String> = sharp_issues.iter().map(|issue| located(sharp, issue)).collect();

    assert_eq!(located_issues, ["7:82 invalid-argument"]);
    assert_eq!(codes(&located_issues), codes(&renamed_regex_call_issues()));
    assert!(sharp_issues[0].message.contains("`Rx.matches`"), "{}", sharp_issues[0].message);
}

/// A message names a class the file renames by the name the file imports it as.
#[test]
fn a_type_in_a_message_names_a_renamed_class_by_its_new_name() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc as Tool;\n\nclass Report\n{\n    public static Tool pick(List<Tool> tools, int|string key) => tools[key];\n}\n";

    let issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);

    assert_eq!(issues.iter().map(|issue| issue.code.as_deref()).collect::<Vec<_>>(), [Some("mismatched-array-index")]);
    assert!(issues[0].message.contains("`List<Tool>`"), "{}", issues[0].message);
}

/// Spec section 23: a renamed import of a class that does not exist reports what the plain import of it reports.
#[test]
fn an_unknown_class_imported_under_another_name_is_reported_as_its_plain_import_is() {
    let settings = || Settings { check_use_statements: true, ..settings() };
    let reported = |code: &'static str| -> Vec<String> {
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", code), &[])
            .iter()
            .map(|issue| format!("{} {}", located(code, issue), issue.message))
            .collect()
    };

    let plain = reported("namespace Demo;\n\nimport Nope.Thing;\n\nclass Report\n{\n}\n");
    let renamed = reported("namespace Demo;\n\nimport Nope.Thing as T;\n\nclass Report\n{\n}\n");

    assert_eq!(
        plain,
        ["3:8 non-existent-use-import Imported class, interface, trait, or enum `Nope\\Thing` does not exist."]
    );
    assert_eq!(renamed, plain);
}

#[test]
fn a_php_use_of_an_unknown_class_renamed_with_as_is_reported_as_its_plain_use_is() {
    let settings = || Settings { check_use_statements: true, ..settings() };

    let plain = issues_with(settings(), ("src/Demo/Report.php", "<?php\n\nnamespace Demo;\n\nuse Nope\\Thing;\n"), &[]);
    let renamed =
        issues_with(settings(), ("src/Demo/Report.php", "<?php\n\nnamespace Demo;\n\nuse Nope\\Thing as T;\n"), &[]);

    assert_eq!(plain, ["5:5 non-existent-use-import"]);
    assert_eq!(renamed, plain);
}

#[test]
fn a_field_is_the_php_property_of_the_same_name() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private int count = 0;\n    protected string label = \"one\";\n\n    public int total(int extra)\n    {\n        this.count += extra;\n        this.label = 2;\n        return this.count;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    private int $count = 0;\n    protected string $label = \"one\";\n\n    public function total(int $extra): int\n    {\n        $this->count += $extra;\n        $this->label = 2;\n        return $this->count;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["11:22 invalid-property-assignment-value"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn new_checks_the_constructor_arguments_as_in_php() {
    let money = "<?php\n\nnamespace Lib;\n\nfinal class Money\n{\n    public function __construct(public int $cents)\n    {\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Money;\n\nclass Report\n{\n    public static int total()\n    {\n        const money = new Money(\"one\");\n        return new Money(cents: 2).cents + money.cents;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Money;\n\nclass Report\n{\n    public static function total(): int\n    {\n        $money = new Money(\"one\");\n        return new Money(cents: 2)->cents + $money->cents;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Money.php", money)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Money.php", money)]);

    assert_eq!(sharp_issues, ["9:33 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn the_constructor_is_the_php_constructor() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private int count;\n\n    public Report(int start)\n    {\n        this.count = start;\n    }\n\n    public int total()\n    {\n        return this.count;\n    }\n\n    public static int make()\n    {\n        return new Report(\"one\").total() + new Report(start: 2).total();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    private int $count;\n\n    public function __construct(int $start)\n    {\n        $this->count = $start;\n    }\n\n    public function total(): int\n    {\n        return $this->count;\n    }\n\n    public static function make(): int\n    {\n        return new Report(\"one\")->total() + new Report(start: 2)->total();\n    }\n}\n";
    let caller =
        "<?php\n\nnamespace App;\n\nfunction run(): int\n{\n    return (new \\Demo\\Report(\"one\"))->total();\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["19:27 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
    assert_eq!(issues(("src/App/run.php", caller), &[("src/Demo/Report.sharp", sharp)]), ["7:30 invalid-argument"]);
}

#[test]
fn an_auto_property_is_the_php_property_with_its_set_visibility() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int views { get; private set; } = 0;\n    public string name { get; set; }\n    public int id { get; }\n\n    public Report(int id)\n    {\n        this.id = id;\n        this.name = \"report\";\n    }\n\n    public int bump()\n    {\n        this.views++;\n        return this.views + this.id;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public private(set) int $views = 0;\n    public string $name;\n    public readonly int $id;\n\n    public function __construct(int $id)\n    {\n        $this->id = $id;\n        $this->name = \"report\";\n    }\n\n    public function bump(): int\n    {\n        $this->views++;\n        return $this->views + $this->id;\n    }\n}\n";
    let caller = "<?php\n\nnamespace App;\n\nfunction run(\\Demo\\Report $report): string\n{\n    $report->name = 'other';\n    $report->views = 2;\n    $report->id = 3;\n    return $report->name . $report->views . $report->id;\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);
    let caller_of_sharp = issues(("src/App/run.php", caller), &[("src/Demo/Report.sharp", sharp)]);
    let caller_of_php = issues(("src/App/run.php", caller), &[("src/Demo/Report.php", php)]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
    assert_eq!(caller_of_sharp.len(), 2, "{caller_of_sharp:?}");
    assert_eq!(caller_of_sharp, caller_of_php);
}

#[test]
fn a_promoted_member_is_the_promoted_php_property() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public Report(private int count, public int id { get; }, public string name { get; protected set; })\n    {\n    }\n\n    public int total()\n    {\n        return this.count + this.id;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public function __construct(private int $count, public readonly int $id, public protected(set) string $name)\n    {\n    }\n\n    public function total(): int\n    {\n        return $this->count + $this->id;\n    }\n}\n";
    let caller = "<?php\n\nnamespace App;\n\nfunction run(\\Demo\\Report $report): string\n{\n    $report->id = 3;\n    $report->name = 'other';\n    return $report->name . $report->id;\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);
    let caller_of_sharp = issues(("src/App/run.php", caller), &[("src/Demo/Report.sharp", sharp)]);
    let caller_of_php = issues(("src/App/run.php", caller), &[("src/Demo/Report.php", php)]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
    assert_eq!(caller_of_sharp.len(), 2, "{caller_of_sharp:?}");
    assert_eq!(caller_of_sharp, caller_of_php);
}

#[test]
fn an_initial_value_that_is_not_constant_is_checked_against_the_member_type() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private Clock clock = new Clock();\n    public int ticks { get; private set; } = new Clock().tick();\n    private string label = new Clock();\n\n    public string read()\n    {\n        return this.label;\n    }\n\n    public Clock now()\n    {\n        return this.clock;\n    }\n}\n\nclass Clock\n{\n    public int tick()\n    {\n        return 1;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["7:28 invalid-property-default-value"]);
}

#[test]
fn a_get_only_property_is_readonly_and_set_once_in_the_constructor() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public string code { get; } = \"none\";\n    public int count { get; }\n\n    public Report(public int id { get; }, int start)\n    {\n        this.count = start;\n        this.count = 2;\n        this.id = 3;\n        this.code = \"x\";\n    }\n\n    public void reset(Report other)\n    {\n        other.count = 1;\n        this.count = 0;\n    }\n}\n";

    // The first write in the constructor sets `count`. Every other write would throw `Error` on `readonly`: a second
    // write, a property set from its parameter or its initial value, another object's property, and a write outside
    // the constructor.
    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        [
            "11:14 invalid-property-write",
            "12:14 invalid-property-write",
            "13:14 invalid-property-write",
            "18:15 invalid-property-write",
            "19:14 invalid-property-write",
        ]
    );
}

const NEGATIVE: &str = "<?php\n\nnamespace Lib;\n\nfinal class Negative extends \\Exception\n{\n}\n";

/// Spec section 6.1: an accessor body reads and writes `field`, the storage, and a `set` body reads `value`, both of
/// the property's type, as PHP's hooks read `$this->name` and `$value`.
#[test]
fn accessor_bodies_read_field_and_value_with_the_property_type() {
    let sharp = "namespace Demo;\n\nimport Lib.Negative;\n\nclass Product\n{\n    public string name { get => field; set => field = trim(value); } = \"\";\n    public int stock { get; private set { if (value < 0) { throw new Negative(); } field = value; } } = 0;\n    public int doubled { get => this.stock * 2; }\n    public int hits { get => field; set { field += value; field++; } } = 0;\n    public List<string> seen { get => field; set { field = []; field.add(value[0]); } } = [];\n    public Map<string, int> counts { get => field; set { field = value; field[\"all\"] = 1; } } = [:];\n    public List<string> tags { get => field; set { field = value; } } = [];\n\n    public void tag(string tag)\n    {\n        this.tags.add(tag);\n        this.stock = this.doubled + 1;\n        this.name = tag;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Product.sharp", sharp), &[("src/Lib/Negative.php", NEGATIVE)]), Vec::<String>::new());
}

/// `field` has the property's type, so a value of another type assigned to it is the error assigning that value to
/// the property is, as `$this->count = 'many'` is in the PHP twin's hook.
#[test]
fn a_value_of_the_wrong_type_assigned_to_field_is_reported_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Counter\n{\n    public int count { get => field; set => field = \"many\"; } = 0;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Counter\n{\n    public int $count = 0 {\n        get => $this->count;\n        set {\n            $this->count = 'many';\n        }\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Counter.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Counter.php", php), &[]);

    assert_eq!(sharp_issues, ["5:53 invalid-property-assignment-value"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// `field` and `value` read as the property's type, and a `get` body returns it, as the PHP twin's hooks do.
#[test]
fn field_value_and_a_get_body_have_the_property_type_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Counter\n{\n    public int count { get => strlen(field); set => field = strlen(value); } = 0;\n    public string label { get { return 1; } }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Counter\n{\n    public int $count = 0 {\n        get => strlen($this->count);\n        set {\n            $this->count = strlen($value);\n        }\n    }\n    public string $label {\n        get {\n            return 1;\n        }\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Counter.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Counter.php", php), &[]);

    assert_eq!(sharp_issues.len(), 3, "{sharp_issues:?}");
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// A property whose accessors all have bodies that never use `field` has no storage, so nothing initializes it, as
/// PHP's virtual property. Every other property is backed, and is initialized as a field is.
#[test]
fn only_a_property_with_storage_needs_initializing_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Store\n{\n    public int open { get => 1; }\n    public int named { get => this.open; set => this.save(value); }\n    public int count { get => field; set => field = value; }\n    public int total { get; set => field = value; }\n\n    public void save(int value)\n    {\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Store\n{\n    public int $open {\n        get => 1;\n    }\n    public int $named {\n        get => $this->open;\n        set {\n            $this->save($value);\n        }\n    }\n    public int $count {\n        get => $this->count;\n        set {\n            $this->count = $value;\n        }\n    }\n    public int $total {\n        set {\n            $this->total = $value;\n        }\n    }\n\n    public function save(int $value): void\n    {\n    }\n}\n";

    let settings = || Settings { check_property_initialization: true, ..settings() };
    let sharp_issues = issues_with(settings(), ("src/Demo/Store.sharp", sharp), &[]);
    let php_issues = issues_with(settings(), ("src/Demo/Store.php", php), &[]);

    assert_eq!(codes(&sharp_issues), codes(&php_issues));
    assert!(!sharp_issues.is_empty(), "the backed properties need initializing: {php_issues:?}");
}

/// A get-only property whose `get` body uses `field` is set where a get-only auto-property is: once, in the
/// constructor.
#[test]
fn a_get_only_property_with_a_get_body_is_set_only_where_readonly_allows() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int count { get => field * 2; }\n\n    public Report(int start)\n    {\n        this.count = start;\n    }\n\n    public void reset()\n    {\n        this.count = 0;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["14:14 invalid-property-write"]);
}

/// An accessor body writes another property as a method of its class does, so a write to a get-only property there
/// is the error the same write in a method is. Writing `field` writes the accessor's own storage, which its get-only
/// property allows.
#[test]
fn a_write_to_a_get_only_property_from_another_accessor_is_reported_as_from_a_method() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int id { get; }\n    public int code { get => field; set { field = value; this.id = value; } } = 0;\n    public int hits { get { field = field + 1; return field; } } = 0;\n\n    public Report(int id)\n    {\n        this.id = id;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["6:63 invalid-property-write"]);
}

/// The same write in a plain PHP hook stays unreported, as before PHP# accessors.
#[test]
fn a_write_to_a_readonly_property_from_a_php_hook_stays_unreported() {
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public readonly int $id;\n    public int $code = 0 {\n        get => $this->code;\n        set {\n            $this->code = $value;\n            $this->id = $value;\n        }\n    }\n\n    public function __construct(int $id)\n    {\n        $this->id = $id;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
}

/// A `get` block runs as PHP's `get` hook, which returns the property's type on every path, as a method with a return
/// type does.
#[test]
fn a_get_block_with_a_path_that_ends_without_returning_is_reported() {
    let sharp = "namespace Demo;\n\nimport Lib.Negative;\n\nclass Report\n{\n    public int open { get { if (this.ready()) { return 1; } } }\n    public int closed { get { if (this.ready()) { return 1; } throw new Negative(); } }\n\n    public bool ready()\n    {\n        return true;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Negative.php", NEGATIVE)]),
        ["7:23 missing-return-statement"]
    );
}

/// A property with accessors that redeclares a plain PHP parent's untyped property is refused as a field that does
/// is, because PHP refuses a type the parent property does not have. The refusal names it a property.
#[test]
fn a_property_with_accessors_over_a_php_parents_untyped_property_is_not_supported_yet() {
    let parent = "<?php\n\nnamespace Lib;\n\nclass Record\n{\n    public $label;\n    public $code;\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Record;\n\nclass Order : Record\n{\n    public string label { get => field; set => field = value; } = \"\";\n    public string code { get; set; } = \"\";\n}\n";
    let others = [("src/Lib/Record.php", parent)];

    assert_eq!(issues(("src/Demo/Order.sharp", sharp), &others), ["7:12 not-supported-yet", "8:12 not-supported-yet"]);
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Order.sharp", sharp), &others)
            .into_iter()
            .map(|issue| (issue.message, issue.help.unwrap_or_default()))
            .collect::<Vec<_>>(),
        [
            (
                "A property that replaces the untyped PHP property `Record.label` is not supported yet.".to_owned(),
                "Rename the property, or give the PHP property a type.".to_owned()
            ),
            (
                "A property that replaces the untyped PHP property `Record.code` is not supported yet.".to_owned(),
                "Rename the property, or give the PHP property a type.".to_owned()
            ),
        ]
    );
}

/// A property over a plain PHP parent's attributes has no storage. A `get` body that returns the `mixed` that
/// `getAttribute` returns without converting it with `as` reports `mixed` as the PHP twin's hook does.
#[test]
fn a_property_over_a_php_parents_attributes_reports_its_mixed_get_as_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Model;\n\nclass Order : Model\n{\n    public Address shipping { get => this.getAttribute(\"shipping\"); set => this.setAttribute(\"shipping\", value); }\n}\n\nclass Address\n{\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Model;\n\nclass Order extends Model\n{\n    public Address $shipping {\n        get => $this->getAttribute('shipping');\n        set {\n            $this->setAttribute('shipping', $value);\n        }\n    }\n}\n\nclass Address\n{\n}\n";

    let sharp_issues = issues(("src/Demo/Order.sharp", sharp), &[("src/Lib/Model.php", MODEL)]);
    let php_issues = issues(("src/Demo/Order.php", php), &[("src/Lib/Model.php", MODEL)]);

    assert_eq!(sharp_issues, ["7:38 mixed-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// A nullable property with accessor bodies has the issues of its PHP twin. Over a plain PHP parent's attributes it
/// has no storage, so its `get` reports `mixed` unless it converts the value with `as`. One whose body uses
/// `field` starts as null, so it needs no constructor, and a get-only one without storage needs no initial value.
#[test]
fn nullable_properties_with_accessor_bodies_have_the_issues_of_their_php_twins() {
    let sharp = "namespace Demo;\n\nimport Lib.Model;\n\nclass Order : Model\n{\n    public Address? shipping { get => this.getAttribute(\"shipping\"); set => this.setAttribute(\"shipping\", value); }\n    public string? note { get => field; set => field = value; }\n    public string? summary { get => this.note; }\n}\n\nclass Address\n{\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Model;\n\nclass Order extends Model\n{\n    public ?Address $shipping {\n        get => $this->getAttribute('shipping');\n        set {\n            $this->setAttribute('shipping', $value);\n        }\n    }\n    public ?string $note = null {\n        get => $this->note;\n        set {\n            $this->note = $value;\n        }\n    }\n    public ?string $summary {\n        get => $this->note;\n    }\n}\n\nclass Address\n{\n}\n";

    let sharp_issues = issues(("src/Demo/Order.sharp", sharp), &[("src/Lib/Model.php", MODEL)]);
    let php_issues = issues(("src/Demo/Order.php", php), &[("src/Lib/Model.php", MODEL)]);

    assert_eq!(sharp_issues, ["7:39 mixed-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn typeof_is_the_class_name_as_in_php() {
    let registry = "<?php\n\nnamespace Lib;\n\nfinal class Registry\n{\n    /** @param class-string<Calc> $class */\n    public static function keep(string $class): string\n    {\n        return $class;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\nimport Lib.Registry;\n\nclass Report\n{\n    public static string total()\n    {\n        Registry.keep(typeof(Report));\n        Registry.keep(typeof(Missing));\n        return Registry.keep(typeof(Calc));\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\nuse Lib\\Registry;\n\nclass Report\n{\n    public static function total(): string\n    {\n        Registry::keep(Report::class);\n        Registry::keep(Missing::class);\n        return Registry::keep(Calc::class);\n    }\n}\n";
    let others = [("src/Lib/Calc.php", CALC), ("src/Lib/Registry.php", registry)];

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &others);
    let php_issues = issues(("src/Demo/Report.php", php), &others);

    assert_eq!(sharp_issues, ["10:23 invalid-argument", "11:30 non-existent-class-like", "11:23 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// A static member written through its class name is checked as PHP checks `Class::$name`.
#[test]
fn a_static_member_write_is_checked_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Counter\n{\n    public const int START = 10;\n    private static int count = 0;\n    public static string last { get; private set; } = \"none\";\n\n    public static void touch()\n    {\n        Counter.count = 5;\n        Counter.count++;\n        Counter.count += 2;\n        Counter.last ??= \"never\";\n        Counter.count = \"many\";\n        Counter.missing = 1;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Counter\n{\n    public const int START = 10;\n    private static int $count = 0;\n    public private(set) static string $last = \"none\";\n\n    public static function touch(): void\n    {\n        Counter::$count = 5;\n        Counter::$count++;\n        Counter::$count += 2;\n        Counter::$last ??= \"never\";\n        Counter::$count = \"many\";\n        Counter::$missing = 1;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Counter.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Counter.php", php), &[]);

    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{sharp_issues:?} {php_issues:?}");
    assert_eq!(php_issues.len(), 3, "{php_issues:?}");
}

/// `Class.y` reads a constant or an enum case when the class has one by that name, and the static property otherwise,
/// so each read is checked as the PHP twin's `::` read of that member.
#[test]
fn a_class_member_read_is_checked_as_its_constant_enum_case_or_static_property_in_php() {
    let registry = "<?php\n\nnamespace Lib;\n\nenum Order: string\n{\n    case Ascending = 'asc';\n}\n\nfinal class Registry\n{\n    public const int VERSION = 2;\n    public static string $label = 'registry';\n    protected static int $hidden = 1;\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Order;\nimport Lib.Registry;\n\nclass Members\n{\n    public static int version()\n    {\n        return Registry.VERSION;\n    }\n\n    public static Order order()\n    {\n        return Order.Ascending;\n    }\n\n    public static int label()\n    {\n        return Registry.label;\n    }\n\n    public static int missing()\n    {\n        return Registry.missing;\n    }\n\n    public static int hidden()\n    {\n        return Registry.hidden;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Order;\nuse Lib\\Registry;\n\nclass Members\n{\n    public static function version(): int\n    {\n        return Registry::VERSION;\n    }\n\n    public static function order(): Order\n    {\n        return Order::Ascending;\n    }\n\n    public static function label(): int\n    {\n        return Registry::$label;\n    }\n\n    public static function missing(): int\n    {\n        return Registry::$missing;\n    }\n\n    public static function hidden(): int\n    {\n        return Registry::$hidden;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Members.sharp", sharp), &[("src/Lib/Registry.php", registry)]);
    let php_issues = issues(("src/Demo/Members.php", php), &[("src/Lib/Registry.php", registry)]);

    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{sharp_issues:?} {php_issues:?}");
    assert_eq!(
        sharp_issues,
        [
            "20:16 invalid-return-statement",
            "25:25 non-existent-property",
            "25:16 invalid-return-statement",
            "30:25 invalid-property-read",
            "30:16 never-return",
        ]
    );
}

/// A constant expression reads `Class.y` as the class constant or enum case, as PHP's `Class::y`: a parameter default,
/// a constant's value and a constant initial value check as their PHP twins, and a static property there is a
/// constant the class does not have.
#[test]
fn a_class_member_read_in_a_constant_expression_is_checked_as_a_class_constant() {
    let registry = "<?php\n\nnamespace Lib;\n\nenum Order: string\n{\n    case Ascending = 'asc';\n}\n\nfinal class Registry\n{\n    public const int VERSION = 2;\n    public static string $label = 'registry';\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Order;\nimport Lib.Registry;\n\nclass Members\n{\n    public const int NEXT = Registry.VERSION + 1;\n    public const string LABEL = Registry.label;\n    private Order sort = Order.Ascending;\n\n    public string run(Order order = Order.Ascending, int version = Registry.VERSION)\n    {\n        return this.sort.value;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Order;\nuse Lib\\Registry;\n\nclass Members\n{\n    public const int NEXT = Registry::VERSION + 1;\n    public const string LABEL = Registry::label;\n    private Order $sort = Order::Ascending;\n\n    public function run(Order $order = Order::Ascending, int $version = Registry::VERSION): string\n    {\n        return $this->sort->value;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Members.sharp", sharp), &[("src/Lib/Registry.php", registry)]);
    let php_issues = issues(("src/Demo/Members.php", php), &[("src/Lib/Registry.php", registry)]);

    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{sharp_issues:?} {php_issues:?}");
    assert_eq!(sharp_issues, ["9:42 non-existent-class-constant"]);
}

/// A PHP class extends a PHP# abstract class and implements a PHP# interface, whose method has no access modifier
/// and is public, as their PHP twins are.
#[test]
fn abstract_and_final_classes_and_interfaces_are_checked_as_in_php() {
    let square = "<?php\n\nnamespace Demo;\n\nfinal class Square extends Shape implements Measured\n{\n    public function area(): float\n    {\n        return 4.0;\n    }\n\n    protected function name(): string\n    {\n        return 'square';\n    }\n}\n\nfunction measure(Measured $shape): float\n{\n    return $shape->area() + (new Square())->area();\n}\n";
    let sharp = "namespace Demo;\n\nabstract class Shape\n{\n    public abstract float area();\n\n    protected abstract string name();\n}\n\nfinal class Unit\n{\n}\n\ninterface Measured\n{\n    float area();\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nabstract class Shape\n{\n    abstract public function area(): float;\n\n    abstract protected function name(): string;\n}\n\nfinal class Unit\n{\n}\n\ninterface Measured\n{\n    public function area(): float;\n}\n";

    let sharp_issues = issues(("src/Demo/Square.php", square), &[("src/Demo/Shape.sharp", sharp)]);
    let php_issues = issues(("src/Demo/Square.php", square), &[("src/Demo/Shape.php", php)]);

    assert_eq!(sharp_issues, php_issues);
    assert_eq!(issues(("src/Demo/Shape.sharp", sharp), &[]), issues(("src/Demo/Shape.php", php), &[]));
}

/// A header names a PHP base class and a PHP interface, so the class calls the base's methods and passes where the
/// interface is expected. `super.size()` calls the PHP# base's method, as `parent::size()` does in PHP.
#[test]
fn a_header_names_the_base_class_and_the_interfaces_as_in_php() {
    let library = "<?php\n\nnamespace Lib;\n\ninterface Named\n{\n    public function name(): string;\n}\n\nabstract class Entity\n{\n    public function id(): int\n    {\n        return 7;\n    }\n}\n\nfinal class Shelf\n{\n    public static function show(Named $named, Entity $entity): string\n    {\n        return $named->name() . $entity->id();\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Entity;\nimport Lib.Named;\nimport Lib.Shelf;\n\npublic class Page : Entity, Named\n{\n    public string name()\n    {\n        return \"page\";\n    }\n\n    public int number()\n    {\n        return this.id();\n    }\n\n    public string shown()\n    {\n        return Shelf.show(this, this);\n    }\n}\n\npublic class Image\n{\n    public virtual int size()\n    {\n        return 10;\n    }\n}\n\npublic class Thumbnail : Image\n{\n    public override int size()\n    {\n        return super.size() + 1;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Entity;\nuse Lib\\Named;\nuse Lib\\Shelf;\n\nclass Page extends Entity implements Named\n{\n    public function name(): string\n    {\n        return \"page\";\n    }\n\n    public function number(): int\n    {\n        return $this->id();\n    }\n\n    public function shown(): string\n    {\n        return Shelf::show($this, $this);\n    }\n}\n\nclass Image\n{\n    public function size(): int\n    {\n        return 10;\n    }\n}\n\nclass Thumbnail extends Image\n{\n    #[\\Override]\n    public function size(): int\n    {\n        return parent::size() + 1;\n    }\n}\n";
    let others = [("src/Lib/Entity.php", library)];

    assert_eq!(issues(("src/Demo/Page.php", php), &others), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Page.sharp", sharp), &others), Vec::<String>::new());
}

/// A header with two classes, a trait, a missing name, a final class, an enum or an interface whose method the class
/// lacks reports what its PHP twin's `extends` and `implements` report.
#[test]
fn a_header_reports_what_extends_and_implements_report_in_php() {
    let library = "<?php\n\nnamespace Lib;\n\ninterface Named\n{\n    public function name(): string;\n}\n\nclass Entity\n{\n}\n\nclass Other\n{\n}\n\ntrait Mixin\n{\n}\n\nfinal class Sealed\n{\n}\n\nenum Suit\n{\n    case Hearts;\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Entity;\nimport Lib.Mixin;\nimport Lib.Named;\nimport Lib.Other;\nimport Lib.Sealed;\nimport Lib.Suit;\n\npublic class Twice : Entity, Other\n{\n}\n\npublic class Blend : Mixin\n{\n}\n\npublic class Lost : Missing\n{\n}\n\npublic class Closed : Sealed\n{\n}\n\npublic class Suited : Suit\n{\n}\n\npublic class Partial : Named\n{\n}\n\npublic interface Wide : Entity\n{\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Entity;\nuse Lib\\Mixin;\nuse Lib\\Named;\nuse Lib\\Other;\nuse Lib\\Sealed;\nuse Lib\\Suit;\n\nclass Twice extends Entity implements Other\n{\n}\n\nclass Blend implements Mixin\n{\n}\n\nclass Lost implements Missing\n{\n}\n\nclass Closed extends Sealed\n{\n}\n\nclass Suited implements Suit\n{\n}\n\nclass Partial implements Named\n{\n}\n\ninterface Wide extends Entity\n{\n}\n";
    let others = [("src/Lib/Named.php", library)];

    let sharp_issues = issues(("src/Demo/Twice.sharp", sharp), &others);
    let php_issues = issues(("src/Demo/Twice.php", php), &others);

    assert_eq!(
        codes(&php_issues),
        [
            "invalid-implement",
            "invalid-implement",
            "non-existent-class-like",
            "extend-final-class",
            "invalid-implement",
            "unimplemented-abstract-method",
            "invalid-extend"
        ]
    );
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// A method is closed unless it is `virtual`, and `override` is required to replace one, spec section 22. So PHP#
/// reports what PHP reports for a `final` method and a missing or stray `#[\Override]`, without the
/// `check-missing-override` setting. Implementing an interface method takes no `override`.
#[test]
fn virtual_and_override_are_checked_as_final_and_the_override_attribute_in_php() {
    let library = "<?php\n\nnamespace Lib;\n\ninterface Named\n{\n    public function name(): string;\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Named;\n\npublic class Base\n{\n    public virtual int size()\n    {\n        return 1;\n    }\n\n    public string label()\n    {\n        return \"base\";\n    }\n\n    public virtual int depth()\n    {\n        return 0;\n    }\n}\n\npublic class Child : Base, Named\n{\n    public int size()\n    {\n        return 2;\n    }\n\n    public override string label()\n    {\n        return \"child\";\n    }\n\n    public override int width()\n    {\n        return 3;\n    }\n\n    public final override int depth()\n    {\n        return 1;\n    }\n\n    public string name()\n    {\n        return \"child\";\n    }\n}\n\npublic class GrandChild : Child\n{\n    public override int depth()\n    {\n        return 2;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Named;\n\nclass Base\n{\n    public function size(): int\n    {\n        return 1;\n    }\n\n    final public function label(): string\n    {\n        return \"base\";\n    }\n\n    public function depth(): int\n    {\n        return 0;\n    }\n}\n\nclass Child extends Base implements Named\n{\n    public function size(): int\n    {\n        return 2;\n    }\n\n    #[\\Override]\n    public function label(): string\n    {\n        return \"child\";\n    }\n\n    #[\\Override]\n    public function width(): int\n    {\n        return 3;\n    }\n\n    #[\\Override]\n    final public function depth(): int\n    {\n        return 1;\n    }\n\n    #[\\Override]\n    public function name(): string\n    {\n        return \"child\";\n    }\n}\n\nclass GrandChild extends Child\n{\n    #[\\Override]\n    public function depth(): int\n    {\n        return 2;\n    }\n}\n";
    let others = [("src/Lib/Named.php", library)];

    let sharp_issues = issues(("src/Demo/Base.sharp", sharp), &others);
    let php_issues =
        issues_with(Settings { check_missing_override: true, ..settings() }, ("src/Demo/Base.php", php), &others);

    assert_eq!(
        codes(&php_issues),
        ["override-final-method", "missing-override-attribute", "invalid-override-attribute", "override-final-method"],
        "{php_issues:?}"
    );
    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{sharp_issues:?}");

    let missing = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Base.sharp", sharp), &others).remove(1);
    assert_eq!(missing.message, "Missing `override` modifier on overriding method `Child.size`.");
}

/// A plain PHP base class with untyped properties typed by `@var`, one declared in a trait, typed properties, and
/// attributes read and written through `getAttribute` and `setAttribute`.
const MODEL: &str = "<?php\n\nnamespace Lib;\n\ntrait HasTimestamps\n{\n    /** @var bool */\n    public $timestamps = true;\n}\n\nabstract class Model\n{\n    use HasTimestamps;\n\n    /** @var string|null */\n    protected $table;\n\n    /** @var array<int, string> */\n    protected $fillable = [];\n\n    protected $with = [];\n\n    protected int $perPage = 15;\n\n    protected array $appends = [];\n\n    private $secret = 'model';\n\n    /** @var array<string, mixed> */\n    private array $attributes = [];\n\n    public function getAttribute(string $key): mixed\n    {\n        return $this->attributes[$key] ?? null;\n    }\n\n    public function setAttribute(string $key, mixed $value): static\n    {\n        $this->attributes[$key] = $value;\n\n        return $this;\n    }\n}\n";

/// An override of a plain PHP parent's property writes a type that fits the parent's `@var`, or any type when the
/// parent has none, and the type of a typed parent, with the parent's access level, spec section 6.1. Its PHP twin
/// declares the untyped properties without a type.
#[test]
fn a_field_overrides_a_plain_php_property_with_a_type_that_fits_the_parent() {
    let sharp = "namespace Demo;\n\nimport Lib.Model;\n\npublic class Order : Model\n{\n    protected override string? table = \"orders\";\n    protected override List<string> fillable = [\"number\", \"total\"];\n    protected override List<string> with = [\"customer\"];\n    public override bool timestamps = false;\n    protected override int perPage = 20;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Model;\n\nclass Order extends Model\n{\n    #[\\Override]\n    protected $table = 'orders';\n    #[\\Override]\n    protected $fillable = ['number', 'total'];\n    #[\\Override]\n    protected $with = ['customer'];\n    #[\\Override]\n    public $timestamps = false;\n    #[\\Override]\n    protected int $perPage = 20;\n}\n";
    let others = [("src/Lib/Model.php", MODEL)];

    assert_eq!(issues(("src/Demo/Order.sharp", sharp), &others), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Order.php", php), &others), Vec::<String>::new());
}

/// An override whose type does not fit the parent's, whose access level differs, that overrides nothing, or a field
/// that replaces a parent's property without `override`, is an error, spec section 6.1.
#[test]
fn an_override_that_does_not_match_the_plain_php_property_is_an_error() {
    let sharp = "namespace Demo;\n\nimport Lib.Model;\n\npublic class Order : Model\n{\n    public override List<string> fillable = [\"number\"];\n    protected override string? table = 5;\n    public override int timestamps = 0;\n    protected override List<string> appends = [];\n    protected string with = \"customer\";\n    protected override string missing = \"none\";\n    protected override string secret = \"order\";\n}\n";

    let issues =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Order.sharp", sharp), &[("src/Lib/Model.php", MODEL)]);
    let reported: Vec<(&str, &str)> =
        issues.iter().map(|issue| (issue.code.as_deref().unwrap_or("none"), issue.message.as_str())).collect();

    for expected in [
        (
            "incompatible-property-access",
            "The override `Order.fillable` is `public`, but `Model.fillable` is `protected`.",
        ),
        (
            "invalid-property-default-value",
            "Default value for property `Order.table` is not assignable to its declared type.",
        ),
        (
            "incompatible-property-type",
            "The override `Order.timestamps` has type `int`, which does not fit `bool`, the type of `Model.timestamps`.",
        ),
        ("incompatible-property-type", "Property `Order.appends` has an incompatible type declaration."),
        ("missing-override-attribute", "Missing `override` modifier on overriding field `Order.with`."),
        ("invalid-override-attribute", "Invalid `override` modifier on `Order.missing`."),
        ("invalid-override-attribute", "Invalid `override` modifier on `Order.secret`."),
    ] {
        assert!(reported.contains(&expected), "{expected:?} is missing from {reported:#?}");
    }
    assert_eq!(reported.len(), 7, "{reported:#?}");
}

/// An override keeps its written type instead of inheriting the parent's `@var` type, so a type that does not fit is
/// one issue, not a second one for its initial value against the inherited type.
#[test]
fn an_override_whose_type_does_not_fit_the_parent_is_reported_once() {
    let sharp = "namespace Demo;\n\nimport Lib.Model;\n\npublic class Order : Model\n{\n    protected override int table = 5;\n}\n";

    let issues =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Order.sharp", sharp), &[("src/Lib/Model.php", MODEL)]);

    assert_eq!(
        issues
            .iter()
            .map(|issue| (issue.code.as_deref().unwrap_or("none"), issue.message.as_str()))
            .collect::<Vec<_>>(),
        [(
            "incompatible-property-type",
            "The override `Order.table` has type `int`, which does not fit `string?`, the type of `Model.table`.",
        )]
    );
}

/// A PHP# class that overrides a plain PHP parent's property, for a subclass to override it again.
const ORDER: &str = "namespace Demo;\n\nimport Lib.Model;\n\npublic class Order : Model\n{\n    protected override string? table = \"orders\";\n}\n";

/// A plain PHP base class that types the property `ORDER` overrides.
const TYPED_MODEL: &str =
    "<?php\n\nnamespace Lib;\n\nabstract class Model\n{\n    protected ?string $table = null;\n}\n";

/// A field overrides a PHP# parent's field as it overrides a plain PHP parent's property, spec section 6.1, whether the
/// plain PHP root of the chain leaves the property untyped or types it. Its PHP twin overrides the PHP twin of `ORDER`.
#[test]
fn a_field_overrides_a_sharp_field_with_its_type_and_access_level() {
    let sharp = "namespace Demo;\n\npublic class RushOrder : Order\n{\n    protected override string? table = \"rush_orders\";\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass RushOrder extends Order\n{\n    #[\\Override]\n    protected $table = 'rush_orders';\n}\n";
    let php_order = "<?php\n\nnamespace Demo;\n\nuse Lib\\Model;\n\nclass Order extends Model\n{\n    #[\\Override]\n    protected $table = 'orders';\n}\n";

    for model in [MODEL, TYPED_MODEL] {
        let others = [("src/Demo/Order.sharp", ORDER), ("src/Lib/Model.php", model)];

        assert_eq!(issues(("src/Demo/RushOrder.sharp", sharp), &others), Vec::<String>::new());
    }
    assert_eq!(
        issues(("src/Demo/RushOrder.php", php), &[("src/Demo/Order.php", php_order), ("src/Lib/Model.php", MODEL)]),
        Vec::<String>::new()
    );
}

/// An override of a PHP# parent's field writes `override`, the parent's written type and its access level, spec section
/// 6.1, measured against that field alone, so each break is one error. A property with accessors keeps its type, so it
/// cannot replace the field while the plain PHP root of the chain leaves the property untyped.
#[test]
fn an_override_that_does_not_match_the_sharp_field_is_an_error() {
    let reported = |sharp: &'static str| {
        analyze(
            &PLUGIN_REGISTRY,
            settings(),
            ("src/Demo/RushOrder.sharp", sharp),
            &[("src/Demo/Order.sharp", ORDER), ("src/Lib/Model.php", MODEL)],
        )
        .into_iter()
        .map(|issue| (issue.code.unwrap_or_default(), issue.message))
        .collect::<Vec<_>>()
    };
    let error = |code: &str, message: &str| vec![(code.to_owned(), message.to_owned())];

    assert_eq!(
        reported(
            "namespace Demo;\n\npublic class RushOrder : Order\n{\n    protected override string table = \"rush_orders\";\n}\n"
        ),
        error("incompatible-property-type", "Property `RushOrder.table` has an incompatible type declaration.")
    );
    assert_eq!(
        reported("namespace Demo;\n\npublic class RushOrder : Order\n{\n    protected override int table = 5;\n}\n"),
        error("incompatible-property-type", "Property `RushOrder.table` has an incompatible type declaration.")
    );
    assert_eq!(
        reported(
            "namespace Demo;\n\npublic class RushOrder : Order\n{\n    public override string? table = \"rush_orders\";\n}\n"
        ),
        error(
            "incompatible-property-access",
            "The override `RushOrder.table` is `public`, but `Order.table` is `protected`."
        )
    );
    assert_eq!(
        reported(
            "namespace Demo;\n\npublic class RushOrder : Order\n{\n    protected string? table = \"rush_orders\";\n}\n"
        ),
        error("missing-override-attribute", "Missing `override` modifier on overriding field `RushOrder.table`.")
    );
    assert_eq!(
        reported(
            "namespace Demo;\n\npublic class RushOrder : Order\n{\n    protected string? table { get; set; } = \"rush_orders\";\n}\n"
        ),
        error(
            "not-supported-yet",
            "A property that replaces the untyped PHP property `Model.table` is not supported yet."
        )
    );
}

/// PHP makes a property whose `set` is private final, so a field cannot override one, whether a PHP# parent writes it
/// `{ get; private set; }` or a plain PHP parent writes `private(set)`, as the engine refuses the class when it links.
#[test]
fn a_field_cannot_override_a_property_whose_set_is_private() {
    let php = "<?php\n\nnamespace Lib;\n\nclass Tally\n{\n    public private(set) int $views = 0;\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Tally;\n\npublic class Counter\n{\n    public int views { get; private set; } = 0;\n    public int likes { get; set; } = 0;\n}\n\npublic class PageCounter : Counter\n{\n    public override int views = 1;\n    public override int likes = 1;\n}\n\npublic class PageTally : Tally\n{\n    public override int views = 1;\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counter.sharp", sharp), &[("src/Lib/Tally.php", php)])
            .into_iter()
            .map(|issue| (issue.code.unwrap_or_default(), issue.message))
            .collect::<Vec<_>>(),
        [
            ("override-final-property".to_owned(), "Cannot override final property `Counter.views`.".to_owned()),
            ("override-final-property".to_owned(), "Cannot override final property `Tally.views`.".to_owned()),
        ]
    );
}

/// A field cannot override a PHP# parent's property with accessor bodies yet: spec section 6.1 overrides it as a
/// property, and PHP would keep the parent's accessors on the field.
#[test]
fn overriding_a_sharp_property_with_accessor_bodies_is_not_supported_yet() {
    let sharp = "namespace Demo;\n\npublic class Base\n{\n    public string slug => \"base\";\n    protected string label = \"base\";\n}\n\npublic class Child : Base\n{\n    public override string slug = \"child\";\n    protected override string label = \"child\";\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Base.sharp", sharp), &[])
            .into_iter()
            .map(|issue| (issue.code.unwrap_or_default(), issue.message))
            .collect::<Vec<_>>(),
        [("not-supported-yet".to_owned(), "Overriding the PHP# property `Base.slug` is not supported yet.".to_owned())]
    );
}

/// A PHP class that implements a class reports it, and keeps the metadata upstream Mago builds: only a PHP# header
/// links its class as the parent.
#[test]
fn a_php_class_that_implements_a_class_is_still_an_error() {
    let php = "<?php\n\nnamespace Demo;\n\nclass Entity\n{\n    public function id(): int\n    {\n        return 7;\n    }\n}\n\nclass Page implements Entity\n{\n    public function number(): int\n    {\n        return parent::id();\n    }\n}\n";

    assert_eq!(
        codes(&issues(("src/Demo/Page.php", php), &[])),
        ["invalid-implement", "invalid-parent-type", "mixed-return-statement"]
    );
}

/// A plain PHP method is open unless PHP marks it `final`, and replacing one needs `override`, spec section 22. So
/// PHP# reports what PHP reports for its `#[\Override]` twin: a missing `override`, `override` with no parent method,
/// replacing a `final` method, and, as errors, renamed parameters.
#[test]
fn replacing_a_php_method_follows_php_rules_with_override_required() {
    let library = "<?php\n\nnamespace Lib;\n\nabstract class Report\n{\n    abstract protected function render(): string;\n\n    public function title(): string\n    {\n        return 'Report';\n    }\n\n    final public function id(): string\n    {\n        return 'report';\n    }\n\n    public function resize(int $width, int $height): void\n    {\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Report;\n\npublic class SalesReport : Report\n{\n    protected override string render()\n    {\n        return \"sales\";\n    }\n\n    public override string title()\n    {\n        return \"Sales\";\n    }\n}\n\npublic class PlainReport : Report\n{\n    protected override string render()\n    {\n        return \"plain\";\n    }\n\n    public string title()\n    {\n        return \"Plain\";\n    }\n}\n\npublic class FinalReport : Report\n{\n    protected override string render()\n    {\n        return \"final\";\n    }\n\n    public override string id()\n    {\n        return \"final\";\n    }\n}\n\npublic class StrayReport : Report\n{\n    protected override string render()\n    {\n        return \"stray\";\n    }\n\n    public override string footer()\n    {\n        return \"\";\n    }\n}\n\npublic class RenamedReport : Report\n{\n    protected override string render()\n    {\n        return \"renamed\";\n    }\n\n    public override void resize(int w, int h)\n    {\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Report;\n\nclass SalesReport extends Report\n{\n    #[\\Override]\n    protected function render(): string\n    {\n        return \"sales\";\n    }\n\n    #[\\Override]\n    public function title(): string\n    {\n        return \"Sales\";\n    }\n}\n\nclass PlainReport extends Report\n{\n    #[\\Override]\n    protected function render(): string\n    {\n        return \"plain\";\n    }\n\n    public function title(): string\n    {\n        return \"Plain\";\n    }\n}\n\nclass FinalReport extends Report\n{\n    #[\\Override]\n    protected function render(): string\n    {\n        return \"final\";\n    }\n\n    #[\\Override]\n    public function id(): string\n    {\n        return \"final\";\n    }\n}\n\nclass StrayReport extends Report\n{\n    #[\\Override]\n    protected function render(): string\n    {\n        return \"stray\";\n    }\n\n    #[\\Override]\n    public function footer(): string\n    {\n        return \"\";\n    }\n}\n\nclass RenamedReport extends Report\n{\n    #[\\Override]\n    protected function render(): string\n    {\n        return \"renamed\";\n    }\n\n    #[\\Override]\n    public function resize(int $w, int $h): void\n    {\n    }\n}\n";
    let others = [("src/Lib/Report.php", library)];

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &others);
    let php_issues =
        issues_with(Settings { check_missing_override: true, ..settings() }, ("src/Demo/Report.php", php), &others);

    assert_eq!(
        codes(&php_issues),
        [
            "missing-override-attribute",
            "override-final-method",
            "invalid-override-attribute",
            "incompatible-parameter-name",
            "incompatible-parameter-name"
        ],
        "{php_issues:?}"
    );
    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{sharp_issues:?}");

    let renamed: Vec<_> = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &others)
        .into_iter()
        .filter(|issue| issue.code.as_deref() == Some("incompatible-parameter-name"))
        .collect();
    assert!(renamed.iter().all(|issue| issue.level == Level::Error), "{renamed:?}");
    assert_eq!(
        renamed[0].message,
        "Parameter #1 of `RenamedReport.resize` is named `w` but parent `Report.resize` names it `width`"
    );
}

/// A plain PHP parent whose methods type a native `array` return and parameter in their docblocks.
const COUNTER: &str = "<?php\n\nnamespace Lib;\n\nabstract class Counter\n{\n    /** @return list<int> */\n    public function counts(): array\n    {\n        return [];\n    }\n\n    /** @param list<string> $tags */\n    public function tag(array $tags): void\n    {\n    }\n}\n";

/// A PHP# override of `COUNTER`'s `counts` that writes a `Map` return type.
const TALLY: &str = "namespace Demo;\n\nimport Lib.Counter;\n\npublic class Tally : Counter\n{\n    public override Map<int, int> counts()\n    {\n        return [:];\n    }\n}\n";

/// A PHP# override returns the type it writes, because PHP# reads no docblock, so the parent's `@return list<int>`
/// never replaces its `Map<int, int>` and an empty `Map` literal is accepted. The signature check compares that type
/// with the parent's `@return` and reports that it does not fit. The PHP twin inherits the parent's `@return`, so a
/// string-keyed array is refused.
#[test]
fn an_override_of_a_plain_php_method_returns_the_type_it_writes() {
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Counter;\n\nclass Tally extends Counter\n{\n    #[\\Override]\n    public function counts(): array\n    {\n        return ['a' => 1];\n    }\n}\n";
    let others = [("src/Lib/Counter.php", COUNTER)];

    assert_eq!(issues(("src/Demo/Tally.php", php), &others), ["12:16 invalid-return-statement"]);
    assert_eq!(
        messages(("src/Demo/Tally.sharp", TALLY), &others),
        [
            "Return type `Map<int, int>` of `Tally.counts` is incompatible with parent return type `List<int>` of `Counter.counts`"
        ]
    );
}

/// A PHP# override whose written return type fits the parent's `@return` passes the signature check. The PHP twin
/// writes the same override in PHP.
#[test]
fn an_override_of_a_plain_php_method_whose_return_type_fits_the_parents_docblock_is_accepted() {
    let sharp = "namespace Demo;\n\nimport Lib.Counter;\n\npublic class Tally : Counter\n{\n    public override List<int> counts()\n    {\n        return [1];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Counter;\n\nclass Tally extends Counter\n{\n    #[\\Override]\n    public function counts(): array\n    {\n        return [1];\n    }\n}\n";
    let others = [("src/Lib/Counter.php", COUNTER)];

    assert_eq!(issues(("src/Demo/Tally.php", php), &others), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Tally.sharp", sharp), &others), Vec::<String>::new());
}

/// A plain PHP subclass of a PHP# override meets the type the override writes, as a subclass of any PHP# method does,
/// and inherits no `@return` from the plain PHP class above the override.
#[test]
fn a_plain_php_subclass_of_a_sharp_override_meets_the_type_the_override_writes() {
    let plain = "namespace Demo;\n\npublic class Tally\n{\n    public virtual Map<int, int> counts()\n    {\n        return [:];\n    }\n}\n";
    let rush = "<?php\n\nnamespace Demo;\n\nclass Rush extends Tally\n{\n    #[\\Override]\n    public function counts(): array\n    {\n        return ['a' => 1];\n    }\n}\n";

    let below_plain = issues(("src/Demo/Rush.php", rush), &[("src/Demo/Tally.sharp", plain)]);
    let below_override =
        issues(("src/Demo/Rush.php", rush), &[("src/Demo/Tally.sharp", TALLY), ("src/Lib/Counter.php", COUNTER)]);

    assert_eq!(below_plain, ["8:21 incompatible-return-type"]);
    assert_eq!(below_override, below_plain);
}

/// A PHP# override takes the parameter type it writes, so `tags` is a `Map` whose `get` takes its key. The signature
/// check compares that type with the parent's `@param` type and reports that it accepts less. The PHP twin inherits
/// the parent's `@param`.
#[test]
fn an_override_of_a_plain_php_method_takes_the_parameter_type_it_writes() {
    let sharp = "namespace Demo;\n\nimport Lib.Counter;\n\npublic class Labels : Counter\n{\n    public int? last = null;\n\n    public override void tag(Map<string, int> tags)\n    {\n        this.last = tags.get(\"a\");\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Counter;\n\nclass Labels extends Counter\n{\n    #[\\Override]\n    public function tag(array $tags): void\n    {\n        /** @psalm-trace $tags */\n        $count = count($tags);\n    }\n}\n";
    let others = [("src/Lib/Counter.php", COUNTER)];

    assert_eq!(messages(("src/Demo/Labels.php", php), &others), ["Trace: Type of `$tags` is `list<string>`"]);
    assert_eq!(
        messages(("src/Demo/Labels.sharp", sharp), &others),
        [
            "Parameter `tags` of `Labels.tag` expects type `Map<string, int>` but parent `Counter.tag` expects type `List<string>`"
        ]
    );
}

/// A PHP# method that implements a plain PHP interface method keeps the types it writes, as an override does, and the
/// signature check reports each one that does not fit the interface's docblock. The PHP twin inherits the interface's
/// `@return` and `@param`.
#[test]
fn an_implementation_of_a_plain_php_interface_method_keeps_the_types_it_writes() {
    let library = "<?php\n\nnamespace Lib;\n\ninterface Source\n{\n    /** @return list<int> */\n    public function counts(): array;\n\n    /** @param list<string> $tags */\n    public function tag(array $tags): void;\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Source;\n\npublic class Feed : Source\n{\n    public int? last = null;\n\n    public Map<int, int> counts()\n    {\n        return [:];\n    }\n\n    public void tag(Map<string, int> tags)\n    {\n        this.last = tags.get(\"a\");\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Source;\n\nclass Feed implements Source\n{\n    public function counts(): array\n    {\n        return ['a' => 1];\n    }\n\n    public function tag(array $tags): void\n    {\n        /** @psalm-trace $tags */\n        $count = count($tags);\n    }\n}\n";
    let others = [("src/Lib/Source.php", library)];

    assert_eq!(issues(("src/Demo/Feed.php", php), &others), ["11:16 invalid-return-statement", "16:26 psalm-trace"]);
    assert_eq!(
        messages(("src/Demo/Feed.sharp", sharp), &others),
        [
            "Return type `Map<int, int>` of `Feed.counts` is incompatible with parent return type `List<int>` of `Source.counts`",
            "Parameter `tags` of `Feed.tag` expects type `Map<string, int>` but parent `Source.tag` expects type `List<string>`"
        ]
    );
}

/// A PHP# override throws nothing its parent's `@throws` names, so a plain PHP caller that checks thrown types has
/// nothing to handle. The PHP twin inherits the `@throws`.
#[test]
fn an_override_of_a_plain_php_method_takes_no_throws_from_the_parent() {
    let library = "<?php\n\nnamespace Lib;\n\nabstract class Loader\n{\n    /** @throws \\RuntimeException */\n    public function load(): int\n    {\n        return 1;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Loader;\n\npublic class FileLoader : Loader\n{\n    public override int load() => 2;\n}\n";
    let php_twin = "<?php\n\nnamespace Demo;\n\nuse Lib\\Loader;\n\nclass FileLoader extends Loader\n{\n    #[\\Override]\n    public function load(): int\n    {\n        return 2;\n    }\n}\n";
    let caller =
        "<?php\n\nnamespace Demo;\n\nfunction run(FileLoader $loader): int\n{\n    return $loader->load();\n}\n";

    assert_eq!(
        issues(("src/Demo/run.php", caller), &[("src/Demo/FileLoader.php", php_twin), ("src/Lib/Loader.php", library)]),
        ["7:12 unhandled-thrown-type"]
    );
    assert_eq!(
        issues(("src/Demo/run.php", caller), &[("src/Demo/FileLoader.sharp", sharp), ("src/Lib/Loader.php", library)]),
        Vec::<String>::new()
    );
}

/// A PHP# override declares no template its parent's `@template` declares, so a call of it is not generic and records
/// no type argument for the running program. The PHP twin inherits the template.
#[test]
fn an_override_of_a_plain_php_method_takes_no_template_from_the_parent() {
    let library = "<?php\n\nnamespace Lib;\n\nabstract class Keeper\n{\n    /**\n     * @template T\n     * @param T $value\n     * @return T\n     */\n    public function keep(mixed $value): mixed\n    {\n        return $value;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Keeper;\n\npublic class Box : Keeper\n{\n    public override Any? keep(Any? value) => value;\n\n    public int run()\n    {\n        this.keep(5);\n        return 1;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Keeper;\n\nclass Box extends Keeper\n{\n    #[\\Override]\n    public function keep(mixed $value): mixed\n    {\n        return $value;\n    }\n\n    public function run(): int\n    {\n        $this->keep(5);\n        return 1;\n    }\n}\n";
    let type_arguments = |analyzed: (&'static str, &'static str), call: &str| {
        let start = analyzed.1.find(call).unwrap() as u32;
        let (issues, artifacts) =
            analyze_with_artifacts(&PLUGIN_REGISTRY, settings(), analyzed, &[("src/Lib/Keeper.php", library)]);
        assert!(issues.is_empty(), "{issues:?}");

        artifacts
            .inferred_type_arguments
            .get(&(start, start + call.len() as u32))
            .map(|arguments| arguments.iter().map(|argument| argument.get_id().to_string()).collect::<Vec<_>>())
    };

    assert_eq!(type_arguments(("src/Demo/Box.php", php), "$this->keep(5)"), Some(vec!["int".to_owned()]));
    assert_eq!(type_arguments(("src/Demo/Box.sharp", sharp), "this.keep(5)"), None);
}

/// A PHP# override takes no `@psalm-assert-if-true` from its parent, so a call of it narrows nothing the parent's
/// docblock promises and a later `is string` check is possible. The PHP twin inherits the assertion.
#[test]
fn an_override_of_a_plain_php_method_takes_no_assertion_from_the_parent() {
    let library = "<?php\n\nnamespace Lib;\n\nabstract class Check\n{\n    /** @psalm-assert-if-true int $value */\n    public function matches(mixed $value): bool\n    {\n        return is_int($value);\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Check;\n\npublic class TextCheck : Check\n{\n    public override bool matches(Any? value) => value is string;\n\n    public int size(Any? value)\n    {\n        if (this.matches(value) && value is string) {\n            return 1;\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Check;\n\nclass TextCheck extends Check\n{\n    #[\\Override]\n    public function matches(mixed $value): bool\n    {\n        return is_string($value);\n    }\n\n    public function size(mixed $value): int\n    {\n        if ($this->matches($value) && is_string($value)) {\n            return 1;\n        }\n        return 0;\n    }\n}\n";
    let others = [("src/Lib/Check.php", library)];

    assert_eq!(
        codes(&issues(("src/Demo/TextCheck.php", php), &others)),
        ["impossible-type-comparison", "redundant-logical-operation", "impossible-condition"]
    );
    assert_eq!(issues(("src/Demo/TextCheck.sharp", sharp), &others), Vec::<String>::new());
}

/// A parse error reports its spot once, so returning what failed to parse adds no `never-return`, in PHP# and PHP.
#[test]
fn returning_a_value_that_failed_to_parse_adds_no_issue() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public Report make(string json)\n    {\n        return new Report.fromJson(json);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfunction make(): int\n{\n    return );\n}\n";

    assert_eq!(issues(("src/Demo/make.php", php), &[]), ["7:12 parse"]);
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["7:20 parse"]);
}

/// `super` names the base class, so a class whose header names only interfaces has none, as `parent::` in a PHP
/// class without `extends` has none.
#[test]
fn super_in_a_class_without_a_base_class_is_an_error_as_in_php() {
    let sharp = "namespace Demo;\n\npublic interface Named\n{\n    string name();\n}\n\npublic class Tag : Named\n{\n    public string name()\n    {\n        return super.name();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\ninterface Named\n{\n    public function name(): string;\n}\n\nclass Tag implements Named\n{\n    public function name(): string\n    {\n        return parent::name();\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Tag.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Tag.php", php), &[]);

    assert_eq!(codes(&php_issues), ["invalid-parent-type", "mixed-return-statement"], "{php_issues:?}");
    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{sharp_issues:?}");

    let invalid = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Tag.sharp", sharp), &[]).remove(0);
    assert_eq!(invalid.level, Level::Error);
    assert_eq!(invalid.message, "Cannot use `super` as the current type (`Tag`) does not have a parent class.");

    let php_invalid = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Tag.php", php), &[]).remove(0);
    assert_eq!(
        php_invalid.message,
        "Cannot use `parent` as the current type (`Demo\\Tag`) does not have a parent class."
    );
}

/// The checker refuses a member of `typeof(X)` once, so the analyzer adds no issue on the refused read, its chain, or
/// the value it gives.
#[test]
fn a_member_read_through_a_class_value_is_checked_as_the_static_member_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public const int MAX = 3;\n    private static int count = 0;\n\n    public static string tag() => \"div\";\n\n    public int total()\n    {\n        const type = typeof(Report);\n        const max = type.MAX;\n        const seen = type.count;\n        return typeof(Report).MAX + max + seen;\n    }\n\n    public string label()\n    {\n        const type = typeof(Report);\n        return type.tag();\n    }\n\n    public int missing()\n    {\n        const type = typeof(Report);\n        return type.absent;\n    }\n\n    public void named()\n    {\n        const type = typeof(Report);\n        type.attributes();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public const int MAX = 3;\n    private static int $count = 0;\n\n    public static function tag(): string\n    {\n        return \"div\";\n    }\n\n    public function total(): int\n    {\n        $type = Report::class;\n        $max = $type::MAX;\n        $seen = $type::$count;\n        return Report::MAX + $max + $seen;\n    }\n\n    public function label(): string\n    {\n        $type = Report::class;\n        return $type::tag();\n    }\n\n    public function missing(): int\n    {\n        $type = Report::class;\n        return $type::$absent;\n    }\n\n    public function named(): void\n    {\n        $type = Report::class;\n        $type::attributes();\n    }\n}\n";

    let (issues, artifacts) =
        analyze_with_artifacts(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let sharp_issues: Vec<String> = issues.iter().map(|issue| issue.code.clone().unwrap_or_default()).collect();
    let php_issues = issues_with(settings(), ("src/Demo/Report.php", php), &[]);
    let type_at = |text: &str| {
        let start = sharp.find(text).unwrap() as u32;
        artifacts.expression_types.get(&(start, start + text.len() as u32)).map(|r#type| r#type.get_id().to_string())
    };

    assert_eq!(sharp_issues, codes(&php_issues), "{php_issues:?}");
    assert_eq!(sharp_issues, ["non-existent-property", "invalid-return-statement", "non-existent-method"]);
    assert_eq!(type_at("type.MAX").as_deref(), Some("int(3)"));
    assert_eq!(type_at("type.count").as_deref(), Some("int"));
    assert_eq!(type_at("type.tag()").as_deref(), Some("string"));
    assert_eq!(type_at("typeof(Report).MAX").as_deref(), Some("int(3)"));
}

/// The receiver of a class value read is a class value: `typeof(X)` and a `const` local holding it are the class-string
/// of `X`.
#[test]
fn a_local_holding_typeof_is_the_class_string_of_its_class() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public const int MAX = 3;\n\n    public int total()\n    {\n        const type = typeof(Report);\n        return type.MAX;\n    }\n}\n";
    let (issues, artifacts) =
        analyze_with_artifacts(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let start = sharp.find("type.MAX").unwrap() as u32;

    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(
        artifacts.expression_types.get(&(start, start + 4)).map(|r#type| r#type.get_id().to_string()).as_deref(),
        Some("class-string('Demo\\Report')")
    );
}

/// PHP reads a class-string's members only through `::`. An arrow read of one stays the error it is in PHP.
#[test]
fn an_arrow_read_of_a_class_string_stays_an_error_in_php() {
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public const int MAX = 3;\n\n    public function total(): mixed\n    {\n        $type = Report::class;\n        return $type->MAX;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["12:23 invalid-property-access"]);
}

/// A null-safe read or call through a class value that may be null reads the member when the value holds a class, and
/// is `null` otherwise, as spec section 14.4 defines `a?.b`.
#[test]
fn a_null_safe_member_read_through_a_class_value_is_the_member_or_null() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public const int MAX = 3;\n    private static int count = 0;\n\n    public static string tag() => \"div\";\n\n    public int? max(bool flag)\n    {\n        const type = flag ? typeof(Report) : null;\n        return type?.MAX;\n    }\n\n    public int? seen(bool flag)\n    {\n        const type = flag ? typeof(Report) : null;\n        return type?.count;\n    }\n\n    public string? label(bool flag)\n    {\n        const type = flag ? typeof(Report) : null;\n        return type?.tag();\n    }\n}\n";
    let (issues, artifacts) =
        analyze_with_artifacts(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let type_at = |text: &str| {
        let start = sharp.find(text).unwrap() as u32;
        artifacts.expression_types.get(&(start, start + text.len() as u32)).map(|r#type| r#type.get_id().to_string())
    };

    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(type_at("type?.MAX").as_deref(), Some("int(3)|null"));
    assert_eq!(type_at("type?.count").as_deref(), Some("int|null"));
    assert_eq!(type_at("type?.tag()").as_deref(), Some("null|string"));
}

/// A null receiver skips a null-safe call's arguments, so a local an argument writes may keep its value from before.
#[test]
fn a_local_a_null_safe_call_through_a_class_value_writes_may_keep_its_value() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string make(int? value) => \"made\";\n\n    public int? last(bool flag)\n    {\n        let seen = null;\n        const type = flag ? typeof(Report) : null;\n        type?.make(seen = 5);\n        return seen;\n    }\n}\n";
    let (issues, artifacts) =
        analyze_with_artifacts(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let start = sharp.rfind("seen;").unwrap() as u32;

    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(
        artifacts.expression_types.get(&(start, start + 4)).map(|r#type| r#type.get_id().to_string()).as_deref(),
        Some("int(5)|null")
    );
}

/// A class value that may be null is refused before a member read, as PHP's `$type::MAX` throws "Cannot use null as
/// class" when `$type` is null.
#[test]
fn a_member_read_through_a_class_value_that_may_be_null_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public const int MAX = 3;\n\n    public int read(bool flag)\n    {\n        const type = flag ? typeof(Report) : null;\n        return type.MAX;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public const int MAX = 3;\n\n    public function read(bool $flag): int\n    {\n        $type = $flag ? Report::class : null;\n        return $type::MAX;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);

    assert_eq!(codes(&sharp_issues), codes(&issues(("src/Demo/Report.php", php), &[])));
    assert_eq!(
        messages(("src/Demo/Report.sharp", sharp), &[]),
        ["Attempting static access on a possibly `null` value."]
    );
    assert_eq!(sharp_issues.len(), 1, "{sharp_issues:?}");
}

#[test]
fn a_catch_variable_has_the_caught_classes_as_its_type_as_in_php() {
    let sharp = "namespace Demo;\n\nimport RuntimeException;\nimport LogicException;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        try {\n            if (extra < 0) {\n                throw new RuntimeException(\"negative\");\n            }\n        } catch (RuntimeException | LogicException failure) {\n            return failure.getLine();\n        } finally {\n            extra += 1;\n        }\n        return extra;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse RuntimeException;\nuse LogicException;\n\nclass Report\n{\n    public static function total(int $extra): int\n    {\n        try {\n            if ($extra < 0) {\n                throw new RuntimeException(\"negative\");\n            }\n        } catch (RuntimeException | LogicException $failure) {\n            return $failure->getLine();\n        } finally {\n            $extra += 1;\n        }\n        return $extra;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn calling_php_built_in_functions_has_no_issues() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        const name = sprintf(\"%d items\", count([extra, 2], mode: 0));\n        return strlen(name) + strlen(random_bytes(extra + 1));\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

/// Spec section 31 makes an exception a bug and an expected failure a `Result`, so PHP# declares no thrown exceptions.
#[test]
fn check_throws_skips_sharp_files_and_keeps_reporting_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string token()\n    {\n        return random_bytes(16);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function token(): string\n    {\n        return random_bytes(16);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["9:16 unhandled-thrown-type"]);
}

#[test]
fn a_template_is_a_string_as_php_interpolation_is() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string label(int count, string name)\n    {\n        return `${name} has ${count + 1} items`;\n    }\n\n    public static int size(string name)\n    {\n        return `${name}`;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function label(int $count, string $name): string\n    {\n        return \"{$name} has \" . ($count + 1) . \" items\";\n    }\n\n    public static function size(string $name): int\n    {\n        return \"{$name}\";\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["12:16 invalid-return-statement"]);
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["14:16 invalid-return-statement"]);
}

#[test]
fn a_global_function_the_app_declares_checks_its_arguments_as_in_php() {
    let helpers = "<?php\n\nfunction helper(int $value): int\n{\n    return $value;\n}\n";
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        return helper(extra) + helper(\"one\");\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(int $extra): int\n    {\n        return helper($extra) + helper(\"one\");\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/helpers.php", helpers)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/helpers.php", helpers)]);

    assert_eq!(sharp_issues, ["7:39 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn calling_a_function_that_does_not_exist_is_reported_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        return missing(extra);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(int $extra): int\n    {\n        return missing($extra);\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["7:16 non-existent-function", "7:16 mixed-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// The engine calls a bare function name as the global function, so a call the analyzer resolves to a namespaced
/// function would run another function than the one it checked.
#[test]
fn calling_a_namespaced_function_is_not_supported_yet() {
    let helpers = "<?php\n\nnamespace Demo;\n\nfunction helper(int $value): int\n{\n    return $value;\n}\n\nfunction strlen(string $value): int\n{\n    return 0;\n}\n";
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int extra)\n    {\n        return helper(extra) + strlen(\"one\");\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(int $extra): int\n    {\n        return helper($extra) + strlen(\"one\");\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Demo/helpers.php", helpers)]),
        ["7:16 not-supported-yet", "7:32 not-supported-yet"]
    );
    assert_eq!(issues(("src/Demo/Report.php", php), &[("src/Demo/helpers.php", helpers)]), Vec::<String>::new());
}

/// Spec section 4 writes every member as `this.m()` or `Class.m()`, so a bare call is the global function's, even
/// when the class declares a method of the same name.
#[test]
fn a_bare_call_named_like_a_method_of_its_class_calls_the_php_function() {
    let sharp = "namespace Sharp.Math;\n\npublic static class Math\n{\n    public static float ceil(float x) => ceil(x);\n\n    public static int count(List<int> values) => count(values);\n}\n";

    assert_eq!(issues(("src/Sharp/Math/Math.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_bare_call_of_a_method_that_no_function_shares_names_the_member_to_write() {
    let sharp = "namespace Demo;\n\nclass Calc\n{\n    public static int total() => 1;\n\n    public static int run() => total();\n\n    public int size() => 1;\n\n    public int measure() => Size();\n}\n";
    let analyzed = ("src/Demo/Calc.sharp", sharp);

    assert_eq!(
        messages(analyzed, &[]),
        [
            "Write `Calc.total()`: a static method reaches the members of its class through the class name.",
            "Could not infer a precise return type for method `Calc.run`. Saw type `Any?`.",
            "Write `this.size()`: members of the same object are always written with `this.`.",
            "Could not infer a precise return type for method `Calc.measure`. Saw type `Any?`.",
        ]
    );
    assert_eq!(
        issues(analyzed, &[]),
        [
            "7:32 non-existent-function",
            "7:32 mixed-return-statement",
            "11:29 non-existent-function",
            "11:29 mixed-return-statement",
        ]
    );
}

#[test]
fn plus_joins_two_strings_into_the_string_dot_gives_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(string name)\n    {\n        const label = \"Order \" + name + `!`;\n        let line = label;\n        line += \"\\n\";\n        return line;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(string $name): int\n    {\n        $label = \"Order \" . $name . \"!\";\n        $line = $label;\n        $line .= \"\\n\";\n        return $line;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["10:16 invalid-return-statement"]);
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["12:16 invalid-return-statement"]);
}

#[test]
fn exponentiation_has_the_type_it_has_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int power(int base, int exponent, float rate)\n    {\n        let result = base ** exponent;\n        result **= 2;\n        return result + rate ** 2;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function power(int $base, int $exponent, float $rate): int\n    {\n        $result = $base ** $exponent;\n        $result **= 2;\n        return $result + $rate ** 2;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert!(!php_issues.is_empty(), "{php_issues:?}");
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// Spec section 24 truncates `/` on two integers toward zero, so in PHP# it gives an `int`, as `/=` does on an `int`
/// field. PHP gives `float|int`, which an `int` hook or field refuses.
#[test]
fn division_of_two_ints_gives_an_int_in_php_sharp_and_float_or_int_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private int total = 7;\n\n    public int half => this.total / 2;\n\n    public int shrink(int by)\n    {\n        this.total /= by;\n        return this.total;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    private int $total = 7;\n\n    public int $half { get => $this->total / 2; }\n\n    public function shrink(int $by): int\n    {\n        $this->total /= $by;\n        return $this->total;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
    assert_eq!(
        issues(("src/Demo/Report.php", php), &[]),
        ["9:31 invalid-return-statement", "13:25 invalid-property-assignment-value"]
    );
}

/// A `float` operand keeps `/` a float division in PHP#, as spec section 24 says.
#[test]
fn division_with_a_float_operand_gives_a_float_in_php_sharp() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public float share(int total, float parts) => total / parts;\n\n    public int whole(int total, float parts) => total / parts;\n}\n";

    assert_eq!(codes(&issues(("src/Demo/Report.sharp", sharp), &[])), ["invalid-return-statement"]);
}

#[test]
fn plus_on_a_string_and_a_value_that_may_not_be_one_is_an_invalid_operand() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string total(int count, float rate, string name, string? maybe)\n    {\n        const a = name + count;\n        const b = rate + name;\n        const c = maybe + name;\n        const d = count + rate;\n        return name;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        ["7:19 invalid-operand", "8:19 invalid-operand", "9:19 invalid-operand"]
    );
}

#[test]
fn a_ternary_with_a_bool_condition_has_the_type_it_has_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string? host(string host, bool secure)\n    {\n        const scheme = secure ? \"https\" : \"http\";\n        return host != \"\" ? scheme + host : (secure ? 1 : null);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function host(string $host, bool $secure): ?string\n    {\n        $scheme = $secure ? \"https\" : \"http\";\n        return $host != \"\" ? $scheme . $host : ($secure ? 1 : null);\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert!(!php_issues.is_empty(), "{php_issues:?}");
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_ternary_whose_condition_is_not_bool_is_an_invalid_operand() {
    let source = "<?php\n\nnamespace Lib;\n\nfinal class Source\n{\n    public static function value(): mixed\n    {\n        return 1;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Source;\n\nclass Report\n{\n    public static int pick(int count, string name, bool? maybe)\n    {\n        const a = count ? 1 : 2;\n        const b = name ? 1 : 2;\n        const c = maybe ? 1 : 2;\n        const d = Source.value() ? 1 : 2;\n        const e = count ?: 2;\n        return a + b + c + d + e;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Source.php", source)]),
        ["9:19 invalid-operand", "10:19 invalid-operand", "11:19 invalid-operand", "12:19 invalid-operand"]
    );
}

#[test]
fn every_condition_keeps_testing_truthiness_in_php() {
    let php = "<?php\n\nnamespace Demo;\n\nfunction pick(int $count, string $name, int $a, string $b): int\n{\n    if ($count) {\n        $count++;\n    } elseif ($name) {\n        $count--;\n    }\n    while ($count) {\n        $count--;\n    }\n    do {\n        $count++;\n    } while ($count < 3 && $name);\n    for ($i = 3; $i; $i--) {\n        $count++;\n    }\n    return $a || !$b ? $count : 2;\n}\n";

    assert_eq!(issues(("src/Demo/report.php", php), &[]), Vec::<String>::new());
}

#[test]
fn every_condition_that_is_bool_has_no_issue() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int pick(int count, string name, bool on)\n    {\n        if (count > 0) {\n            count++;\n        } else if (name != \"\") {\n            count--;\n        }\n        while (count > 10 && on) {\n            count--;\n        }\n        do {\n            count++;\n        } while (count < 3 || !on);\n        for (let i = 3; i > 0; i--) {\n            count++;\n        }\n        while (true) {\n            break;\n        }\n        return on ? count : 0;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_condition_that_is_not_bool_is_an_invalid_operand_everywhere() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int pick(int count, string name, bool on, int n, string s)\n    {\n        if (count) {\n            count++;\n        } else if (name) {\n            count--;\n        }\n        while (count) {\n            count--;\n        }\n        do {\n            count++;\n        } while (count % 3);\n        for (let i = 3; i; i--) {\n            count += s ? 1 : 2;\n        }\n        const a = n && on;\n        const b = on || s;\n        const c = !n;\n        return a && b && c ? 1 : 0;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        [
            "7:13 invalid-operand",
            "9:20 invalid-operand",
            "12:16 invalid-operand",
            "17:18 invalid-operand",
            "19:22 invalid-operand",
            "18:25 invalid-operand",
            "21:19 invalid-operand",
            "22:25 invalid-operand",
            "23:20 invalid-operand",
        ]
    );
}

/// `check_slice` refuses `!entity is HasDesign` and writes `entity is not HasDesign`, so the analyzer adds no issue on
/// it, while `!` on any other value that is not `bool` keeps its report.
#[test]
fn not_before_is_adds_no_issue_and_not_of_a_value_that_is_not_bool_keeps_its_report() {
    let design = "<?php\n\nnamespace Demo;\n\ninterface HasDesign\n{\n}\n";
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool run(Any entity, int done)\n    {\n        if (!entity is HasDesign) {\n            return true;\n        }\n        if (!done) {\n            return false;\n        }\n        return true;\n    }\n}\n";

    let issues: Vec<String> =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[("src/Demo/HasDesign.php", design)])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect();

    assert_eq!(issues, ["10:14 invalid-operand `!` takes a `bool`, but this is `int`."]);
}

/// Spec section 19's flags: ints joined with `|`, tested with `&`, and `&` binds tighter than `!=` in PHP#. The `.php`
/// twin writes the parentheses PHP needs for the same meaning.
#[test]
fn the_bitwise_operators_and_their_compound_forms_take_ints_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int flags(int granted, int count, int n)\n    {\n        const READ = 1;\n        const WRITE = 2;\n        const DELETE = 4;\n        let permissions = granted | WRITE;\n        permissions |= DELETE;\n        if (permissions & WRITE != 0) {\n            permissions = permissions ^ READ;\n        }\n        if ((permissions & WRITE) != 0) {\n            permissions >>= 1;\n        }\n        const mask = 1 << count;\n        n %= 3;\n        n &= ~mask;\n        n ^= permissions >> 1;\n        n <<= 2;\n        return ~mask & permissions | n;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function flags(int $granted, int $count, int $n): int\n    {\n        $READ = 1;\n        $WRITE = 2;\n        $DELETE = 4;\n        $permissions = $granted | $WRITE;\n        $permissions |= $DELETE;\n        if (($permissions & $WRITE) != 0) {\n            $permissions = $permissions ^ $READ;\n        }\n        if (($permissions & $WRITE) != 0) {\n            $permissions >>= 1;\n        }\n        $mask = 1 << $count;\n        $n %= 3;\n        $n &= ~$mask;\n        $n ^= $permissions >> 1;\n        $n <<= 2;\n        return ~$mask & $permissions | $n;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{php_issues:?}");
}

/// A flag test is a comparison: the `int` that `&` gives is no condition, as spec section 21 makes every condition a
/// `bool`. PHP tests its truthiness.
#[test]
fn a_bitwise_and_as_a_condition_is_an_int_that_is_not_bool() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool can(int permissions)\n    {\n        const WRITE = 2;\n        if (permissions & WRITE) {\n            return true;\n        }\n        return false;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function can(int $permissions): bool\n    {\n        $WRITE = 2;\n        if ($permissions & $WRITE) {\n            return true;\n        }\n        return false;\n    }\n}\n";

    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[]),
        [
            "8:13 invalid-operand `if` takes a `bool`, but this is `int`. | Compare the value, as in `count > 0` or `name != \"\"`."
        ]
    );
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
}

/// `~` on an `int` gives an `int`, so `~0`, which is `-1`, never reads as `0`. PHP's analysis kept the operand's own
/// type, which made `~0 == 0` look always true in both dialects.
#[test]
fn bitwise_not_of_an_int_is_an_int_in_both_dialects() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int all()\n    {\n        const bits = ~0;\n        if (bits == 0) {\n            return 1;\n        }\n        return bits;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function all(): int\n    {\n        $bits = ~0;\n        if ($bits === 0) {\n            return 1;\n        }\n        return $bits;\n    }\n}\n";

    assert_eq!(messages(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
    assert_eq!(messages(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
}

/// Spec section 21 narrows no property, because it could change between the test and the use, so `order.total` stays
/// `Any?` after `is int`. The error says so and names the fix, a name bound by the test. PHP narrows the property, and
/// its error on an untested `mixed` operand keeps its wording.
#[test]
fn an_operand_read_from_a_tested_property_says_properties_are_not_narrowed() {
    let sharp = "namespace Demo;\n\npublic class Order\n{\n    public Any? total { get; set; }\n\n    public Order(Any? total)\n    {\n        this.total = total;\n    }\n\n    public static int next(Order order)\n    {\n        if (order.total is int) {\n            return order.total + 1;\n        }\n        if (order.total is int t) {\n            return t + 1;\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Order\n{\n    public function __construct(public mixed $total)\n    {\n    }\n\n    public static function next(Order $order): int\n    {\n        if (is_int($order->total)) {\n            return $order->total + 1;\n        }\n        return 0;\n    }\n}\n";

    assert_eq!(
        refusals(("src/Demo/Order.sharp", sharp), &[]),
        [
            "15:20 mixed-operand `order.total` is `Any?` here: a property is not narrowed, because it could change between the test and the use. | Copy the value into a local first: test it with a name, as in `if (order.total is int t)`, and use `t`.",
            "15:20 mixed-return-statement Could not infer a precise return type for method `Order.next`. Saw type `Any?`. | Add specific type hints to variables, parameters, or properties involved in calculating the return value. Consider adding a specific return type declaration to the method signature to catch potential mismatches earlier.",
        ]
    );
    assert_eq!(issues(("src/Demo/Order.php", php), &[]), Vec::<String>::new());

    let untested = "<?php\n\nnamespace Demo;\n\nfinal class Order\n{\n    public function __construct(public mixed $total)\n    {\n    }\n\n    public static function next(Order $order): void\n    {\n        echo $order->total + 1;\n        echo 1 + $order->total;\n    }\n}\n";
    assert_eq!(
        refusals(("src/Demo/Order.php", untested), &[]),
        [
            "13:14 mixed-operand Left operand in binary operation has type `mixed`. | Ensure the left operand has a known type (e.g., `int`, `float`, `string`) using type hints, assertions, or checks.",
            "13:14 mixed-argument The first value for `echo` is too general. | Add a specific type hint or assertion for this value.",
            "14:18 mixed-operand Right operand in binary operation has type `mixed`. | Ensure the right operand has a known type (e.g., `int`, `float`, `string`) using type hints, assertions, or checks.",
            "14:14 mixed-argument The first value for `echo` is too general. | Add a specific type hint or assertion for this value.",
        ]
    );
}

/// A condition that always holds names its PHP# type, as the condition error beside it does. PHP names its own type.
#[test]
fn a_condition_that_always_holds_names_its_php_sharp_type() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int can()\n    {\n        const WRITE = 2;\n        if (WRITE) {\n            return 1;\n        }\n        let ready = true;\n        if (ready) {\n            return 2;\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function can(): int\n    {\n        $WRITE = 2;\n        if ($WRITE) {\n            return 1;\n        }\n        $ready = true;\n        if ($ready) {\n            return 2;\n        }\n        return 0;\n    }\n}\n";

    assert_eq!(
        messages(("src/Demo/Report.sharp", sharp), &[]),
        [
            "This condition (type `2`) will always evaluate to `true`.",
            "`if` takes a `bool`, but this is `int`.",
            "This condition (type `true`) will always evaluate to `true`.",
        ]
    );
    assert_eq!(
        messages(("src/Demo/Report.php", php), &[]),
        [
            "This condition (type `int(2)`) will always evaluate to true.",
            "This condition (type `true`) will always evaluate to true.",
        ]
    );
}

/// A loop condition that never holds names its PHP# type, in the loop's report and in the report on the variable it
/// tests. PHP names its own type.
#[test]
fn a_loop_condition_that_never_holds_names_its_php_sharp_type() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int count()\n    {\n        const n = 0;\n        while (n) {\n            return 1;\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function count(): int\n    {\n        $n = 0;\n        while ($n) {\n            return 1;\n        }\n        return 0;\n    }\n}\n";
    let impossible = |analyzed| -> Vec<String> {
        worded(analyzed, &[]).into_iter().filter(|line| line.contains(" impossible-condition ")).collect()
    };

    assert_eq!(
        impossible(("src/Demo/Report.sharp", sharp)),
        [
            "8:16 impossible-condition Impossible condition: variable `n` (type `0`) will always evaluate to `false`. | This condition always evaluates to `false` | Variable `n` (type `0`) is never `true`, so this condition is always `false`. | Review the logic or type of the variable; this condition will never pass.",
            "8:16 impossible-condition This loop condition (type `0`) will always evaluate to `false`. | This condition is always `false`, the loop body will never execute | Check the logic of this loop condition. The loop body is unreachable.",
        ]
    );
    assert_eq!(
        impossible(("src/Demo/Report.php", php)),
        [
            "10:16 impossible-condition Impossible condition: variable `$n` (type `int(0)`) will always evaluate to false. | This condition always evaluates to false | Variable `$n` (type `int(0)`) is always falsy and can never satisfy a truthiness check. | Review the logic or type of the variable; this condition will never pass.",
            "10:16 impossible-condition This loop condition (type `int(0)`) will always evaluate to false. | This condition is always false, the loop body will never execute | Check the logic of this loop condition. The loop body is unreachable.",
        ]
    );
}

/// A condition that always or never holds speaks of the `bool` it is: always `true` or always `false`. PHP# has no
/// truthiness. PHP keeps its truthy and falsy wording.
#[test]
fn a_condition_that_always_or_never_holds_speaks_of_its_bool() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int never() { const off = false; if (off) { return 1; } return 0; }\n    public static int notNever() { const on = true; if (!on) { return 1; } return 0; }\n    public static int always() { const on = true; if (on) { return 1; } return 0; }\n    public static int notAlways() { const off = false; if (!off) { return 1; } return 0; }\n    public static int ternary() { const on = true; const off = false; return (on ? 1 : 2) + (off ? 1 : 2); }\n    public static bool logical(bool flag) { const on = true; const off = false; return (off && flag) || (on || flag); }\n    public static int loops() { const on = true; const off = false; while (off) { return 1; } while (!on) { return 2; } while (on) { return 3; } return 0; }\n    public static int negatedLoop() { const off = false; while (!off) { return 4; } return 0; }\n    public static bool otherwise(bool flag) { const off = false; return off || flag; }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function never(): int { $off = false; if ($off) { return 1; } return 0; }\n    public static function notNever(): int { $on = true; if (!$on) { return 1; } return 0; }\n    public static function always(): int { $on = true; if ($on) { return 1; } return 0; }\n    public static function notAlways(): int { $off = false; if (!$off) { return 1; } return 0; }\n    public static function ternary(): int { $on = true; $off = false; return ($on ? 1 : 2) + ($off ? 1 : 2); }\n    public static function logical(bool $flag): bool { $on = true; $off = false; return ($off && $flag) || ($on || $flag); }\n    public static function loops(): int { $on = true; $off = false; while ($off) { return 1; } while (!$on) { return 2; } while ($on) { return 3; } return 0; }\n    public static function negatedLoop(): int { $off = false; while (!$off) { return 4; } return 0; }\n    public static function otherwise(bool $flag): bool { $off = false; return $off || $flag; }\n}\n";

    assert_eq!(
        worded(("src/Demo/Report.php", php), &[]),
        [
            "7:61 impossible-condition This condition (type `false`) will always evaluate to false. | Expression of type `false` is always falsy | Because this condition is always false, the code block it controls will never be executed. | Check the logic of this expression. If the code block is intended to be unreachable, consider removing it. Otherwise, revise the condition.",
            "8:62 impossible-condition This condition (type `false`) will always evaluate to false. | Expression of type `false` is always falsy | Because this condition is always false, the code block it controls will never be executed. | Check the logic of this expression. If the code block is intended to be unreachable, consider removing it. Otherwise, revise the condition.",
            "9:60 redundant-condition This condition (type `true`) will always evaluate to true. | Expression of type `true` is always truthy | Because this condition is always true, the code block it controls will always execute if this part of the code is reached. | The explicit condition might be redundant. | Consider simplifying or removing the conditional check if the guarded code should always execute, or verify the expression's logic if a conditional check is truly needed.",
            "10:65 redundant-condition This condition (type `true`) will always evaluate to true. | Expression of type `true` is always truthy | Because this condition is always true, the code block it controls will always execute if this part of the code is reached. | The explicit condition might be redundant. | Consider simplifying or removing the conditional check if the guarded code should always execute, or verify the expression's logic if a conditional check is truly needed.",
            "11:79 redundant-condition Redundant ternary operator: condition is always truthy. | This condition (type `true`) is always truthy | This `then` branch is always evaluated, making it the result of the expression | This `else` branch will never be evaluated | The ternary operator `? :` evaluates the `else` branch only when the condition is falsy. | Consider replacing the entire expression with just this `then` branch.",
            "11:95 impossible-condition Redundant ternary operator: condition is always falsy. | This condition (type `false`) is always falsy | This `then` branch will never be evaluated | This `else` branch is always evaluated, making it the result of the expression | The ternary operator `? :` evaluates the `then` branch only when the condition is truthy. | Consider replacing the entire expression with just this `else` branch.",
            "12:90 redundant-logical-operation Redundant `&&` operation: left operand is always falsy and right operand is not evaluated. | Left operand is always falsy | Right operand is not evaluated | The `&&` operator will always return `false` in this case. | Consider simplifying this expression to `false`.",
            "12:109 redundant-logical-operation Redundant `||` operation: left operand is always true and right operand is not evaluated. | Left operand is always true | Right operand is not evaluated | The `||` operator will always return `true` in this case. | Consider simplifying this expression to `true`.",
            "12:89 redundant-logical-operation Redundant `||` operation: left operand is always falsy and right operand is always truthy. | Left operand is always falsy | Right operand is always truthy | The `||` operator will always return `true` in this case. | Consider simplifying this expression to `true`.",
            "13:76 impossible-condition Impossible condition: variable `$off` (type `false`) will always evaluate to false. | This condition always evaluates to false | Variable `$off` (type `false`) is always falsy and can never satisfy a truthiness check. | Review the logic or type of the variable; this condition will never pass.",
            "13:76 impossible-condition This loop condition (type `false`) will always evaluate to false. | This condition is always false, the loop body will never execute | Check the logic of this loop condition. The loop body is unreachable.",
            "13:103 impossible-condition Impossible condition: variable `$on` (type `true`) will always evaluate to false. | This condition always evaluates to false | Variable `$on` (type `true`) is always truthy, so asserting it is falsy will always be false. | Review the logic or type of the variable; this condition will never pass.",
            "13:103 impossible-condition This loop condition (type `false`) will always evaluate to false. | This condition is always false, the loop body will never execute | Check the logic of this loop condition. The loop body is unreachable.",
            "13:130 redundant-condition Redundant condition: variable `$on` (type `true`) will always evaluate to true. | This condition always evaluates to true | Variable `$on` (type `true`) is always truthy. This condition is redundant and the code block will always execute if reached. | Simplify or remove the redundant condition if the guarded code should always run.",
            "14:70 redundant-condition Redundant condition: variable `$off` (type `false`) will always evaluate to true. | This condition always evaluates to true | Variable `$off` (type `false`) is always falsy, so asserting it's falsy is always true and redundant. | Simplify or remove the redundant condition if the guarded code should always run.",
            "15:79 redundant-condition Redundant condition: variable `$off` (type `false`) will always evaluate to true. | This condition always evaluates to true | Variable `$off` (type `false`) is always falsy, so asserting it's falsy is always true and redundant. | Simplify or remove the redundant condition if the guarded code should always run.",
            "15:79 redundant-logical-operation Redundant `||` operation: left operand is always false and right operand is evaluated. | Left operand is always false | Right operand is evaluated | The `||` operator will always return the boolean value of the right-hand side in this case. | Consider simplifying this expression to just the right operand.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Report.sharp", sharp), &[]),
        [
            "5:56 impossible-condition This condition (type `false`) will always evaluate to `false`. | Expression of type `false` is always `false` | Because this condition is always `false`, the code block it controls will never be executed. | Check the logic of this expression. If the code block is intended to be unreachable, consider removing it. Otherwise, revise the condition.",
            "6:57 impossible-condition This condition (type `false`) will always evaluate to `false`. | Expression of type `false` is always `false` | Because this condition is always `false`, the code block it controls will never be executed. | Check the logic of this expression. If the code block is intended to be unreachable, consider removing it. Otherwise, revise the condition.",
            "7:55 redundant-condition This condition (type `true`) will always evaluate to `true`. | Expression of type `true` is always `true` | Because this condition is always `true`, the code block it controls will always execute if this part of the code is reached. | The explicit condition might be redundant. | Consider simplifying or removing the conditional check if the guarded code should always execute, or verify the expression's logic if a conditional check is truly needed.",
            "8:60 redundant-condition This condition (type `true`) will always evaluate to `true`. | Expression of type `true` is always `true` | Because this condition is always `true`, the code block it controls will always execute if this part of the code is reached. | The explicit condition might be redundant. | Consider simplifying or removing the conditional check if the guarded code should always execute, or verify the expression's logic if a conditional check is truly needed.",
            "9:79 redundant-condition Redundant ternary operator: condition is always `true`. | This condition (type `true`) is always `true` | This `then` branch is always evaluated, making it the result of the expression | This `else` branch will never be evaluated | The ternary operator `? :` evaluates the `else` branch only when the condition is `false`. | Consider replacing the entire expression with just this `then` branch.",
            "9:94 impossible-condition Redundant ternary operator: condition is always `false`. | This condition (type `false`) is always `false` | This `then` branch will never be evaluated | This `else` branch is always evaluated, making it the result of the expression | The ternary operator `? :` evaluates the `then` branch only when the condition is `true`. | Consider replacing the entire expression with just this `else` branch.",
            "10:89 redundant-logical-operation Redundant `&&` operation: left operand is always `false` and right operand is not evaluated. | Left operand is always `false` | Right operand is not evaluated | The `&&` operator will always return `false` in this case. | Consider simplifying this expression to `false`.",
            "10:106 redundant-logical-operation Redundant `||` operation: left operand is always `true` and right operand is not evaluated. | Left operand is always `true` | Right operand is not evaluated | The `||` operator will always return `true` in this case. | Consider simplifying this expression to `true`.",
            "10:88 redundant-logical-operation Redundant `||` operation: left operand is always `false` and right operand is always `true`. | Left operand is always `false` | Right operand is always `true` | The `||` operator will always return `true` in this case. | Consider simplifying this expression to `true`.",
            "11:76 impossible-condition Impossible condition: variable `off` (type `false`) will always evaluate to `false`. | This condition always evaluates to `false` | Variable `off` (type `false`) is never `true`, so this condition is always `false`. | Review the logic or type of the variable; this condition will never pass.",
            "11:76 impossible-condition This loop condition (type `false`) will always evaluate to `false`. | This condition is always `false`, the loop body will never execute | Check the logic of this loop condition. The loop body is unreachable.",
            "11:102 impossible-condition Impossible condition: variable `on` (type `true`) will always evaluate to `false`. | This condition always evaluates to `false` | Variable `on` (type `true`) is never `false`, so this condition is always `false`. | Review the logic or type of the variable; this condition will never pass.",
            "11:102 impossible-condition This loop condition (type `false`) will always evaluate to `false`. | This condition is always `false`, the loop body will never execute | Check the logic of this loop condition. The loop body is unreachable.",
            "11:128 redundant-condition Redundant condition: variable `on` (type `true`) will always evaluate to `true`. | This condition always evaluates to `true` | Variable `on` (type `true`) is never `false`, so this condition is always `true`. | Simplify or remove the redundant condition if the guarded code should always run.",
            "12:65 redundant-condition Redundant condition: variable `off` (type `false`) will always evaluate to `true`. | This condition always evaluates to `true` | Variable `off` (type `false`) is never `true`, so this condition is always `true`. | Simplify or remove the redundant condition if the guarded code should always run.",
            "13:73 redundant-condition Redundant condition: variable `off` (type `false`) will always evaluate to `true`. | This condition always evaluates to `true` | Variable `off` (type `false`) is never `true`, so this condition is always `true`. | Simplify or remove the redundant condition if the guarded code should always run.",
            "13:73 redundant-logical-operation Redundant `||` operation: left operand is always `false` and right operand is evaluated. | Left operand is always `false` | Right operand is evaluated | The `||` operator will always return the boolean value of the right-hand side in this case. | Consider simplifying this expression to just the right operand.",
        ]
    );
}

/// A null check or a type check that always or never holds speaks of the `bool` it is: always `true` or always
/// `false`. PHP keeps its own wording.
#[test]
fn a_null_or_type_check_that_always_or_never_holds_speaks_of_its_bool() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int isNull(int x) { while (x == null) { return 1; } return 0; }\n    public static int notNull(int x) { while (x != null) { return 1; } return 0; }\n    public static int known(int? x) { if (x == null) { while (x != null) { return 1; } } return 0; }\n    public static int other() { const on = true; while (on == false) { return 1; } return 0; }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function isNull(int $x): int { while ($x === null) { return 1; } return 0; }\n    public static function notNull(int $x): int { while ($x !== null) { return 1; } return 0; }\n    public static function known(?int $x): int { if ($x === null) { while ($x !== null) { return 1; } } return 0; }\n    public static function other(): int { $on = true; while ($on === false) { return 1; } return 0; }\n}\n";
    let checks = |analyzed| -> Vec<String> {
        worded(analyzed, &[]).into_iter().filter(|line| line.contains("-type-comparison ")).collect()
    };

    assert_eq!(
        checks(("src/Demo/Report.php", php)),
        [
            "7:57 impossible-null-type-comparison Impossible condition: variable `$x` (type `int`) can never be `null`. | This condition always evaluates to false | Variable `$x` (type `int`) does not include `null`. | The condition checking if `$x` is `null` will always be false. Remove or refactor the condition.",
            "8:58 impossible-null-type-comparison Impossible condition: variable `$x` (type `int`) will always be `null`. | This condition always evaluates to false | Variable `$x` (type `int`) is already known to be `null`, so asserting it's not `null` is impossible. | The condition checking if `$x` is not `null` will always be false. Review the variable's state or condition.",
            "9:76 redundant-type-comparison Redundant condition: variable `$x` (type `null`) is already known to be `null`. | This condition always evaluates to true | The type of variable `$x` (type `null`) already satisfies the condition that it is `null`. This check is redundant. | This condition is always true and the associated code block will always execute if reached. Consider simplifying.",
            "10:62 impossible-type-comparison Impossible condition: variable `$on` (type `true`) can never be `false`. | This condition always evaluates to false | The type of variable `$on` (type `true`) is incompatible with the assertion that it is `false`. | This condition is impossible and the associated code block will never execute. Review the types and condition logic.",
        ]
    );
    assert_eq!(
        checks(("src/Demo/Report.sharp", sharp)),
        [
            "5:46 impossible-null-type-comparison Impossible condition: variable `x` (type `int`) can never be `null`. | This condition always evaluates to `false` | Variable `x` (type `int`) does not include `null`. | The condition checking if `x` is `null` will always be `false`. Remove or refactor the condition.",
            "6:47 impossible-null-type-comparison Impossible condition: variable `x` (type `int`) will always be `null`. | This condition always evaluates to `false` | Variable `x` (type `int`) is already known to be `null`, so asserting it's not `null` is impossible. | The condition checking if `x` is not `null` will always be `false`. Review the variable's state or condition.",
            "7:63 redundant-type-comparison Redundant condition: variable `x` (type `null`) is already known to be `null`. | This condition always evaluates to `true` | The type of variable `x` (type `null`) already satisfies the condition that it is `null`. This check is redundant. | This condition is always `true` and the associated code block will always execute if reached. Consider simplifying.",
            "8:57 impossible-type-comparison Impossible condition: variable `on` (type `true`) can never be `false`. | This condition always evaluates to `false` | The type of variable `on` (type `true`) is incompatible with the assertion that it is `false`. | This condition is impossible and the associated code block will never execute. Review the types and condition logic.",
        ]
    );
}

/// A comparison that always or never holds writes `true` and `false` as PHP# writes them. A `bool` has no order, so
/// PHP# refuses an ordering of `false` before any PHP rule for `false` is named. PHP keeps its own wording.
#[test]
fn a_comparison_that_always_or_never_holds_speaks_of_its_bool() {
    let store = (
        "src/Lib/Store.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Store\n{\n    public static function off(): false { return false; }\n}\n",
    );
    let sharp = "namespace Demo;\n\nimport Lib.Store;\n\nclass Report\n{\n    public static bool same() { const a = 1; return a != 1; }\n    public static bool other() { const a = 1; return a != 2; }\n    public static bool less() { return Store.off() < 1; }\n    public static int order() { return Store.off() <=> 1; }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Store;\n\nclass Report\n{\n    public static function same(): bool { $a = 1; return $a != 1; }\n    public static function other(): bool { $a = 1; return $a != 2; }\n    public static function less(): bool { return Store::off() < 1; }\n    public static function order(): int { return Store::off() <=> 1; }\n}\n";
    let comparisons = |analyzed| -> Vec<String> {
        worded(analyzed, &[store])
            .into_iter()
            .filter(|line| {
                line.contains(" redundant-comparison ")
                    || line.contains(" false-operand ")
                    || line.contains(" invalid-operand ")
            })
            .collect()
    };

    assert_eq!(
        comparisons(("src/Demo/Report.php", php)),
        [
            "9:58 redundant-comparison Redundant `!=` comparison: left-hand side is never equal to (always false for !=) right-hand side. | Left operand is `int(1)` | Right operand is `int(1)` | The `!=` operator will always return `false` in this case. | Consider simplifying or removing this comparison as it always evaluates to `false`.",
            "10:59 redundant-comparison Redundant `!=` comparison: left-hand side is always not equal to (always true for !=) right-hand side. | Left operand is `int(1)` | Right operand is `int(2)` | The `!=` operator will always return `true` in this case. | Consider simplifying or removing this comparison as it always evaluates to `true`.",
            "11:50 false-operand Left operand in `<` comparison is `false`. | This is `false` | PHP compares `false` with other types according to specific rules (e.g., `false == 0` is true using `<`). This can hide bugs. | Ensure this operand is not `false` or explicitly handle the `false` case if it represents a distinct state (e.g., an error from a function).",
            "11:50 redundant-comparison Redundant `<` comparison: left-hand side is always less than right-hand side. | Left operand is `false` | Right operand is `int(1)` | The `<` operator will always return `true` in this case. | Consider simplifying or removing this comparison as it always evaluates to `true`.",
            "12:50 false-operand Left operand in spaceship comparison (`<=>`) is `false`. | This is `false` | PHP compares `false` with other types according to specific rules (e.g., `false == 0` is true, `false < 1` is true). | Ensure this comparison with `false` is intended, or provide a non-false operand.",
        ]
    );
    assert_eq!(
        comparisons(("src/Demo/Report.sharp", sharp)),
        [
            "7:53 redundant-comparison Redundant `!=` comparison: left-hand side is never equal to (always `false` for !=) right-hand side. | Left operand is `1` | Right operand is `1` | The `!=` operator will always return `false` in this case. | Consider simplifying or removing this comparison as it always evaluates to `false`.",
            "8:54 redundant-comparison Redundant `!=` comparison: left-hand side is always not equal to (always `true` for !=) right-hand side. | Left operand is `1` | Right operand is `2` | The `!=` operator will always return `true` in this case. | Consider simplifying or removing this comparison as it always evaluates to `true`.",
            "9:40 invalid-operand `<` orders numbers, strings and classes that declare `operator <=>`, and `bool` is none of them. | This is `bool`. | This is `int`. | A `bool`, an enum case and a collection have no order. | Order a number or a string taken from the value instead.",
            "10:40 invalid-operand `<=>` orders numbers, strings and classes that declare `operator <=>`, and `bool` is none of them. | This is `bool`. | This is `int`. | A `bool`, an enum case and a collection have no order. | Order a number or a string taken from the value instead.",
        ]
    );
}

/// A `match` arm that never or always matches writes the `bool` of its condition as PHP# writes it. PHP keeps its own
/// wording.
#[test]
fn a_match_arm_that_never_or_always_matches_speaks_of_its_bool() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string twice(int x) => match (x) { 1 => \"one\", 1 => \"again\", default => \"other\" };\n    public static string always() => match (true) { true => \"yes\", default => \"no\" };\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function twice(int $x): string { return match ($x) { 1 => 'one', 1 => 'again', default => 'other' }; }\n    public static function always(): string { return match (true) { true => 'yes', default => 'no' }; }\n}\n";
    let arms = |analyzed| -> Vec<String> {
        worded(analyzed, &[])
            .into_iter()
            .filter(|line| line.contains(" unreachable-match-arm ") || line.contains(" match-arm-always-true "))
            .collect()
    };

    assert_eq!(
        arms(("src/Demo/Report.php", php)),
        [
            "7:84 unreachable-match-arm This match arm is unreachable. | This arm can never be reached | In this match expression | The condition is always false in this context.",
            "8:69 match-arm-always-true This match arm is always true, making subsequent arms unreachable. | This arm covers all remaining cases for the subject | In this match expression | Any arms after this one can never be reached.",
        ]
    );
    assert_eq!(
        arms(("src/Demo/Report.sharp", sharp)),
        [
            "5:66 unreachable-match-arm This match arm is unreachable. | This arm can never be reached | In this match expression | The condition is always `false` in this context.",
            "6:53 match-arm-always-true This match arm is always `true`, making subsequent arms unreachable. | This arm covers all remaining cases for the subject | In this match expression | Any arms after this one can never be reached.",
        ]
    );
}

/// A `??` on a key the shape never holds writes the `bool` of the check as PHP# writes it. PHP keeps its own wording.
#[test]
fn a_fallback_on_a_missing_key_speaks_of_its_bool() {
    let store = (
        "src/Lib/Store.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Store\n{\n    /** @return array{a: int} */\n    public static function shape(): array { return ['a' => 1]; }\n}\n",
    );
    let sharp = "namespace Demo;\n\nimport Lib.Store;\n\nclass Report\n{\n    public static int missing() { const m = Store.shape(); return m[\"b\"] ?? 0; }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Store;\n\nclass Report\n{\n    public static function missing(): int { $m = Store::shape(); return $m['b'] ?? 0; }\n}\n";
    let checks = |analyzed| -> Vec<String> {
        worded(analyzed, &[store])
            .into_iter()
            .filter(|line| line.contains(" impossible-nonnull-entry-check "))
            .collect()
    };

    assert_eq!(
        checks(("src/Demo/Report.php", php)),
        [
            "9:76 impossible-nonnull-entry-check Impossible `isset` check on key `'b'` accessed on `array{'a': int}`. | `isset` on key `'b'` will always be false here. | The analysis determined that the key `'b'` definitely does not exist in this array, so checking `isset` is unnecessary. | Remove the redundant `isset` check.",
        ]
    );
    assert_eq!(
        checks(("src/Demo/Report.sharp", sharp)),
        [
            "7:69 impossible-nonnull-entry-check Impossible `isset` check on key `'b'` accessed on `Map<\"a\", int>`. | `isset` on key `'b'` will always be `false` here. | The analysis determined that the key `'b'` definitely does not exist in this array, so checking `isset` is unnecessary. | Remove the redundant `isset` check.",
        ]
    );
}

/// A method that may return `false` from a PHP function writes `false` as PHP# writes it, and asks for no `int|false`
/// return type, which PHP# cannot write. PHP keeps its own wording. `a_message_names_an_accessor_as_sharp_writes_it`
/// pins the same report on an accessor.
#[test]
fn a_return_that_may_be_false_speaks_of_its_bool() {
    let sharp =
        "namespace Demo;\n\nclass Report\n{\n    public static int position() { return strpos(\"ab\", \"b\"); }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function position(): int { return strpos('ab', 'b'); }\n}\n";
    let returns = |analyzed| -> Vec<String> {
        worded(analyzed, &[]).into_iter().filter(|line| line.contains(" falsable-return-statement ")).collect()
    };

    assert_eq!(
        returns(("src/Demo/Report.php", php)),
        [
            "7:53 falsable-return-statement Function `Demo\\Report::position` is declared to return `int` but possibly returns 'false' (inferred as `false|non-negative-int`). | Potentially 'false' returned here. | Return type declared as non-falsable `int` here | The declared return type does not permit 'false', but the analysis indicates that 'false' or a falsable type could be returned from this path. | You can either change the return type declaration of `Demo\\Report::position` to include 'false' (e.g., 'int|false'), or ensure that this function path never returns 'false'.",
        ]
    );
    assert_eq!(
        returns(("src/Demo/Report.sharp", sharp)),
        [
            "5:43 falsable-return-statement Method `Report.position` is declared to return `int` but possibly returns `false` (inferred as `bool|int`). | Potentially `false` returned here. | Return type declared as non-falsable `int` here | The declared return type does not permit `false`, but this path could return `false`. | Ensure this method path never returns `false`.",
        ]
    );
}

/// A `null` operand of arithmetic, from a PHP method, asks for a number and names no cast, since PHP# casts only
/// between numbers. PHP keeps its own wording.
#[test]
fn a_null_operand_of_arithmetic_asks_for_a_number() {
    let store = (
        "src/Lib/Store.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Store\n{\n    public static function nothing(): null { return null; }\n}\n",
    );
    let sharp = "namespace Demo;\n\nimport Lib.Store;\n\nclass Report\n{\n    public static int left() => Store.nothing() - 1;\n\n    public static int right() => 1 - Store.nothing();\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Store;\n\nclass Report\n{\n    public static function left(): int { return Store::nothing() - 1; }\n\n    public static function right(): int { return 1 - Store::nothing(); }\n}\n";
    let operands = |analyzed| -> Vec<String> {
        worded(analyzed, &[store]).into_iter().filter(|line| line.contains(" null-operand ")).collect()
    };

    assert_eq!(
        operands(("src/Demo/Report.php", php)),
        [
            "9:49 null-operand Left operand in arithmetic operation cannot be `null`. | This is `null`. | Performing arithmetic operations on `null` typically results in `0`. | Ensure the left operand is a number (int/float) or a type that can be cast to a number.",
            "11:54 null-operand Right operand in arithmetic operation cannot be `null`. | This is `null`. | Performing arithmetic operations on `null` typically results in `0`. | Ensure the right operand is a number (int/float) or a type that can be cast to a number.",
        ]
    );
    assert_eq!(
        operands(("src/Demo/Report.sharp", sharp)),
        [
            "7:33 null-operand Left operand in arithmetic operation cannot be `null`. | This is `null`. | Performing arithmetic operations on `null` typically results in `0`. | Ensure the left operand is a number (int/float).",
            "9:38 null-operand Right operand in arithmetic operation cannot be `null`. | This is `null`. | Performing arithmetic operations on `null` typically results in `0`. | Ensure the right operand is a number (int/float).",
        ]
    );
}

/// A literal keyed by a value that cannot key a `Map` names the `Map` key rule, as a `Map` type that breaks it does,
/// and names no cast, since PHP# casts only between numbers. PHP keeps its own wording.
#[test]
fn a_literal_keyed_by_a_value_that_cannot_key_a_map_names_the_map_key_rule() {
    let store = (
        "src/Lib/Store.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Store\n{\n    public static function object(): object { return new \\stdClass(); }\n}\n",
    );
    let sharp = "namespace Demo;\n\nimport Lib.Store;\n\nclass Report\n{\n    public static int count() { const m = [Store.object(): 1]; return m.count(); }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Store;\n\nclass Report\n{\n    public static function count(): int { $m = [Store::object() => 1]; return count($m); }\n}\n";
    let keys = |analyzed| -> Vec<String> {
        worded(analyzed, &[store]).into_iter().filter(|line| line.contains(" invalid-array-element-key ")).collect()
    };

    assert_eq!(
        keys(("src/Demo/Report.php", php)),
        [
            "9:49 invalid-array-element-key Invalid array key type. | This has type `object`, which cannot be cast to a string or integer. | In PHP, array keys must be strings or integers. While types like `bool` or `float` are automatically cast, a value of type `object` cannot be. | Ensure the array key is either a string or an integer.",
        ]
    );
    assert_eq!(
        keys(("src/Demo/Report.sharp", sharp)),
        [
            "7:44 invalid-array-element-key A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value, and `Object` has none. | `Object` keys this `Map`. | Key the `Map` by an `int`, a `string` or a backed enum, such as `enum Status : string`.",
        ]
    );
}

/// An operand that may be `false`, from a PHP method, asks for a check of `false`. PHP# has no falsiness, and keeps
/// `(int)` between numbers, so no cast turns `false` into one. PHP keeps its own wording.
#[test]
fn an_operand_that_may_be_false_asks_for_a_check_of_false() {
    let store = (
        "src/Lib/Store.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Store\n{\n    public static function count(): int|false { return 1; }\n}\n",
    );
    let sharp = "namespace Demo;\n\nimport Lib.Store;\n\nclass Report\n{\n    public static int add() { return Store.count() + 1; }\n    public static int addRight() { return 1 + Store.count(); }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Store;\n\nclass Report\n{\n    public static function add(): int { return Store::count() + 1; }\n    public static function addRight(): int { return 1 + Store::count(); }\n}\n";
    let possibly_false = |analyzed| -> Vec<String> {
        worded(analyzed, &[store]).into_iter().filter(|line| line.contains(" possibly-false-operand ")).collect()
    };

    assert_eq!(
        possibly_false(("src/Demo/Report.sharp", sharp)),
        [
            "7:38 possibly-false-operand Left operand in arithmetic operation might be `false` (type `false|int`). | This might be `false`. | Performing arithmetic operations on `false` typically results in `0`. | Ensure the left operand is not `false` before the operation.",
            "8:47 possibly-false-operand Right operand in arithmetic operation might be `false` (type `false|int`). | This might be `false`. | Performing arithmetic operations on `false` typically results in `0`. | Ensure the right operand is not `false` before the operation.",
        ]
    );
    assert_eq!(
        possibly_false(("src/Demo/Report.php", php)),
        [
            "9:48 possibly-false-operand Left operand in arithmetic operation might be `false` (type `false|int`). | This might be `false`. | Performing arithmetic operations on `false` typically results in `0`. | Ensure the left operand is non-falsy before the operation, or explicitly cast if coercion is intended.",
            "10:57 possibly-false-operand Right operand in arithmetic operation might be `false` (type `false|int`). | This might be `false`. | Performing arithmetic operations on `false` typically results in `0`. | Ensure the right operand is non-falsy before the operation, or explicitly cast if coercion is intended.",
        ]
    );
}

/// A loop over a value that may be `false`, from a PHP method, asks for a check of `false` before the loop. PHP# has
/// no truthiness. PHP keeps its own wording.
#[test]
fn a_loop_over_a_value_that_may_be_false_asks_for_a_check_of_false() {
    let store = (
        "src/Lib/Store.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Store\n{\n    /** @return list<int>|false */\n    public static function ids(): array|false { return [1]; }\n}\n",
    );
    let sharp = "namespace Demo;\n\nimport Lib.Store;\n\nclass Report\n{\n    public static int total() { let sum = 0; for (const id of Store.ids()) { sum += id; } return sum; }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Store;\n\nclass Report\n{\n    public static function total(): int { $sum = 0; foreach (Store::ids() as $id) { $sum += $id; } return $sum; }\n}\n";
    let possibly_false = |analyzed| -> Vec<String> {
        worded(analyzed, &[store]).into_iter().filter(|line| line.contains(" possibly-false-iterator ")).collect()
    };

    assert_eq!(
        possibly_false(("src/Demo/Report.sharp", sharp)),
        [
            "7:63 possibly-false-iterator Expression being iterated (type `false|List<int>`) might be `false` at runtime. | This might be `false` | This loop might not be executed | If this expression is `false`, it will be treated as an empty list, and the loop body will not execute. | Consider checking for `false` before the loop if this is not intended.",
        ]
    );
    assert_eq!(
        possibly_false(("src/Demo/Report.php", php)),
        [
            "9:62 possibly-false-iterator Expression being iterated (type `false|list<int>`) might be `false` at runtime. | This might be `false` | This `foreach` might not be executed | If this expression is `false`, it will be treated as an empty array, and the loop body will not execute. | Consider checking for `false` or truthiness before the loop if this is not intended.",
        ]
    );
}

/// A PHP method's `@psalm-assert non-empty-mixed` is named by the type its docblock writes. PHP# has no name for it,
/// and `Any` would drop the non-empty that makes the assertion hold. PHP keeps Mago's `truthy-mixed`.
#[test]
fn an_assertion_of_non_empty_mixed_names_the_type_its_docblock_writes() {
    let check = (
        "src/Lib/Check.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Check\n{\n    /**\n     * @psalm-pure\n     *\n     * @psalm-assert non-empty-mixed $value\n     */\n    public static function filled(mixed $value): void {}\n}\n",
    );
    let sharp = "namespace Demo;\n\nimport Lib.Check;\n\nclass Report\n{\n    public static void run() { const one = 1; Check.filled(one); }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Check;\n\nclass Report\n{\n    public static function run(): void { $one = 1; Check::filled($one); }\n}\n";
    let redundant = |analyzed| -> Vec<String> {
        worded(analyzed, &[check]).into_iter().filter(|line| line.contains(" redundant-type-comparison ")).collect()
    };

    assert_eq!(
        redundant(("src/Demo/Report.sharp", sharp)),
        [
            "7:47 redundant-type-comparison Redundant type assertion: `one` is already `1`. | Argument `one` already has type `1` | The assertion against `non-empty-mixed` always holds because `one` is `1`. | Consider removing this assertion or replacing it with `default` if used in a `match` arm.",
        ]
    );
    assert_eq!(
        redundant(("src/Demo/Report.php", php)),
        [
            "9:52 redundant-type-comparison Redundant type assertion: `$one` is already `int(1)`. | Argument `$one` already has type `int(1)` | The assertion against `truthy-mixed` always holds because `$one` is `int(1)`. | Consider removing this assertion or replacing it with `default` if used in a `match` arm.",
        ]
    );
}

/// A condition that contradicts or repeats an earlier one names its variables and types as PHP# writes them. The
/// earlier condition reads as the `||` it is. PHP keeps its own wording.
#[test]
fn a_paradoxical_or_repeated_condition_names_its_php_sharp_types() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int pick(int? count, bool done)\n    {\n        if (count == 2 || done) {\n            if (count != 2 && !done) {\n                return 1;\n            }\n        }\n        if (done) {\n            return 2;\n        } else if (done) {\n            return 3;\n        }\n        if (!done && done) {\n            return 4;\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function pick(?int $count, bool $done): int\n    {\n        if ($count === 2 || $done) {\n            if ($count !== 2 && !$done) {\n                return 1;\n            }\n        }\n        if ($done) {\n            return 2;\n        } else if ($done) {\n            return 3;\n        }\n        if (!$done && $done) {\n            return 4;\n        }\n        return 0;\n    }\n}\n";
    let conditions = |analyzed| -> Vec<String> {
        worded(analyzed, &[])
            .into_iter()
            .filter(|line| line.contains(" paradoxical-condition ") || line.contains(" redundant-condition Redundant "))
            .collect()
    };

    assert_eq!(
        conditions(("src/Demo/Report.sharp", sharp)),
        [
            "8:17 paradoxical-condition Paradoxical condition | This condition (`!done && count is not 2`) can never be `true` here | Because of this preceding condition... | ...the analyzer knows that `count is 2 || done` must be `true` for this code path to be taken. | Therefore, this new condition (`!done && count is not 2`) directly contradicts that established fact. | As a result, the code this condition guards is unreachable. | Remove the unreachable code or refactor the conditional logic.",
            "17:13 redundant-condition Redundant condition | This condition (`!done`) is always `true` here | This was already established as `true` by a previous condition here | The analyzer determined this condition is guaranteed to be `true` based on preceding logic, making this check unnecessary. | Consider removing this redundant conditional check to simplify the code.",
        ]
    );
    assert_eq!(
        conditions(("src/Demo/Report.php", php)),
        [
            "10:17 paradoxical-condition Paradoxical condition | This condition (`!$done && $count is not int(2)`) can never be true here | Because of this preceding condition... | ...the analyzer knows that `$count is int(2) && $done` must be true for this code path to be taken. | Therefore, this new condition (`!$done && $count is not int(2)`) directly contradicts that established fact. | As a result, the code this condition guards is unreachable. | Remove the unreachable code or refactor the conditional logic.",
            "19:13 redundant-condition Redundant condition | This condition (`!$done`) is always true here | This was already established as true by a previous condition here | The analyzer determined this condition is guaranteed to be true based on preceding logic, making this check unnecessary. | Consider removing this redundant conditional check to simplify the code.",
        ]
    );
}

/// `|`, `&` and `^` on two `bool`s name the operator that joins them, and the rest of the code reads the `bool` it
/// meant. A compound form is named as written. PHP turns both `bool`s into ints.
#[test]
fn a_bitwise_operator_on_two_bools_names_the_bool_operator_to_write() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool show(bool isAdmin, bool isOwner)\n    {\n        const either = isAdmin | isOwner;\n        const both = isAdmin & isOwner;\n        const one = isAdmin ^ isOwner;\n        isAdmin |= isOwner;\n        return either && both && one && isAdmin;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function show(bool $isAdmin, bool $isOwner): bool\n    {\n        $either = $isAdmin | $isOwner;\n        $both = $isAdmin & $isOwner;\n        $one = $isAdmin ^ $isOwner;\n        $isAdmin |= $isOwner;\n        return $either && $both && $one && $isAdmin;\n    }\n}\n";
    let help = "`||`, `&&` and `!=` join two `bool` values.";

    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[]),
        [
            format!(
                "7:24 invalid-operand `|` takes `int`, but both sides are `bool`: write `||` for two `bool` values. | {help}"
            ),
            format!(
                "8:22 invalid-operand `&` takes `int`, but both sides are `bool`: write `&&` for two `bool` values. | {help}"
            ),
            format!(
                "9:21 invalid-operand `^` takes `int`, but both sides are `bool`: write `!=` for two `bool` values. | {help}"
            ),
            format!(
                "10:9 invalid-operand `|=` takes `int`, but both sides are `bool`: write `||` for two `bool` values. | {help}"
            ),
        ]
    );
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
}

/// Any other operand that is not an `int` is refused by its PHP# type, a nullable `int` included. PHP turns a `float`
/// into an int silently, refuses the `string`, warns on the nullable `int`, and reads its unchecked results as
/// `mixed`.
#[test]
fn a_bitwise_operator_on_a_value_that_is_not_an_int_is_refused_by_its_type() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int pack(float ratio, string name, int? count)\n    {\n        const a = ratio | 1;\n        const b = name & 1;\n        const c = count << 1;\n        const d = ~ratio;\n        return a + b + c + d;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function pack(float $ratio, string $name, ?int $count): int\n    {\n        $a = $ratio | 1;\n        $b = $name & 1;\n        $c = $count << 1;\n        $d = ~$ratio;\n        return $a + $b + $c + $d;\n    }\n}\n";
    let convert = "Use an `int`: `(int)` converts a `float`, and `Int.parse` reads a `string`.";

    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[]),
        [
            format!("7:19 invalid-operand `|` takes `int`, but this is `float`. | {convert}"),
            format!("8:19 invalid-operand `&` takes `int`, but this is `string`. | {convert}"),
            "9:19 invalid-operand `<<` takes `int`, but this is `int?`. | Test it with `!= null` first.".to_string(),
            format!("10:20 invalid-operand `~` takes `int`, but this is `float`. | {convert}"),
        ]
    );
    assert_eq!(
        issues(("src/Demo/Report.php", php), &[]),
        [
            "10:14 invalid-operand",
            "10:9 mixed-assignment",
            "11:14 possibly-null-operand",
            "13:21 mixed-operand",
            "13:16 mixed-operand",
            "13:16 mixed-operand",
            "13:16 mixed-return-statement",
        ]
    );
}

#[test]
fn casts_between_numbers_have_the_types_they_have_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string cents(float price, int count)\n    {\n        const cents = (int)(price * 100);\n        const share = (float)count / 3;\n        return (string)cents + (string)share + (string)(int)share;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function cents(float $price, int $count): string\n    {\n        $cents = (int)($price * 100);\n        $share = (float)$count / 3;\n        return (string)$cents . (string)$share . (string)(int)$share;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_cast_of_a_value_that_is_not_a_number_is_an_invalid_operand() {
    let source = "<?php\n\nnamespace Lib;\n\nfinal class Source\n{\n    public static function value(): mixed\n    {\n        return 1;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Source;\n\nclass Report\n{\n    public static string pick(string name, bool flag, int? maybe)\n    {\n        const a = (int)name;\n        const b = (float)flag;\n        const c = (string)name;\n        const d = (int)maybe;\n        const e = (string)Source.value();\n        return `${a}${b}${c}${d}${e}`;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Source.php", source)]),
        [
            "9:19 invalid-operand",
            "10:19 invalid-operand",
            "11:19 invalid-operand",
            "12:19 invalid-operand",
            "13:19 invalid-operand",
        ]
    );
}

/// In a `.sharp` file, an `exit` argument that can hold a string gets the `die` message, because `exit("…")` prints the
/// message and exits with status 0, which reports success. A `.php` file still takes it.
#[test]
fn exit_with_a_value_that_is_not_an_int_names_the_message_to_write_and_exit_1() {
    let any = "<?php\n\nnamespace Lib;\n\nfinal class Any\n{\n    public static function value(): mixed\n    {\n        return 1;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Any;\n\nclass Shutdown\n{\n    public void reason(string reason)\n    {\n        exit(reason);\n    }\n\n    public void either(int|string status)\n    {\n        exit(status);\n    }\n\n    public void any()\n    {\n        exit(Any.value());\n    }\n\n    public void named(string reason)\n    {\n        exit(status: reason);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Shutdown\n{\n    public function reason(string $reason): void\n    {\n        exit($reason);\n    }\n\n    public function either(int|string $status): void\n    {\n        exit($status);\n    }\n}\n";
    let others = [("src/Lib/Any.php", any)];

    assert_eq!(
        issues(("src/Demo/Shutdown.sharp", sharp), &others),
        ["9:9 invalid-argument", "14:9 invalid-argument", "19:9 invalid-argument", "24:9 invalid-argument"]
    );
    let refusals = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Shutdown.sharp", sharp), &others);
    for refusal in &refusals {
        assert_eq!(refusal.message, "PHP# has no `die`: write the message to STDERR, then `exit(1)`.");
        assert_eq!(
            refusal.notes,
            ["`die(\"…\")` and `exit(\"…\")` print the message and exit with status 0, which reports success."]
        );
    }
    let annotations: Vec<String> =
        refusals.iter().filter_map(|issue| issue.primary_annotation()?.message.clone()).collect();
    assert_eq!(
        annotations,
        [
            "This is `string`, not an `int`.",
            "This is `int|string`, not an `int`.",
            "This is `Any?`, not an `int`.",
            "This is `string`, not an `int`.",
        ]
    );
    assert_eq!(issues(("src/Demo/Shutdown.php", php), &[]), Vec::<String>::new());
}

/// An `exit` argument that cannot hold a string is no message, so it names the `int` status `exit` takes.
#[test]
fn exit_with_a_value_that_cannot_be_a_message_names_the_int_status() {
    let sharp = "namespace Demo;\n\nclass Shutdown\n{\n    public void maybe(int? code)\n    {\n        exit(code);\n    }\n\n    public void fraction(float code)\n    {\n        exit(code);\n    }\n\n    public void flag(bool done)\n    {\n        exit(done);\n    }\n\n    public void listed(List<int> codes)\n    {\n        exit(codes);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Shutdown.sharp", sharp), &[]),
        ["7:9 invalid-argument", "12:9 invalid-argument", "17:9 invalid-argument", "22:9 invalid-argument"]
    );
    let refusals = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Shutdown.sharp", sharp), &[]);
    let messages: Vec<&str> = refusals.iter().map(|issue| issue.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "`exit` takes an `int` status: this is `int?`.",
            "`exit` takes an `int` status: this is `float`.",
            "`exit` takes an `int` status: this is `bool`.",
            "`exit` takes an `int` status: this is `List<int>`.",
        ]
    );
    for refusal in &refusals {
        assert_eq!(
            refusal.primary_annotation().and_then(|annotation| annotation.message.as_deref()),
            Some("This status is not an `int`.")
        );
        assert!(refusal.notes.is_empty(), "{:?}", refusal.notes);
    }
}

#[test]
fn exit_with_an_int_or_no_value_adds_no_issue() {
    let sharp = "namespace Demo;\n\nclass Shutdown\n{\n    public void code(int code)\n    {\n        exit(code);\n    }\n\n    public void failed()\n    {\n        exit(1);\n    }\n\n    public void done()\n    {\n        exit();\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Shutdown.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn exit_checks_only_its_first_argument() {
    let sharp =
        "namespace Demo;\n\nclass Shutdown\n{\n    public void stop()\n    {\n        exit(1, 2.5);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Shutdown.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn exit_with_a_never_value_has_the_issues_of_its_php_twin() {
    let halt = "<?php\n\nnamespace Lib;\n\nfinal class Halt\n{\n    public static function now(): never\n    {\n        exit(1);\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Halt;\n\nclass Shutdown\n{\n    public void stop()\n    {\n        exit(Halt.now());\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Halt;\n\nclass Shutdown\n{\n    public function stop(): void\n    {\n        exit(Halt::now());\n    }\n}\n";
    let others = [("src/Lib/Halt.php", halt)];

    let sharp_issues = issues(("src/Demo/Shutdown.sharp", sharp), &others);
    let php_issues = issues(("src/Demo/Shutdown.php", php), &others);

    assert_eq!(sharp_issues, ["9:14 no-value"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// The standard library's `Int` and `Float`, with the library's signatures, which a project reads from `vendor/`.
const LIBRARY_TYPE_CLASSES: [(&str, &str); 2] = [
    (
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Int.sharp",
        "namespace Sharp;\n\npublic static class Int\n{\n    public static int parse(Any? value) => 0;\n\n    public static int? tryParse(Any? value) => null;\n}\n",
    ),
    (
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Float.sharp",
        "namespace Sharp;\n\npublic static class Float\n{\n    public static float parse(Any? value) => 0.0;\n\n    public static float? tryParse(Any? value) => null;\n}\n",
    ),
];

/// The semantic checks let any file whose namespace is `Sharp` declare `Int`, `Float` and `Bool`, as the engine does,
/// and only the analyzer knows the file is not the standard library's. It refuses them with the semantic checks'
/// reserved-name message, under its own code.
#[test]
fn a_type_class_of_the_standard_library_in_a_project_file_is_a_reserved_name() {
    let class =
        "namespace Sharp;\n\npublic static class Bool\n{\n    public static bool? tryParse(Any? value) => null;\n}\n";
    let r#enum = "namespace Sharp;\n\npublic enum Int\n{\n    case One;\n}\n";

    let class_issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Sharp/Bool.sharp", class), &[]);
    let enum_issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Sharp/Int.sharp", r#enum), &[]);

    let messages: Vec<String> = class_issues
        .iter()
        .map(|issue| format!("{} {}", located(class, issue), issue.message))
        .chain(enum_issues.iter().map(|issue| format!("{} {}", located(r#enum, issue), issue.message)))
        .collect();
    assert_eq!(
        messages,
        [
            "3:21 reserved-name-outside-library Cannot use `Bool` as a class name: it is reserved.",
            "3:13 reserved-name-outside-library Cannot use `Int` as a class name: it is reserved.",
        ]
    );
}

#[test]
fn int_and_float_parse_have_the_types_of_the_sharp_library_classes() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int count(string text, int? fallback)\n    {\n        const price = Float.parse(text) + (Float.tryParse(fallback) ?? 0.0);\n        const count = Int.parse(text) + (Int.tryParse(null) ?? 0);\n        return price > 1.0 ? count : Int.tryParse(text);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function count(string $text, ?int $fallback): int\n    {\n        $price = \\Sharp\\Float::parse($text) + (\\Sharp\\Float::tryParse($fallback) ?? 0.0);\n        $count = \\Sharp\\Int::parse($text) + (\\Sharp\\Int::tryParse(null) ?? 0);\n        return $price > 1.0 ? $count : \\Sharp\\Int::tryParse($text);\n    }\n}\n";

    // `check_throws` skips `.sharp` files, so the PHP twin is analyzed without it.
    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &LIBRARY_TYPE_CLASSES);
    let php_issues = issues_with(
        Settings { check_throws: false, ..settings() },
        ("src/Demo/Report.php", php),
        &LIBRARY_TYPE_CLASSES,
    );

    assert_eq!(sharp_issues, ["9:16 nullable-return-statement", "9:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// `Position.current()` replaces `__DIR__`, `__FILE__` and `__LINE__`, spec section 27.
#[test]
fn position_current_has_the_types_of_the_sharp_library_class() {
    let sharp = "namespace Demo;\n\nclass Reports\n{\n    public string stubsFolder()\n    {\n        const stubs = Position.current().directory + \"/stubs\";\n        return stubs;\n    }\n\n    public int line() => Position.current().line;\n\n    public int wrong() => Position.current().directory;\n}\n";

    assert_eq!(issues(("src/Demo/Reports.sharp", sharp), &[]), ["13:27 invalid-return-statement"]);
}

/// `Environment` replaces `$_ENV` and `getenv()`, spec section 29.
#[test]
fn an_environment_variable_is_a_nullable_string() {
    let sharp = "namespace Demo;\n\nclass Deploy\n{\n    public Deploy(private Environment environment) { }\n\n    public static Deploy make() => new Deploy(new Environment());\n\n    public string region() => this.environment.variable(\"X\") ?? \"d\";\n\n    public int wrong() => this.environment.variable(\"X\") ?? \"d\";\n}\n";

    assert_eq!(issues(("src/Demo/Deploy.sharp", sharp), &[]), ["11:27 invalid-return-statement"]);
}

/// `arguments` and `currentDirectory` are `{ get; }` properties, so the process environment is read, never written.
#[test]
fn the_environment_arguments_and_current_directory_are_read_only() {
    let sharp = "namespace Demo;\n\nclass Deploy\n{\n    public Deploy(private Environment environment) { }\n\n    public List<string> arguments() => this.environment.arguments;\n\n    public string folder() => this.environment.currentDirectory;\n\n    public List<int> numbers() => this.environment.arguments;\n\n    public void change()\n    {\n        this.environment.arguments = [\"deploy\"];\n        this.environment.currentDirectory = \"/tmp\";\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Deploy.sharp", sharp), &[]),
        ["11:35 invalid-return-statement", "15:26 invalid-property-write", "16:26 invalid-property-write"]
    );
    assert_eq!(
        messages(("src/Demo/Deploy.sharp", sharp), &[])[1..],
        [
            "Cannot initialize readonly property `Environment.arguments` from within `Deploy`.",
            "Cannot initialize readonly property `Environment.currentDirectory` from within `Deploy`.",
        ]
    );
}

/// `List.wrap` replaces `(array)value`, spec section 24: a value that is never itself a list wraps as one call.
#[test]
fn list_wrap_of_a_value_or_a_list_of_it_is_a_list_of_the_value() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public List<string> read(string|List<string> value)\n    {\n        List<string> tags = List.wrap(value);\n        return tags;\n    }\n\n    public List<int> wrong(string|List<string> value) => List.wrap(value);\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), ["11:58 invalid-return-statement"]);
}

/// A `List` runs as a PHP array, so `wrap` cannot tell a list of lists from a list to wrap. The refusal names the `is`
/// form that decides it, and the call keeps the type the code asked for, so the assignment adds no second issue.
#[test]
fn list_wrap_of_a_list_is_refused_with_the_is_form_that_decides_it() {
    let sharp = "namespace Demo;\n\nclass Rows\n{\n    public List<List<int>> read(List<int> numbers)\n    {\n        List<List<int>> rows = List.wrap(numbers);\n        return rows;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Rows.sharp", sharp), &[]), ["7:42 invalid-argument"]);
    assert_eq!(
        messages(("src/Demo/Rows.sharp", sharp), &[]),
        ["T is List<int>, itself a list; write `numbers is List<int> one ? [one] : numbers`"]
    );
}

/// A value from plain PHP typed `mixed` arrives as `Any?`, which could hold a list.
#[test]
fn list_wrap_of_a_value_of_any_type_is_refused() {
    let settings = "<?php\n\nnamespace Lib;\n\nfinal class Settings\n{\n    public static function raw(string $key): mixed\n    {\n        return $key;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Settings;\n\nclass Plans\n{\n    public void read()\n    {\n        List.wrap(Settings.raw(\"plan\"));\n        List.wrap(Settings.raw(\"plan\") ?? \"free\");\n    }\n}\n";
    let others = [("src/Lib/Settings.php", settings)];

    assert_eq!(issues(("src/Demo/Plans.sharp", sharp), &others), ["9:19 invalid-argument", "10:19 invalid-argument"]);
    assert_eq!(
        messages(("src/Demo/Plans.sharp", sharp), &others),
        [
            "T is Any?, which could itself be a list; check what `Settings.raw(\"plan\")` is with `is` first",
            "T is Any, which could itself be a list; check what `Settings.raw(\"plan\") ?? \"free\"` is with `is` first",
        ]
    );
}

/// An editor shows the docblocks of the PHP# stub on hover. A PHP# user reads them, not the spec's numbering, so each
/// states its reason in words and cites no spec section.
#[test]
fn the_php_sharp_stub_states_its_reasons_without_a_spec_section() {
    let stub = PRELUDE
        .database
        .files()
        .find(|file| file.name.ends_with(b"extensions/sharp.php"))
        .expect("the prelude holds the PHP# stub");
    let contents = String::from_utf8_lossy(&stub.contents);

    assert_eq!(contents.lines().filter(|line| line.contains("section")).collect::<Vec<_>>(), Vec::<&str>::new());
}

/// A `Map` runs as a PHP array too, so `wrap` would return it as the list it was asked to build.
#[test]
fn list_wrap_of_a_map_or_a_list_of_lists_is_refused() {
    let sharp = "namespace Demo;\n\nclass Rows\n{\n    public void read(Map<string, int> counts, List<List<int>> rows, string|Map<string, int> either)\n    {\n        List.wrap(counts);\n        List.wrap(rows);\n        List.wrap(either);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Rows.sharp", sharp), &[]),
        ["7:19 invalid-argument", "8:19 invalid-argument", "9:19 invalid-argument"]
    );
    assert_eq!(
        messages(("src/Demo/Rows.sharp", sharp), &[]),
        [
            "T is Map<string, int>, itself a map; write `counts is Map<string, int> one ? [one] : counts`",
            "T is List<List<int>>, itself a list; write `rows is List<List<int>> one ? [one] : rows`",
            "T is string|Map<string, int>, which can be a map; write `either is Map<string, int> one ? [one] : either`",
        ]
    );
}

/// Plain PHP calls `\Sharp\List::wrap` under PHP's rules, with no PHP# refusal.
#[test]
fn list_wrap_called_from_php_keeps_the_php_checks() {
    let php = "<?php\n\nnamespace Demo;\n\nclass Rows\n{\n    /**\n     * @param list<int> $numbers\n     * @param array<string, int> $counts\n     */\n    public function read(array $numbers, array $counts, mixed $raw): void\n    {\n        \\Sharp\\List::wrap($numbers);\n        \\Sharp\\List::wrap($counts);\n        \\Sharp\\List::wrap($raw);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Rows.php", php), &[]), Vec::<String>::new());
}

#[test]
fn expression_bodies_and_computed_properties_are_checked_as_php_checks_their_twins() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private string name = \"\";\n    private int count = 0;\n\n    public Report(string name) => this.name = name;\n\n    public string slug => strtolower(this.name);\n    public int size => this.name;\n    public int total() => this.count + 1;\n    public string label() => this.count;\n    public void touch() => this.count++;\n    public void reset(int unused) => this.count = 0;\n    public void rename() => this.slug = \"x\";\n}\n";
    let php = "<?php\n\nnamespace Demo;\nclass Report\n{\n    private string $name = \"\";\n    private int $count = 0;\n\n    public function __construct(string $name) { $this->name = $name; }\n\n    public string $slug { get => strtolower($this->name); }\n    public int $size { get => $this->name; }\n    public function total(): int { return $this->count + 1; }\n    public function label(): string { return $this->count; }\n    public function touch(): void { $this->count++; }\n    public function reset(int $unused): void { $this->count = 0; }\n    public function rename(): void { $this->slug = \"x\"; }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(
        sharp_issues,
        ["11:24 invalid-return-statement", "13:30 invalid-return-statement", "16:34 invalid-property-write"]
    );
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn plus_keeps_adding_numbers_and_rejecting_nothing_new_in_php() {
    let php = "<?php\n\nnamespace Demo;\n\nfunction total(int $count, string $name): int|float\n{\n    return $count + \"1\";\n}\n";

    assert_eq!(issues(("src/Demo/report.php", php), &[]), Vec::<String>::new());
}

const CUSTOMER: &str = "namespace Demo;\n\nclass Customer\n{\n    public string name { get; set; } = \"\";\n\n    public string label()\n    {\n        return this.name;\n    }\n}\n\nclass Plan\n{\n    public int price { get; set; } = 0;\n}\n";

/// Analyzes `analyzed` together with `others`, and returns its errors as `line:column code`.
fn errors(analyzed: (&'static str, &'static str), others: &[(&'static str, &'static str)]) -> Vec<String> {
    analyze(&PLUGIN_REGISTRY, settings(), analyzed, others)
        .iter()
        .filter(|issue| issue.level == Level::Error)
        .map(|issue| located(analyzed.1, issue))
        .collect()
}

/// Spec section 14.4: a null check on a value whose type has no `?` is a compile error.
#[test]
fn a_null_check_on_a_value_that_is_never_null_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Billing\n{\n    public void renew(Customer customer, Plan plan, Plan? maybe)\n    {\n        const a = customer != null;\n        const b = customer == null;\n        const c = customer !== null;\n        const d = null === customer;\n        const price = plan.price ?? 0;\n        let total = plan.price;\n        total ??= 1;\n        const name = customer?.name;\n        const label = customer?.label();\n        const e = maybe ?? plan;\n        const f = maybe?.price;\n        const g = maybe !== null;\n    }\n}\n";

    assert_eq!(
        errors(("src/Demo/Billing.sharp", sharp), &[("src/Demo/Customer.sharp", CUSTOMER)]),
        [
            "7:19 redundant-comparison",
            "8:19 redundant-comparison",
            "9:19 redundant-comparison",
            "10:28 redundant-comparison",
            "11:23 redundant-null-coalesce",
            "13:9 redundant-null-coalesce",
            "14:30 redundant-nullsafe-operator",
            "15:31 redundant-nullsafe-operator",
        ]
    );
}

#[test]
fn a_null_check_on_a_value_that_is_never_null_keeps_its_php_report() {
    let php = "<?php\n\nnamespace Demo;\n\nclass Billing\n{\n    public function renew(Customer $customer, Plan $plan, ?Plan $maybe): void\n    {\n        $a = $customer != null;\n        $b = $customer == null;\n        $c = $customer !== null;\n        $d = null === $customer;\n        $price = $plan->price ?? 0;\n        $total = $plan->price;\n        $total ??= 1;\n        $name = $customer?->name;\n        $label = $customer?->label();\n        $e = $maybe ?? $plan;\n        $f = $maybe?->price;\n        $g = $maybe !== null;\n    }\n}\n";

    let levelled: Vec<_> =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Billing.php", php), &[("src/Demo/Customer.sharp", CUSTOMER)])
            .iter()
            .map(|issue| format!("{} {:?}", located(php, issue), issue.level))
            .collect();

    assert_eq!(
        levelled,
        [
            "9:27 null-operand Error",
            "9:14 impossible-null-type-comparison Warning",
            "10:27 null-operand Error",
            "10:14 impossible-null-type-comparison Warning",
            "11:14 redundant-comparison Help",
            "11:14 impossible-null-type-comparison Warning",
            "12:14 redundant-comparison Help",
            "12:14 impossible-null-type-comparison Warning",
            "13:18 redundant-null-coalesce Help",
            "15:9 redundant-null-coalesce Help",
            "16:26 redundant-nullsafe-operator Help",
        ]
    );
}

/// A null check that cannot matter names the test as the developer wrote it: `is null` and `is not null` run as
/// PHP's `===`, which the developer never wrote. PHP names its own operator.
#[test]
fn a_redundant_null_check_names_the_test_it_is_written_with() {
    let sharp = "namespace Demo;\n\nclass Billing\n{\n    public void renew(Customer report)\n    {\n        const a = report is null;\n        const b = report is not null;\n        const c = report == null;\n        const d = report != null;\n        const e = match (report) {\n            null => 0,\n            default => 1,\n        };\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Billing\n{\n    public function renew(Customer $report): void\n    {\n        $a = $report === null;\n        $b = $report !== null;\n    }\n}\n";
    let redundant = |analyzed| -> Vec<String> {
        worded(analyzed, &[("src/Demo/Customer.sharp", CUSTOMER)])
            .into_iter()
            .filter(|line| line.contains(" redundant-comparison "))
            .collect()
    };

    let sharp_report = |at: &str, check: &str| {
        format!(
            "{at} redundant-comparison Redundant {check}: `Customer` is never `null`. | This is `Customer`, which is never `null` | This null check cannot matter | In PHP# a type holds null only when written with `?`, so a `?` or a null check that cannot matter is an error. | Remove the null check."
        )
    };

    assert_eq!(
        redundant(("src/Demo/Billing.php", php)),
        [
            "9:14 redundant-comparison Redundant `===` comparison: left-hand side is never identical to right-hand side. | Left operand is `Demo\\Customer` | Right operand is `null` | The `===` operator will always return `false` in this case. | Consider simplifying or removing this comparison as it always evaluates to `false`.",
            "10:14 redundant-comparison Redundant `!==` comparison: left-hand side is always not identical to right-hand side. | Left operand is `Demo\\Customer` | Right operand is `null` | The `!==` operator will always return `true` in this case. | Consider simplifying or removing this comparison as it always evaluates to `true`.",
        ]
    );
    assert_eq!(
        redundant(("src/Demo/Billing.sharp", sharp)),
        [
            sharp_report("7:19", "`is null` check"),
            sharp_report("8:19", "`is not null` check"),
            sharp_report("9:19", "`==` comparison"),
            sharp_report("10:19", "`!=` comparison"),
            sharp_report("11:34", "`null` check"),
        ]
    );
}

/// A redundant null check names its pattern by the words the pattern is written with, so a comment between them stays
/// out of the message. The `is` and each `not` lead the check only when they are written right before it. PHP names its
/// own operator.
#[test]
fn a_redundant_null_check_names_its_pattern_without_its_comments() {
    let sharp = "namespace Demo;\n\nclass Billing\n{\n    public void renew(Customer report)\n    {\n        const a = report is /* c */ null;\n        const b = report is /* c */ not /* d */ null;\n        const c = report is (/* e */ null) or not null;\n        const d = report is { name: /* f */ null };\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Billing\n{\n    public function renew(Customer $report): void\n    {\n        $a = $report === /* c */ null;\n        $b = $report !== /* c */ null;\n    }\n}\n";
    let redundant = |analyzed| -> Vec<String> {
        worded(analyzed, &[("src/Demo/Customer.sharp", CUSTOMER)])
            .into_iter()
            .filter(|line| line.contains(" redundant-comparison "))
            .map(|line| line.split(" | ").next().unwrap_or_default().to_owned())
            .collect()
    };

    assert_eq!(
        redundant(("src/Demo/Billing.php", php)),
        [
            "9:14 redundant-comparison Redundant `===` comparison: left-hand side is never identical to right-hand side.",
            "10:14 redundant-comparison Redundant `!==` comparison: left-hand side is always not identical to right-hand side.",
        ]
    );
    assert_eq!(
        redundant(("src/Demo/Billing.sharp", sharp)),
        [
            "7:19 redundant-comparison Redundant `is null` check: `Customer` is never `null`.",
            "8:19 redundant-comparison Redundant `is not null` check: `Customer` is never `null`.",
            "9:19 redundant-comparison Redundant `null` check: `Customer` is never `null`.",
            "9:47 redundant-comparison Redundant `not null` check: `Customer` is never `null`.",
            "10:31 redundant-comparison Redundant `null` check: `string` is never `null`.",
        ]
    );
}

/// `== null` and `!= null` run as PHP's `=== null` and `!== null`, so they test for null alone and are no loose
/// comparison with `null`.
#[test]
fn equality_with_null_tests_for_null_with_no_operand_issue() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? extra)\n    {\n        if (extra != null) {\n            return extra;\n        }\n        return 0;\n    }\n\n    public static bool missing(int? extra)\n    {\n        return null == extra;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

/// Each issue in `analyzed`, analyzed together with `others`, as `line:column code message | help`.
fn refusals(analyzed: (&'static str, &'static str), others: &[(&'static str, &'static str)]) -> Vec<String> {
    analyze(&PLUGIN_REGISTRY, settings(), analyzed, others)
        .iter()
        .map(|issue| {
            format!("{} {} | {}", located(analyzed.1, issue), issue.message, issue.help.as_deref().unwrap_or_default())
        })
        .collect()
}

const CART: &str = "<?php\n\nnamespace Demo;\n\nfinal class Cart\n{\n}\n";

/// Spec section 19: numbers compare by value, so `1 == 1.0` is true, a nullable side lifted. The pair narrows nothing,
/// so `count == 1.0` makes no impossible test. PHP reports what its loose `==` and its narrowing give.
#[test]
fn equality_of_an_int_and_a_float_compares_their_values_with_no_issue() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool a(int count, float ratio) => count == ratio;\n\n    public static bool b(float ratio, int? count) => ratio != count;\n\n    public static int c(int count)\n    {\n        if (count == 1.0) {\n            return count;\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function a(int $count, float $ratio): bool { return $count == $ratio; }\n\n    public static function b(float $ratio, ?int $count): bool { return $ratio != $count; }\n\n    public static function c(int $count): int\n    {\n        if ($count == 1.0) {\n            return $count;\n        }\n        return 0;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["9:82 possibly-null-operand"]);
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

/// Spec section 19: values of two different types are never equal, so `==` on them is refused. PHP keeps its loose
/// `==`, where `"1" == 1` is true.
#[test]
fn equality_of_two_different_value_types_is_refused() {
    let enums = [
        ("src/Demo/Status.php", "<?php\n\nnamespace Demo;\n\nenum Status\n{\n    case Open;\n}\n"),
        ("src/Demo/Level.php", "<?php\n\nnamespace Demo;\n\nenum Level\n{\n    case Low;\n}\n"),
    ];
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool a(string text, int total) => text == total;\n\n    public static bool b(Status status, Level level) => status != level;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function a(string $text, int $total): bool { return $text == $total; }\n\n    public static function b(Status $status, Level $level): bool { return $status != $level; }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &enums), Vec::<String>::new());
    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &enums),
        [
            "5:53 invalid-operand `==` cannot compare `string` with `int`. | Convert one side so both sides have the same type.",
            "7:57 invalid-operand `!=` cannot compare `Status` with `Level`. | Convert one side so both sides have the same type.",
        ]
    );
}

/// Spec section 19: `==` on a class instance exists only where the class declares `operator ==`, which `Cart` does not.
/// `===` tests for the same object, and `!= null` tests a nullable instance for null.
#[test]
fn equality_of_a_class_instance_is_refused_until_the_class_declares_operator_equality() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool a(Cart cart, Cart other) => cart == other;\n\n    public static bool b(Cart? maybe, Cart cart) => maybe != cart;\n\n    public static bool c(Cart? maybe) => maybe != null;\n\n    public static bool d(Cart cart, Cart other) => cart === other;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function a(Cart $cart, Cart $other): bool { return $cart == $other; }\n\n    public static function b(?Cart $maybe, Cart $cart): bool { return $maybe != $cart; }\n\n    public static function c(?Cart $maybe): bool { return $maybe != null; }\n\n    public static function d(Cart $cart, Cart $other): bool { return $cart === $other; }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.php", php), &[("src/Demo/Cart.php", CART)]),
        ["9:71 possibly-null-operand", "11:59 possibly-null-operand", "11:69 null-operand"]
    );
    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[("src/Demo/Cart.php", CART)]),
        [
            "5:52 invalid-operand `==` cannot compare `Cart` with `Cart`: `Cart` declares no `operator ==`. | Use `===` to test whether both sides are the same object.",
            "7:53 invalid-operand `!=` cannot compare `Cart?` with `Cart`: `Cart` declares no `operator ==`. | Use `!==` to test whether both sides are the same object.",
        ]
    );
}

/// A `List`, `Map` or `Set` is a PHP array at runtime, which `==` cannot compare strictly yet, also against an `Any?`.
#[test]
fn equality_of_a_collection_is_not_supported_yet() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool a(List<int> items, List<int> others) => items == others;\n\n    public static bool b(Any? value, List<int> items) => value != items;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    /** @param list<int> $items @param list<int> $others */\n    public static function a(array $items, array $others): bool { return $items == $others; }\n\n    /** @param list<int> $items */\n    public static function b(mixed $value, array $items): bool { return $value != $items; }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["11:73 mixed-operand"]);
    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[]),
        [
            "5:64 not-supported-yet `==` on a collection is not supported yet, so it cannot compare `List<int>` with `List<int>`. | Compare the elements one by one.",
            "7:58 not-supported-yet `!=` on a collection is not supported yet, so it cannot compare `Any?` with `List<int>`. | Compare the elements one by one.",
        ]
    );
}

/// Spec section 19: `===` and `!==` test whether two class instances are the same object. On any other value they are
/// refused, naming the `==` or `!=` that compares it. PHP keeps its `===`.
#[test]
fn identity_of_a_value_that_is_no_class_instance_is_refused_naming_equality() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool a(int total) => total === 1;\n\n    public static bool b(string text) => text !== \"a\";\n\n    public static bool c(int? maybe) => maybe === null;\n\n    public static bool d(Cart? cart, Cart other) => cart === other;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function a(int $total): bool { return $total === 1; }\n\n    public static function b(string $text): bool { return $text !== \"a\"; }\n\n    public static function c(?int $maybe): bool { return $maybe === null; }\n\n    public static function d(?Cart $cart, Cart $other): bool { return $cart === $other; }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[("src/Demo/Cart.php", CART)]), Vec::<String>::new());
    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[("src/Demo/Cart.php", CART)]),
        [
            "5:40 invalid-operand `===` cannot compare `int` with `int`: it tests whether two class instances are the same object. | Use `==` to compare the values.",
            "7:42 invalid-operand `!==` cannot compare `string` with `string`: it tests whether two class instances are the same object. | Use `!=` to compare the values.",
            "9:41 invalid-operand `===` cannot compare `int?` with `null`: it tests whether two class instances are the same object. | Use `==` to compare the values.",
        ]
    );
}

/// PHP# orders a string only against a string, by its bytes, so a string against a number is refused as `==` refuses
/// it. Two numbers keep PHP's `<`, an `int` against a `float` too. PHP keeps its loose `<`.
#[test]
fn ordering_a_string_against_a_number_is_refused() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool a(string text, int total) => text < total;\n\n    public static bool b(string text, float ratio) => text >= ratio;\n\n    public static bool c(string text, string other) => text < other;\n\n    public static bool d(int total, float ratio) => total < ratio;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function a(string $text, int $total): bool { return $text < $total; }\n\n    public static function b(string $text, float $ratio): bool { return $text >= $ratio; }\n\n    public static function c(string $text, string $other): bool { return $text < $other; }\n\n    public static function d(int $total, float $ratio): bool { return $total < $ratio; }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[]),
        [
            "5:53 invalid-operand `<` cannot compare `string` with `int`. | Convert one side so both sides have the same type.",
            "7:55 invalid-operand `>=` cannot compare `string` with `float`. | Convert one side so both sides have the same type.",
        ]
    );
}

/// Spec section 19 lifts only `==` and `!=` over `null`, so an ordering or `<=>` with a side that may be `null` is refused
/// for a string, an int and a float, as for an instance. Once the side is tested for `null`, it orders. PHP keeps its
/// loose `<`, which orders `null` as `""` or `0`.
#[test]
fn ordering_a_nullable_value_is_refused_until_it_is_tested_for_null() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool a(string? name) => name < \"b\";\n\n    public static bool b(int? count) => count <= 1;\n\n    public static bool c(float? ratio) => ratio > 1.5;\n\n    public static int d(int? count, int total) => count <=> total;\n\n    public static bool e(string? name) => \"b\" >= name;\n\n    public static bool f(string? name) => name != null && name < \"b\";\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function a(?string $name): bool { return $name < \"b\"; }\n\n    public static function b(?int $count): bool { return $count <= 1; }\n\n    public static function c(?float $ratio): bool { return $ratio > 1.5; }\n\n    public static function d(?int $count, int $total): int { return $count <=> $total; }\n\n    public static function e(?string $name): bool { return \"b\" >= $name; }\n\n    public static function f(?string $name): bool { return $name !== null && $name < \"b\"; }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.php", php), &[]),
        [
            "7:60 possibly-null-operand",
            "9:58 possibly-null-operand",
            "11:60 possibly-null-operand",
            "13:69 possibly-null-operand",
            "15:67 possibly-null-operand",
        ]
    );
    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[]),
        [
            "5:43 invalid-operand `<` cannot compare `string?` with `string`: only `==` and `!=` take `null`, so test the value for `null` first. | Test it with `!= null` before the comparison.",
            "7:41 invalid-operand `<=` cannot compare `int?` with `int`: only `==` and `!=` take `null`, so test the value for `null` first. | Test it with `!= null` before the comparison.",
            "9:43 invalid-operand `>` cannot compare `float?` with `float`: only `==` and `!=` take `null`, so test the value for `null` first. | Test it with `!= null` before the comparison.",
            "11:51 invalid-operand `<=>` cannot compare `int?` with `int`: only `==` and `!=` take `null`, so test the value for `null` first. | Test it with `!= null` before the comparison.",
            "13:43 invalid-operand `>=` cannot compare `string` with `string?`: only `==` and `!=` take `null`, so test the value for `null` first. | Test it with `!= null` before the comparison.",
        ]
    );
}

/// A `bool` has no order, so an ordering of `true` is refused, and so is one of a plain PHP value typed `int|false`,
/// whose type holds a `bool`. PHP keeps its loose `<`.
#[test]
fn ordering_a_bool_is_refused() {
    let store = (
        "src/Lib/Store.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Store\n{\n    public static function count(): int|false { return 1; }\n}\n",
    );
    let sharp = "namespace Demo;\n\nimport Lib.Store;\n\nclass Report\n{\n    public static bool a() => true < 1;\n\n    public static bool b() => Store.count() < 1;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Store;\n\nclass Report\n{\n    public static function a(): bool { return true < 1; }\n\n    public static function b(): bool { return Store::count() < 1; }\n}\n";

    assert_eq!(
        worded(("src/Demo/Report.php", php), &[store]),
        [
            "9:47 redundant-comparison Redundant `<` comparison: left-hand side is never less than right-hand side. | Left operand is `true` | Right operand is `int(1)` | The `<` operator will always return `false` in this case. | Consider simplifying or removing this comparison as it always evaluates to `false`.",
            "11:47 possibly-false-operand Left operand in `<` comparison might be `false` (type `false|int`). | This might be `false` | If this operand is `false` at runtime, PHP's specific comparison rules for `false` with `<` will apply. | Ensure this operand is non-false or that comparison with `false` is intended and handled safely.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Report.sharp", sharp), &[store]),
        [
            "7:31 invalid-operand `<` orders numbers, strings and classes that declare `operator <=>`, and `bool` is none of them. | This is `bool`. | This is `int`. | A `bool`, an enum case and a collection have no order. | Order a number or a string taken from the value instead.",
            "9:31 invalid-operand `<` orders numbers, strings and classes that declare `operator <=>`, and `bool` is none of them. | This is `bool|int`. | This is `int`. | A `bool`, an enum case and a collection have no order. | Order a number or a string taken from the value instead.",
        ]
    );
}

/// An enum case has no order, so `<` and `<=>` on two cases are refused. PHP orders two cases as always `false`.
#[test]
fn ordering_an_enum_case_is_refused() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Report\n{\n    public static bool a() => Status.Active < Status.Closed;\n\n    public static int b(Status left, Status right) => left <=> right;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Status;\n\nclass Report\n{\n    public static function a(): bool { return Status::Active < Status::Closed; }\n\n    public static function b(Status $left, Status $right): int { return $left <=> $right; }\n}\n";
    let status = ("src/Lib/Status.php", STATUS);

    assert_eq!(worded(("src/Demo/Report.php", php), &[status]), Vec::<String>::new());
    assert_eq!(
        worded(("src/Demo/Report.sharp", sharp), &[status]),
        [
            "7:31 invalid-operand `<` orders numbers, strings and classes that declare `operator <=>`, and `Status` is none of them. | This is `Status`. | This is `Status`. | A `bool`, an enum case and a collection have no order. | Order a number or a string taken from the value instead.",
            "9:55 invalid-operand `<=>` orders numbers, strings and classes that declare `operator <=>`, and `Status` is none of them. | This is `Status`. | This is `Status`. | A `bool`, an enum case and a collection have no order. | Order a number or a string taken from the value instead.",
        ]
    );
}

/// A `List` has no order, so an ordering of two lists is refused. PHP orders two arrays by its own rules.
#[test]
fn ordering_a_collection_is_refused() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool a(List<int> lines, List<int> other) => lines < other;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    /** @param list<int> $lines @param list<int> $other */\n    public static function a(array $lines, array $other): bool { return $lines < $other; }\n}\n";

    assert_eq!(worded(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        worded(("src/Demo/Report.sharp", sharp), &[]),
        [
            "5:63 invalid-operand `<` orders numbers, strings and classes that declare `operator <=>`, and `List<int>` is none of them. | This is `List<int>`. | This is `List<int>`. | A `bool`, an enum case and a collection have no order. | Order a number or a string taken from the value instead.",
        ]
    );
}

/// An `Any` or `Any?` ordered against a number keeps its own report, as it must be checked with `is` first. Against a
/// `bool` the ordering is refused, as no check makes a `bool` ordered.
#[test]
fn ordering_an_any_keeps_its_report_unless_the_other_side_has_no_order() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool a(Any value) => value < 1;\n\n    public static bool b(Any? value) => value <= 1;\n\n    public static bool c(Any value) => value < true;\n}\n";

    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[]),
        [
            "5:40 mixed-operand Left operand in `<` comparison has `mixed` type. | Ensure this operand has a known, comparable type before using this comparison operator.",
            "7:41 mixed-operand Left operand in `<=` comparison has `mixed` type. | Ensure this operand has a known, comparable type before using this comparison operator.",
            "9:40 invalid-operand `<` orders numbers, strings and classes that declare `operator <=>`, and `bool` is none of them. | Order a number or a string taken from the value instead.",
        ]
    );
}

/// Numbers, strings and instances of a class that declares `operator <=>` keep their order.
#[test]
fn numbers_strings_and_a_class_declaring_operator_spaceship_still_order() {
    let sharp = "namespace App;\n\npublic class Ranking\n{\n    public bool a() => 1 < 2;\n\n    public bool b() => \"a\" < \"b\";\n\n    public bool c() => 1.5 <= 2;\n\n    public bool d(Money low, Money high) => low < high;\n\n    public int e(Money low, Money high) => low <=> high;\n}\n";

    assert_eq!(errors(("src/App/Ranking.sharp", sharp), &[MONEY_OPERATORS]), Vec::<String>::new());
}

/// `sortedBy` orders by the key its function returns, as `<=>` orders, so a key that holds a `bool`, an enum case, a
/// collection or an instance of a class that declares no `operator <=>` is refused, as `<=>` refuses it. A `.php` call
/// of the same method keeps PHP's report.
#[test]
fn sorted_by_a_key_with_no_order_is_refused() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Report\n{\n    public List<Status> a(List<Status> statuses) => statuses.sortedBy(s => s);\n\n    public List<bool> b(List<bool> flags) => flags.sortedBy(f => f);\n\n    public List<Row> c(List<Row> rows) => rows.sortedBy(r => r.tags);\n\n    public List<Cart> d(List<Cart> orders) => orders.sortedBy(o => o);\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Status;\nuse Sharp\\ListMethods;\n\nfinal class Report\n{\n    /**\n     * @param ListMethods<Status> $statuses\n     *\n     * @return list<Status>\n     */\n    public function a(ListMethods $statuses): array\n    {\n        return $statuses->sortedBy(fn (Status $s): Status => $s);\n    }\n}\n";
    let row = "<?php\n\nnamespace Demo;\n\nfinal class Row\n{\n    /** @var list<string> */\n    public array $tags = [];\n}\n";
    let others = [("src/Lib/Status.php", STATUS), ("src/Demo/Cart.php", CART), ("src/Demo/Row.php", row)];

    assert_eq!(worded(("src/Demo/Report.php", php), &others), Vec::<String>::new());
    assert_eq!(
        worded(("src/Demo/Report.sharp", sharp), &others),
        [
            "7:71 invalid-argument `sortedBy` orders numbers, strings and classes that declare `operator <=>`, and `Status` is none of them. | This returns `Status`. | A `bool`, an enum case and a collection have no order. | Order a number or a string taken from the value instead.",
            "9:61 invalid-argument `sortedBy` orders numbers, strings and classes that declare `operator <=>`, and `bool` is none of them. | This returns `bool`. | A `bool`, an enum case and a collection have no order. | Order a number or a string taken from the value instead.",
            "11:57 invalid-argument `sortedBy` orders numbers, strings and classes that declare `operator <=>`, and `List<string>` is none of them. | This returns `List<string>`. | A `bool`, an enum case and a collection have no order. | Order a number or a string taken from the value instead.",
            "13:63 invalid-argument `sortedBy` orders numbers, strings and classes that declare `operator <=>`, and `Cart` is none of them. | This returns `Cart`. | A class instance has an order only where its class declares `operator <=>`. | Order a number or a string taken from the value instead.",
        ]
    );
}

/// A `Map` and a `Set` order their values by `sortedBy` as a `List` does, and so does a list a chain of calls gives, so
/// each refuses a key with no order.
#[test]
fn sorted_by_on_a_map_a_set_and_a_chain_refuses_a_key_with_no_order() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Report\n{\n    public List<Status> a(Map<string, Status> owners) => owners.sortedBy(s => s);\n\n    public List<Status> b(Set<Status> statuses) => statuses.sortedBy(s => s);\n\n    public List<Status> c(List<Status> statuses) => statuses.filter(s => s != Status.Closed).sortedBy(s => s);\n}\n";
    let refused = "invalid-argument `sortedBy` orders numbers, strings and classes that declare `operator <=>`, and `Status` is none of them. | This returns `Status`. | A `bool`, an enum case and a collection have no order. | Order a number or a string taken from the value instead.";

    assert_eq!(
        worded(("src/Demo/Report.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        [format!("7:74 {refused}"), format!("9:70 {refused}"), format!("11:103 {refused}")]
    );
}

/// A loop body is analyzed more than once, and a `sortedBy` in it is refused once.
#[test]
fn sorted_by_in_a_loop_is_refused_once() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Report\n{\n    public int a(List<Status> statuses)\n    {\n        let total = 0;\n        while (total < 3) {\n            total += count(statuses.sortedBy(s => s));\n        }\n        return total;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Status.php", STATUS)]), ["11:46 invalid-argument"]);
}

/// `sortedBy` by a number, a string or an instance of a class that declares `operator <=>` still orders, and so does a
/// key that may be `null`.
#[test]
fn sorted_by_a_number_a_string_or_a_class_declaring_operator_spaceship_still_orders() {
    let sharp = "namespace App;\n\npublic class Ranking\n{\n    public List<int> a(List<int> numbers) => numbers.sortedBy(n => -n);\n\n    public List<float> b(List<float> ratios) => ratios.sortedBy(r => r);\n\n    public List<string> c(List<string> names) => names.sortedBy(n => n);\n\n    public List<int?> d(List<int?> counts) => counts.sortedBy(c => c);\n\n    public List<Money> e(List<Money> prices) => prices.sortedBy(p => p);\n}\n";

    assert_eq!(errors(("src/App/Ranking.sharp", sharp), &[MONEY_OPERATORS]), Vec::<String>::new());
}

/// Spec section 19: `==` and `!=` on a nullable type are lifted, so null equals only null, and an `Any?` compares by
/// value with a string or a number. None of them is an issue in PHP#, though PHP's loose `==` reports them.
#[test]
fn equality_of_a_nullable_value_is_lifted_with_no_issue() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool equal(int? maybe, int total) => maybe == total;\n\n    public static bool known(int? maybe) => maybe != null;\n\n    public static bool missing(int? maybe) => null == maybe;\n\n    public static bool named(Any? value) => value == \"a\";\n\n    public static bool other(Any? value, int total) => value != total;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function equal(?int $maybe, int $total): bool { return $maybe == $total; }\n\n    public static function known(?int $maybe): bool { return $maybe != null; }\n\n    public static function missing(?int $maybe): bool { return null == $maybe; }\n\n    public static function named(mixed $value): bool { return $value == \"a\"; }\n\n    public static function other(mixed $value, int $total): bool { return $value != $total; }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.php", php), &[]),
        [
            "7:74 possibly-null-operand",
            "9:62 possibly-null-operand",
            "9:72 null-operand",
            "11:64 null-operand",
            "11:72 possibly-null-operand",
            "13:63 mixed-operand",
            "15:75 mixed-operand",
        ]
    );
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

/// A refused comparison in a condition is reported once: it narrows nothing, so the condition reports no impossible
/// or redundant test on top of it. PHP keeps the report its loose `==` narrowing gives.
#[test]
fn a_refused_comparison_in_a_condition_is_reported_once() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int a(string text, int total)\n    {\n        if (text == total || total > 2) {\n            return 1;\n        }\n        return 0;\n    }\n\n    public static int b(string text, int total)\n    {\n        if (text != total) {\n            return 1;\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function a(string $text, int $total): int\n    {\n        if ($text == $total || $total > 2) {\n            return 1;\n        }\n        return 0;\n    }\n\n    public static function b(string $text, int $total): int\n    {\n        if ($text != $total) {\n            return 1;\n        }\n        return 0;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["9:13 impossible-type-comparison"]);
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["7:13 invalid-operand", "15:13 invalid-operand"]);
}

/// Spec section 19: `==` and `!=` run as `===` and `!==`, so they narrow as `===` and `!==` do. After `value == "a"` the
/// value is that string, and a `string?` that is not `""` may still be null. PHP narrows its loose `==`, under which
/// `null == ""` holds.
#[test]
fn equality_narrows_as_identity() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string a(Any? value)\n    {\n        if (value == \"a\") {\n            return value;\n        }\n        return \"\";\n    }\n\n    public static int b(Any? value)\n    {\n        if (value != 1) {\n            return 0;\n        }\n        return value;\n    }\n\n    public static string c(string? text)\n    {\n        if (text == \"\") {\n            return \"empty\";\n        }\n        return text ?? \"none\";\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function a(mixed $value): string\n    {\n        if ($value == \"a\") {\n            return $value;\n        }\n        return \"\";\n    }\n\n    public static function b(mixed $value): int\n    {\n        if ($value != 1) {\n            return 0;\n        }\n        return $value;\n    }\n\n    public static function c(?string $text): string\n    {\n        if ($text == \"\") {\n            return \"empty\";\n        }\n        return $text ?? \"none\";\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.php", php), &[]),
        [
            "9:13 mixed-operand",
            "10:20 mixed-return-statement",
            "17:13 mixed-operand",
            "20:16 mixed-return-statement",
            "25:13 possibly-null-operand",
            "28:16 redundant-null-coalesce",
        ]
    );
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

/// Spec section 14.4: a method that never returns null drops the `?` from its return type. A method a subclass may
/// override keeps it, because the override may return null.
#[test]
fn a_nullable_return_type_on_a_method_that_never_returns_null_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Ledger\n{\n    private Customer customer;\n\n    public Ledger(Customer customer)\n    {\n        this.customer = customer;\n    }\n\n    public Customer? owner(bool known)\n    {\n        if (known) {\n            return this.cached();\n        }\n        return this.find(known);\n    }\n\n    private Customer? cached()\n    {\n        return this.customer;\n    }\n\n    private Customer? find(bool known)\n    {\n        if (known) {\n            return this.customer;\n        }\n        return null;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Ledger\n{\n    public function __construct(private Customer $customer)\n    {\n    }\n\n    public function owner(): ?Customer\n    {\n        return $this->cached();\n    }\n\n    private function cached(): ?Customer\n    {\n        return $this->customer;\n    }\n}\n";

    assert_eq!(
        errors(("src/Demo/Ledger.sharp", sharp), &[("src/Demo/Customer.sharp", CUSTOMER)]),
        ["20:13 overly-wide-return-type"]
    );
    assert_eq!(issues(("src/Demo/Ledger.php", php), &[("src/Demo/Customer.sharp", CUSTOMER)]), Vec::<String>::new());

    let help = analyze(
        &PLUGIN_REGISTRY,
        settings(),
        ("src/Demo/Ledger.sharp", sharp),
        &[("src/Demo/Customer.sharp", CUSTOMER)],
    )
    .into_iter()
    .find(|issue| issue.code.as_deref() == Some("overly-wide-return-type"))
    .and_then(|issue| issue.help);
    assert_eq!(help.as_deref(), Some("Remove `null` from the return type, giving `Customer`."));
}

/// A PHP# method is closed unless it is `virtual` or an `override`, spec section 22, so a public method drops a `?`
/// that it never returns, as a `final override` does. A `virtual` method and an open `override` keep it.
#[test]
fn a_closed_method_drops_a_nullable_return_type_and_an_open_one_keeps_it() {
    let sharp = "namespace Demo;\n\npublic class Source\n{\n    public Customer? owner(Customer customer)\n    {\n        return customer;\n    }\n\n    public Customer? first(Customer customer) => customer;\n\n    public virtual Customer? fallback(Customer customer)\n    {\n        return customer;\n    }\n}\n\npublic class Archive : Source\n{\n    public override Customer? fallback(Customer customer)\n    {\n        return customer;\n    }\n}\n\npublic class Vault : Archive\n{\n    public final override Customer? fallback(Customer customer)\n    {\n        return customer;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Source\n{\n    final public function owner(Customer $customer): ?Customer\n    {\n        return $customer;\n    }\n\n    final public function first(Customer $customer): ?Customer\n    {\n        return $customer;\n    }\n\n    public function fallback(Customer $customer): ?Customer\n    {\n        return $customer;\n    }\n}\n\nclass Archive extends Source\n{\n    #[\\Override]\n    public function fallback(Customer $customer): ?Customer\n    {\n        return $customer;\n    }\n}\n\nclass Vault extends Archive\n{\n    #[\\Override]\n    final public function fallback(Customer $customer): ?Customer\n    {\n        return $customer;\n    }\n}\n";

    assert_eq!(
        errors(("src/Demo/Source.sharp", sharp), &[("src/Demo/Customer.sharp", CUSTOMER)]),
        ["5:12 overly-wide-return-type", "10:12 overly-wide-return-type", "28:27 overly-wide-return-type"]
    );
    assert_eq!(issues(("src/Demo/Source.php", php), &[("src/Demo/Customer.sharp", CUSTOMER)]), Vec::<String>::new());
}

#[test]
fn an_abstract_method_keeps_its_nullable_return_type() {
    let sharp = "namespace Demo;\n\nabstract class Source\n{\n    public abstract Customer? current();\n}\n";

    assert_eq!(
        errors(("src/Demo/Source.sharp", sharp), &[("src/Demo/Customer.sharp", CUSTOMER)]),
        Vec::<String>::new()
    );
}

/// Spec section 14.4: a nullable parameter that the method rejects on every path drops its `?`, and the caller checks.
#[test]
fn a_nullable_parameter_the_method_rejects_on_every_path_is_an_error() {
    let sharp = "namespace Demo;\n\nimport RuntimeException;\n\nclass Billing\n{\n    public void notify(Customer? customer)\n    {\n        Customer c = customer ?? throw new RuntimeException(\"none\");\n    }\n\n    public string remind(Customer? customer)\n    {\n        if (customer === null) {\n            throw new RuntimeException(\"none\");\n        }\n        return customer.name;\n    }\n\n    public int discount(Plan? plan)\n    {\n        return plan?.price ?? 0;\n    }\n\n    public void log(Customer? customer, bool loud)\n    {\n        if (loud) {\n            Customer c = customer ?? throw new RuntimeException(\"none\");\n        }\n    }\n\n    public string greet(Customer? customer)\n    {\n        const name = customer?.name;\n        Customer c = customer ?? throw new RuntimeException(\"none\");\n        return c.name;\n    }\n\n    public Customer verify(Customer? customer) => customer ?? throw new RuntimeException(\"none\");\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse RuntimeException;\n\nclass Billing\n{\n    public function notify(?Customer $customer): void\n    {\n        $c = $customer ?? throw new RuntimeException(\"none\");\n    }\n}\n";

    assert_eq!(
        errors(("src/Demo/Billing.sharp", sharp), &[("src/Demo/Customer.sharp", CUSTOMER)]),
        ["7:24 rejected-nullable-parameter", "12:26 rejected-nullable-parameter", "39:28 rejected-nullable-parameter"]
    );
    assert!(
        !codes(&issues(("src/Demo/Billing.php", php), &[("src/Demo/Customer.sharp", CUSTOMER)]))
            .contains(&"rejected-nullable-parameter")
    );

    let help = analyze(
        &PLUGIN_REGISTRY,
        settings(),
        ("src/Demo/Billing.sharp", sharp),
        &[("src/Demo/Customer.sharp", CUSTOMER)],
    )
    .into_iter()
    .find(|issue| issue.code.as_deref() == Some("rejected-nullable-parameter"))
    .and_then(|issue| issue.help);
    assert_eq!(help.as_deref(), Some("Declare `customer` as `Customer`, and check for null where the value enters."));
}

/// Each hook event, with the spans of the call's class, method, arguments and whole call.
type StaticCallEvents = Vec<(&'static str, [Span; 4])>;

/// Records every static call its hooks see.
#[derive(Clone, Default)]
struct StaticCallRecorder(Arc<Mutex<StaticCallEvents>>);

impl Provider for StaticCallRecorder {
    fn meta() -> &'static ProviderMeta {
        static META: ProviderMeta = ProviderMeta::new("test::static-call", "static call", "Records static calls.");

        &META
    }
}

impl StaticCallRecorder {
    fn record(&self, event: &'static str, call: &StaticCall<'_, '_>) {
        let spans = [call.class.span(), call.method.span(), call.argument_list.span(), call.span];
        self.0.lock().unwrap().push((event, spans));
    }

    /// The recorded calls, each span read back from `code`.
    fn seen(&self, code: &str) -> Vec<String> {
        let text = |span: Span| &code[span.start.offset as usize..span.end.offset as usize];

        self.0.lock().unwrap().iter().map(|(event, spans)| format!("{event} {}", spans.map(text).join(" "))).collect()
    }
}

impl StaticMethodCallHook for StaticCallRecorder {
    fn before_static_method_call(
        &self,
        call: &StaticCall<'_, '_>,
        _context: &mut HookContext<'_, '_>,
    ) -> HookResult<ExpressionHookResult> {
        self.record("before", call);

        Ok(ExpressionHookResult::Continue)
    }

    fn after_static_method_call(
        &self,
        call: &StaticCall<'_, '_>,
        _context: &mut HookContext<'_, '_>,
    ) -> HookResult<()> {
        self.record("after", call);

        Ok(())
    }
}

/// Records each method call an expression hook sees, as static or instance by the binding of its object.
#[derive(Clone, Default)]
struct CallKindRecorder(Arc<Mutex<Vec<String>>>);

impl Provider for CallKindRecorder {
    fn meta() -> &'static ProviderMeta {
        static META: ProviderMeta = ProviderMeta::new("test::call-kind", "call kind", "Records method call kinds.");

        &META
    }
}

impl ExpressionHook for CallKindRecorder {
    fn before_expression(
        &self,
        expression: &Expression<'_>,
        context: &mut HookContext<'_, '_>,
    ) -> HookResult<ExpressionHookResult> {
        if let Expression::Call(Call::Method(call)) = expression
            && let ClassLikeMemberSelector::Identifier(method) = &call.method
        {
            let is_static = StaticCall::from_method_call(call, context.resolved_names()).is_some();
            let kind = if is_static { "static" } else { "instance" };
            self.record(format!("{} {kind}", String::from_utf8_lossy(method.value)));
        }

        Ok(ExpressionHookResult::Continue)
    }
}

impl CallKindRecorder {
    fn record(&self, call: String) {
        self.0.lock().unwrap().push(call);
    }
}

#[test]
fn an_expression_hook_tells_a_sharp_static_call_by_the_binding_of_its_object() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int total()\n    {\n        return Calc.make().add(1, 2);\n    }\n}\n";
    let recorder = CallKindRecorder::default();
    let mut registry = PluginRegistry::default();
    registry.register_expression_hook(recorder.clone());

    let issues = issues_in(&registry, settings(), ("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);

    assert_eq!(issues, Vec::<String>::new());
    assert_eq!(*recorder.0.lock().unwrap(), ["add instance", "make static"]);
}

#[test]
fn static_call_hooks_see_a_sharp_static_call_as_its_parts_as_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static Calc total()\n    {\n        return Calc.make();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    public static function total(): Calc\n    {\n        return Calc::make();\n    }\n}\n";

    let seen = |analyzed: (&'static str, &'static str)| {
        let recorder = StaticCallRecorder::default();
        let mut registry = PluginRegistry::default();
        registry.register_static_method_call_hook(recorder.clone());

        let issues = issues_in(&registry, settings(), analyzed, &[("src/Lib/Calc.php", CALC)]);
        assert_eq!(issues, Vec::<String>::new());

        recorder.seen(analyzed.1)
    };

    assert_eq!(
        seen(("src/Demo/Report.sharp", sharp)),
        ["before Calc make () Calc.make()", "after Calc make () Calc.make()"]
    );
    assert_eq!(
        seen(("src/Demo/Report.php", php)),
        ["before Calc make () Calc::make()", "after Calc make () Calc::make()"]
    );
}

#[test]
fn a_lambda_reads_and_writes_the_locals_around_it_as_its_php_closure_does() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        let count = 0;\n        const add = (int a, int b) => a + b + extra;\n        const increment = () => { count += 1; };\n        increment();\n        return add(count, 1);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public function run(int $extra): int\n    {\n        $count = 0;\n        $add = fn (int $a, int $b) => $a + $b + $extra;\n        $increment = function () use (&$count) { $count += 1; };\n        $increment();\n        return $add($count, 1);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_lambda_called_with_a_wrong_argument_or_returning_a_wrong_type_is_an_error_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        const twice = (int a) => a * 2;\n        const label = () => \"none\";\n        twice(\"x\");\n        return label();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public function run(int $extra): int\n    {\n        $twice = fn (int $a) => $a * 2;\n        $label = fn () => \"none\";\n        $twice(\"x\");\n        return $label();\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);

    assert_eq!(sharp_issues, ["9:15 invalid-argument", "10:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&issues(("src/Demo/Report.php", php), &[])));
}

#[test]
fn a_lambda_passed_to_php_takes_its_parameter_types_from_the_php_signature_as_in_php() {
    let numbers = "<?php\n\nnamespace Lib;\n\nfinal class Numbers\n{\n    /**\n     * @param \\Closure(int): bool $keep\n     */\n    public static function count(\\Closure $keep): int\n    {\n        return $keep(1) ? 1 : 0;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Numbers;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        return Numbers.count(n => n > extra) + Numbers.count((string s) => s == \"\");\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Numbers;\n\nclass Report\n{\n    public function run(int $extra): int\n    {\n        return Numbers::count(fn ($n) => $n > $extra) + Numbers::count(fn (string $s) => $s == \"\");\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Numbers.php", numbers)]);

    assert_eq!(sharp_issues, ["9:62 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&issues(("src/Demo/Report.php", php), &[("src/Lib/Numbers.php", numbers)])));
}

#[test]
fn a_function_type_is_a_closure_type_that_takes_lambdas_and_calls_as_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private Function<int(int)> scale;\n\n    public Report(int factor)\n    {\n        this.scale = n => n * factor;\n    }\n\n    public Function<bool(int)> above(int floor) => n => n > floor;\n\n    public int run(int extra)\n    {\n        const scale = this.scale;\n        const check = this.above(extra);\n        Function<int(int)> twice = n => n * 2;\n        return check(twice(scale(extra))) ? 1 : 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    /** @var \\Closure(int): int */\n    private \\Closure $scale;\n\n    public function __construct(int $factor)\n    {\n        $this->scale = fn (int $n) => $n * $factor;\n    }\n\n    /** @return \\Closure(int): bool */\n    public function above(int $floor): \\Closure { return fn (int $n) => $n > $floor; }\n\n    public function run(int $extra): int\n    {\n        $scale = $this->scale;\n        $check = $this->above($extra);\n        $twice = fn (int $n) => $n * 2;\n        return $check($twice($scale($extra))) ? 1 : 0;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_function_type_refuses_a_lambda_of_another_type_and_a_string_or_array_callable() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int apply(Function<int(int)> step, int value) => step(value);\n\n    public static int run(int extra)\n    {\n        Report.apply((string s) => 1, extra);\n        Report.apply(n => \"text\", extra);\n        Report.apply(\"abs\", extra);\n        return Report.apply([\"Demo\\\\Report\", \"run\"], extra);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        ["9:22 invalid-argument", "10:22 invalid-argument", "11:22 invalid-argument", "12:29 invalid-argument"]
    );
}

/// Spec section 12's methods that take a function give each lambda the element type, and type what they return from
/// it: `filter` and `sortedBy` keep the element type, `map` and `sumOf` take the lambda's return type, `groupBy` and
/// `associateBy` key a map by it, and `filterValues` keeps a map's keys.
#[test]
fn collection_methods_that_take_a_lambda_type_its_parameter_and_their_result() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int run(List<int> numbers, Map<string, int> prices)\n    {\n        List<int> evens = numbers.filter(n => n % 2 == 0);\n        List<string> labels = numbers.map(n => `#${n}`);\n        int total = evens.sumOf(n => n * 2);\n        List<string> sorted = labels.sortedBy(s => strlen(s));\n        Map<int, List<int>> groups = numbers.groupBy(n => n % 3);\n        Map<string, string> byLabel = labels.associateBy(s => s);\n        Map<string, int> cheap = prices.filterValues(p => p < 100);\n        List<int> kept = prices.filter(p => p > 0);\n        List<float> halves = prices.map(p => p / 2);\n        int first = numbers.first(n => n > 2);\n        bool expensive = prices.any(p => p > 1000);\n        return total + first + count(sorted) + count(groups) + count(byLabel) + count(cheap) + count(kept) + count(halves) + (expensive ? 1 : 0);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

/// A method value passed to a collection method types what it returns, as a lambda does.
#[test]
fn a_method_value_passed_to_a_collection_method_types_its_result() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public List<string> run(List<int> numbers)\n    {\n        List<string> labels = numbers.map(this.label);\n        List<int> wrong = numbers.map(this.label);\n        return labels;\n    }\n\n    private string label(int n) => (string) n;\n}\n";

    assert_eq!(codes(&issues(("src/Demo/Report.sharp", sharp), &[])), ["invalid-local-assignment-value"]);
}

/// A lambda passed to a collection method is checked against the element type, as any argument is.
#[test]
fn a_collection_method_refuses_a_lambda_of_another_element_type() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int run(List<int> numbers)\n    {\n        List<int> kept = numbers.filter((string s) => s == \"\");\n        List<string> labels = numbers.map(n => n * 2);\n        return count(kept) + count(labels);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        ["7:41 invalid-argument", "8:31 invalid-local-assignment-value"]
    );
}

/// Spec section 14 calls a property that holds a function as a method: `x.priceOf(line)` calls the property when the
/// class has a property `priceOf` and no method `priceOf`, on `this`, on another object and on an inherited
/// property, and checks the call against the property's function type as `($x->priceOf)($line)` is checked in PHP.
#[test]
fn a_call_of_a_property_holding_a_function_is_checked_against_its_function_type() {
    let pricing = "<?php\n\nnamespace Lib;\n\nabstract class BasePricing\n{\n    /** @var \\Closure(int): int */\n    public \\Closure $priceOf;\n}\n\nfinal class Pricing extends BasePricing\n{\n    public function __construct()\n    {\n        $this->priceOf = fn (int $amount): int => $amount * 2;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Pricing;\n\nclass Report\n{\n    private Function<int(int)> scale;\n\n    public Report(int factor)\n    {\n        this.scale = n => n * factor;\n    }\n\n    public int run(Pricing pricing, int extra) => this.scale(extra) + pricing.priceOf(extra);\n\n    public int wrong(Pricing pricing) => this.scale(\"x\") + strlen(pricing.priceOf(1));\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Pricing;\n\nclass Report\n{\n    /** @var \\Closure(int): int */\n    private \\Closure $scale;\n\n    public function __construct(int $factor)\n    {\n        $this->scale = fn (int $n) => $n * $factor;\n    }\n\n    public function run(Pricing $pricing, int $extra): int { return ($this->scale)($extra) + ($pricing->priceOf)($extra); }\n\n    public function wrong(Pricing $pricing): int { return ($this->scale)(\"x\") + strlen(($pricing->priceOf)(1)); }\n}\n";
    let others = [("src/Lib/Pricing.php", pricing)];

    let codes = |issues: Vec<String>| -> Vec<String> {
        issues.into_iter().map(|issue| issue.split_once(' ').unwrap().1.to_owned()).collect()
    };
    let php_issues = codes(issues(("src/Demo/Report.php", php), &others));

    assert_eq!(php_issues, ["invalid-argument", "invalid-argument"]);
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &others), ["16:53 invalid-argument", "16:67 invalid-argument"]);
}

#[test]
fn attribute_arguments_are_checked_against_the_attribute_constructor_as_in_php() {
    let field = "<?php\n\nnamespace Lib;\n\n#[\\Attribute]\nfinal class Field\n{\n    public function __construct(public string $label, public int $width = 1)\n    {\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Field;\n\n[Field(\"Report\")]\nclass Report\n{\n    [Field(label: 3)] private int count = 0;\n\n    [Field(\"run\", width: \"wide\")]\n    public int run([Field(width: 2)] int extra, [Field(\"page\", 2, 3)] int page)\n    {\n        return extra + page + this.count;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Field;\n\n#[Field(\"Report\")]\nclass Report\n{\n    #[Field(label: 3)] private int $count = 0;\n\n    #[Field(\"run\", width: \"wide\")]\n    public function run(#[Field(width: 2)] int $extra, #[Field(\"page\", 2, 3)] int $page): int\n    {\n        return $extra + $page + $this->count;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Field.php", field)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Field.php", field)]);

    assert_eq!(
        sharp_issues,
        ["8:19 invalid-argument", "10:26 invalid-argument", "11:26 too-few-arguments", "11:67 too-many-arguments"]
    );
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// A plain PHP class whose values arrive in PHP# as `Any?`: typed `mixed`, or not typed at all.
const SOURCE: &str = "<?php\n\nnamespace Demo;\n\nfinal class Source\n{\n    public static function value(string $key): mixed\n    {\n        return $key;\n    }\n\n    public static function untyped(string $key)\n    {\n        return $key;\n    }\n\n    public static function take(mixed $value): void\n    {\n    }\n\n    public static function takeUntyped($value): void\n    {\n    }\n\n    public static function takeInt(int $value): void\n    {\n    }\n}\n";

#[test]
fn a_php_mixed_or_untyped_value_arrives_as_any_and_goes_back_to_php() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    public Any? read(string key)\n    {\n        const value = Source.value(key);\n        Any? raw = Source.untyped(key);\n        Source.take(value);\n        Source.takeUntyped(raw);\n        return key == \"raw\" ? raw : value;\n    }\n\n    public Any keep(Any value)\n    {\n        const kept = value;\n        Source.take(kept);\n        return kept;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Inbox.sharp", sharp), &[("src/Demo/Source.php", SOURCE)]), Vec::<String>::new());
}

#[test]
fn an_unchecked_any_is_refused_wherever_its_type_matters() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    public int use(Any? value)\n    {\n        const a = value.name;\n        const b = value.run();\n        const c = value + 1;\n        const d = -value;\n        const e = value < 1;\n        Source.takeInt(value);\n        int f = value;\n        const g = (int)value;\n        if (value) {\n        }\n        return value;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Inbox.sharp", sharp), &[("src/Demo/Source.php", SOURCE)]),
        [
            "7:25 mixed-property-access",
            "8:25 mixed-method-access",
            "9:19 mixed-operand",
            "10:20 mixed-operand",
            "11:19 mixed-operand",
            "12:24 mixed-argument",
            "13:17 invalid-local-assignment-value",
            "14:19 invalid-operand",
            "15:13 invalid-operand",
            "17:16 mixed-return-statement",
        ]
    );
}

const SHAPES: &str = "<?php\n\nnamespace Lib;\n\ninterface Shape\n{\n}\n\nfinal class Circle implements Shape\n{\n    public function __construct(public float $radius)\n    {\n    }\n}\n\nfinal class Square implements Shape\n{\n    public function __construct(public float $side)\n    {\n    }\n}\n";

#[test]
fn is_is_not_and_as_narrow_as_their_php_does() {
    let sharp = "namespace Demo;\n\nimport Lib.Shape;\nimport Lib.Circle;\nimport Lib.Square;\n\nclass Report\n{\n    public static float area(Shape shape)\n    {\n        if (shape is Circle circle) {\n            return circle.radius;\n        }\n        if (shape is not Square square) {\n            return 0.0;\n        }\n        return square.side;\n    }\n\n    public static float side(Shape shape)\n    {\n        if (shape is Square) {\n            return shape.side;\n        }\n        return 0.0;\n    }\n\n    public static float radius(Shape shape)\n    {\n        const circle = shape as Circle;\n        return circle?.radius ?? 0.0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Shape;\nuse Lib\\Circle;\nuse Lib\\Square;\n\nclass Report\n{\n    public static function area(Shape $shape): float\n    {\n        if (($circle = $shape) instanceof Circle) {\n            return $circle->radius;\n        }\n        if (!(($square = $shape) instanceof Square)) {\n            return 0.0;\n        }\n        return $square->side;\n    }\n\n    public static function side(Shape $shape): float\n    {\n        if ($shape instanceof Square) {\n            return $shape->side;\n        }\n        return 0.0;\n    }\n\n    public static function radius(Shape $shape): float\n    {\n        $circle = $shape instanceof Circle ? $shape : null;\n        return $circle?->radius ?? 0.0;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Shapes.php", SHAPES)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Shapes.php", SHAPES)]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_match_narrows_each_arm_as_its_php_does() {
    let sharp = "namespace Demo;\n\nimport Lib.Shape;\nimport Lib.Circle;\nimport Lib.Square;\n\nclass Report\n{\n    public static string describe(Shape? shape) => match (shape) {\n        null => \"nothing\",\n        Circle c when c.radius > 10.0 => \"big\",\n        Circle => \"circle\",\n        default => \"other\",\n    };\n\n    public static string grade(int score) => match (score) {\n        < 0 => \"invalid\",\n        >= 90 => \"top\",\n        >= 50 and < 70 => \"pass\",\n        default => \"fail\",\n    };\n\n    public static float count(Shape? shape)\n    {\n        let total = 0.0;\n        match (shape) {\n            Circle c => {\n                total = c.radius;\n            },\n            Square s when s.side > 1.0 => total = s.side,\n            default => {},\n        }\n        return total;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Shape;\nuse Lib\\Circle;\nuse Lib\\Square;\n\nclass Report\n{\n    public static function describe(?Shape $shape): string { return match (true) {\n        $shape === null => \"nothing\",\n        ($c = $shape) instanceof Circle and $c->radius > 10.0 => \"big\",\n        $shape instanceof Circle => \"circle\",\n        default => \"other\",\n    }; }\n\n    public static function grade(int $score): string { return match (true) {\n        $score < 0 => \"invalid\",\n        $score >= 90 => \"top\",\n        $score >= 50 && $score < 70 => \"pass\",\n        default => \"fail\",\n    }; }\n\n    public static function count(?Shape $shape): float\n    {\n        $total = 0.0;\n        if (($c = $shape) instanceof Circle) {\n            $total = $c->radius;\n        } else if (($s = $shape) instanceof Square and $s->side > 1.0) {\n            $total = $s->side;\n        } else {\n        }\n        return $total;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Shapes.php", SHAPES)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Shapes.php", SHAPES)]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_default_arm_no_value_reaches_is_required_and_silent_in_sharp_and_reported_in_php() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static string kind(bool flag) => match (flag) {\n        true => \"yes\",\n        false => \"no\",\n        default => \"never\",\n    };\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function kind(bool $flag): string { return match (true) {\n        $flag === true => \"yes\",\n        $flag === false => \"no\",\n        default => \"never\",\n    }; }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert!(!codes(&sharp_issues).contains(&"unreachable-match-default-arm"), "{sharp_issues:?}");
    assert!(codes(&php_issues).contains(&"unreachable-match-default-arm"), "{php_issues:?}");
}

#[test]
fn a_properties_pattern_on_a_value_that_cannot_be_null_reports_nothing_and_its_php_reports_the_object_check() {
    let sharp = "namespace Demo;\n\nimport Lib.Circle;\n\nclass Report\n{\n    public static bool wide(Circle circle) => circle is { radius: >= 2.0 };\n\n    public static bool known(Circle? circle) => circle is { radius: >= 2.0 };\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Circle;\n\nclass Report\n{\n    public static function wide(Circle $circle): bool { return \\is_object($circle) && $circle->radius >= 2.0; }\n\n    public static function known(?Circle $circle): bool { return \\is_object($circle) && $circle->radius >= 2.0; }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Shapes.php", SHAPES)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Shapes.php", SHAPES)]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&php_issues), ["redundant-type-comparison", "redundant-logical-operation"]);
}

#[test]
fn a_pattern_that_can_never_match_is_an_error_at_the_pattern() {
    let sharp = "namespace Demo;\n\nimport Lib.Circle;\nimport Lib.Square;\n\nclass Report\n{\n    public static bool square(Circle circle) => circle is Square;\n\n    public static bool text(int count) => count is string;\n\n    public static bool named(int count) => count is \"none\";\n\n    public static Square? converted(Circle circle) => circle as Square;\n\n    public static int arm(Circle circle) => match (circle) {\n        Square => 1,\n        default => 0,\n    };\n\n    public static int value(int count) => match (count) {\n        1 => 1,\n        \"none\" => 0,\n        default => 2,\n    };\n\n    public static bool possible(int? count) => count is int and > 0;\n}\n";

    let first =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[("src/Lib/Shapes.php", SHAPES)]);
    let errors: Vec<String> = first
        .iter()
        .filter(|issue| issue.level == Level::Error)
        .map(|issue| {
            let span = issue.primary_span().expect("an error has a primary span");
            format!(
                "{} {} {}",
                &sharp[span.start.offset as usize..span.end.offset as usize],
                issue.code.as_deref().unwrap_or(""),
                issue.message
            )
        })
        .collect();

    assert_eq!(
        errors,
        [
            "Square impossible-type-comparison This pattern never matches the value it tests.",
            "string impossible-type-comparison Impossible type assertion: `count` of type `int` can never be `string`.",
            "\"none\" impossible-type-comparison This pattern never matches the value it tests.",
            "Square impossible-type-comparison This pattern never matches the value it tests.",
            "Square impossible-type-comparison This pattern never matches the value it tests.",
            "\"none\" impossible-type-comparison This pattern never matches the value it tests.",
        ]
    );
}

/// `is` and `match` check an `Any?`, so the value they narrow is used as its checked type, spec section 24.
#[test]
fn is_and_match_check_an_any_before_its_use() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    public int size(Any? value)\n    {\n        if (value is string text) {\n            return strlen(text);\n        }\n        return match (value) {\n            int count => count + 1,\n            null => 0,\n            default => -1,\n        };\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Inbox.sharp", sharp), &[("src/Demo/Source.php", SOURCE)]), Vec::<String>::new());
}

/// `== null` runs as `=== null` in PHP#, so it checks an `Any?`, and on an `Any`, which is never null, it is redundant.
#[test]
fn equality_with_null_checks_an_any_nullable_and_is_redundant_on_an_any() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    public bool missing(Any? value) => value == null;\n\n    public bool gone(Any value) => value == null;\n}\n";

    assert_eq!(issues(("src/Demo/Inbox.sharp", sharp), &[]), ["7:36 redundant-comparison"]);
}

/// An accessor of an `Any?` property takes and gives null, and one of an `Any` property gives any value but null.
#[test]
fn an_accessor_of_any_gives_no_null_and_of_any_nullable_does() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    public Any? note { get => field; set => field = value; }\n\n    public Any label { get => this.note ?? \"none\"; }\n\n    public Any blank { get => null; }\n}\n";

    assert_eq!(issues(("src/Demo/Inbox.sharp", sharp), &[]), ["9:31 invalid-return-statement"]);
}

/// An override of a plain PHP property of PHP's `mixed`, written or in `@var`, or of an untyped one, writes `Any?`, or
/// `Any`, which lowers to the same `mixed`, spec sections 6.1 and 24.
#[test]
fn an_override_of_a_plain_php_mixed_property_writes_any() {
    let library = "<?php\n\nnamespace Lib;\n\nabstract class Message\n{\n    /** @var mixed */\n    protected $payload;\n\n    protected mixed $data = null;\n\n    protected $raw;\n\n    protected mixed $body = 1;\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Message;\n\npublic class Mail : Message\n{\n    protected override Any? payload = null;\n    protected override Any? data = null;\n    protected override Any raw = 1;\n    protected override Any body = \"text\";\n}\n";

    assert_eq!(issues(("src/Demo/Mail.sharp", sharp), &[("src/Lib/Message.php", library)]), Vec::<String>::new());
}

/// A `let` takes its first value's type, so one that starts as `Any?` takes any value, null too, and one that starts
/// as `Any` takes any value but null.
#[test]
fn a_let_local_that_starts_as_any_keeps_whether_it_takes_null() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    public Any? keep(string key, Any value)\n    {\n        let raw = Source.untyped(key);\n        raw = 1;\n        raw = null;\n        let kept = value;\n        kept = \"one\";\n        kept = null;\n        return key == \"raw\" ? raw : kept;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Inbox.sharp", sharp), &[("src/Demo/Source.php", SOURCE)]),
        ["12:16 invalid-local-assignment-value"]
    );
}

#[test]
fn a_get_body_that_converts_a_mixed_attribute_with_as_has_no_issues() {
    let sharp = "namespace Demo;\n\nimport Lib.Model;\n\nclass Order : Model\n{\n    public Address? shipping { get => this.getAttribute(\"shipping\") as Address; set => this.setAttribute(\"shipping\", value); }\n}\n\nclass Address\n{\n}\n";

    assert_eq!(issues(("src/Demo/Order.sharp", sharp), &[("src/Lib/Model.php", MODEL)]), Vec::<String>::new());
}

/// A `let` holding a `mixed` attribute is an `Any?` local, so storing it reports nothing in PHP#, where PHP reports
/// `mixed-assignment`, and spec section 24 refuses its use instead, as `name.length` shows.
#[test]
fn a_let_local_holding_a_mixed_attribute_is_any_and_reports_nothing_where_php_reports_mixed_assignment() {
    let sharp = "namespace Demo;\n\nimport Lib.Model;\n\nclass Order : Model\n{\n    public bool named()\n    {\n        let name = this.getAttribute(\"name\");\n        return name is string;\n    }\n\n    public Any? length()\n    {\n        let name = this.getAttribute(\"name\");\n        return name.length;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Model;\n\nclass Order extends Model\n{\n    public function named(): bool\n    {\n        $name = $this->getAttribute('name');\n        return \\is_string($name);\n    }\n\n    public function length(): mixed\n    {\n        $name = $this->getAttribute('name');\n        return $name->length;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Order.sharp", sharp), &[("src/Lib/Model.php", MODEL)]);
    let php_issues = issues(("src/Demo/Order.php", php), &[("src/Lib/Model.php", MODEL)]);

    assert_eq!(sharp_issues, ["16:21 mixed-property-access"]);
    assert_eq!(codes(&php_issues), ["mixed-assignment", "mixed-assignment", "mixed-property-access"]);
}

#[test]
fn is_and_match_over_a_mixed_attribute_have_no_issues() {
    let sharp = "namespace Demo;\n\nimport Lib.Model;\n\nclass Order : Model\n{\n    public bool named() => this.getAttribute(\"name\") is string;\n\n    public int size() => match (this.getAttribute(\"size\")) {\n        int => 1,\n        string => 2,\n        default => 0,\n    };\n\n    public int count()\n    {\n        let total = 0;\n        match (this.getAttribute(\"count\")) {\n            int => total = 1,\n            default => {},\n        }\n        return total;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Order.sharp", sharp), &[("src/Lib/Model.php", MODEL)]), Vec::<String>::new());
}

#[test]
fn a_never_subject_reports_its_value_and_no_assignment_the_user_never_wrote() {
    let stop = "<?php\n\nnamespace Lib;\n\nfinal class Stop\n{\n    public static function now(): never\n    {\n        throw new \\RuntimeException();\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Stop;\n\nclass Order\n{\n    public static int size() => match (Stop.now()) {\n        int => 1,\n        default => 0,\n    };\n}\n";

    assert_eq!(
        issues(("src/Demo/Order.sharp", sharp), &[("src/Lib/Stop.php", stop)]),
        ["7:40 no-value", "8:9 redundant-type-comparison"]
    );
}

const TICKETS: &str = "<?php\n\nnamespace Lib;\n\nenum Status\n{\n    case Open;\n    case Closed;\n    case Archived;\n}\n\nfinal class Ticket\n{\n    public function status(): Status\n    {\n        return Status::Open;\n    }\n}\n\nfinal class Limits\n{\n    public const LOW = 1;\n    public const HIGH = 2;\n}\n";

/// Each error of `sharp` as its primary span's text, its code and its message.
fn written_errors(sharp: &'static str, issues: &[Issue]) -> Vec<String> {
    issues
        .iter()
        .filter(|issue| issue.level == Level::Error)
        .map(|issue| {
            let span = issue.primary_span().expect("an error has a primary span");
            format!(
                "{} {} {}",
                &sharp[span.start.offset as usize..span.end.offset as usize],
                issue.code.as_deref().unwrap_or(""),
                issue.message
            )
        })
        .collect()
}

#[test]
fn a_match_without_default_names_the_enum_cases_it_misses() {
    let sharp = "namespace Demo;\n\nimport Lib.Limits;\nimport Lib.Status;\n\nclass Report\n{\n    public static string label(Status status) => match (status) {\n        Status.Open => \"open\",\n        Status.Closed => \"closed\",\n    };\n\n    public static void close(Status? status, bool forced)\n    {\n        match (status) {\n            Status.Open when forced => {},\n            Status.Closed => {},\n        }\n    }\n\n    public static int level(int count) => match (count) {\n        Limits.LOW => 1,\n        Limits.HIGH => 2,\n    };\n}\n";

    let issues =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[("src/Lib/Status.php", TICKETS)]);

    assert_eq!(
        written_errors(sharp, &issues),
        [
            "match match-not-exhaustive This `match` misses `Status.Archived`.",
            "match match-not-exhaustive This `match` misses `Status.Open`, `Status.Archived` and `null`.",
            "match match-not-exhaustive A `match` needs a `default` arm.",
        ]
    );
}

#[test]
fn a_match_without_default_that_handles_every_enum_case_reports_nothing() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\nimport Lib.Ticket;\n\nclass Report\n{\n    public static string label(Status status) => match (status) {\n        Status.Open => \"open\",\n        Status.Closed or Status.Archived => \"done\",\n    };\n\n    public static void close(Status? status)\n    {\n        match (status) {\n            Status.Open => {},\n            Status.Closed or Status.Archived => {},\n            null => {},\n        }\n    }\n\n    public static string state(Ticket ticket) => match (ticket.status()) {\n        Status.Open => \"open\",\n        Status.Closed or Status.Archived => \"done\",\n    };\n}\n";

    let issues =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[("src/Lib/Status.php", TICKETS)]);
    let reported: Vec<&str> = issues
        .iter()
        .filter(|issue| matches!(issue.level, Level::Error | Level::Warning))
        .map(|issue| issue.message.as_str())
        .collect();

    assert_eq!(reported, Vec::<&str>::new());
}

/// An enum declared in PHP# has its cases counted as a PHP enum's, inside its own methods too.
#[test]
fn a_match_without_default_over_a_sharp_enum_names_the_cases_it_misses() {
    let sharp = "namespace Demo;\n\npublic enum Stage : string\n{\n    case Open = \"o\";\n    case Paid = \"p\";\n    case Closed = \"c\";\n\n    public string label() => match (this) {\n        Stage.Open => \"open\",\n        Stage.Paid or Stage.Closed => \"done\",\n    };\n}\n\nclass Orders\n{\n    public static string missing(Stage stage) => match (stage) {\n        Stage.Open => \"open\",\n        Stage.Paid => \"paid\",\n    };\n}\n";

    let issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Orders.sharp", sharp), &[]);

    assert_eq!(written_errors(sharp, &issues), ["match match-not-exhaustive This `match` misses `Stage.Closed`."]);
}

/// Only the last arm of a `match` takes what the arms before it leave, as `default` does. An arm before it that is
/// always true leaves nothing for the arms after it.
#[test]
fn an_arm_that_is_always_true_before_the_last_is_reported_with_the_arms_it_hides() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Report\n{\n    public static string label(Status status) => match (status) {\n        Status.Open or Status.Closed or Status.Archived => \"any\",\n        Status.Open => \"open\",\n    };\n\n    public static string kind(Status status) => match (status) {\n        Status s => \"any\",\n        Status.Closed => \"closed\",\n        default => \"none\",\n    };\n}\n";

    let issues =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[("src/Lib/Status.php", TICKETS)]);
    let warnings: Vec<String> = issues
        .iter()
        .filter(|issue| matches!(issue.level, Level::Error | Level::Warning))
        .map(|issue| {
            let span = issue.primary_span().expect("a report has a primary span");
            format!(
                "{} {}",
                &sharp[span.start.offset as usize..span.end.offset as usize],
                issue.code.as_deref().unwrap_or("")
            )
        })
        .collect();

    assert_eq!(
        warnings,
        [
            "Status.Open or Status.Closed or Status.Archived => \"any\" match-arm-always-true",
            "Status.Open => \"open\" unreachable-match-arm",
            "Status s => \"any\" match-arm-always-true",
            "Status.Closed => \"closed\" unreachable-match-arm",
        ]
    );
    assert!(
        issues.iter().any(|issue| issue.code.as_deref() == Some("redundant-logical-operation")),
        "the arm before the last keeps its redundancy report: {issues:?}"
    );
}

#[test]
fn a_pattern_the_parser_refuses_reports_only_its_parse_error() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static void count(int? count)\n    {\n        match (count) {\n            [int first] => {},\n            Shape.Circle(radius) => {},\n            default => {},\n        }\n    }\n}\n";

    let issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let messages: Vec<&str> = issues.iter().map(|issue| issue.message.as_str()).collect();

    assert_eq!(
        messages,
        ["A list pattern is not supported yet in PHP#.", "An enum case pattern is not supported yet in PHP#."]
    );
}

#[test]
fn a_when_condition_that_is_not_bool_is_an_invalid_operand_named_when() {
    let sharp = "namespace Demo;\n\nimport Lib.Shape;\nimport Lib.Circle;\n\nclass Report\n{\n    public static string round(Shape shape) => match (shape) {\n        Circle c when c.radius => \"round\",\n        default => \"other\",\n    };\n}\n";

    let first =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[("src/Lib/Shapes.php", SHAPES)])
            .remove(0);

    assert_eq!(first.code.as_deref(), Some("invalid-operand"));
    assert_eq!(first.message, "`when` takes a `bool`, but this is `float`.");
}

const PRICES: &str = "<?php\n\nnamespace Lib;\n\nfinal class Prices\n{\n    /** @return array<string, int> */\n    public static function named(): array\n    {\n        return ['a' => 1];\n    }\n\n    /** @return iterable<int, int> */\n    public static function stream(): iterable\n    {\n        yield 1;\n    }\n\n    /** @return list<int> */\n    public static function listed(): array\n    {\n        return [1, 2];\n    }\n}\n";

#[test]
fn a_spread_of_a_value_that_is_not_a_list_is_an_invalid_argument() {
    let sharp = "namespace Demo;\n\nimport Lib.Prices;\n\nclass Report\n{\n    public Report(int first, int second)\n    {\n    }\n\n    public int size() => 1;\n\n    public static int sum(int ...values) => count(values);\n\n    public static int spread()\n    {\n        const named = Report.sum(...Prices.named());\n        const stream = Report.sum(...Prices.stream());\n        const made = new Report(...Prices.named());\n        return named + stream + made.size() + Report.sum(...Prices.listed());\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Prices;\n\nclass Report\n{\n    public function __construct(int $first, int $second)\n    {\n    }\n\n    public function size(): int { return 1; }\n\n    public static function sum(int ...$values): int { return count($values); }\n\n    public static function spread(): int\n    {\n        $named = Report::sum(...Prices::named());\n        $stream = Report::sum(...Prices::stream());\n        $made = new Report(...Prices::named());\n        return $named + $stream + $made->size() + Report::sum(...Prices::listed());\n    }\n}\n";

    // An `array<string, int>` may be empty, so the constructor may get too few arguments, in PHP too.
    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Prices.php", PRICES)]),
        ["17:37 invalid-argument", "18:38 invalid-argument", "19:36 invalid-argument", "19:32 too-few-arguments"]
    );
    assert_eq!(issues(("src/Demo/Report.php", php), &[("src/Lib/Prices.php", PRICES)]), ["21:27 too-few-arguments"]);
}

#[test]
fn a_spread_of_a_value_that_is_not_iterable_is_one_invalid_argument() {
    let into_defaults = "namespace Demo;\n\nclass Report\n{\n    public static int part(int first = 1, int second = 2) => first + second;\n\n    public static int run(int number) => Report.part(...number);\n}\n";
    let into_variadic = "namespace Demo;\n\nclass Report\n{\n    public static int sum(int ...values) => count(values);\n\n    public static int run(int number) => Report.sum(...number);\n}\n";

    for sharp in [into_defaults, into_variadic] {
        let issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[]);
        let messages: Vec<&str> = issues.iter().map(|issue| issue.message.as_str()).collect();

        assert_eq!(messages, ["Cannot spread a value of type `int`: PHP# spreads only a list."], "{sharp}");
    }
}

#[test]
fn a_named_argument_never_fills_a_variadic_parameter_of_a_method_or_a_php_function() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int sum(int ...values) => max(0, ...values);\n\n    public static int run()\n    {\n        const named = Report.sum(first: 1, second: 2);\n        const own = Report.sum(values: 1);\n        const after = Report.sum(1, values: 2);\n        return named + own + after + strlen(sprintf(\"%d\", 1, other: 2));\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function sum(int ...$values): int { return max(0, ...$values); }\n\n    public static function run(): int\n    {\n        $named = Report::sum(first: 1, second: 2);\n        $own = Report::sum(values: 1);\n        $after = Report::sum(1, values: 2);\n        return $named + $own + $after + strlen(sprintf(\"%d\", 1, other: 2));\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        [
            "9:34 invalid-named-argument",
            "10:32 invalid-named-argument",
            "11:37 invalid-named-argument",
            "12:62 invalid-named-argument",
        ]
    );
    let annotations: Vec<String> = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[])
        .iter()
        .filter_map(|issue| issue.primary_annotation()?.message.clone())
        .collect();
    assert_eq!(annotations, ["A variadic parameter takes no named argument in PHP#"; 4]);
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), ["13:33 named-argument-after-positional"]);
}

#[test]
fn a_default_that_does_not_fit_its_type_is_an_error_unless_the_type_is_nullable() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    protected int key = null;\n    protected int|string other = false;\n    protected int? kept = null;\n\n    public Report(protected int|string id = null)\n    {\n    }\n\n    public static int size(int|string id = null, int x = null, int? y = null) => 1;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    protected int $key = null;\n    protected int|string $other = false;\n    protected ?int $kept = null;\n\n    public function __construct(protected int|string $id = null)\n    {\n    }\n\n    public static function size(int|string $id = null, int $x = null, ?int $y = null): int { return 1; }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        [
            "5:25 invalid-property-default-value",
            "6:34 invalid-property-default-value",
            "9:45 invalid-parameter-default-value",
            "13:44 invalid-parameter-default-value",
            "13:58 invalid-parameter-default-value",
        ]
    );
    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
}

#[test]
fn a_variadic_parameter_is_a_list_that_spreads_into_methods_and_php_functions_with_no_issue() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int sum(int ...values)\n    {\n        const most = max(0, ...values);\n        return Report.total(...values) + new Report(...values).size() + most;\n    }\n\n    public Report(int ...values)\n    {\n    }\n\n    public int size() => 1;\n\n    public static int total(int ...values) => count(values);\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_union_type_is_checked_as_php_checks_its_twin() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int|string key = 1;\n\n    public int|string find(int|Calc id)\n    {\n        int|string found = this.key;\n        if (found == 1) {\n            return 1.5;\n        }\n        return id;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Calc;\n\nclass Report\n{\n    private int|string $key = 1;\n\n    public function find(int|Calc $id): int|string\n    {\n        $found = $this->key;\n        if ($found === 1) {\n            return 1.5;\n        }\n        return $id;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Calc.php", CALC)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Calc.php", CALC)]);

    assert_eq!(sharp_issues, ["13:20 invalid-return-statement", "15:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// A lambda's union and variadic parameters, and a function type's union parameter, are checked as their PHP twins'.
#[test]
fn a_lambda_with_union_and_variadic_parameters_is_checked_as_its_php_twin() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int run()\n    {\n        const pick = (int|string key, int ...rest) => count(rest);\n        Function<int(int|string)> size = key => 1;\n        pick(1.5);\n        pick(\"a\", 1, \"b\");\n        return pick(1, ...[2, 3]) + size(2.5);\n    }\n}\n";
    let php = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Demo;\n\nclass Report\n{\n    public function run(): int\n    {\n        $pick = fn(int|string $key, int ...$rest): int => count($rest);\n        $size = fn(int|string $key): int => 1;\n        $pick(1.5);\n        $pick(\"a\", 1, \"b\");\n        return $pick(1, ...[2, 3]) + $size(2.5);\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["9:14 invalid-argument", "10:22 invalid-argument", "11:42 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{php_issues:?}");
}

/// An enum's method that returns `Self` is checked as one returning `static`, and its union-typed constant as a
/// typed constant, as in PHP.
#[test]
fn an_enum_returning_self_with_a_union_constant_is_checked_as_its_php_twin() {
    let sharp = "namespace Demo;\n\nenum Status : string\n{\n    public const int|string Key = 1;\n\n    case Active = \"a\";\n\n    public static Self first() => Status.Active;\n\n    public static List<Self> all() => Self.cases();\n\n    public static Self? find(string ...codes) => Status.tryFrom(codes[0] ?? \"a\");\n\n    public static Self wrong() => \"a\";\n\n    public static int|string key() => Status.Key;\n}\n";
    let php = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Demo;\n\nenum Status: string\n{\n    public const int|string Key = 1;\n\n    case Active = \"a\";\n\n    public static function first(): static { return Status::Active; }\n\n    /** @return list<static> */\n    public static function all(): array { return static::cases(); }\n\n    public static function find(string ...$codes): ?static { return Status::tryFrom($codes[0] ?? \"a\"); }\n\n    public static function wrong(): static { return \"a\"; }\n\n    public static function key(): int|string { return Status::Key; }\n}\n";

    let sharp_issues = issues(("src/Demo/Status.sharp", sharp), &[]);
    let php_issues = issues_with(Settings { check_throws: false, ..settings() }, ("src/Demo/Status.php", php), &[]);

    assert_eq!(sharp_issues, ["15:35 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{php_issues:?}");
}

#[test]
fn a_nullable_union_holds_its_types_and_null_as_its_php_twin_does() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private (int|string)? key = null;\n\n    public (int|string)? find((int|string)? id)\n    {\n        (int|string)? found = id ?? this.key;\n        return found;\n    }\n\n    public int run()\n    {\n        this.find(null);\n        this.find(\"one\");\n        this.find(1.5);\n        return 1;\n    }\n}\n";
    let php = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Demo;\n\nclass Report\n{\n    private int|string|null $key = null;\n\n    public function find(int|string|null $id): int|string|null\n    {\n        $found = $id ?? $this->key;\n        return $found;\n    }\n\n    public function run(): int\n    {\n        $this->find(null);\n        $this->find(\"one\");\n        $this->find(1.5);\n        return 1;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["17:19 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn a_nullable_field_without_an_initial_value_starts_as_null_as_its_php_twin_does() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private int? total;\n    private (int|string)? key;\n    public Report? next { get; set; }\n\n    public int? current()\n    {\n        return this.total;\n    }\n\n    public int sum()\n    {\n        return (this.total ?? 0) + (this.next?.sum() ?? 0);\n    }\n\n    public (int|string)? find() => this.key;\n}\n";
    let php = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Demo;\n\nclass Report\n{\n    private ?int $total = null;\n    private int|string|null $key = null;\n    public ?Report $next = null;\n\n    public function current(): ?int\n    {\n        return $this->total;\n    }\n\n    public function sum(): int\n    {\n        return ($this->total ?? 0) + ($this->next?->sum() ?? 0);\n    }\n\n    public function find(): int|string|null\n    {\n        return $this->key;\n    }\n}\n";
    let php_without_default = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    private ?int $total;\n\n    public function current(): ?int\n    {\n        return $this->total;\n    }\n}\n";
    let settings = || Settings { check_property_initialization: true, ..settings() };

    let sharp_issues = issues_with(settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues_with(settings(), ("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
    assert_eq!(issues_with(settings(), ("src/Demo/Report.php", php_without_default), &[]), ["5:7 missing-constructor"]);
}

#[test]
fn a_nullable_field_returned_as_a_value_is_reported_as_its_php_twin_is() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    private int? total;\n\n    public int current()\n    {\n        return this.total;\n    }\n}\n";
    let php = "<?php\n\ndeclare(strict_types=1);\n\nnamespace Demo;\n\nclass Report\n{\n    private ?int $total = null;\n\n    public function current(): int\n    {\n        return $this->total;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["9:16 nullable-return-statement", "9:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

const ROW: &str = "<?php\n\nnamespace Lib;\n\nfinal class Row\n{\n}\n";
const CLOCK: &str = "<?php\n\nnamespace Lib;\n\nfinal class Clock\n{\n}\n";

/// Spec section 25's example: `Self` is PHP's `static`, and a `required` constructor is the one every subclass keeps,
/// as PHP's `@consistent-constructor`. So `Order.fromSchema(row)` returns an `Order`, and a subclass constructor that
/// adds a parameter without a default is an error, as in PHP.
#[test]
fn self_and_a_required_constructor_are_checked_as_static_and_a_consistent_constructor_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Clock;\nimport Lib.Row;\n\npublic abstract class DatabaseEntity\n{\n    public required DatabaseEntity(Row row)\n    {\n    }\n\n    public static Self fromSchema(Row row)\n    {\n        return new Self(row);\n    }\n}\n\npublic class Order : DatabaseEntity\n{\n    public Order(Row row, Clock? clock = null)\n    {\n        super.__construct(row);\n    }\n\n    public static Order make(Row row) => Order.fromSchema(row);\n\n    public static Invoice wrong(Row row) => Order.fromSchema(row);\n}\n\npublic class Invoice : DatabaseEntity\n{\n    public Invoice(Row row, Clock clock)\n    {\n        super.__construct(row);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Clock;\nuse Lib\\Row;\n\n/** @consistent-constructor */\nabstract class DatabaseEntity\n{\n    public function __construct(Row $row)\n    {\n    }\n\n    public static function fromSchema(Row $row): static\n    {\n        return new static($row);\n    }\n}\n\nclass Order extends DatabaseEntity\n{\n    public function __construct(Row $row, ?Clock $clock = null)\n    {\n        parent::__construct($row);\n    }\n\n    public static function make(Row $row): Order\n    {\n        return Order::fromSchema($row);\n    }\n\n    public static function wrong(Row $row): Invoice\n    {\n        return Order::fromSchema($row);\n    }\n}\n\nclass Invoice extends DatabaseEntity\n{\n    public function __construct(Row $row, Clock $clock)\n    {\n        parent::__construct($row);\n    }\n}\n";
    let others = [("src/Lib/Row.php", ROW), ("src/Lib/Clock.php", CLOCK)];

    let sharp_issues = issues(("src/Demo/DatabaseEntity.sharp", sharp), &others);
    let php_issues = issues(("src/Demo/DatabaseEntity.php", php), &others);

    assert_eq!(sharp_issues, ["27:45 invalid-return-statement", "32:12 incompatible-parameter-count"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{php_issues:?}");
}

/// `Self` can be any descendant, so a grandchild's constructor is compared with its parent's as a child's is, spec
/// section 25.
#[test]
fn every_descendant_of_a_required_constructor_keeps_a_matching_constructor() {
    let sharp = "namespace Demo;\n\nimport Lib.Clock;\nimport Lib.Row;\n\npublic abstract class DatabaseEntity\n{\n    public required DatabaseEntity(Row row)\n    {\n    }\n\n    public static Self fromSchema(Row row) => new Self(row);\n}\n\npublic class Order : DatabaseEntity\n{\n    public Order(Row row, Clock? clock = null)\n    {\n        super.__construct(row);\n    }\n}\n\npublic class Shipment : Order\n{\n    public Shipment()\n    {\n        super.__construct(new Row());\n    }\n}\n\npublic class LineItem : Order\n{\n    public LineItem(Row row, Clock clock)\n    {\n        super.__construct(row, clock);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/DatabaseEntity.sharp", sharp), &[("src/Lib/Row.php", ROW), ("src/Lib/Clock.php", CLOCK)]),
        ["25:12 incompatible-parameter-count", "33:12 incompatible-parameter-count"]
    );
}

/// Only a `required` constructor binds the subclasses' constructors, so without one a subclass constructor may add a
/// parameter without a default, as in PHP.
#[test]
fn a_subclass_constructor_of_a_class_without_a_required_constructor_may_add_parameters() {
    let sharp = "namespace Demo;\n\nimport Lib.Clock;\nimport Lib.Row;\n\npublic abstract class DatabaseEntity\n{\n    public DatabaseEntity(Row row)\n    {\n        row;\n    }\n}\n\npublic class Invoice : DatabaseEntity\n{\n    public Invoice(Row row, Clock clock)\n    {\n        super.__construct(row);\n        clock;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/DatabaseEntity.sharp", sharp), &[("src/Lib/Row.php", ROW), ("src/Lib/Clock.php", CLOCK)]),
        ["10:9 unused-statement", "19:9 unused-statement"]
    );
}

/// `Self.count()` is PHP's `static::count()`.
#[test]
fn self_calls_a_static_method_as_static_does_in_php() {
    let sharp = "namespace Demo;\n\npublic class Counter\n{\n    public static int count() => 1;\n\n    public static int twice() => Self.count() + Self.count();\n\n    public static int missing() => Self.absent();\n\n    public static Self make() => new Self(Self.count());\n\n    public required Counter(int start)\n    {\n        start;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\n/** @consistent-constructor */\nclass Counter\n{\n    public static function count(): int\n    {\n        return 1;\n    }\n\n    public static function twice(): int\n    {\n        return static::count() + static::count();\n    }\n\n    public static function missing(): int\n    {\n        return static::absent();\n    }\n\n    public static function make(): static\n    {\n        return new static(static::count());\n    }\n\n    public function __construct(int $start)\n    {\n        $start;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Counter.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Counter.php", php), &[]);

    assert_eq!(sharp_issues, ["9:41 non-existent-method", "9:36 mixed-return-statement", "15:9 unused-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{php_issues:?}");
}

/// PHP reads `Self->count()` as an instance call on `self`, and only a `.sharp` file reads `Self.count()` as a
/// static call.
#[test]
fn a_php_self_arrow_call_keeps_its_upstream_issue() {
    let php = "<?php\n\nnamespace Demo;\n\nclass Counter\n{\n    public static function count(): int\n    {\n        return Self->count();\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Counter.php", php), &[]),
        ["9:16 invalid-scope-keyword-context", "9:16 mixed-return-statement"]
    );
}

/// An override of a method whose parameter is variadic declares it variadic too, spec section 7, as PHP refuses
/// otherwise when it links the class. Upstream Mago misses it, so a `.php` file keeps reporting nothing.
#[test]
fn an_override_of_a_variadic_parameter_is_variadic() {
    let sharp = "namespace Demo;\n\npublic class Base\n{\n    public virtual int sum(int ...values) => count(values);\n}\n\npublic class Child : Base\n{\n    public override int sum(int values) => values;\n}\n\npublic class Fine : Base\n{\n    public override int sum(int ...values) => count(values) + 1;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Base\n{\n    public function sum(int ...$values): int\n    {\n        return count($values);\n    }\n}\n\nclass Child extends Base\n{\n    #[\\Override]\n    public function sum(int $values): int\n    {\n        return $values;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Base.sharp", sharp), &[]), ["10:25 incompatible-parameter-count"]);
    assert_eq!(issues(("src/Demo/Base.php", php), &[]), Vec::<String>::new());

    let issue = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Base.sharp", sharp), &[]).remove(0);
    assert_eq!(issue.message, "`Child.sum` must declare parameter `values` variadic like `Base.sum`");
}

#[test]
fn a_backed_enum_and_a_class_using_it_have_the_issues_of_their_php_twins() {
    let sharp = "namespace Demo;\n\nenum Status : string\n{\n    case Active = \"a\";\n    case Paused = \"p\";\n\n    public string label()\n    {\n        return this.name + \": \" + this.value;\n    }\n\n    public static Status fallback()\n    {\n        return Status.from(\"a\");\n    }\n}\n\nclass Report\n{\n    private Status status;\n\n    public Report(Status status)\n    {\n        this.status = status;\n    }\n\n    public string describe(string code)\n    {\n        const found = Status.tryFrom(code);\n        if (found == null || count(Status.cases()) < 2) {\n            return this.status.label();\n        }\n        return found.value + found.name;\n    }\n\n    public static Report make()\n    {\n        return new Report(Status.fallback());\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nenum Status: string\n{\n    case Active = \"a\";\n    case Paused = \"p\";\n\n    public function label(): string\n    {\n        return $this->name . \": \" . $this->value;\n    }\n\n    public static function fallback(): Status\n    {\n        return Status::from(\"a\");\n    }\n}\n\nclass Report\n{\n    private Status $status;\n\n    public function __construct(Status $status)\n    {\n        $this->status = $status;\n    }\n\n    public function describe(string $code): string\n    {\n        $found = Status::tryFrom($code);\n        if ($found === null || count(Status::cases()) < 2) {\n            return $this->status->label();\n        }\n        return $found->value . $found->name;\n    }\n\n    public static function make(): Report\n    {\n        return new Report(Status::fallback());\n    }\n}\n";

    // `check_throws` skips `.sharp` files, so the PHP twin is analyzed without it.
    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues_with(Settings { check_throws: false, ..settings() }, ("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn php_calls_a_sharp_enum_and_sharp_calls_a_php_enum_with_no_issues() {
    let status = "namespace Demo;\n\nenum Status : string\n{\n    case Active = \"a\";\n\n    public string label() => this.name;\n}\n";
    let php_caller =
        "<?php\n\nnamespace App;\n\nfunction run(): string\n{\n    return \\Demo\\Status::from('a')->label();\n}\n";
    let color = "<?php\n\nnamespace Lib;\n\nenum Color: string\n{\n    case Red = 'r';\n}\n";
    let sharp_caller = "namespace Demo;\n\nimport Lib.Color;\n\nclass Paint\n{\n    public static string code(string raw)\n    {\n        return Color.from(raw).value;\n    }\n}\n";
    // `check_throws` reports the `ValueError` of `from` in a PHP file, whichever dialect declares the enum.
    let php_settings = Settings { check_throws: false, ..settings() };

    assert_eq!(
        issues_with(php_settings, ("src/App/run.php", php_caller), &[("src/Demo/Status.sharp", status)]),
        Vec::<String>::new()
    );
    assert_eq!(issues(("src/Demo/Paint.sharp", sharp_caller), &[("src/Lib/Color.php", color)]), Vec::<String>::new());
}

#[test]
fn from_with_a_value_of_the_wrong_backing_type_is_reported_as_in_php() {
    let sharp = "namespace Demo;\n\nenum Status : string\n{\n    case Active = \"a\";\n}\n\nclass Report\n{\n    public static Status make()\n    {\n        return Status.from(1);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nenum Status: string\n{\n    case Active = \"a\";\n}\n\nclass Report\n{\n    public static function make(): Status\n    {\n        return Status::from(1);\n    }\n}\n";

    // `check_throws` skips `.sharp` files, so the PHP twin is analyzed without it.
    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues_with(Settings { check_throws: false, ..settings() }, ("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["12:28 invalid-argument"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// An enum case's value is a constant expression, so it reads `Class.y` as the class constant, as PHP's `Class::y`,
/// and a static property there is a constant the class does not have. A class reads the enum's case as `Status.Active`.
#[test]
fn an_enum_case_value_reads_a_class_member_as_a_class_constant_and_a_class_reads_the_case() {
    let registry = "<?php\n\nnamespace Lib;\n\nfinal class Registry\n{\n    public const int VERSION = 2;\n    public static int $count = 0;\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Registry;\n\nenum Status : int\n{\n    case Active = Registry.VERSION;\n    case Paused = Registry.count;\n\n    public static Status first() => Status.Active;\n}\n\nclass Report\n{\n    public static bool run(Status status = Status.Active)\n    {\n        return status == Status.first();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Registry;\n\nenum Status: int\n{\n    case Active = Registry::VERSION;\n    case Paused = Registry::count;\n\n    public static function first(): Status\n    {\n        return Status::Active;\n    }\n}\n\nclass Report\n{\n    public static function run(Status $status = Status::Active): bool\n    {\n        return $status === Status::first();\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Registry.php", registry)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Registry.php", registry)]);

    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{sharp_issues:?} {php_issues:?}");
    assert_eq!(sharp_issues, ["8:28 non-existent-class-constant", "8:19 invalid-enum-case-value"]);
}

/// An enum's header names its interfaces, as PHP's `implements` does, so its case passes where an interface is
/// expected, with and without a backing type.
#[test]
fn an_enum_header_names_the_interfaces_as_implements_does_in_php() {
    let library = "<?php\n\nnamespace Lib;\n\ninterface HasLabel\n{\n    public function label(): string;\n}\n\nfinal class Shelf\n{\n    public static function show(HasLabel $labeled): string\n    {\n        return $labeled->label();\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.HasLabel;\nimport Lib.Shelf;\n\npublic enum Status : string, HasLabel\n{\n    case Active = \"a\";\n\n    public string label() => this.name;\n\n    public static string shown() => Shelf.show(Status.Active);\n}\n\nenum Suit : HasLabel\n{\n    case Hearts;\n\n    public string label() => this.name;\n\n    public static string shown() => Shelf.show(Suit.Hearts);\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\HasLabel;\nuse Lib\\Shelf;\n\nenum Status: string implements HasLabel\n{\n    case Active = \"a\";\n\n    public function label(): string\n    {\n        return $this->name;\n    }\n\n    public static function shown(): string\n    {\n        return Shelf::show(Status::Active);\n    }\n}\n\nenum Suit implements HasLabel\n{\n    case Hearts;\n\n    public function label(): string\n    {\n        return $this->name;\n    }\n\n    public static function shown(): string\n    {\n        return Shelf::show(Suit::Hearts);\n    }\n}\n";
    let others = [("src/Lib/HasLabel.php", library)];

    assert_eq!(issues(("src/Demo/Status.php", php), &others), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Status.sharp", sharp), &others), Vec::<String>::new());
}

/// A case is read as `Status.Active` in a class, in the enum's own method, in a parameter default, in a constant, and
/// with a method called on it, and a case's value reads another class's constant. Each is checked as the PHP twin's
/// `::` read.
#[test]
fn a_case_read_in_every_place_has_the_issues_of_its_php_twin() {
    let registry = "<?php\n\nnamespace Lib;\n\nfinal class Registry\n{\n    public const string PAUSED = 'p';\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Registry;\n\npublic enum Status : string\n{\n    case Active = \"a\";\n    case Paused = Registry.PAUSED;\n\n    public const Status Default = Status.Active;\n\n    public bool active() => this == Status.Active;\n\n    public string label() => this.name;\n}\n\nclass Report\n{\n    public string run(Status status = Status.Active)\n    {\n        if (status == Status.Active) {\n            return Status.Active.label();\n        }\n        return Status.Default.label();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Registry;\n\nenum Status: string\n{\n    case Active = \"a\";\n    case Paused = Registry::PAUSED;\n\n    public const Status Default = Status::Active;\n\n    public function active(): bool\n    {\n        return $this === Status::Active;\n    }\n\n    public function label(): string\n    {\n        return $this->name;\n    }\n}\n\nclass Report\n{\n    public function run(Status $status = Status::Active): string\n    {\n        if ($status === Status::Active) {\n            return Status::Active->label();\n        }\n        return Status::Default->label();\n    }\n}\n";
    let others = [("src/Lib/Registry.php", registry)];

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &others);
    let php_issues = issues(("src/Demo/Report.php", php), &others);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{php_issues:?}");
}

const MONEY: &str = "<?php\n\nnamespace App\\Shared;\n\nfinal class Money\n{\n    public const int MAX = 100;\n\n    public static function of(int $cents): int\n    {\n        return $cents;\n    }\n}\n";

/// The messages of every issue in `analyzed`, analyzed together with `others`.
fn messages(analyzed: (&'static str, &'static str), others: &[(&'static str, &'static str)]) -> Vec<String> {
    analyze(&PLUGIN_REGISTRY, settings(), analyzed, others).into_iter().map(|issue| issue.message).collect()
}

/// The issues of `analyzed` as `line:column code message`, each followed by every annotation, note and help it
/// carries, so a test sees each type the issue prints.
fn worded(analyzed: (&'static str, &'static str), others: &[(&'static str, &'static str)]) -> Vec<String> {
    worded_with(settings(), analyzed, others)
}

/// The issues of `analyzed` under `settings`, worded as `worded` words them.
fn worded_with(
    settings: Settings,
    analyzed: (&'static str, &'static str),
    others: &[(&'static str, &'static str)],
) -> Vec<String> {
    analyze(&PLUGIN_REGISTRY, settings, analyzed, others)
        .iter()
        .map(|issue| {
            let annotations = issue.annotations.iter().filter_map(|annotation| annotation.message.as_deref());
            std::iter::once(format!("{} {}", located(analyzed.1, issue), issue.message))
                .chain(annotations.map(str::to_owned))
                .chain(issue.notes.iter().cloned())
                .chain(issue.help.clone())
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .collect()
}

/// Spec section 23 keeps full names in `import` lines. A chain whose root names no class, but whose dotted start names
/// one, is a full name, which only the codebase tells from a class and its member. The refused value has no type, as
/// a call on a missing class has none, so returning it reports `mixed` as it does there.
#[test]
fn a_full_name_inside_code_names_the_import_to_add() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        return App.Shared.Money.of(extra);\n    }\n}\n";
    let analyzed = ("src/App/Tenant/Report.sharp", code);
    let others = [("src/App/Shared/Money.php", MONEY)];

    assert_eq!(
        messages(analyzed, &others),
        [
            "Full names appear only in `import` lines: add `import App.Shared.Money;` and write `Money`.",
            "Could not infer a precise return type for method `Report.run`. Saw type `Any?`.",
        ]
    );
    assert_eq!(issues(analyzed, &others), ["7:16 non-existent-class-like", "7:16 mixed-return-statement"]);
}

#[test]
fn a_member_read_through_a_full_name_names_the_import_to_add() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int run()\n    {\n        return App.Shared.Money.MAX;\n    }\n}\n";
    let analyzed = ("src/App/Tenant/Report.sharp", code);
    let others = [("src/App/Shared/Money.php", MONEY)];

    assert_eq!(
        messages(analyzed, &others),
        [
            "Full names appear only in `import` lines: add `import App.Shared.Money;` and write `Money`.",
            "Could not infer a precise return type for method `Report.run`. Saw type `Any?`.",
        ]
    );
    assert_eq!(issues(analyzed, &others), ["7:16 non-existent-class-like", "7:16 mixed-return-statement"]);
}

/// A root that names a class is a class whatever follows it, even when its name and the next one also name a class.
#[test]
fn a_class_declared_in_the_file_is_never_the_root_of_a_full_name() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public static int Totals = 1;\n\n    public int run(int extra)\n    {\n        return Report.Totals + extra;\n    }\n}\n";
    let totals = "<?php\n\nnamespace Report;\n\nfinal class Totals\n{\n}\n";

    assert_eq!(
        issues(("src/App/Tenant/Report.sharp", code), &[("src/Report/Totals.php", totals)]),
        Vec::<String>::new()
    );
}

/// A class of the same namespace needs no import, so `Status.Active.label()` reads a case of an enum another file
/// declares and calls its method.
#[test]
fn a_case_of_an_enum_in_another_file_of_the_namespace_takes_a_method_call() {
    let status = "namespace App.Tenant;\n\npublic enum Status : string\n{\n    case Active = \"a\";\n\n    public string label() => this.name;\n}\n";
    let report = "namespace App.Tenant;\n\nclass Report\n{\n    public string run()\n    {\n        return Status.Active.label();\n    }\n}\n";

    assert_eq!(
        issues(("src/App/Tenant/Report.sharp", report), &[("src/App/Tenant/Status.sharp", status)]),
        Vec::<String>::new()
    );
}

/// A class, a missing name or an interface whose method the enum lacks in an enum's header reports what its PHP twin's
/// `implements` reports. PHP refuses a class there when it links the enum, so the analyzer reports it first.
#[test]
fn an_enum_header_reports_what_implements_reports_in_php() {
    let library = "<?php\n\nnamespace Lib;\n\ninterface HasLabel\n{\n    public function label(): string;\n}\n\nclass Entity\n{\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Entity;\nimport Lib.HasLabel;\n\nenum Kind : string, Entity\n{\n    case One = \"1\";\n}\n\nenum Lost : Missing\n{\n    case One;\n}\n\nenum Partial : HasLabel\n{\n    case One;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Entity;\nuse Lib\\HasLabel;\n\nenum Kind: string implements Entity\n{\n    case One = \"1\";\n}\n\nenum Lost implements Missing\n{\n    case One;\n}\n\nenum Partial implements HasLabel\n{\n    case One;\n}\n";
    let others = [("src/Lib/HasLabel.php", library)];

    let sharp_issues = issues(("src/Demo/Kind.sharp", sharp), &others);
    let php_issues = issues(("src/Demo/Kind.php", php), &others);

    assert_eq!(codes(&php_issues), ["invalid-implement", "non-existent-class-like", "unimplemented-abstract-method"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{sharp_issues:?}");
}

/// An enum's static method named through the enum is a closure of its signature, `from` among them, as a class's is,
/// and a lambda over a list of cases takes the enum as its element type.
#[test]
fn an_enum_static_method_is_a_function_value_and_a_lambda_takes_its_cases_from_a_list() {
    let sharp = "namespace Demo;\n\nenum Status : string\n{\n    case Active = \"a\";\n    case Paused = \"p\";\n\n    public static Status fallback() => Status.Active;\n}\n\nclass Report\n{\n    public int run(List<string> codes)\n    {\n        const Function<Status(string)> parse = Status.from;\n        const Function<Status()> fallback = Status.fallback;\n        List<Status> found = codes.map(parse);\n        List<Status> active = found.filter(s => s == Status.Active || s == fallback());\n        List<int> wrong = codes.map(Status.from);\n        return count(active) + count(wrong);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), ["19:27 invalid-local-assignment-value"]);
}

/// A constant expression may be a list or map literal, which a backed enum's case value cannot be, so the analyzer
/// reports one there as it reports the array of its PHP twin.
#[test]
fn a_list_or_map_literal_as_an_enum_case_value_is_reported_as_in_php() {
    let sharp =
        "namespace Demo;\n\nenum Status : string\n{\n    case Active = [\"a\"];\n    case Paused = [\"p\": 1];\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nenum Status: string\n{\n    case Active = ['a'];\n    case Paused = ['p' => 1];\n}\n";

    let php_issues = issues(("src/Demo/Status.php", php), &[]);
    assert_eq!(codes(&php_issues), ["invalid-enum-case-value", "invalid-enum-case-value"], "{php_issues:?}");
    assert_eq!(
        issues(("src/Demo/Status.sharp", sharp), &[]),
        ["5:19 invalid-enum-case-value", "6:19 invalid-enum-case-value"]
    );
}

const TOTALS: &str = "<?php\n\nnamespace Lib;\n\nfinal class Totals\n{\n    /** @param list<int> $values */\n    public static function sum(array $values): int\n    {\n        return array_sum($values);\n    }\n\n    /** @return list<int> */\n    public static function sizes(): array\n    {\n        return [1, 2];\n    }\n}\n";

#[test]
fn lists_and_maps_type_their_literals_indexes_and_loops_as_php_arrays() {
    let sharp = "namespace Demo;\n\nimport Lib.Totals;\n\nclass Order\n{\n    public List<Line> lines { get; private set; } = [];\n    private Map<string, int> counts = [:];\n\n    public int total(List<Line> extra)\n    {\n        List<Line> all = [new Line(1), extra[0]];\n        this.lines = all;\n        this.counts[\"lines\"] = count(this.lines);\n        const named = [\"a\": 1, \"b\": 2];\n        let sum = (named[\"a\"] ?? 0) + Totals.sum(Totals.sizes()) + all[1].cents;\n        for (const line of all) {\n            sum += line.cents;\n        }\n        for (const [name, size] of this.counts) {\n            if (name == \"lines\") {\n                sum += size;\n            }\n        }\n        for (const line of this.lines) {\n            sum += line.cents;\n        }\n        if (in_array(2, Totals.sizes(), true)) {\n            sum += 1;\n        }\n        return sum + (this.counts[\"lines\"] ?? 0);\n    }\n}\n\nclass Line\n{\n    public Line(public int cents { get; })\n    {\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Order.sharp", sharp), &[("src/Lib/Totals.php", TOTALS)]), Vec::<String>::new());
}

#[test]
fn a_wrong_element_type_is_reported_where_it_enters_the_collection() {
    let sharp = "namespace Demo;\n\nimport Lib.Totals;\n\nclass Order\n{\n    public List<Line> lines { get; private set; } = [];\n    private Map<string, int> counts = [:];\n\n    public List<int> wrong(List<Line> extra)\n    {\n        List<Line> all = [1];\n        Map<string, int> sizes = [\"a\": \"one\"];\n        this.counts[\"lines\"] = \"many\";\n        this.lines = [2];\n        Totals.sum(extra);\n        return extra;\n    }\n}\n\nclass Line\n{\n}\n";

    assert_eq!(
        issues(("src/Demo/Order.sharp", sharp), &[("src/Lib/Totals.php", TOTALS)]),
        [
            "12:26 invalid-local-assignment-value",
            "13:34 invalid-local-assignment-value",
            "14:9 invalid-property-assignment-value",
            "15:22 invalid-property-assignment-value",
            "16:20 invalid-argument",
            "17:16 invalid-return-statement",
        ]
    );
}

#[test]
fn any_never_holds_null_and_any_question_mark_may() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    private Any last = 0;\n\n    public Any keep(Any? value, Any sure)\n    {\n        this.keep(sure, sure);\n        this.keep(value, value);\n        Any held = value;\n        this.last = value;\n        this.keep(this.last, held);\n        return value;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Inbox.sharp", sharp), &[]),
        [
            "10:26 mixed-argument",
            "11:20 invalid-local-assignment-value",
            "12:21 invalid-property-assignment-value",
            "14:16 mixed-return-statement",
        ]
    );
}

/// Spec section 12 writes a change to a collection in a property back through the property's `set`, so code that
/// cannot reach the `set` cannot change the collection, as PHP refuses the same write when it runs.
#[test]
fn an_index_write_to_a_property_needs_its_set() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public Map<int, int> lines { get; private set; } = [:];\n    public Map<string, int> codes { get; }\n\n    public Order()\n    {\n        this.codes = [:];\n    }\n\n    public void change(Order other)\n    {\n        this.lines[0] = 1;\n        other.lines[0] = 1;\n        this.codes[\"a\"] = 1;\n    }\n}\n\nclass Shop\n{\n    public void change(Order order)\n    {\n        order.lines[0] = 1;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public private(set) array $lines = [];\n    public readonly array $codes;\n\n    public function __construct()\n    {\n        $this->codes = [];\n    }\n\n    public function change(Order $other): void\n    {\n        $this->lines[0] = 1;\n        $other->lines[0] = 1;\n        $this->codes['a'] = 1;\n    }\n}\n\nclass Shop\n{\n    public function change(Order $order): void\n    {\n        $order->lines[0] = 1;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Order.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Order.php", php), &[]);

    assert_eq!(sharp_issues, ["17:14 invalid-property-write", "25:15 invalid-property-write"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

/// A bare `x[i]` throws when the key is missing, which fits a `List`, whose keys run without gaps. A `Map` key is
/// often missing, so a `Map` is read with `??`, as spec section 12 decides. A compound assignment reads first.
#[test]
fn a_bare_index_read_on_a_map_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Prices\n{\n    private Map<string, int> prices = [\"pro\": 5];\n\n    public int read(Map<string, int> plans, List<int> sizes, string plan)\n    {\n        let total = sizes[0] + (plans[plan] ?? 0) + (this.prices[plan] ?? 0);\n        total += plans[plan];\n        this.prices[plan] += 1;\n        const named = [\"a\": 1];\n        return total + named[\"a\"];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Prices.sharp", sharp), &[]),
        [
            "10:18 possibly-undefined-array-index",
            "11:9 possibly-undefined-array-index",
            "13:24 possibly-undefined-array-index",
        ]
    );
}

#[test]
fn a_bare_index_read_on_a_map_keyed_by_a_backed_enum_is_an_error() {
    let sharp = "namespace Demo;\n\nimport Lib.Size;\nimport Lib.Status;\n\nclass Tally\n{\n    public int standing(Map<Status, int> counts, Map<Status, Map<Size, int>> nested, Status status, Size size)\n    {\n        counts[status] += 1;\n        counts[status]++;\n        return counts[status] + nested[status][size];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        [
            "10:9 possibly-undefined-array-index",
            "11:9 possibly-undefined-array-index",
            "12:16 possibly-undefined-array-index",
            "12:33 possibly-undefined-array-index",
            "12:33 possibly-undefined-array-index",
        ]
    );
}

#[test]
fn a_bare_map_read_names_the_forms_that_handle_a_missing_key() {
    let sharp =
        "namespace Demo;\n\nclass Tally\n{\n    public int count(Map<string, int> counts, string k) => counts[k];\n}\n";

    assert_eq!(
        worded(("src/Demo/Tally.sharp", sharp), &[]),
        [
            "5:60 possibly-undefined-array-index A `Map` is not read by a bare index, because its key may be missing. | This read throws when the key is missing. | Read it with `??`, as in `map[key] ?? fallback`, or with `map.get(key)`, which gives null for a missing key. `+=`, `++` and `--` read first, so write `m[k] = (m[k] ?? 0) + 1`."
        ]
    );
}

/// Spec section 12 handles a `Map` read where it is read, with `??`, `?.`, `is`, `as`, `match` or `get`. Each form
/// reads a parameter, a local and a property without a report.
#[test]
fn a_map_read_handled_by_coalescing_reports_nothing() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    private Map<string, int> stock = [\"a\": 1];\n\n    public int count(Map<string, int> counts, string k)\n    {\n        Map<string, int> local = [\"a\": 1];\n        return (counts[k] ?? 0) + (local[k] ?? 0) + (this.stock[k] ?? 0);\n    }\n}\n";

    assert_eq!(worded(("src/Demo/Tally.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_map_read_handled_by_a_null_safe_read_reports_nothing() {
    let sharp = "namespace Demo;\n\nclass Plan\n{\n    public Plan(public int price) { }\n}\n\nclass Tally\n{\n    private Map<string, Plan> stock = [:];\n\n    public int count(Map<string, Plan> plans, string k)\n    {\n        Map<string, Plan> local = [\"a\": new Plan(1)];\n        return (plans[k]?.price ?? 0) + (local[k]?.price ?? 0) + (this.stock[k]?.price ?? 0);\n    }\n}\n";

    assert_eq!(worded(("src/Demo/Tally.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_map_read_handled_by_is_reports_nothing() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    private Map<string, int> stock = [\"a\": 1];\n\n    public int count(Map<string, int> counts, string k)\n    {\n        Map<string, int> local = [\"a\": 1];\n        let total = 0;\n        if (counts[k] is int n) {\n            total += n;\n        }\n        if (local[k] is int m) {\n            total += m;\n        }\n        if (this.stock[k] is int s) {\n            total += s;\n        }\n        if (counts[k] is int) {\n            total += 1;\n        }\n        return total;\n    }\n}\n";

    assert_eq!(worded(("src/Demo/Tally.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_map_read_handled_by_is_not_reports_nothing() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    private Map<string, int> stock = [\"a\": 1];\n\n    public int count(Map<string, int> counts, string k)\n    {\n        Map<string, int> local = [\"a\": 1];\n        if (counts[k] is not int n) {\n            return 0;\n        }\n        if (local[k] is not int m) {\n            return n;\n        }\n        if (this.stock[k] is not int s) {\n            return n + m;\n        }\n        return n + m + s;\n    }\n}\n";

    assert_eq!(worded(("src/Demo/Tally.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_map_read_handled_by_as_reports_nothing() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    private Map<string, int> stock = [\"a\": 1];\n\n    public int count(Map<string, int> counts, string k)\n    {\n        Map<string, int> local = [\"a\": 1];\n        int? fromCounts = counts[k] as int;\n        int? fromLocal = local[k] as int;\n        int? fromStock = this.stock[k] as int;\n        return (fromCounts ?? 0) + (fromLocal ?? 0) + (fromStock ?? 0);\n    }\n}\n";

    assert_eq!(worded(("src/Demo/Tally.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_map_read_handled_by_match_reports_nothing() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    private Map<string, int> stock = [\"a\": 1];\n\n    public int count(Map<string, int> counts, string k)\n    {\n        Map<string, int> local = [\"a\": 1];\n        let total = match (counts[k]) {\n            int n => n,\n            default => 0,\n        };\n        total += match (local[k]) {\n            int m => m,\n            default => 0,\n        };\n        match (this.stock[k]) {\n            int s => {\n                total += s;\n            },\n            default => {},\n        }\n        total += match (k) {\n            \"a\" when counts[k] is int => 1,\n            default => 0,\n        };\n        return total;\n    }\n}\n";

    assert_eq!(worded(("src/Demo/Tally.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn a_map_read_handled_by_get_reports_nothing() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    private Map<string, int> stock = [\"a\": 1];\n\n    public int count(Map<string, int> counts, string k)\n    {\n        Map<string, int> local = [\"a\": 1];\n        return (counts.get(k) ?? 0) + (local.get(k) ?? 0) + (this.stock.get(k) ?? 0);\n    }\n}\n";

    assert_eq!(worded(("src/Demo/Tally.sharp", sharp), &[]), Vec::<String>::new());
}

/// A local that holds one of two `Map`s is a `Map`, so `is` handles its read and a bare read stays refused.
#[test]
fn a_read_of_a_local_holding_one_of_two_maps_is_handled_by_is() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    public int count(Map<string, int> a, Map<string, int> b, bool ready, string k)\n    {\n        let counts = ready ? a : b;\n        if (counts[k] is int n) {\n            return n;\n        }\n        if (counts[k] is not int m) {\n            return counts[k] ?? 0;\n        }\n        return m + counts[k];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tally.sharp", sharp), &[]), ["14:20 possibly-undefined-array-index"]);
}

/// The closest PHP to an `is` on a `Map` read tests the key with `isset` first. Its type test of an `int` entry stays
/// redundant, as PHP reports it.
#[test]
fn an_isset_test_before_a_type_test_of_a_php_array_entry_keeps_its_php_report() {
    let php = "<?php\n\nnamespace Demo;\n\nclass Tally\n{\n    /** @var array<string, int> */\n    private array $stock = ['a' => 1];\n\n    /** @param array<string, int> $counts */\n    public function count(array $counts, string $k): int\n    {\n        $local = ['a' => 1];\n        $total = 0;\n        if (isset($counts[$k]) && is_int($counts[$k])) {\n            $total += $counts[$k];\n        }\n        if (isset($local[$k]) && is_int($local[$k])) {\n            $total += $local[$k];\n        }\n        if (isset($this->stock[$k]) && is_int($this->stock[$k])) {\n            $total += $this->stock[$k];\n        }\n        return $total;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.php", php), &[]),
        [
            "15:35 redundant-type-comparison",
            "15:13 redundant-logical-operation",
            "18:34 redundant-type-comparison",
            "18:13 redundant-logical-operation",
            "21:40 redundant-type-comparison",
            "21:13 redundant-logical-operation",
        ]
    );
}

/// An index a pattern tests reads a missing key as null on every collection, as `??` and `?.` do, so a `List` read
/// that `is` tests may not match.
#[test]
fn a_list_read_tested_by_is_may_not_match() {
    let sharp = "namespace Demo;\n\nclass Sizes\n{\n    public int first(List<int> sizes, int i)\n    {\n        if (sizes[i] is int n) {\n            return n;\n        }\n        if (sizes[0] is not int m) {\n            return 0;\n        }\n        return m;\n    }\n}\n";

    assert_eq!(worded(("src/Demo/Sizes.sharp", sharp), &[]), Vec::<String>::new());
}

#[test]
fn coalescing_an_unchecked_any_gives_a_value_that_is_never_null() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    public Any pick(Any? maybe, Any sure) => maybe ?? sure;\n\n    public Any label(Any? maybe, string fallback)\n    {\n        Any shown = maybe ?? fallback;\n        return shown;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Inbox.sharp", sharp), &[]), Vec::<String>::new());
}

/// Spec section 18 lets a template show an `int`, a `float`, a `string` or a `bool`, and section 24 refuses a value
/// that may be null until it is checked. The PHP twin keeps PHP's own interpolation.
#[test]
fn a_template_refuses_every_type_but_int_float_string_and_bool() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Order\n{\n}\n\nclass Receipt\n{\n    public string status(Status status) => `Status: ${status}`;\n\n    public string items(List<int> items) => `Items: ${items}`;\n\n    public string order(Order order) => `Order: ${order}`;\n\n    public string count(int? count) => `Count: ${count}`;\n\n    public string value(Any value) => `Value: ${value}`;\n\n    public string maybe(Any? value) => `Maybe: ${value}`;\n\n    public string step(Function<int(int)> step) => `Step: ${step}`;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Status;\n\nclass Order\n{\n}\n\nclass Receipt\n{\n    public function status(Status $status): string { return \"Status: {$status}\"; }\n\n    /** @param list<int> $items */\n    public function items(array $items): string { return \"Items: {$items}\"; }\n\n    public function order(Order $order): string { return \"Order: {$order}\"; }\n\n    public function count(?int $count): string { return \"Count: {$count}\"; }\n\n    public function value(mixed $value): string { return \"Value: {$value}\"; }\n\n    public function step(\\Closure $step): string { return \"Step: {$step}\"; }\n}\n";
    let others = [("src/Lib/Status.php", STATUS)];

    assert_eq!(
        worded(("src/Demo/Receipt.sharp", sharp), &others),
        [
            "11:53 invalid-operand A template shows `int`, `float`, `string` or `bool`, and `Status` is none of them. | This is `Status`. | Show a value taken from it instead, such as a property or the result of a method.",
            "13:53 invalid-operand A template shows `int`, `float`, `string` or `bool`, and `List<int>` is none of them. | This is `List<int>`. | Show a value taken from it instead, such as a property or the result of a method.",
            "15:49 invalid-operand A template shows `int`, `float`, `string` or `bool`, and `Order` is none of them. | This is `Order`. | Show a value taken from it instead, such as a property or the result of a method.",
            "17:48 invalid-operand A template shows `int`, `float`, `string` or `bool`, and `int?` is none of them. | This is `int?`. | Test it with `!= null` first.",
            "19:47 invalid-operand A template shows `int`, `float`, `string` or `bool`, and `Any` is none of them. | This is `Any`. | Check what the value is with `is`, `as` or `match` first.",
            "21:48 invalid-operand A template shows `int`, `float`, `string` or `bool`, and `Any?` is none of them. | This is `Any?`. | Check what the value is with `is`, `as` or `match` first.",
            "23:59 invalid-operand A template shows `int`, `float`, `string` or `bool`, and `Function<int(int)>` is none of them. | This is `Function<int(int)>`. | Show a value taken from it instead, such as a property or the result of a method.",
        ]
    );
    assert_eq!(
        codes(&issues(("src/Demo/Receipt.php", php), &others)),
        ["invalid-type-cast", "array-to-string-conversion", "invalid-type-cast", "invalid-type-cast"]
    );
}

/// A literal or a narrowed type counts by its scalar type, so `1|2` is an `int` and `true` is a `bool`, and a template
/// shows a `bool` as `true` or `false`, which the checker reads as the template's text.
#[test]
fn a_template_shows_an_int_a_float_a_string_and_a_bool_and_what_narrows_to_one() {
    let sharp = "namespace Demo;\n\nclass Receipt\n{\n    public string show(int count, float ratio, string name, bool paid, int? maybe, Any value)\n    {\n        const level = paid ? 1 : 2;\n        const shown = `${count} ${ratio} ${name} ${paid} ${level} ${1} ${2.5} ${\"x\"} ${true}`;\n        if (maybe == null) {\n            return shown;\n        }\n        if (value is string) {\n            return `${shown} ${maybe} ${value}`;\n        }\n\n        return `${shown} ${maybe}`;\n    }\n\n    public int code(bool paid) => match (`${paid}`) {\n        \"true\" => 1,\n        default => 0,\n    };\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Receipt\n{\n    public function show(int $count, float $ratio, string $name, bool $paid, ?int $maybe, mixed $value): string\n    {\n        $level = $paid ? 1 : 2;\n        $shown = \"{$count} {$ratio} {$name} {$paid} {$level}\";\n        if ($maybe === null) {\n            return $shown;\n        }\n        if (is_string($value)) {\n            return \"{$shown} {$maybe} {$value}\";\n        }\n\n        return \"{$shown} {$maybe}\";\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Receipt.sharp", sharp), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Receipt.php", php), &[]), Vec::<String>::new());
}

#[test]
fn a_null_default_needs_a_type_written_with_a_question_mark() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    public Any first = null;\n    public string name = null;\n    public int? count = null;\n    public Any? maybe = null;\n\n    public Any keep(Any value = null) => value;\n\n    public string named(string value = null) => value;\n\n    public string? label(string? value = null) => value;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Inbox\n{\n    public mixed $first = null;\n\n    public function keep(mixed $value = null): mixed\n    {\n        return $value;\n    }\n\n    public function named(string $value = null): ?string\n    {\n        return $value;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Inbox.sharp", sharp), &[]),
        [
            "5:24 invalid-property-default-value",
            "6:26 invalid-property-default-value",
            "10:33 invalid-parameter-default-value",
            "12:40 invalid-parameter-default-value",
        ]
    );
    assert_eq!(issues(("src/Demo/Inbox.php", php), &[]), Vec::<String>::new());
}

#[test]
fn a_static_field_a_constant_and_a_computed_property_of_type_any_never_hold_null() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    public const Any NONE = null;\n\n    private static Any last = 0;\n\n    public Any latest => Inbox.pick(null);\n\n    public static Any? pick(Any? value)\n    {\n        Inbox.last = value;\n        return Inbox.last;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Inbox.sharp", sharp), &[]),
        ["5:29 invalid-constant-value", "9:26 mixed-return-statement", "13:9 invalid-property-assignment-value"]
    );
}

#[test]
fn a_collection_of_any_never_holds_null_and_a_collection_of_any_question_mark_may() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    private Map<string, Any> sure = [:];\n    private Map<string, Any?> maybe = [:];\n\n    public void keep(string key, Any? value, Any held, List<Any> items)\n    {\n        this.sure[key] = value;\n        this.sure = [key: value];\n        items.add(value);\n        items.set(0, value);\n        this.maybe[key] = value;\n        this.maybe = [key: value];\n        this.sure[key] = held;\n        items.add(held);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Inbox.sharp", sharp), &[]),
        [
            "10:9 invalid-property-assignment-value",
            "11:21 invalid-property-assignment-value",
            "12:19 mixed-argument",
            "13:22 mixed-argument",
        ]
    );
}

#[test]
fn a_php_non_null_mixed_keeps_accepting_a_value_that_may_be_null() {
    let php = "<?php\n\nnamespace Demo;\n\nclass Inbox\n{\n    /** @var non-empty-mixed */\n    private mixed $last = 1;\n\n    /** @var non-empty-mixed */\n    private static mixed $first = 1;\n\n    /**\n     * @param non-empty-mixed $sure\n     *\n     * @return non-empty-mixed\n     */\n    public function keep(mixed $value, mixed $sure): mixed\n    {\n        $this->keep($value, $value);\n        $this->last = $value;\n        $this->keep($this->last, $sure);\n        self::$first = $value;\n        $this->keep(self::$first, $sure);\n        return $value;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Inbox.php", php), &[]), Vec::<String>::new());
}

#[test]
fn an_unchecked_any_compares_with_a_string_or_a_number_but_not_with_another_value() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    public bool same(Any? value, Any? other, Inbox inbox)\n    {\n        const a = value == \"1\";\n        const b = value != 2;\n        const c = 1.5 == value;\n        const d = value == other;\n        const e = value == inbox;\n        return a && b && c && d && e;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Inbox.sharp", sharp), &[]),
        ["10:19 mixed-operand", "10:28 mixed-operand", "11:19 invalid-operand"]
    );
}

/// `?.` reads a missing key as null, as `??` does, because `x[k]?.name` runs as `($x[$k] ?? null)?->name`.
#[test]
fn a_null_safe_access_on_an_index_reads_a_missing_key_as_null() {
    let sharp = "namespace Demo;\n\nclass Shelf\n{\n    public string? first(Map<string, Item> items, string key)\n    {\n        return items[key]?.name ?? items[key]?.label();\n    }\n}\n\nclass Item\n{\n    public Item(public string name { get; })\n    {\n    }\n\n    public string? label()\n    {\n        return null;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Shelf.sharp", sharp), &[]), Vec::<String>::new());
}

/// Writing `x[i] = v` to a `List` could leave a gap in its keys, so spec section 12 writes a `List` with `set` and
/// `add`. Every step of the target that is a `List` is reported.
#[test]
fn an_index_write_to_a_list_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Sizes\n{\n    public void change(List<int> sizes, Map<string, List<int>> groups, List<Map<string, int>> rows)\n    {\n        sizes[0] = 1;\n        sizes[0] += 1;\n        groups[\"a\"][0] = 1;\n        rows[0][\"a\"] = 1;\n        groups[\"b\"] = [1];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Sizes.sharp", sharp), &[]),
        [
            "7:9 invalid-array-access",
            "8:9 invalid-array-access",
            "9:9 invalid-array-access",
            "10:9 invalid-array-access"
        ]
    );
}

/// A `List`'s keys are the `int`s from 0 to its last index, so a read whose index may be anything else is an error,
/// as an unchecked `Any` is (spec section 24). A plain PHP `list<int>` keeps reading a wider key, as PHP does.
#[test]
fn a_list_read_with_an_index_that_may_not_be_an_int_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Lookup\n{\n    public int read(List<int> items, Any sure, Any? maybe, int|string either, int? missing, string name)\n    {\n        const a = items[sure];\n        const b = items[maybe];\n        const c = items[either];\n        const d = items[missing];\n        const e = items[name];\n        return a + b + c + d + e;\n    }\n\n    public string show(List<List<int>> rows, List<string> names, int|string key)\n    {\n        return `${names[key]} ${rows[0][key]}`;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Lookup\n{\n    /**\n     * @param list<int> $items\n     */\n    public function read(array $items, mixed $sure, mixed $maybe, int|string $either, ?int $missing, string $name): int\n    {\n        $a = $items[$sure];\n        $b = $items[$maybe];\n        $c = $items[$either];\n        $d = $items[$missing];\n        $e = $items[$name];\n        return $a + $b + $c + $d + $e;\n    }\n\n    /**\n     * @param list<list<int>> $rows\n     * @param list<string> $names\n     */\n    public function show(array $rows, array $names, int|string $key): string\n    {\n        return \"{$names[$key]} {$rows[0][$key]}\";\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Lookup.php", php), &[]),
        ["15:21 possibly-null-array-index", "16:21 mismatched-array-index"]
    );

    let sharp_issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Lookup.sharp", sharp), &[]);
    let located_issues: Vec<String> = sharp_issues.iter().map(|issue| located(sharp, issue)).collect();

    assert_eq!(
        located_issues,
        [
            "7:19 mismatched-array-index",
            "8:19 mismatched-array-index",
            "9:19 mismatched-array-index",
            "10:19 mismatched-array-index",
            "11:19 mismatched-array-index",
            "17:19 mismatched-array-index",
            "17:33 mismatched-array-index",
        ]
    );
    assert_eq!(
        sharp_issues.iter().map(|issue| issue.message.as_str()).collect::<Vec<_>>()[..3],
        [
            "`List<int>` is indexed by `int`, but this index is `Any`.",
            "`List<int>` is indexed by `int`, but this index is `Any?`.",
            "`List<int>` is indexed by `int`, but this index is `int|string`.",
        ]
    );
    assert_eq!(
        sharp_issues[0].primary_annotation().and_then(|annotation| annotation.message.as_deref()),
        Some("This index may not be an `int`.")
    );
    assert_eq!(
        sharp_issues[0].help.as_deref(),
        Some("Check the index with `is int` first, as in `if (index is int) { … }`.")
    );
}

/// An index of a type `int` contains, such as an `int` literal or a loop counter, reads a `List` as `int` does. A
/// handled read takes an `int` index too.
#[test]
fn a_list_read_with_an_index_of_a_type_int_contains_is_accepted_and_a_handled_read_checks_its_index_too() {
    let sharp = "namespace Demo;\n\nclass Lookup\n{\n    public int read(List<int> items, int index, int|string either)\n    {\n        let total = items[index] + items[0] + (items[index] ?? 0);\n        for (let i = 0; i < count(items); i++) {\n            total += items[i];\n        }\n        return total + (items[either] ?? 0);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Lookup\n{\n    /**\n     * @param list<int> $items\n     */\n    public function read(array $items, int $index, int|string $either): int\n    {\n        $total = $items[$index] + $items[0] + ($items[$index] ?? 0);\n        for ($i = 0; $i < count($items); $i++) {\n            $total += $items[$i];\n        }\n        return $total + ($items[$either] ?? 0);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Lookup.php", php), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Lookup.sharp", sharp), &[]), ["11:25 mismatched-array-index"]);
}

/// `strict_list_index_checks` asks a plain PHP `list<int>` for a `non-negative-int` index. A PHP# `List` is indexed by
/// any `int`, and the engine throws `OutOfRangeException` for `items[-1]` as for any index past the end.
#[test]
fn a_list_read_takes_any_int_index_under_strict_list_index_checks() {
    let sharp = "namespace Demo;\n\nclass Lookup\n{\n    public int read(List<int> items, int index) => items[index] + items[-1];\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Lookup\n{\n    /**\n     * @param list<int> $items\n     */\n    public function read(array $items, int $index): int\n    {\n        return $items[$index] + $items[-1];\n    }\n}\n";
    let strict = || Settings { strict_list_index_checks: true, ..settings() };

    assert_eq!(issues_with(strict(), ("src/Demo/Lookup.php", php), &[]), ["12:40 mismatched-array-index"]);
    assert_eq!(issues_with(strict(), ("src/Demo/Lookup.sharp", sharp), &[]), Vec::<String>::new());
}

/// A `Map` key is an `int`, a `string` or a backed enum, as the `Map` declares it (spec section 12), and never null. A
/// handled read takes the key a bare read takes, so a key that may be null or of another type is an error. A plain PHP
/// `array<string, int>` keeps reading a handled key of any array key type, as PHP does.
#[test]
fn a_map_read_with_a_key_outside_its_key_type_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    public int read(Map<string, int> counts, string? name, int id, int|string either)\n    {\n        const a = counts[name] ?? 0;\n        const b = counts[id] ?? 0;\n        const c = counts[either] ?? 0;\n        const d = isset(counts[name]) ? 1 : 0;\n        return a + b + c + d;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Tally\n{\n    /**\n     * @param array<string, int> $counts\n     */\n    public function read(array $counts, ?string $name, int $id, int|string $either): int\n    {\n        $a = $counts[$name] ?? 0;\n        $b = $counts[$id] ?? 0;\n        $c = $counts[$either] ?? 0;\n        $d = isset($counts[$name]) ? 1 : 0;\n        return $a + $b + $c + $d;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.php", php), &[]),
        ["12:22 possibly-null-array-index", "15:28 possibly-null-array-index"]
    );

    let sharp_issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Tally.sharp", sharp), &[]);
    let located_issues: Vec<String> = sharp_issues.iter().map(|issue| located(sharp, issue)).collect();

    assert_eq!(
        located_issues,
        [
            "7:26 mismatched-array-index",
            "8:26 mismatched-array-index",
            "9:26 mismatched-array-index",
            "10:32 mismatched-array-index",
        ]
    );
    assert_eq!(
        sharp_issues.iter().map(|issue| issue.message.as_str()).collect::<Vec<_>>()[..3],
        [
            "`Map<string, int>` is keyed by `string`, but this key is `string?`.",
            "`Map<string, int>` is keyed by `string`, but this key is `int`.",
            "`Map<string, int>` is keyed by `string`, but this key is `int|string`.",
        ]
    );
    assert_eq!(
        sharp_issues[0].primary_annotation().and_then(|annotation| annotation.message.as_deref()),
        Some("This key may not be of type `string`.")
    );

    let either = "namespace Demo;\n\nclass Keys\n{\n    public int read(Map<int|string, int> counts, Any? value) => counts[value] ?? 0;\n}\n";
    let either_issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Keys.sharp", either), &[]);

    assert_eq!(
        either_issues.iter().map(|issue| (located(either, issue), issue.help.as_deref())).collect::<Vec<_>>(),
        [(
            "5:72 mismatched-array-index".to_owned(),
            Some("Check the key with `is int or string` first, as in `if (name is int or string) { … }`.")
        )]
    );
}

/// A key of the `Map`'s key type reads it, handled or not: a `string` reads a `Map<string, int>` and a `Map` literal,
/// and a `Status` reads a `Map<Status, int>`, which runs as its backing value.
#[test]
fn a_map_read_with_a_key_of_its_key_type_is_accepted() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Tally\n{\n    public int read(Map<string, int> counts, Map<Status, int> states, string name, Status status)\n    {\n        const named = [\"a\": 1];\n        const a = counts[name] ?? 0;\n        const b = states[status] ?? 0;\n        const c = isset(counts[name]) ? 1 : 0;\n        const d = named[name] ?? 0;\n        return a + b + c + d;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Status;\n\nclass Tally\n{\n    /**\n     * @param array<string, int> $counts\n     * @param array<string, int> $states\n     */\n    public function read(array $counts, array $states, string $name, Status $status): int\n    {\n        $named = ['a' => 1];\n        $a = $counts[$name] ?? 0;\n        $b = $states[$status->value] ?? 0;\n        $c = isset($counts[$name]) ? 1 : 0;\n        $d = $named[$name] ?? 0;\n        return $a + $b + $c + $d;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tally.php", php), &[("src/Lib/Status.php", STATUS)]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]), Vec::<String>::new());

    let checked = "namespace Demo;\n\nimport Lib.Status;\n\nclass Keys\n{\n    public int named(Map<string, int> counts, Any? value)\n    {\n        if (value is string) {\n            return counts[value] ?? 0;\n        }\n        return 0;\n    }\n\n    public int numbered(Map<int, int> counts, Any? value)\n    {\n        if (value is int) {\n            return counts[value] ?? 0;\n        }\n        return 0;\n    }\n\n    public int stated(Map<Status, int> counts, Any? value)\n    {\n        if (value is Status) {\n            return counts[value] ?? 0;\n        }\n        return 0;\n    }\n\n    public int either(Map<int|string, int> counts, Any? value)\n    {\n        if (value is int or string) {\n            return counts[value] ?? 0;\n        }\n        return 0;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Keys.sharp", checked), &[("src/Lib/Status.php", STATUS)]), Vec::<String>::new());
}

/// A bare `Map` read is refused for its missing key, and its key is checked as a handled read's is.
#[test]
fn a_bare_map_read_with_a_key_outside_its_key_type_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    public int read(Map<string, int> counts, string? name, int id)\n    {\n        return counts[name] + counts[id];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Tally\n{\n    /**\n     * @param array<string, int> $counts\n     */\n    public function read(array $counts, ?string $name, int $id): int\n    {\n        return $counts[$name] + $counts[$id];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.php", php), &[]),
        ["12:24 possibly-null-array-index", "12:41 mismatched-array-index"]
    );

    let sharp_issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Tally.sharp", sharp), &[]);
    let located_issues: Vec<String> = sharp_issues.iter().map(|issue| located(sharp, issue)).collect();

    assert_eq!(
        located_issues,
        [
            "7:23 mismatched-array-index",
            "7:16 possibly-undefined-array-index",
            "7:38 mismatched-array-index",
            "7:31 possibly-undefined-array-index",
        ]
    );
    assert_eq!(sharp_issues[2].message, "`Map<string, int>` is keyed by `string`, but this key is `int`.");
}

/// `for (const [k, v] of x)` reads the keys of a `Map`. A `List`'s indexes come from `entries()`, as spec section 12
/// writes. The key of a `Map<string, V>` reads back as a `string`, because the lowering casts a key PHP stored as an
/// `int`.
#[test]
fn a_key_and_value_loop_reads_a_map_and_its_keys_as_the_map_types_them() {
    let sharp = "namespace Demo;\n\nclass Loops\n{\n    public int run(List<int> sizes, Map<string, int> counts)\n    {\n        let total = 0;\n        for (const [index, size] of sizes) {\n            total += index + size;\n        }\n        for (const [name, count] of counts) {\n            total += strlen(name) + count;\n        }\n        return total;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Loops.sharp", sharp), &[]), ["8:37 invalid-iterator"]);
}

/// `tags = []` empties a `List<string>` and leaves it a `List`, as its methods are: its bare index read stays bare,
/// and `for (const [k, v] of tags)` still reads the keys of a `Map`, which a `List` is not.
#[test]
fn an_emptied_list_stays_a_list_for_an_index_read_and_a_key_and_value_loop() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public int count()\n    {\n        List<string> tags = [\"a\"];\n        tags = [];\n        tags.add(\"x\");\n        let total = 0;\n        for (const [index, tag] of tags) {\n            total += index + strlen(tag);\n        }\n        return total + strlen(tags[0]);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), ["11:36 invalid-iterator"]);
}

/// A parameter and a property emptied by `[]` stay the `List`s they are declared, before any method fills them.
#[test]
fn an_emptied_list_parameter_or_property_stays_a_list() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public List<string> names = [\"a\"];\n\n    public int count(List<string> tags)\n    {\n        tags = [];\n        this.names = [];\n        let total = 0;\n        for (const [index, tag] of tags) {\n            total += index + strlen(tag);\n        }\n        for (const [index, name] of this.names) {\n            total += index + strlen(name);\n        }\n        return total + strlen(tags[0]) + strlen(this.names[0]);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), ["12:36 invalid-iterator", "15:37 invalid-iterator"]);
}

/// A static property emptied by `[]` stays the `List` it is declared, as an instance property does.
#[test]
fn an_emptied_static_list_property_stays_a_list() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public static List<string> names = [\"a\"];\n\n    public int count()\n    {\n        Tags.names = [];\n        let total = 0;\n        for (const [i, n] of Tags.names) {\n            total += i + strlen(n);\n        }\n        return total + strlen(Tags.names[0]);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), ["11:30 invalid-iterator"]);
}

/// A parameter's default `[]` and a returned `[]` narrow nothing: the parameter and the call are the `List`s they are
/// declared.
#[test]
fn an_empty_list_default_or_return_stays_a_list() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public int count(List<string> tags = [])\n    {\n        let total = 0;\n        for (const [index, tag] of tags) {\n            total += index + strlen(tag);\n        }\n        for (const [index, tag] of this.none()) {\n            total += index + strlen(tag);\n        }\n        return total + strlen(tags[0]) + strlen(this.none()[0]);\n    }\n\n    private List<string> none()\n    {\n        return [];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), ["8:36 invalid-iterator", "11:36 invalid-iterator"]);
}

/// `field = []` in an accessor empties the property's storage, which stays the `List` the property is declared.
#[test]
fn an_emptied_list_field_stays_a_list() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public List<string> names {\n        get;\n        set {\n            field = [];\n            for (const name of value) {\n                field.add(name + field[0]);\n            }\n        }\n    } = [];\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), Vec::<String>::new());
}

/// `counts = []` empties a `Map<string, int>` and leaves it a `Map` of those types: `for (const [k, v] of counts)`
/// reads its keys and values, and a bare `counts[k]` read is refused, as on any `Map`.
#[test]
fn an_emptied_map_keeps_the_map_rules() {
    let sharp = "namespace Demo;\n\nclass Counts\n{\n    public int total()\n    {\n        Map<string, int> counts = [\"a\": 1];\n        counts = [:];\n        let total = 0;\n        for (const [name, count] of counts) {\n            total += strlen(name) + count;\n        }\n        counts.delete(\"a\");\n        return total + counts[\"b\"];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Counts.sharp", sharp), &[]), ["14:24 possibly-undefined-array-index"]);
}

/// A `List` literal assigned to a `Map<int, string>` is an error, and the place stays the `Map` it is declared: its
/// keys are read by `[k, v]`, and a bare read is refused.
#[test]
fn a_list_literal_assigned_to_a_map_is_an_error_and_the_map_rules_stay() {
    let sharp = "namespace Demo;\n\nclass Names\n{\n    public int total(Map<int, string> names)\n    {\n        names = [\"a\"];\n        let total = 0;\n        for (const [id, name] of names) {\n            total += id + strlen(name);\n        }\n        return total + strlen(names[0]);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Names.sharp", sharp), &[]),
        ["7:17 invalid-array-element", "12:31 possibly-undefined-array-index"]
    );
}

/// A method that changes a `List` leaves it the `List` it is declared, so an index past the literal it held reads it.
#[test]
fn a_changed_list_reads_past_the_literal_it_held() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public int count(List<string> tags, Map<string, int> counts)\n    {\n        tags = [\"a\"];\n        tags.add(\"b\");\n        counts = [\"a\": 1];\n        counts.delete(\"a\");\n        return strlen(tags[1]) + (counts[\"a\"] ?? 0);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), Vec::<String>::new());
}

/// PHP has no `List`: `$tags = []` narrows a `list<string>` parameter to an empty array, whose read gives no `string`.
#[test]
fn an_emptied_php_array_narrows_to_an_empty_array() {
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @param list<string> $tags */\n    public function first(array $tags): string\n    {\n        $tags = [];\n\n        return $tags[0];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tags.php", php), &[]),
        ["12:22 mismatched-array-index", "12:16 invalid-return-statement"]
    );
}

/// `[]` is an empty `List`, so it is an error wherever a `Map` is declared: a property default, a parameter default,
/// a declaration, an assignment, an argument and a return.
#[test]
fn an_empty_list_literal_where_a_map_is_declared_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Counts\n{\n    public Map<string, int> byName = [];\n\n    public Map<string, int> run(Map<string, int> seed = [])\n    {\n        Map<string, int> counts = [];\n        counts = [];\n        this.keep(counts);\n        this.keep(seed);\n        this.keep(this.none());\n        this.keep([]);\n        return [];\n    }\n\n    private Map<string, int> none() => [];\n\n    private void keep(Map<string, int> counts)\n    {\n        this.byName = counts;\n    }\n}\n";

    let message = "invalid-array-element `[]` is an empty List. An empty Map is written `[:]`.";
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counts.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [
            format!("5:38 {message}"),
            format!("7:57 {message}"),
            format!("9:35 {message}"),
            format!("10:18 {message}"),
            format!("14:19 {message}"),
            format!("15:16 {message}"),
            format!("18:40 {message}"),
        ]
    );
}

/// PHP has one empty array, so a typed PHP place takes `[]` wherever an `array<string, int>` is declared.
#[test]
fn an_empty_php_array_where_a_string_keyed_array_is_declared_is_accepted() {
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Counts\n{\n    /** @var array<string, int> */\n    public array $byName = [];\n\n    /**\n     * @param array<string, int> $seed\n     *\n     * @return array<string, int>\n     */\n    public function run(array $seed = []): array\n    {\n        /** @var array<string, int> $counts */\n        $counts = [];\n        $counts = [];\n        $this->keep($counts);\n        $this->keep($seed);\n        $this->keep($this->none());\n        $this->keep([]);\n\n        return [];\n    }\n\n    /** @return array<string, int> */\n    private function none(): array\n    {\n        return [];\n    }\n\n    /** @param array<string, int> $counts */\n    private function keep(array $counts): void\n    {\n        $this->byName = $counts;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Counts.php", php), &[]), Vec::<String>::new());
}

/// `[:]` is an empty `Map`, so it is an error wherever a `List` is declared: a property default, a parameter default,
/// a declaration, an assignment, an argument and a return.
#[test]
fn an_empty_map_literal_where_a_list_is_declared_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public List<string> all = [:];\n\n    public List<string> run(List<string> seed = [:])\n    {\n        List<string> tags = [:];\n        tags = [:];\n        this.keep(tags);\n        this.keep(seed);\n        this.keep(this.none());\n        this.keep([:]);\n        return [:];\n    }\n\n    private List<string> none() => [:];\n\n    private void keep(List<string> tags)\n    {\n        this.all = tags;\n    }\n}\n";

    let message = "invalid-array-element `[:]` is an empty Map. An empty List is written `[]`.";
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Tags.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [
            format!("5:31 {message}"),
            format!("7:49 {message}"),
            format!("9:29 {message}"),
            format!("10:16 {message}"),
            format!("14:19 {message}"),
            format!("15:16 {message}"),
            format!("18:36 {message}"),
        ]
    );
}

/// `[:]` and `[key: value]` are `Map` literals, so each is an error wherever a `Set` is declared: a property default, a
/// parameter default, a declaration, an assignment, an argument and a return. `[]` and `["a"]` are the `Set`. PHP has
/// one empty array, and its array of a `Set`'s elements takes `[]`.
#[test]
fn a_map_literal_where_a_set_is_declared_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public Set<string> all = [:];\n\n    public Set<string> run(Set<string> seed = [:])\n    {\n        Set<string> tags = [:];\n        tags = [\"a\": \"a\"];\n        this.keep([:]);\n        this.keep([]);\n        this.keep([\"a\"]);\n        return [:];\n    }\n\n    private void keep(Set<string> tags)\n    {\n        this.all = tags;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @var array<string, string> */\n    public array $all = [];\n\n    public function run(): void\n    {\n        $this->all = [];\n    }\n}\n";

    let empty = "invalid-array-element `[:]` is an empty Map. An empty Set is written `[]`.";
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Tags.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [
            format!("5:30 {empty}"),
            format!("7:47 {empty}"),
            format!("9:28 {empty}"),
            "10:16 invalid-array-element A Set literal is written `[a, b]`.".to_owned(),
            format!("11:19 {empty}"),
            format!("14:16 {empty}"),
        ]
    );
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// A `List` and a `Set` each take a `List` literal and neither takes a `Map` literal, so a place declared as either
/// refuses `[:]`. PHP has one empty array, and its array of either takes `[]`.
#[test]
fn a_place_declared_a_list_or_a_set_refuses_an_empty_map() {
    let sharp =
        "namespace Demo;\n\nclass Tags\n{\n    public List<int>|Set<int> ids()\n    {\n        return [:];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @return list<int>|array<int, int> */\n    public function ids(): array\n    {\n        return [];\n    }\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Tags.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["7:16 invalid-array-element `[:]` is an empty Map. An empty List is written `[]`."]
    );
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// A `Set` takes a `List` literal, so a place declared a `Map` or a `Set` takes `[1]` as the `Set`. PHP has one kind of
/// array, and its array of either takes `[1]`.
#[test]
fn a_place_declared_a_map_or_a_set_takes_a_list_literal() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public Map<string, int>|Set<int> ids()\n    {\n        return [1];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @return array<string, int>|array<int, int> */\n    public function ids(): array\n    {\n        return [1];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// A local or a property declared a `Set` or a `Map` takes a literal of either, whichever it names first: `[]` and
/// `[1]` are the `Set`, and `[:]` and `["a": 1]` the `Map`. PHP has one kind of array, and its array of either takes
/// every literal.
#[test]
fn a_local_or_a_property_declared_a_set_or_a_map_takes_either_literal() {
    let set_first = "namespace Demo;\n\nclass Tags\n{\n    private Set<int>|Map<string, int> kept = [1];\n\n    public int ids()\n    {\n        Set<int>|Map<string, int> tags = [\"a\": 1];\n        tags = [1];\n        tags = [\"a\": 1];\n        tags = [];\n        tags = [:];\n        this.kept = [1];\n        this.kept = [\"a\": 1];\n        this.kept = [];\n        this.kept = [:];\n        return count(tags) + count(this.kept);\n    }\n}\n";
    let map_first = "namespace Demo;\n\nclass Tags\n{\n    private Map<string, int>|Set<int> kept = [\"a\": 1];\n\n    public int ids()\n    {\n        Map<string, int>|Set<int> tags = [1];\n        tags = [1];\n        tags = [\"a\": 1];\n        tags = [];\n        tags = [:];\n        this.kept = [1];\n        this.kept = [\"a\": 1];\n        this.kept = [];\n        this.kept = [:];\n        return count(tags) + count(this.kept);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @var array<string, int>|array<int, int> */\n    private array $kept = [1];\n\n    public function ids(): int\n    {\n        /** @var array<string, int>|array<int, int> $tags */\n        $tags = [1];\n        $tags = ['a' => 1];\n        $tags = [];\n        $this->kept = [1];\n        $this->kept = ['a' => 1];\n        $this->kept = [];\n        return count($tags) + count($this->kept);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", set_first), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Tags.sharp", map_first), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// A local whose first value may be a `Map` or a `Set` is either from then on, so it takes a literal of either. An
/// emptied local holds the collection its literal names: `[]` is the `Set`, which a `Set` return takes, and
/// `["a": 1]` is the `Map`, which a `Map` local takes. PHP has one kind of array.
#[test]
fn a_local_that_may_be_a_map_or_a_set_takes_either_literal_and_keeps_its_kind() {
    let map_first = "namespace Demo;\n\nclass Tags\n{\n    public Set<int> pick(bool ready, Map<string, int> counts, Set<int> ids)\n    {\n        let tags = ready ? counts : ids;\n        tags = [\"a\": 1];\n        Map<string, int> named = tags;\n        tags = [1];\n        tags = [];\n        return count(named) > 0 ? tags : ids;\n    }\n}\n";
    let set_first = "namespace Demo;\n\nclass Tags\n{\n    public Set<int> pick(bool ready, Map<string, int> counts, Set<int> ids)\n    {\n        let tags = ready ? ids : counts;\n        tags = [\"a\": 1];\n        Map<string, int> named = tags;\n        tags = [1];\n        tags = [];\n        return count(named) > 0 ? tags : ids;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /**\n     * @param array<string, int> $counts\n     * @param array<int, int> $ids\n     *\n     * @return array<int, int>\n     */\n    public function pick(bool $ready, array $counts, array $ids): array\n    {\n        $tags = $ready ? $counts : $ids;\n        $tags = ['a' => 1];\n        $tags = [1];\n        $tags = [];\n        return $tags;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", map_first), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Tags.sharp", set_first), &[]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// PHP has one empty array, so a typed PHP place takes `[]` wherever a `list<string>` is declared.
#[test]
fn an_empty_php_array_where_a_list_is_declared_is_accepted() {
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @var list<string> */\n    public array $all = [];\n\n    /**\n     * @param list<string> $seed\n     *\n     * @return list<string>\n     */\n    public function run(array $seed = []): array\n    {\n        /** @var list<string> $tags */\n        $tags = [];\n        $tags = [];\n        $this->keep($tags);\n        $this->keep($seed);\n        $this->keep($this->none());\n        $this->keep([]);\n\n        return [];\n    }\n\n    /** @return list<string> */\n    private function none(): array\n    {\n        return [];\n    }\n\n    /** @param list<string> $tags */\n    private function keep(array $tags): void\n    {\n        $this->all = $tags;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// `["a"]` is a `List` literal, so it is an error wherever a `Map` is declared, even a `Map<int, string>` whose types
/// it fits: a property default, a parameter default, a declaration, an assignment, an argument and a return.
#[test]
fn a_list_literal_where_a_map_is_declared_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Names\n{\n    public Map<int, string> byId = [\"a\"];\n\n    public Map<int, string> run(Map<int, string> seed = [\"a\"])\n    {\n        Map<int, string> names = [\"a\"];\n        names = [\"a\"];\n        this.keep(names);\n        this.keep(seed);\n        this.keep(this.none());\n        this.keep([\"a\"]);\n        return [\"a\"];\n    }\n\n    private Map<int, string> none() => [\"a\"];\n\n    private void keep(Map<int, string> names)\n    {\n        this.byId = names;\n    }\n}\n";

    let message = "invalid-array-element A Map literal is written `[key: value]`.";
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Names.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [
            format!("5:36 {message}"),
            format!("7:57 {message}"),
            format!("9:34 {message}"),
            format!("10:17 {message}"),
            format!("14:19 {message}"),
            format!("15:16 {message}"),
            format!("18:40 {message}"),
        ]
    );
}

/// PHP has one array, so a typed PHP place takes `['a']` wherever an `array<int, string>` is declared.
#[test]
fn a_php_list_where_an_int_keyed_array_is_declared_is_accepted() {
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Names\n{\n    /** @var array<int, string> */\n    public array $byId = ['a'];\n\n    /**\n     * @param array<int, string> $seed\n     *\n     * @return array<int, string>\n     */\n    public function run(array $seed = ['a']): array\n    {\n        /** @var array<int, string> $names */\n        $names = ['a'];\n        $names = ['a'];\n        $this->keep($names);\n        $this->keep($seed);\n        $this->keep($this->none());\n        $this->keep(['a']);\n\n        return ['a'];\n    }\n\n    /** @return array<int, string> */\n    private function none(): array\n    {\n        return ['a'];\n    }\n\n    /** @param array<int, string> $names */\n    private function keep(array $names): void\n    {\n        $this->byId = $names;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Names.php", php), &[]), Vec::<String>::new());
}

/// `[0: 1]` is a `Map` literal, so it is an error wherever a `List` is declared, even where its keys run from 0 as a
/// `List`'s do: a property default, a parameter default, a declaration, an assignment, an argument and a return.
#[test]
fn a_map_literal_where_a_list_is_declared_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Sizes\n{\n    public List<int> all = [0: 1];\n\n    public List<int> run(List<int> seed = [0: 1])\n    {\n        List<int> sizes = [0: 1];\n        sizes = [0: 1];\n        this.keep(sizes);\n        this.keep(seed);\n        this.keep(this.none());\n        this.keep([0: 1]);\n        return [0: 1];\n    }\n\n    private List<int> none() => [0: 1];\n\n    private void keep(List<int> sizes)\n    {\n        this.all = sizes;\n    }\n}\n";

    let message = "invalid-array-element A List literal is written `[a, b]`.";
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Sizes.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [
            format!("5:28 {message}"),
            format!("7:43 {message}"),
            format!("9:27 {message}"),
            format!("10:17 {message}"),
            format!("14:19 {message}"),
            format!("15:16 {message}"),
            format!("18:33 {message}"),
        ]
    );
}

/// PHP has one array, so a typed PHP place takes `[0 => 1]` wherever a `list<int>` is declared.
#[test]
fn a_php_array_with_list_keys_where_a_list_is_declared_is_accepted() {
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Sizes\n{\n    /** @var list<int> */\n    public array $all = [0 => 1];\n\n    /**\n     * @param list<int> $seed\n     *\n     * @return list<int>\n     */\n    public function run(array $seed = [0 => 1]): array\n    {\n        /** @var list<int> $sizes */\n        $sizes = [0 => 1];\n        $sizes = [0 => 1];\n        $this->keep($sizes);\n        $this->keep($seed);\n        $this->keep($this->none());\n        $this->keep([0 => 1]);\n\n        return [0 => 1];\n    }\n\n    /** @return list<int> */\n    private function none(): array\n    {\n        return [0 => 1];\n    }\n\n    /** @param list<int> $sizes */\n    private function keep(array $sizes): void\n    {\n        $this->all = $sizes;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Sizes.php", php), &[]), Vec::<String>::new());
}

/// A literal of the collection its place declares compiles at every site, as does a literal of spreads, which names
/// no collection of its own, and a literal passed to a plain PHP function, whose `array` is neither a `List` nor a
/// `Map`.
#[test]
fn a_literal_of_the_declared_collection_is_accepted() {
    let sharp = "namespace Demo;\n\nclass Clean\n{\n    public List<string> tags = [];\n    public Map<string, int> counts = [:];\n\n    public Map<string, int> run(List<string> seed = [], Map<string, int> more = [\"a\": 1])\n    {\n        List<string> a = [];\n        Map<string, int> m = [:];\n        Map<string, int> n = [\"a\": 1];\n        a = [\"x\"];\n        m = [:];\n        this.keep(a, [], m, [\"b\": 2]);\n        this.keep(seed, [...a, ...seed], more, [...m, \"c\": 3]);\n        this.tags = [\"y\"];\n        this.counts = [\"d\": 4];\n        return n;\n    }\n\n    public List<string> none() => [];\n\n    public Map<string, int> nothing()\n    {\n        return [:];\n    }\n\n    public string joined()\n    {\n        return implode(\",\", [\"a\", \"b\"]) + implode(\",\", []);\n    }\n\n    private void keep(List<string> a, List<string> b, Map<string, int> c, Map<string, int> d)\n    {\n        this.tags = [...a, ...b];\n        this.counts = [...c, ...d];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Clean.sharp", sharp), &[]), Vec::<String>::new());
}

/// A call through a local declared as a `Function` takes only literals of the collections its parameters declare.
#[test]
fn a_literal_of_the_other_collection_passed_to_a_function_local_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Counter\n{\n    public int run()\n    {\n        Function<int(List<string>)> size = names => count(names);\n        return size([:]);\n    }\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counter.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["8:21 invalid-array-element `[:]` is an empty Map. An empty List is written `[]`."]
    );
}

/// A call through a `Function` parameter takes only literals of the collections its parameters declare.
#[test]
fn a_literal_of_the_other_collection_passed_to_a_function_parameter_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Counter\n{\n    public int run(Function<int(Map<string, int>)> total) => total([]);\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counter.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["5:68 invalid-array-element `[]` is an empty List. An empty Map is written `[:]`."]
    );
}

/// A call through a `Function` property takes only literals of the collections its parameters declare.
#[test]
fn a_literal_of_the_other_collection_passed_to_a_function_property_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Counter\n{\n    private Function<int(List<int>)> size;\n\n    public Counter()\n    {\n        this.size = sizes => count(sizes);\n    }\n\n    public int run() => this.size([0: 1]);\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counter.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["12:35 invalid-array-element A List literal is written `[a, b]`."]
    );
}

/// A call through a local, a parameter or a property declared as a `Function` takes the literals its parameters
/// declare, and a PHP `\Closure` declares no collection, so a call through one takes either literal.
#[test]
fn a_literal_of_the_declared_collection_passed_to_a_function_value_is_accepted() {
    let hooks = "<?php\n\nnamespace Lib;\n\nfinal class Hooks\n{\n    public \\Closure $run;\n\n    public function __construct()\n    {\n        $this->run = fn (mixed ...$values): int => 0;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Hooks;\n\nclass Counter\n{\n    private Function<int(List<int>)> size;\n\n    public Counter()\n    {\n        this.size = sizes => count(sizes);\n    }\n\n    public int run(Function<int(Map<string, int>)> total, Hooks hooks)\n    {\n        Function<int(List<string>)> names = values => count(values);\n        hooks.run([:]);\n        return names([]) + names([\"a\"]) + total([:]) + total([\"a\": 1]) + this.size([]) + this.size([1, 2]);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Counter.sharp", sharp), &[("src/Lib/Hooks.php", hooks)]), Vec::<String>::new());
}

/// A PHP closure's docblock names PHP types, and a plain PHP `array` names neither a `List` nor a `Map`, so a call
/// through one takes either literal, as a PHP method's `array` parameter does.
#[test]
fn a_php_closure_whose_docblock_names_a_php_array_takes_either_literal() {
    let hooks = "<?php\n\nnamespace Lib;\n\nfinal class Hooks\n{\n    /** @var \\Closure(array): int */\n    public \\Closure $run;\n\n    public function __construct()\n    {\n        $this->run = fn (array $values): int => count($values);\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Hooks;\n\nclass Counter\n{\n    public int run(Hooks hooks) => hooks.run([]) + hooks.run([:]);\n}\n";

    assert_eq!(issues(("src/Demo/Counter.sharp", sharp), &[("src/Lib/Hooks.php", hooks)]), Vec::<String>::new());
}

/// A `Function` value and a PHP closure of the same signature are assignable to each other both ways: whether PHP#
/// wrote a signature decides only which literals a call through it takes.
#[test]
fn a_function_value_and_a_php_closure_of_the_same_signature_are_assignable_both_ways() {
    let tools = "<?php\n\nnamespace Lib;\n\nfinal class Tools\n{\n    /** @param \\Closure(list<string>): int $count */\n    public static function apply(\\Closure $count): int\n    {\n        return $count(['a']);\n    }\n\n    /** @return \\Closure(list<string>): int */\n    public static function make(): \\Closure\n    {\n        return fn (array $names): int => count($names);\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Tools;\n\nclass Counter\n{\n    public int run(Function<int(List<string>)> size)\n    {\n        Function<int(List<string>)> made = Tools.make();\n        return Tools.apply(size) + Counter.take(Tools.make()) + made([\"a\"]);\n    }\n\n    private static int take(Function<int(List<string>)> count) => count([\"b\"]);\n}\n";

    assert_eq!(issues(("src/Demo/Counter.sharp", sharp), &[("src/Lib/Tools.php", tools)]), Vec::<String>::new());
}

const HOOKS: &str = "<?php\n\nnamespace Lib;\n\nfinal class Hooks\n{\n    /** @var \\Closure(list<string>): int */\n    public \\Closure $names;\n\n    /** @var \\Closure(list<string>): int */\n    public \\Closure $more;\n\n    /** @var \\Closure(list<string>): int<0, max> */\n    public \\Closure $counted;\n\n    public function __construct()\n    {\n        $this->names = fn (array $names): int => count($names);\n        $this->more = fn (array $names): int => count($names) + 1;\n        $this->counted = fn (array $names): int => count($names);\n    }\n}\n";

/// A value that may be a `Function` value or a PHP closure of the same signature names the collections the `Function`
/// value declares, in either order, after it is combined with another PHP closure and after a null check, so a call
/// through it refuses `[:]` and takes `[]`.
#[test]
fn a_value_that_may_be_a_function_value_or_a_php_closure_of_one_signature_keeps_its_parameter_collections() {
    let sharp = "namespace Demo;\n\nimport Lib.Hooks;\n\nclass Counter\n{\n    public int sharpFirst(bool pick, Function<int(List<string>)> size, Hooks hooks)\n    {\n        const either = pick ? size : hooks.names;\n        return either([:]) + either([]);\n    }\n\n    public int phpFirst(bool pick, Function<int(List<string>)> size, Hooks hooks)\n    {\n        const either = pick ? hooks.names : size;\n        return either([:]) + either([]);\n    }\n\n    public int third(bool pick, bool other, Function<int(List<string>)> size, Hooks hooks)\n    {\n        const either = pick ? size : hooks.names;\n        const any = other ? either : hooks.more;\n        return any([:]) + any([]);\n    }\n\n    public int narrowed(bool pick, Function<int(List<string>)>? size, Hooks hooks)\n    {\n        const either = pick ? size : hooks.names;\n        if (either != null)\n        {\n            return either([:]) + either([]);\n        }\n\n        return 0;\n    }\n}\n";

    let message = "invalid-array-element `[:]` is an empty Map. An empty List is written `[]`.";
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counter.sharp", sharp), &[("src/Lib/Hooks.php", HOOKS)])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [
            format!("10:23 {message}"),
            format!("16:23 {message}"),
            format!("23:20 {message}"),
            format!("31:27 {message}")
        ]
    );
}

/// A method value of a PHP# class is a function PHP# wrote, so a call through it checks literals as a call of the
/// method does, also when the value may be a PHP closure of the same signature. A method value of a PHP class takes
/// either literal, as the PHP method does.
#[test]
fn a_literal_of_the_other_collection_passed_to_a_method_value_is_an_error() {
    let adder = "<?php\n\nnamespace Lib;\n\nfinal class Adder\n{\n    public function add(array $names): int\n    {\n        return count($names);\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Adder;\nimport Lib.Hooks;\n\nclass Counter\n{\n    public int run(Adder adder)\n    {\n        const own = this.add;\n        const shared = Counter.size;\n        const plain = adder.add;\n        return own([:]) + shared([:]) + plain([]) + plain([:]);\n    }\n\n    public int either(bool pick, Hooks hooks)\n    {\n        const own = pick ? this.add : hooks.names;\n        const shared = pick ? hooks.names : Counter.size;\n        return own([:]) + shared([:]) + own([]) + shared([]);\n    }\n\n    private int add(List<string> names) => count(names);\n\n    private static int size(List<string> names) => count(names);\n}\n";

    let message = "invalid-array-element `[:]` is an empty Map. An empty List is written `[]`.";
    assert_eq!(
        analyze(
            &PLUGIN_REGISTRY,
            settings(),
            ("src/Demo/Counter.sharp", sharp),
            &[("src/Lib/Adder.php", adder), ("src/Lib/Hooks.php", HOOKS)]
        )
        .iter()
        .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
        .collect::<Vec<_>>(),
        [
            format!("13:20 {message}"),
            format!("13:34 {message}"),
            format!("20:20 {message}"),
            format!("20:34 {message}")
        ]
    );
}

/// A lambda's parameters are types PHP# wrote, so a call through a lambda checks literals as a call of a method does,
/// also when the value may be a PHP closure of the same signature.
#[test]
fn a_literal_of_the_other_collection_passed_to_a_lambda_is_an_error() {
    let sharp = "namespace Demo;\n\nimport Lib.Hooks;\n\nclass Counter\n{\n    public int run(bool pick, Hooks hooks)\n    {\n        const size = (List<string> names) => count(names);\n        const either = pick ? size : hooks.counted;\n        const other = pick ? hooks.counted : size;\n        return size([:]) + either([:]) + other([:]) + size([]) + either([]);\n    }\n}\n";

    let message = "invalid-array-element `[:]` is an empty Map. An empty List is written `[]`.";
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counter.sharp", sharp), &[("src/Lib/Hooks.php", HOOKS)])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [format!("12:21 {message}"), format!("12:35 {message}"), format!("12:48 {message}")]
    );
}

/// Plain PHP names a PHP# `Function` type as PHP names a closure type, so a `.php` message about one reads as it reads
/// about a PHP closure.
#[test]
fn a_php_message_names_a_function_type_as_a_php_closure_type() {
    let sharp = "namespace Demo;\n\npublic class Counter\n{\n    public int apply(Function<int(List<string>)> size) => size([\"a\"]);\n}\n";
    let php = "<?php\n\nnamespace Lib;\n\nuse Demo\\Counter;\n\nfunction run(Counter $counter): int\n{\n    return $counter->apply(1);\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Lib/run.php", php), &[("src/Demo/Counter.sharp", sharp)])
            .iter()
            .map(|issue| format!("{} {}", located(php, issue), issue.message))
            .collect::<Vec<_>>(),
        [
            "9:28 invalid-argument Invalid argument type for argument #1 of `Demo\\Counter::apply`: expected `(closure(list<string>): int)`, but found `int(1)`."
        ]
    );
}

/// A function type's effects are PHP# alone, so a `.php` message names a function type with effects as the PHP closure
/// type of its parameters and return type.
#[test]
fn a_php_message_names_a_function_type_with_effects_as_a_php_closure_type() {
    let sharp = "namespace Demo;\n\npublic class Counter\n{\n    public int apply(Function<int(List<string>) uses Http> size) => size([\"a\"]);\n}\n";
    let php = "<?php\n\nnamespace Lib;\n\nuse Demo\\Counter;\n\nfunction run(Counter $counter): int\n{\n    return $counter->apply(1);\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Lib/run.php", php), &[("src/Demo/Counter.sharp", sharp)])
            .iter()
            .map(|issue| format!("{} {}", located(php, issue), issue.message))
            .collect::<Vec<_>>(),
        [
            "9:28 invalid-argument Invalid argument type for argument #1 of `Demo\\Counter::apply`: expected `(closure(list<string>): int)`, but found `int(1)`."
        ]
    );
}

/// `App\Shop\Cart` and `App\Shop\Charge`, the parameter and return types of the function types below.
const SHOP: (&str, &str) =
    ("src/App/Shop/Shop.php", "<?php\n\nnamespace App\\Shop;\n\nfinal class Cart\n{\n}\n\nfinal class Charge\n{\n}\n");

/// Spec section 29: a function type lists its effects with `uses` inside its brackets, and the PHP twin writes the
/// closure type it runs as.
#[test]
fn a_field_of_a_function_type_with_an_effect_is_checked_as_its_php_twin() {
    let sharp = "namespace App.Shop;\n\npublic class Checkout\n{\n    private Function<Charge(Cart) uses Http> charge;\n\n    public Checkout(Function<Charge(Cart) uses Http> charge)\n    {\n        this.charge = charge;\n    }\n\n    public Charge pay(Cart cart) => this.charge(cart);\n}\n";
    let php = "<?php\n\nnamespace App\\Shop;\n\nclass Checkout\n{\n    /** @var \\Closure(Cart): Charge */\n    private \\Closure $charge;\n\n    /** @param \\Closure(Cart): Charge $charge */\n    public function __construct(\\Closure $charge)\n    {\n        $this->charge = $charge;\n    }\n\n    public function pay(Cart $cart): Charge { return ($this->charge)($cart); }\n}\n";

    assert_eq!(issues(("src/App/Shop/Checkout.php", php), &[SHOP]), Vec::<String>::new());
    assert_eq!(issues(("src/App/Shop/Checkout.sharp", sharp), &[SHOP]), Vec::<String>::new());
}

#[test]
fn a_nullable_field_of_a_function_type_with_an_effect_is_checked_as_its_php_twin() {
    let sharp = "namespace App.Shop;\n\npublic class Checkout\n{\n    private Function<Charge(Cart) uses Http>? fallback;\n\n    public Function<Charge(Cart) uses Http>? current() => this.fallback;\n\n    public void replace(Function<Charge(Cart) uses Http> next)\n    {\n        this.fallback = next;\n    }\n}\n";
    let php = "<?php\n\nnamespace App\\Shop;\n\nclass Checkout\n{\n    /** @var null|\\Closure(Cart): Charge */\n    private ?\\Closure $fallback = null;\n\n    /** @return null|\\Closure(Cart): Charge */\n    public function current(): ?\\Closure { return $this->fallback; }\n\n    /** @param \\Closure(Cart): Charge $next */\n    public function replace(\\Closure $next): void\n    {\n        $this->fallback = $next;\n    }\n}\n";

    assert_eq!(issues(("src/App/Shop/Checkout.php", php), &[SHOP]), Vec::<String>::new());
    assert_eq!(issues(("src/App/Shop/Checkout.sharp", sharp), &[SHOP]), Vec::<String>::new());
}

/// `Http` and `Mail` are both effects of the map's value type, so the map takes two type arguments.
#[test]
fn a_map_of_function_types_with_two_effects_is_checked_as_its_php_twin() {
    let sharp = "namespace App.Shop;\n\npublic class Checkout\n{\n    private Map<string, Function<Charge(Cart) uses Http, Mail>> handlers;\n\n    public Checkout(Map<string, Function<Charge(Cart) uses Http, Mail>> handlers)\n    {\n        this.handlers = handlers;\n    }\n\n    public int size() => count(this.handlers);\n}\n";
    let php = "<?php\n\nnamespace App\\Shop;\n\nclass Checkout\n{\n    /** @var array<string, \\Closure(Cart): Charge> */\n    private array $handlers;\n\n    /** @param array<string, \\Closure(Cart): Charge> $handlers */\n    public function __construct(array $handlers)\n    {\n        $this->handlers = $handlers;\n    }\n\n    public function size(): int { return count($this->handlers); }\n}\n";

    assert_eq!(issues(("src/App/Shop/Checkout.php", php), &[SHOP]), Vec::<String>::new());
    assert_eq!(issues(("src/App/Shop/Checkout.sharp", sharp), &[SHOP]), Vec::<String>::new());
}

#[test]
fn a_function_type_taking_a_function_type_with_an_effect_is_checked_as_its_php_twin() {
    let sharp = "namespace App.Shop;\n\npublic class Checkout\n{\n    private Function<int(Function<int(int) uses Http>)> apply;\n\n    public Checkout(Function<int(Function<int(int) uses Http>)> apply)\n    {\n        this.apply = apply;\n    }\n\n    public int run(Function<int(int) uses Http> step) => this.apply(step);\n}\n";
    let php = "<?php\n\nnamespace App\\Shop;\n\nclass Checkout\n{\n    /** @var \\Closure(\\Closure(int): int): int */\n    private \\Closure $apply;\n\n    /** @param \\Closure(\\Closure(int): int): int $apply */\n    public function __construct(\\Closure $apply)\n    {\n        $this->apply = $apply;\n    }\n\n    /** @param \\Closure(int): int $step */\n    public function run(\\Closure $step): int { return ($this->apply)($step); }\n}\n";

    assert_eq!(issues(("src/App/Shop/Checkout.php", php), &[SHOP]), Vec::<String>::new());
    assert_eq!(issues(("src/App/Shop/Checkout.sharp", sharp), &[SHOP]), Vec::<String>::new());
}

/// A `.sharp` message writes a function type's effects inside its brackets, each named as the file names the class:
/// a standard effect by its short name, and an imported one by its import's name.
#[test]
fn a_sharp_message_names_a_function_type_with_its_effects_inside_its_brackets() {
    let sharp = "namespace Demo;\n\nimport Sharp.Mail as Post;\n\nclass Receipt\n{\n    public string step(Function<int(int) uses Http, Post> step) => `Step: ${step}`;\n}\n";

    assert_eq!(
        worded(("src/Demo/Receipt.sharp", sharp), &[]),
        [
            "7:75 invalid-operand A template shows `int`, `float`, `string` or `bool`, and `Function<int(int) uses Http, Post>` is none of them. | This is `Function<int(int) uses Http, Post>`. | Show a value taken from it instead, such as a property or the result of a method.",
        ]
    );
}

/// A `Function` value keeps the collections its parameters declare when a `let` local copies it, when a template
/// passes it through, and when a `List` of `Function` values gives it back.
#[test]
fn a_function_value_keeps_its_parameter_collections_through_a_copy_a_template_and_a_list() {
    let pass = "<?php\n\nnamespace Lib;\n\nfinal class Pass\n{\n    /**\n     * @template T\n     * @param T $value\n     * @return T\n     */\n    public static function keep(mixed $value): mixed\n    {\n        return $value;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Pass;\n\nclass Counter\n{\n    public int run(Function<int(List<string>)> size)\n    {\n        let copy = size;\n        const kept = Pass.keep(size);\n        List<Function<int(List<string>)>> all = [size];\n        int total = copy([:]) + kept([:]);\n        for (const each of all) {\n            total += each([:]);\n        }\n        return total;\n    }\n}\n";

    let message = "invalid-array-element `[:]` is an empty Map. An empty List is written `[]`.";
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counter.sharp", sharp), &[("src/Lib/Pass.php", pass)])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [format!("12:26 {message}"), format!("12:38 {message}"), format!("14:27 {message}")]
    );
}

/// The right side of `??` flows into the place its value is assigned to, so its literal is checked as the place's.
#[test]
fn a_list_literal_on_the_right_of_null_coalescing_where_a_map_is_declared_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Options\n{\n    public Map<string, int> run(Map<string, int>? given)\n    {\n        Map<string, int> options = given ?? [];\n        return options;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Options\n{\n    public function run(?array $given): array\n    {\n        $options = $given ?? [];\n        return $options;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Options.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Options.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["7:45 invalid-array-element `[]` is an empty List. An empty Map is written `[:]`."]
    );
}

/// Both branches of `? :` flow into the place, through any nesting, so each branch's literal is checked as the
/// place's.
#[test]
fn a_list_literal_in_a_ternary_branch_where_a_map_is_declared_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Counts\n{\n    public Map<string, int> run(bool ready, Map<string, int>? given, Map<string, int> seed)\n    {\n        Map<string, int> counts = ready ? [] : seed;\n        Map<string, int> nested = ready ? seed : (given ?? []);\n        return ready ? counts : nested;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Counts\n{\n    public function run(bool $ready, ?array $given, array $seed): array\n    {\n        $counts = $ready ? [] : $seed;\n        $nested = $ready ? $seed : ($given ?? []);\n        return $ready ? $counts : $nested;\n    }\n}\n";

    let message = "invalid-array-element `[]` is an empty List. An empty Map is written `[:]`.";
    assert_eq!(issues(("src/Demo/Counts.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counts.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [format!("7:43 {message}"), format!("8:60 {message}")]
    );
}

/// Every arm of a `match` flows into the place, so each arm's literal is checked as the place's.
#[test]
fn a_list_literal_in_a_match_arm_where_a_map_is_returned_is_an_error() {
    let kind_sharp = "namespace Demo;\n\npublic enum Kind\n{\n    case None;\n    case Some;\n}\n";
    let kind_php = "<?php\n\nnamespace Demo;\n\nenum Kind\n{\n    case None;\n    case Some;\n}\n";
    let sharp = "namespace Demo;\n\nclass Counts\n{\n    public Map<string, int> run(Kind kind, Map<string, int> seed)\n    {\n        return match (kind) { Kind.None => [], default => seed };\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Counts\n{\n    public function run(Kind $kind, array $seed): array\n    {\n        return match ($kind) { Kind::None => [], default => $seed };\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Counts.php", php), &[("src/Demo/Kind.php", kind_php)]), Vec::<String>::new());
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counts.sharp", sharp), &[("src/Demo/Kind.sharp", kind_sharp)])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["7:44 invalid-array-element `[]` is an empty List. An empty Map is written `[:]`."]
    );
}

/// `??=` stores its right side unchanged when the place is null, so its literal is checked as the place's.
#[test]
fn a_list_literal_assigned_with_null_coalescing_to_a_map_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Counts\n{\n    public Map<string, int> run(Map<string, int>? counts)\n    {\n        counts ??= [];\n        return counts;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Counts\n{\n    public function run(?array $counts): array\n    {\n        $counts ??= [];\n        return $counts;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Counts.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counts.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["7:20 invalid-array-element `[]` is an empty List. An empty Map is written `[:]`."]
    );
}

/// A literal of the declared collection compiles on the right of `??` and `??=`, in a `? :` branch and in a `match`
/// arm.
#[test]
fn a_literal_of_the_declared_collection_in_a_coalesce_ternary_or_match_is_accepted() {
    let kind = "namespace Demo;\n\npublic enum Kind\n{\n    case None;\n    case Some;\n}\n";
    let sharp = "namespace Demo;\n\nclass Clean\n{\n    public Map<string, int> run(Map<string, int>? given, Map<string, int>? later, bool ready, Kind kind)\n    {\n        later ??= [:];\n        Map<string, int> options = given ?? [:];\n        Map<string, int> counts = ready ? [:] : later;\n        return match (kind) { Kind.None => [:], default => ready ? counts : options };\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Clean.sharp", sharp), &[("src/Demo/Kind.sharp", kind)]), Vec::<String>::new());
}

const STORE: (&str, &str) = (
    "src/Lib/Store.php",
    "<?php\n\nnamespace Lib;\n\ntrait Keeps\n{\n    public array $kept = [];\n\n    public function keepAll(array $rows): int\n    {\n        return count($rows);\n    }\n}\n\ninterface Sink\n{\n    public function sink(array $rows): int;\n}\n\n/**\n * @method int magic(array $rows)\n */\nclass Store\n{\n    use Keeps;\n\n    public static array $shared = [];\n\n    public array $items = [];\n\n    public function __construct(array $seed = [])\n    {\n        $this->items = $seed;\n    }\n\n    public function keep(array $rows): int\n    {\n        return count($rows);\n    }\n\n    public function pair(array $left, array $right): int\n    {\n        return count($left) + count($right);\n    }\n\n    public static function make(array $rows): int\n    {\n        return count($rows);\n    }\n\n    /** @param list<mixed> $arguments */\n    public function __call(string $name, array $arguments): int\n    {\n        return count($arguments);\n    }\n}\n",
);

const ROWS: (&str, &str) =
    ("src/Demo/Rows.sharp", "namespace Demo;\n\nimport Lib.Store;\n\npublic class Rows : Store\n{\n}\n");

/// A plain PHP parent's method keeps its PHP `array` parameter when a PHP# class inherits it, so a call through the
/// PHP# class takes either literal, as a call through the PHP class does.
#[test]
fn a_php_parents_method_called_on_a_sharp_class_takes_either_literal() {
    let sharp =
        "namespace Demo;\n\nclass Report\n{\n    public int run(Rows rows) => rows.keep([]) + rows.keep([:]);\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Report\n{\n    public function run(Rows $rows): int\n    {\n        return $rows->keep([]) + $rows->keep([]);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[STORE, ROWS]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[STORE, ROWS]), Vec::<String>::new());
}

/// A plain PHP parent's static method called through a PHP# class takes either literal.
#[test]
fn a_php_parents_static_method_called_on_a_sharp_class_takes_either_literal() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int run() => Rows.make([]) + Rows.make([:]);\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Report\n{\n    public function run(): int\n    {\n        return Rows::make([]) + Rows::make([]);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[STORE, ROWS]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[STORE, ROWS]), Vec::<String>::new());
}

/// A plain PHP parent's method partially applied through a PHP# class takes either literal for the arguments it fixes.
#[test]
fn a_php_parents_method_partially_applied_on_a_sharp_class_takes_either_literal() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int run(Rows rows)\n    {\n        const keep = rows.pair(?, [:]);\n        return keep([]);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Report\n{\n    public function run(Rows $rows): int\n    {\n        $keep = $rows->pair(?, []);\n        return $keep([]);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[STORE, ROWS]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[STORE, ROWS]), Vec::<String>::new());
}

/// A plain PHP trait's method that a PHP# class inherits keeps its PHP `array` parameter, so it takes either literal.
#[test]
fn a_php_traits_method_called_on_a_sharp_class_takes_either_literal() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int run(Rows rows) => rows.keepAll([]) + rows.keepAll([:]);\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Report\n{\n    public function run(Rows $rows): int\n    {\n        return $rows->keepAll([]) + $rows->keepAll([]);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[STORE, ROWS]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[STORE, ROWS]), Vec::<String>::new());
}

/// A plain PHP interface's method that an abstract PHP# class does not declare again keeps its PHP `array` parameter,
/// so a call through the PHP# class takes either literal.
#[test]
fn a_php_interfaces_method_called_on_an_abstract_sharp_class_takes_either_literal() {
    let sharp = "namespace Demo;\n\nimport Lib.Sink;\n\npublic abstract class Drain : Sink\n{\n}\n\nclass Report\n{\n    public int run(Drain drain) => drain.sink([]) + drain.sink([:]);\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Sink;\n\nabstract class Drain implements Sink\n{\n}\n\nfinal class Report\n{\n    public function run(Drain $drain): int\n    {\n        return $drain->sink([]) + $drain->sink([]);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[STORE]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[STORE]), Vec::<String>::new());
}

/// A PHP# class without a constructor of its own is created through its plain PHP parent's constructor, whose PHP
/// `array` parameter takes either literal.
#[test]
fn a_php_parents_constructor_called_by_new_of_a_sharp_class_takes_either_literal() {
    let sharp =
        "namespace Demo;\n\nclass Report\n{\n    public List<Rows> run() => [new Rows([]), new Rows([:])];\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Report\n{\n    /** @return list<Rows> */\n    public function run(): array\n    {\n        return [new Rows([]), new Rows([])];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[STORE, ROWS]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[STORE, ROWS]), Vec::<String>::new());
}

/// A method a plain PHP parent's `__call` serves, as its `@method` tag writes it, takes either literal for its PHP
/// `array` parameter when called through a PHP# class.
#[test]
fn a_php_parents_magic_method_called_on_a_sharp_class_takes_either_literal() {
    let sharp =
        "namespace Demo;\n\nclass Report\n{\n    public int run(Rows rows) => rows.magic([]) + rows.magic([:]);\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Report\n{\n    public function run(Rows $rows): int\n    {\n        return $rows->magic([]) + $rows->magic([]);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[STORE, ROWS]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[STORE, ROWS]), Vec::<String>::new());
}

/// A PHP# method keeps the collections its parameters declare when a plain PHP subclass inherits it, so a call
/// through the PHP subclass refuses a literal of the other collection.
#[test]
fn a_sharp_parents_method_called_on_a_php_subclass_keeps_its_parameter_collections() {
    let base = (
        "src/Demo/Base.sharp",
        "namespace Demo;\n\npublic class Base\n{\n    public int keep(List<int> rows) => count(rows);\n}\n",
    );
    let child = ("src/Demo/Child.php", "<?php\n\nnamespace Demo;\n\nclass Child extends Base\n{\n}\n");
    let sharp =
        "namespace Demo;\n\nclass Report\n{\n    public int run(Child child) => child.keep([]) + child.keep([:]);\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Report\n{\n    public function run(Child $child): int\n    {\n        return $child->keep([]) + $child->keep([]);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[base, child]), Vec::<String>::new());
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[base, child])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["5:64 invalid-array-element `[:]` is an empty Map. An empty List is written `[]`."]
    );
}

/// A `List`'s `add` takes only a literal of the collection its elements declare, though PHP# declares the method in a
/// plain PHP stub: a `List<List<int>>` takes `[]` and refuses `[:]`.
#[test]
fn a_map_literal_added_to_a_list_of_lists_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Grid\n{\n    public void fill(List<List<int>> rows)\n    {\n        rows.add([]);\n        rows.add([:]);\n    }\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Grid.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["8:18 invalid-array-element `[:]` is an empty Map. An empty List is written `[]`."]
    );
}

/// PHP has one empty array, so a `list<list<int>>` takes `[]` as an appended element.
#[test]
fn an_empty_php_array_appended_to_a_list_of_lists_is_accepted() {
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Grid\n{\n    /**\n     * @param list<list<int>> $rows\n     *\n     * @return list<list<int>>\n     */\n    public function fill(array $rows): array\n    {\n        $rows[] = [];\n        $rows[] = [];\n\n        return $rows;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Grid.php", php), &[]), Vec::<String>::new());
}

/// A `List`'s `set` takes only a literal of the collection its elements declare: a `List<List<int>>` takes `[1]` and
/// refuses `[:]`.
#[test]
fn a_map_literal_set_into_a_list_of_lists_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Grid\n{\n    public void fill(List<List<int>> rows)\n    {\n        rows.set(0, [1]);\n        rows.set(0, [:]);\n    }\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Grid.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["8:21 invalid-array-element `[:]` is an empty Map. An empty List is written `[]`."]
    );
}

/// PHP has one empty array, so a `list<list<int>>` takes `[]` as a replaced element.
#[test]
fn an_empty_php_array_set_into_a_list_of_lists_is_accepted() {
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Grid\n{\n    /**\n     * @param list<list<int>> $rows\n     *\n     * @return list<list<int>>\n     */\n    public function fill(array $rows): array\n    {\n        $rows[0] = [1];\n        $rows[0] = [];\n\n        return $rows;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Grid.php", php), &[]), Vec::<String>::new());
}

/// `List.wrap`'s value is `T|List<T>`, so `[:]` may be the `T` it wraps, and `wrap` itself refuses every value that can
/// be a collection. A `Map` literal passed to it is refused once, by `wrap`, and not again as a literal of the wrong
/// collection.
#[test]
fn list_wrap_of_a_map_literal_is_refused_once() {
    let sharp = "namespace Demo;\n\nclass Grid\n{\n    public List<List<int>> rows() => List.wrap([:]);\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Grid.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [
            "5:48 invalid-argument T is Map<never, never>, itself a map; write `[:] is Map<never, never> one ? [one] : [:]`"
        ]
    );
}

/// Plain PHP calls `\Sharp\List::wrap` with an empty array under PHP's rules, with no PHP# refusal.
#[test]
fn list_wrap_of_an_empty_php_array_called_from_php_keeps_the_php_checks() {
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Grid\n{\n    /** @return list<list<int>> */\n    public function rows(): array\n    {\n        return \\Sharp\\List::wrap([]);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Grid.php", php), &[]), Vec::<String>::new());
}

/// A return or a parameter that declares both a `List` and a `Map` takes a literal of either collection.
#[test]
fn a_literal_where_a_list_or_a_map_is_declared_is_accepted() {
    let sharp = "namespace Demo;\n\nclass Grid\n{\n    public List<int>|Map<string, int> none() => [:];\n\n    public List<int>|Map<string, int> empty() => [];\n\n    public int fill() => Grid.size([:]) + Grid.size([]);\n\n    private static int size(List<int>|Map<string, int> cells) => count(cells);\n}\n";

    assert_eq!(issues(("src/Demo/Grid.sharp", sharp), &[]), Vec::<String>::new());
}

/// A return or a parameter that declares a `Map` before a `List` takes a literal of either collection too.
#[test]
fn a_literal_where_a_map_or_a_list_is_declared_is_accepted() {
    let sharp = "namespace Demo;\n\nclass Grid\n{\n    public Map<string, int>|List<int> none() => [:];\n\n    public Map<string, int>|List<int> empty() => [];\n\n    public int fill() => Grid.size([:]) + Grid.size([]);\n\n    private static int size(Map<string, int>|List<int> cells) => count(cells);\n}\n";

    assert_eq!(issues(("src/Demo/Grid.sharp", sharp), &[]), Vec::<String>::new());
}

/// PHP has one empty array, so a return or a parameter typed `list<int>|array<string, int>` takes `[]`.
#[test]
fn an_empty_php_array_where_a_list_or_a_string_keyed_array_is_declared_is_accepted() {
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Grid\n{\n    /** @return list<int>|array<string, int> */\n    public function none(): array\n    {\n        return [];\n    }\n\n    /** @return list<int>|array<string, int> */\n    public function empty(): array\n    {\n        return [];\n    }\n\n    public function fill(): int\n    {\n        return self::size([]) + self::size([]);\n    }\n\n    /** @param list<int>|array<string, int> $cells */\n    private static function size(array $cells): int\n    {\n        return count($cells);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Grid.php", php), &[]), Vec::<String>::new());
}

/// A plain PHP class's `array` property names neither a `List` nor a `Map`, so PHP# assigns it either literal, as an
/// instance property and as a static one.
#[test]
fn a_php_classes_array_property_takes_either_literal() {
    let sharp = "namespace Demo;\n\nimport Lib.Store;\n\nclass Report\n{\n    public void run(Store store)\n    {\n        store.items = [];\n        store.items = [:];\n        Store.shared = [];\n        Store.shared = [:];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Store;\n\nfinal class Report\n{\n    public function run(Store $store): void\n    {\n        $store->items = [];\n        $store->items = [];\n        Store::$shared = [];\n        Store::$shared = [];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[STORE]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[STORE]), Vec::<String>::new());
}

/// A plain PHP parent's `array` property stays PHP's when a PHP# class inherits it, so the PHP# class assigns it
/// either literal, through `this` and through another object.
#[test]
fn a_php_parents_array_property_a_sharp_class_inherits_takes_either_literal() {
    let sharp = "namespace Demo;\n\nimport Lib.Store;\n\npublic class Rows : Store\n{\n    public void clear(Rows other)\n    {\n        this.items = [];\n        this.items = [:];\n        other.items = [:];\n        other.items = [];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Store;\n\nclass Rows extends Store\n{\n    public function clear(Rows $other): void\n    {\n        $this->items = [];\n        $this->items = [];\n        $other->items = [];\n        $other->items = [];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Rows.php", php), &[STORE]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Rows.sharp", sharp), &[STORE]), Vec::<String>::new());
}

/// A plain PHP trait's `array` property stays PHP's in every class that uses the trait, so a PHP# class below one
/// assigns it either literal.
#[test]
fn a_php_traits_array_property_takes_either_literal() {
    let sharp = "namespace Demo;\n\nimport Lib.Store;\n\npublic class Rows : Store\n{\n    public void clear()\n    {\n        this.kept = [];\n        this.kept = [:];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Store;\n\nclass Rows extends Store\n{\n    public function clear(): void\n    {\n        $this->kept = [];\n        $this->kept = [];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Rows.php", php), &[STORE]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Rows.sharp", sharp), &[STORE]), Vec::<String>::new());
}

/// A PHP# property declared as a `List` refuses a `Map` literal in the class that declares it, through a promoted
/// parameter, and in a PHP# class that inherits it.
#[test]
fn a_sharp_list_property_refuses_a_map_literal_where_it_is_declared_and_inherited() {
    let sharp = "namespace Demo;\n\npublic class Sizes\n{\n    public List<int> all = [];\n\n    public Sizes(public List<int> seed { get; set; })\n    {\n    }\n\n    public void clear()\n    {\n        this.all = [:];\n    }\n}\n\npublic class Counts : Sizes\n{\n    public void reset()\n    {\n        this.all = [:];\n        this.seed = [:];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Sizes\n{\n    /** @var list<int> */\n    public array $all = [];\n\n    /** @param list<int> $seed */\n    public function __construct(public array $seed)\n    {\n    }\n\n    public function clear(): void\n    {\n        $this->all = [];\n    }\n}\n\nclass Counts extends Sizes\n{\n    public function reset(): void\n    {\n        $this->all = [];\n        $this->seed = [];\n    }\n}\n";

    let message = "invalid-array-element `[:]` is an empty Map. An empty List is written `[]`.";
    assert_eq!(issues(("src/Demo/Sizes.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Sizes.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [format!("13:20 {message}"), format!("21:20 {message}"), format!("22:21 {message}")]
    );
}

/// A PHP# property declared as a `List` refuses a `Map` literal written through a parameter of its class.
#[test]
fn a_sharp_list_property_refuses_a_map_literal_through_a_parameter() {
    let sharp = "namespace Demo;\n\npublic class Sizes\n{\n    public List<int> all { get; set; } = [];\n\n    public void clear(Sizes other)\n    {\n        other.all = [:];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Sizes\n{\n    /** @var list<int> */\n    public array $all = [];\n\n    public function clear(Sizes $other): void\n    {\n        $other->all = [];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Sizes.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Sizes.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["9:21 invalid-array-element `[:]` is an empty Map. An empty List is written `[]`."]
    );
}

/// A PHP# property declared as a `List` refuses a `Map` literal written through a local that holds its object, and
/// through a property of `this` that holds it.
#[test]
fn a_sharp_list_property_refuses_a_map_literal_through_a_local_and_a_property_of_this() {
    let sharp = "namespace Demo;\n\npublic class Sizes\n{\n    public List<int> all { get; set; } = [];\n\n    public Sizes(public Sizes peer { get; })\n    {\n    }\n\n    public void clear()\n    {\n        let copy = this.peer;\n        copy.all = [:];\n        this.peer.all = [:];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Sizes\n{\n    /** @var list<int> */\n    public array $all = [];\n\n    public function __construct(public Sizes $peer)\n    {\n    }\n\n    public function clear(): void\n    {\n        $copy = $this->peer;\n        $copy->all = [];\n        $this->peer->all = [];\n    }\n}\n";

    let message = "invalid-array-element `[:]` is an empty Map. An empty List is written `[]`.";
    assert_eq!(issues(("src/Demo/Sizes.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Sizes.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [format!("14:20 {message}"), format!("15:25 {message}")]
    );
}

/// `other.all = []` empties a PHP# `List<int>` property through a parameter and leaves it the `List<int>` it is
/// declared, so its index reads an `int`. PHP narrows the emptied `list<int>` property to an empty array.
#[test]
fn a_sharp_list_property_emptied_through_a_parameter_stays_a_list() {
    let sharp = "namespace Demo;\n\npublic class Sizes\n{\n    public List<int> all { get; set; } = [];\n\n    public int clear(Sizes other)\n    {\n        other.all = [];\n        return other.all[0];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Sizes\n{\n    /** @var list<int> */\n    public array $all = [];\n\n    public function clear(Sizes $other): int\n    {\n        $other->all = [];\n\n        return $other->all[0];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Sizes.php", php), &[]),
        ["14:28 mismatched-array-index", "14:16 invalid-return-statement"]
    );
    assert_eq!(issues(("src/Demo/Sizes.sharp", sharp), &[]), Vec::<String>::new());
}

/// A PHP# property declared as a `List` refuses a `Map` literal written through an element of a `List` that holds
/// its object.
#[test]
fn a_sharp_list_property_refuses_a_map_literal_through_an_index() {
    let sharp = "namespace Demo;\n\npublic class Sizes\n{\n    public List<int> all { get; set; } = [];\n\n    public void clear(List<Sizes> others)\n    {\n        others[0].all = [:];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Sizes\n{\n    /** @var list<int> */\n    public array $all = [];\n\n    /** @param list<Sizes> $others */\n    public function clear(array $others): void\n    {\n        $others[0]->all = [];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Sizes.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Sizes.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["9:25 invalid-array-element `[:]` is an empty Map. An empty List is written `[]`."]
    );
}

/// A PHP# property declared as a `List` refuses a `Map` literal written through the object a call returns.
#[test]
fn a_sharp_list_property_refuses_a_map_literal_through_a_call() {
    let sharp = "namespace Demo;\n\npublic class Sizes\n{\n    public List<int> all { get; set; } = [];\n\n    public Sizes(public Sizes peer { get; })\n    {\n    }\n\n    public Sizes next() => this.peer;\n\n    public void clear()\n    {\n        this.next().all = [:];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Sizes\n{\n    /** @var list<int> */\n    public array $all = [];\n\n    public function __construct(public Sizes $peer)\n    {\n    }\n\n    public function next(): Sizes\n    {\n        return $this->peer;\n    }\n\n    public function clear(): void\n    {\n        $this->next()->all = [];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Sizes.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Sizes.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["15:27 invalid-array-element `[:]` is an empty Map. An empty List is written `[]`."]
    );
}

/// A PHP# property declared as a `List` refuses a `Map` literal written through a property of a property of `this`.
#[test]
fn a_sharp_list_property_refuses_a_map_literal_through_a_property_of_a_property() {
    let sharp = "namespace Demo;\n\npublic class Sizes\n{\n    public List<int> all { get; set; } = [];\n\n    public Sizes(public Sizes peer { get; })\n    {\n    }\n\n    public void clear()\n    {\n        this.peer.peer.all = [:];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Sizes\n{\n    /** @var list<int> */\n    public array $all = [];\n\n    public function __construct(public Sizes $peer)\n    {\n    }\n\n    public function clear(): void\n    {\n        $this->peer->peer->all = [];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Sizes.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Sizes.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["13:30 invalid-array-element `[:]` is an empty Map. An empty List is written `[]`."]
    );
}

/// `[]` empties a PHP# `List<int>` property through any object, an element, a call or a property of a property, and
/// leaves it the `List<int>` it is declared, so its index reads an `int`. PHP narrows each emptied `list<int>`
/// property it can name to an empty array.
#[test]
fn a_sharp_list_property_emptied_through_any_object_stays_a_list() {
    let sharp = "namespace Demo;\n\npublic class Sizes\n{\n    public List<int> all { get; set; } = [];\n\n    public Sizes(public Sizes peer { get; })\n    {\n    }\n\n    public Sizes next() => this.peer;\n\n    public int clear(List<Sizes> others)\n    {\n        others[0].all = [];\n        this.next().all = [];\n        this.peer.peer.all = [];\n        return others[0].all[0] + this.next().all[0] + this.peer.peer.all[0];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Sizes\n{\n    /** @var list<int> */\n    public array $all = [];\n\n    public function __construct(public Sizes $peer)\n    {\n    }\n\n    public function next(): Sizes\n    {\n        return $this->peer;\n    }\n\n    /** @param list<Sizes> $others */\n    public function clear(array $others): int\n    {\n        $others[0]->all = [];\n        $this->next()->all = [];\n        $this->peer->peer->all = [];\n\n        return $others[0]->all[0] + $this->next()->all[0] + $this->peer->peer->all[0];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Sizes.php", php), &[]),
        ["26:32 mismatched-array-index", "26:16 null-operand", "26:16 mixed-operand", "26:16 mixed-return-statement"]
    );
    assert_eq!(issues(("src/Demo/Sizes.sharp", sharp), &[]), Vec::<String>::new());
}

/// A PHP# local copied from a plain PHP `array` property is PHP#'s own place, and PHP# reads a PHP `array` as a `Map`,
/// so the local refuses a `List` literal.
#[test]
fn a_local_copied_from_a_php_array_property_refuses_a_list_literal() {
    let sharp = "namespace Demo;\n\nimport Lib.Store;\n\nclass Report\n{\n    public int run(Store store)\n    {\n        let items = store.items;\n        items = [];\n        return count(items);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Store;\n\nfinal class Report\n{\n    public function run(Store $store): int\n    {\n        $items = $store->items;\n        $items = [];\n        return count($items);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[STORE]), Vec::<String>::new());
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[STORE])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["10:17 invalid-array-element `[]` is an empty List. An empty Map is written `[:]`."]
    );
}

/// A plain PHP `array` property a PHP# class empties is typed by the value it holds, as in PHP, and PHP# still reads it
/// as a `Map`: its `Map` methods and its `[k, v]` loop check clean, and a bare index read is refused.
#[test]
fn an_emptied_php_array_property_keeps_the_map_rules() {
    let sharp = "namespace Demo;\n\nimport Lib.Store;\n\npublic class Rows : Store\n{\n    public int total()\n    {\n        this.items = [];\n        this.items.delete(\"a\");\n        let total = 0;\n        for (const [key, value] of this.items) {\n            total += 1;\n        }\n        this.items = [:];\n        const first = this.items.get(\"a\");\n        return total + count(this.items) + (first == null ? 0 : 1);\n    }\n\n    public Any? read() => this.items[\"b\"];\n}\n";

    assert_eq!(issues(("src/Demo/Rows.sharp", sharp), &[STORE]), ["20:27 possibly-undefined-array-index"]);
}

/// A `List` has `add`, `set`, `get` and `entries()`, and a `Map` has `delete` and `get`, each typed by the elements of
/// the collection it is called on, as spec section 12 decides.
#[test]
fn collection_methods_take_and_give_the_types_of_their_elements() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public List<Line> lines { get; private set; } = [];\n    private Map<string, int> counts = [:];\n\n    public int total(Line line, List<Line> extra)\n    {\n        this.lines.add(line);\n        extra.add(line);\n        extra.set(0, line);\n        this.counts.delete(\"a\");\n        let sum = (this.counts.get(\"b\") ?? 0) + (extra.get(0)?.cents ?? 0);\n        for (const [index, item] of extra.entries()) {\n            sum += index + item.cents;\n        }\n        return sum;\n    }\n\n    public void wrong(List<Line> extra, Map<string, int> counts)\n    {\n        extra.add(1);\n        extra.set(\"a\", new Line(1));\n        counts.add(1);\n        extra.delete(0);\n    }\n}\n\nclass Line\n{\n    public Line(public int cents { get; })\n    {\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Order.sharp", sharp), &[]),
        ["23:19 invalid-argument", "24:19 invalid-argument", "25:16 non-existent-method", "26:15 non-existent-method"]
    );
}

/// The analyzer checks a collection method against `Sharp\ListMethods` or `Sharp\MapMethods`, and every message names
/// the PHP# type the code wrote instead, as in "`add` doesn't exist on `Map<string, int>`".
#[test]
fn a_collection_method_message_names_the_sharp_type() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public void wrong(List<Line> lines, List<int> sizes, Map<string, int> counts)\n    {\n        lines.add(1);\n        sizes.set(\"a\", 1);\n        counts.add(1);\n        sizes.delete(0);\n        counts.get(1.5);\n        sizes.get();\n        sizes.get(0, 1);\n    }\n}\n\nclass Line\n{\n}\n";
    let issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Order.sharp", sharp), &[]);

    let messages: Vec<&str> = issues.iter().map(|issue| issue.message.as_str()).collect();
    assert_eq!(
        messages,
        [
            "Invalid argument type for argument #1 of `List<Line>.add`: expected `Line`, but found `int`.",
            "Invalid argument type for argument #1 of `List<int>.set`: expected `int`, but found `string`.",
            "Method `add` does not exist on `Map<string, int>`.",
            "Method `delete` does not exist on `List<int>`.",
            "Invalid argument type for argument #1 of `Map<string, int>.get`: expected `string`, but found `float`.",
            "Too few arguments provided for method `List<int>.get`.",
            "Too many arguments provided for method `List<int>.get`.",
        ]
    );
    for issue in &issues {
        let text = format!("{issue:?}");
        assert!(!text.contains("ListMethods") && !text.contains("MapMethods"), "{text}");
    }
}

/// A collection method takes a value of the type its place is declared with, whatever the analyzer saw assigned last,
/// as `$list[] = $x` is checked against the declared property: a `List<Shape>` holding a `Circle` takes a `Square`,
/// and a `List<int>` reset to `[]` takes an int.
#[test]
fn a_collection_method_takes_values_of_the_type_its_place_is_declared_with() {
    let sharp = "namespace Demo;\n\nimport Lib.Circle;\nimport Lib.Shape;\nimport Lib.Square;\n\nclass Board\n{\n    public List<Shape> shapes { get; private set; } = [];\n    public List<int> sizes { get; private set; } = [];\n\n    public int fill(List<Shape> drawn)\n    {\n        this.shapes = [new Circle()];\n        this.shapes.add(new Square());\n        drawn = [new Circle()];\n        drawn.add(new Square());\n        List<Shape> local = [new Square()];\n        local = [new Circle()];\n        local.add(new Square());\n        List<int> reset = [1];\n        reset = [];\n        reset.add(2);\n        this.sizes = [];\n        this.sizes.add(3);\n        this.sizes.add(\"x\");\n        return count(drawn) + count(local) + count(reset);\n    }\n}\n";
    let shapes = "<?php\n\nnamespace Lib;\n\ninterface Shape\n{\n}\n\nfinal class Circle implements Shape\n{\n}\n\nfinal class Square implements Shape\n{\n}\n";

    assert_eq!(issues(("src/Demo/Board.sharp", sharp), &[("src/Lib/Shape.php", shapes)]), ["26:24 invalid-argument"]);
}

#[test]
fn a_list_of_a_nullable_type_takes_null_and_refuses_another_type() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Tray\n{\n    public void fill(List<int?> sizes, List<Calc?> calcs)\n    {\n        sizes.add(null);\n        sizes.add(1);\n        sizes.add(\"x\");\n        calcs.add(null);\n        calcs.add(Calc.make());\n        calcs.add(\"x\");\n        List<int?> held = [null, 2];\n        held.add(\"y\");\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tray.sharp", sharp), &[("src/Lib/Calc.php", CALC)]),
        ["11:19 invalid-argument", "14:19 invalid-argument", "16:18 invalid-argument"]
    );
}

/// The semantic checks refuse an empty literal declared without a type, so a method called on it reports nothing
/// more, and never names the `Map<never, never>` the literal alone would give.
#[test]
fn a_method_on_an_untyped_empty_literal_adds_no_issue() {
    let sharp = "namespace Demo;\n\nclass Inbox\n{\n    public int run()\n    {\n        let messages = [];\n        messages.add(\"a\");\n        const totals = [:];\n        totals.set(\"a\", 1);\n        return count(messages);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Inbox.sharp", sharp), &[]), Vec::<String>::new());
}

/// A PHP# collection holds any value of its element type, as a `List<int>` holds any int, so a method takes one
/// even where the analyzer knows the elements are literals, as TypeScript's `let a = [5]` is a `number[]`.
#[test]
fn collection_methods_take_any_value_of_a_type_the_elements_are_literals_of() {
    let sharp = "namespace Demo;\n\nclass Tray\n{\n    public List<int> items { get; private set; } = [];\n\n    public void fill()\n    {\n        let sizes = [5];\n        sizes.add(6);\n        this.items = [5];\n        this.items.add(6);\n        this.items.set(0, 7);\n        let names = [\"tea\": true];\n        names.delete(\"pie\");\n        let found = names.get(\"pie\") ?? false;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tray.sharp", sharp), &[]), Vec::<String>::new());
}

/// A method that changes a collection writes it back where it lives, so spec section 12 allows it only on a place
/// the caller can write: a local, a parameter, or a property whose `set` the caller reaches. The runtime never meets
/// a write the checker accepts and it refuses.
#[test]
fn a_changing_collection_method_needs_a_place_the_caller_can_write() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public List<int> lines { get; private set; } = [];\n    public List<int> fixed { get; } = [];\n    public List<int> computed => [1];\n}\n\nclass Shop\n{\n    public int change(Order order, Map<string, List<int>> groups)\n    {\n        order.lines.add(1);\n        order.fixed.add(1);\n        order.computed.add(1);\n        this.make().add(1);\n        return count(order.lines.entries()) + (order.fixed.get(0) ?? 0) + count(groups);\n    }\n\n    public List<int> make()\n    {\n        return [];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Shop.sharp", sharp), &[]),
        [
            "14:15 invalid-property-write",
            "15:15 invalid-property-write",
            "16:15 invalid-property-write",
            "17:9 invalid-pass-by-reference"
        ]
    );
}

/// A property with hooks changes through `get` and then `set`, as Swift and decision 014 do, so a changing method
/// may run on it where its `set` is reachable.
#[test]
fn a_changing_collection_method_may_run_on_a_property_with_a_set_hook() {
    let sharp = "namespace Demo;\n\nimport Lib.Box;\n\nclass Shop\n{\n    public void fill(Box box)\n    {\n        box.items.add(1);\n    }\n}\n";
    let box_class = "<?php\n\nnamespace Lib;\n\nfinal class Box\n{\n    /** @var list<int> */\n    public array $items = [] {\n        get => $this->items;\n        set(array $value) {\n            $this->items = $value;\n        }\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Shop.sharp", sharp), &[("src/Lib/Box.php", box_class)]), Vec::<String>::new());
}

/// `+=`, `++` and `--` read an index, then write it. On a `Map` the read is bare, and on a `List` the write is, so
/// both are refused, as Kotlin refuses `map[k] += 1`, and the help writes the form that compiles.
#[test]
fn a_compound_assignment_or_increment_on_an_index_names_the_form_that_compiles() {
    let sharp = "namespace Demo;\n\nclass Counts\n{\n    public void bump(Map<string, int> counts, List<int> sizes)\n    {\n        counts[\"a\"] += 1;\n        counts[\"b\"]++;\n        sizes[0] += 1;\n        sizes[1]--;\n    }\n}\n";

    let issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Counts.sharp", sharp), &[]);
    let helps: Vec<_> = issues.iter().map(|issue| issue.help.as_deref().unwrap_or("")).collect();

    assert_eq!(issues.len(), 4, "{issues:?}");
    assert!(helps[..2].iter().all(|help| help.contains("m[k] = (m[k] ?? 0) + 1")), "{helps:?}");
    assert!(helps[2..].iter().all(|help| help.contains("list.set(i, list[i] + 1)")), "{helps:?}");
}

const STATUS: &str = "<?php\n\nnamespace Lib;\n\nenum Status: string\n{\n    case Active = 'active';\n    case Closed = 'closed';\n}\n\nenum Size: int\n{\n    case Small = 1;\n}\n\nenum Pure\n{\n    case One;\n}\n\nfinal class Line\n{\n}\n";

/// Spec section 12 keys a `Map` by a backed enum, so every key position takes the case: an index write, `??`,
/// `??=`, a nested write, a literal key and the key of `get` and `delete`.
#[test]
fn a_map_keyed_by_a_backed_enum_takes_the_case_in_every_key_position() {
    let sharp = "namespace Demo;\n\nimport Lib.Size;\nimport Lib.Status;\n\nclass Tally\n{\n    private Map<Status, int> counts = [:];\n    private Map<Status, int> seeded = [Status.Active: 0];\n\n    public int count(Status status, Size size)\n    {\n        this.counts[status] = (this.counts[status] ?? 0) + 1;\n        this.counts[status] ??= 0;\n        Map<Status, Map<Size, int>> nested = [:];\n        nested[status] = [:];\n        nested[status][size] = 1;\n        Map<Status, int> built = [Status.Active: 1, status: 2];\n        built.delete(Status.Closed);\n        return (this.counts[status] ?? 0) + (built.get(status) ?? 0) + (this.seeded.get(status) ?? 0) + (nested[status][size] ?? 0);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]), Vec::<String>::new());
}

/// Inside PHP# a `Map<Status, int>` is keyed by `Status` alone, so its backing values, another enum and a
/// `Map<string, int>` are refused where it takes a key. The runtime never meets a key the typed loop cannot read
/// back as `Status`.
#[test]
fn a_map_keyed_by_a_backed_enum_takes_exactly_the_enum_as_a_key() {
    let sharp = "namespace Demo;\n\nimport Lib.Size;\nimport Lib.Status;\n\nclass Tally\n{\n    public int wrong(Map<Status, int> counts, Map<string, int> named, Size size)\n    {\n        counts[\"active\"] = 1;\n        counts[size] = 2;\n        Map<Status, int> built = [\"active\": 1];\n        Map<Status, int> narrowed = named;\n        return (built.get(Status.Active) ?? 0) + (narrowed.get(Status.Active) ?? 0) + (counts.get(\"active\") ?? 0);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        [
            "10:16 mismatched-array-index",
            "11:16 invalid-array-index",
            "12:34 invalid-local-assignment-value",
            "13:37 invalid-local-assignment-value",
            "14:99 invalid-argument"
        ]
    );
}

/// `Sharp\MapMethods` declares the key of `get` and `delete` as `K`, which a `Map<Status, int>` fills with `Status`,
/// so both take the case. A `.php` call on `MapMethods<string, int>` takes its `K` the same way.
#[test]
fn map_get_and_delete_take_a_backed_enum_key_in_sharp_and_in_php() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Tally\n{\n    public int? lookup(Map<Status, int> counts, Status status)\n    {\n        counts.delete(status);\n        return counts.get(status);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Sharp\\MapMethods;\n\nfinal class Tally\n{\n    /** @param MapMethods<string, int> $counts */\n    public function lookup(MapMethods $counts, string $name): ?int\n    {\n        return $counts->get($name);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]), Vec::<String>::new());
    assert_eq!(issues(("src/Demo/Tally.php", php), &[("src/Lib/Status.php", STATUS)]), Vec::<String>::new());
}

/// Plain PHP receives a `Map<Status, int>`'s backing values, so where the `Map` meets a plain PHP array it is an
/// `array<string, int>`: an `array` parameter, an `iterable`, `count`, and a template such as `array_keys`' `K`.
#[test]
fn a_map_keyed_by_a_backed_enum_meets_plain_php_as_an_array_of_its_backing_values() {
    let sharp = "namespace Demo;\n\nimport Lib.Sink;\nimport Lib.Status;\n\nclass Tally\n{\n    public int total(Map<Status, int> counts, Status status)\n    {\n        List<string> keys = array_keys(counts);\n        Map<string, int> named = counts;\n        return count(counts) + Sink.byName(counts) + Sink.plain(counts) + Sink.each(counts) + count(keys) + count(named) + (array_key_exists(status.value, counts) ? 1 : 0);\n    }\n}\n";
    let sink = "<?php\n\nnamespace Lib;\n\nfinal class Sink\n{\n    /** @param array<string, int> $counts */\n    public static function byName(array $counts): int\n    {\n        return count($counts);\n    }\n\n    public static function plain(array $counts): int\n    {\n        return count($counts);\n    }\n\n    /** @param iterable<string, int> $counts */\n    public static function each(iterable $counts): int\n    {\n        return iterator_count($counts);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS), ("src/Lib/Sink.php", sink)]),
        Vec::<String>::new()
    );
}

/// Spec section 11 checks a value from plain PHP where it enters PHP#, so a plain PHP caller passes the backing
/// values where PHP# takes a `Map<Status, int>`, and only those. That border check does not look inside a
/// collection yet, so a key that is no case fails when PHP# reads it back as `Status`.
#[test]
fn a_plain_php_caller_passes_the_backing_values_where_php_sharp_takes_a_map_keyed_by_a_backed_enum() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Tally\n{\n    public int total(Map<Status, int> counts)\n    {\n        return count(counts);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Caller\n{\n    public function run(Tally $tally): int\n    {\n        return $tally->total(['active' => 1]) + $tally->total([1 => 1]);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Caller.php", php), &[("src/Demo/Tally.sharp", sharp), ("src/Lib/Status.php", STATUS)]),
        ["9:63 possibly-invalid-argument"]
    );
}

/// A plain PHP docblock may write `array<Status, int>`, a type PHP arrays cannot hold. It is the backing values'
/// array, as a PHP# `Map` keyed by `Status` is where it meets plain PHP, so it agrees with the native `array`.
#[test]
fn a_php_docblock_array_keyed_by_a_backed_enum_agrees_with_the_native_array() {
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Status;\n\nfinal class Report\n{\n    /** @return array<Status, int> */\n    public static function counts(): array\n    {\n        return [];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[("src/Lib/Status.php", STATUS)]), Vec::<String>::new());
}

/// Spec section 12 keys a `Map` by an `int`, a `string` or a type with an `int` or `string` backing value, so a
/// class or a pure enum is refused where a `Map` type is written.
#[test]
fn a_map_key_type_without_a_backing_value_is_an_error() {
    let sharp = "namespace Demo;\n\nimport Lib.Line;\nimport Lib.Pure;\n\nclass Tally\n{\n    public Map<Line, int> lines = [:];\n\n    public Map<Pure, int> count(Map<Line, int> counts)\n    {\n        Map<Pure, int> local = [:];\n        return local;\n    }\n}\n";

    assert_eq!(
        refusals(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        [
            "8:12 template-constraint-violation A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value, and `Line` has none. | Key the `Map` by an `int`, a `string` or a backed enum, such as `enum Status : string`.",
            "10:12 template-constraint-violation A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value, and `Pure` has none. | Key the `Map` by an `int`, a `string` or a backed enum, such as `enum Status : string`.",
            "10:33 template-constraint-violation A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value, and `Line` has none. | Key the `Map` by an `int`, a `string` or a backed enum, such as `enum Status : string`.",
            "12:9 template-constraint-violation A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value, and `Pure` has none. | Key the `Map` by an `int`, a `string` or a backed enum, such as `enum Status : string`.",
        ]
    );
}

/// A PHP# field's written type is checked where it is written, as a parameter's is, though its name has no `$`.
#[test]
fn a_field_type_naming_a_missing_class_is_reported_as_a_parameter_type_is() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    public Missing? item = null;\n\n    public int count(Missing? other)\n    {\n        return 0;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[]),
        ["5:12 non-existent-class-like", "7:22 non-existent-class-like"]
    );
}

/// `keys()` and `entries()` would read a backed enum key back as its backing value, so a `Map` has neither until
/// the loop's written key type reads it back as the case.
#[test]
fn a_map_has_no_keys_or_entries_method() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Tally\n{\n    public void read(Map<Status, int> counts)\n    {\n        counts.keys();\n        counts.entries();\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        ["9:16 non-existent-method", "10:16 non-existent-method"]
    );
}

/// A loop over a `Map` keyed by a backed enum reads each key back as its case, through the key type the loop writes.
/// The value's type may be written or left out, and a loop over the values alone needs no key type.
#[test]
fn a_loop_over_a_map_keyed_by_a_backed_enum_reads_the_key_as_the_case_it_writes() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Tally\n{\n    public int read(Map<Status, int> counts)\n    {\n        let total = 0;\n        for (const [Status status, int n] of counts) {\n            total = total + n + this.weight(status);\n        }\n        for (const [Status status, n] of counts) {\n            total = total + n + this.weight(status);\n        }\n        for (const int n of counts) {\n            total = total + n;\n        }\n\n        return total;\n    }\n\n    private int weight(Status status)\n    {\n        return 1;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]), Vec::<String>::new());
}

/// The engine holds a backed enum key as its backing value, and the lowering reads it back as the case from the
/// `Map`'s key type, so a loop reads each key as its case with its type written or not. A written type that cannot
/// hold the case is refused once, as a typed local's.
#[test]
fn a_loop_over_a_map_keyed_by_a_backed_enum_reads_each_key_as_its_case() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Tally\n{\n    public void read(Map<Status, int> counts)\n    {\n        for (const [status, n] of counts) {\n            this.weigh(status);\n        }\n        for (const [Status? status, int n] of counts) {\n        }\n        for (const [string status, int n] of counts) {\n        }\n        for (const [Status status, string n] of counts) {\n        }\n    }\n\n    private void weigh(Status status)\n    {\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        ["14:28 invalid-local-assignment-value", "16:43 invalid-local-assignment-value"]
    );
}

/// A loop reads a backed enum key back through one enum's `from`, so a key type that mixes a backed enum with other
/// types cannot be read back, written on the key or not.
#[test]
fn a_loop_over_a_map_whose_keys_mix_a_backed_enum_with_other_types_is_an_error() {
    let sharp = "namespace Demo;\n\nimport Lib.Size;\nimport Lib.Status;\n\nclass Tally\n{\n    public void read(Map<Status|Size, int> counts, Map<int|Status, int> totals, Map<Status, int> statuses)\n    {\n        for (const [key, n] of counts) {\n        }\n        for (const [key, n] of totals) {\n        }\n        for (const [key, n] of statuses) {\n        }\n    }\n}\n";

    assert_eq!(
        messages(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        [
            "The keys of `counts` mix a backed enum with other types, so the loop cannot read them back.",
            "The keys of `totals` mix a backed enum with other types, so the loop cannot read them back.",
        ]
    );
    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        ["10:21 invalid-foreach-key", "12:21 invalid-foreach-key"]
    );
}

/// A written key or value type checks as a typed local's does. Spec section 12 reads a `Map<string, V>` key back as a
/// `string`, even the key `"5"` PHP stores as an int, because the lowering casts every key of a `Map<string, V>` loop
/// back to `string`, written or not.
#[test]
fn a_written_loop_variable_type_checks_as_a_typed_local_does() {
    let sharp = "namespace Demo;\n\nimport Lib.Line;\n\nclass Tally\n{\n    public void read(Map<string, int> stock, List<Line> lines)\n    {\n        for (const [int|string sku, int n] of stock) {\n        }\n        for (const [string sku, int n] of stock) {\n            this.reserve(sku);\n        }\n        for (const [sku, n] of stock) {\n            this.reserve(sku);\n        }\n        for (const [string? sku, int n] of stock) {\n        }\n        for (const Line line of lines) {\n        }\n        for (const int line of lines) {\n        }\n    }\n\n    private void reserve(string sku)\n    {\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        ["21:24 invalid-local-assignment-value"]
    );
}

/// A `Set` runs as a PHP array keyed by each element, or by a backed enum's backing value, so the first `Set` slice
/// holds `int`, `string` and backed enums. A `Set` of a class or a pure enum is a type that is not supported yet, as
/// the semantic checks refuse any other element type. PHP writes no `Set`.
#[test]
fn a_set_of_a_class_or_a_pure_enum_is_not_supported_yet() {
    let sharp = "namespace Demo;\n\nimport Lib.Line;\nimport Lib.Pure;\nimport Lib.Status;\n\nclass Tally\n{\n    public Set<Line> lines = [];\n\n    public Set<Pure> count(Set<Status> statuses, Set<Line> others)\n    {\n        Set<Pure> local = [];\n        return local;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Line;\n\nfinal class Tally\n{\n    /** @var array<Line> */\n    public array $lines = [];\n\n    /** @param array<Line> $others */\n    public function count(array $others): array\n    {\n        return $others;\n    }\n}\n";

    assert_eq!(
        refusals(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        [
            "9:12 not-supported-yet This type is not supported yet in PHP#. | ",
            "11:12 not-supported-yet This type is not supported yet in PHP#. | ",
            "11:50 not-supported-yet This type is not supported yet in PHP#. | ",
            "13:9 not-supported-yet This type is not supported yet in PHP#. | ",
        ]
    );
    assert_eq!(issues(("src/Demo/Tally.php", php), &[("src/Lib/Status.php", STATUS)]), Vec::<String>::new());
}

/// A `Set` has the methods of `Sharp\SetMethods`, each typed by its elements: `add`, `remove` and `clear` change it,
/// `filter` gives a `Set`, and `map` and `sortedBy` give a `List`.
#[test]
fn set_methods_take_and_give_the_types_of_their_elements() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Tags\n{\n    public int run(Set<string> tags, Set<Status> statuses)\n    {\n        bool added = tags.add(\"vip\");\n        bool removed = tags.remove(\"new\");\n        bool held = tags.contains(\"vip\") && statuses.contains(Status.Active);\n        List<string> listed = tags.toList();\n        Set<string> kept = tags.filter(tag => tag != \"x\");\n        List<int> sizes = tags.map(tag => strlen(tag));\n        bool lengthy = tags.any(tag => strlen(tag) > 3);\n        string first = tags.first(tag => tag != \"\");\n        List<Status> sorted = statuses.sortedBy(status => status.value);\n        statuses.add(Status.Closed);\n        statuses.remove(Status.Active);\n        tags.clear();\n        return tags.count() + tags.sumOf(tag => strlen(tag)) + count(listed) + count(kept) + count(sizes) + count(sorted) + (added && removed && held && lengthy ? 1 : 0) + strlen(first);\n    }\n\n    public void wrong(Set<string> tags, Set<Status> statuses)\n    {\n        tags.add(1);\n        statuses.add(\"active\");\n        List<int> kept = tags.filter(tag => tag != \"x\");\n        Set<int> sizes = tags.map(tag => strlen(tag));\n        tags.get(0);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tags.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        [
            "26:18 invalid-argument",
            "27:22 invalid-argument",
            "28:26 invalid-local-assignment-value",
            "29:26 invalid-local-assignment-value",
            "30:14 non-existent-method",
        ]
    );
}

/// A message about a `Set` method names the `Set` type the code wrote, never `Sharp\SetMethods`, and a parameter's
/// `Set` type reads as PHP# writes it. A `.php` call on `SetMethods<string>` names the receiver as `Set<string>` too,
/// as one on `MapMethods` names a `Map`, and the argument by its PHP type.
#[test]
fn a_set_message_names_the_sharp_type() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Tally\n{\n    public void wrong(Set<string> tags, Set<Status> statuses, Map<string, int> counts)\n    {\n        tags.add(1);\n        statuses.contains(\"active\");\n        tags.get(0);\n        this.mark(counts);\n    }\n\n    public void mark(Set<Status> statuses)\n    {\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Sharp\\SetMethods;\n\nfinal class Tally\n{\n    /** @param SetMethods<string> $tags */\n    public function wrong(SetMethods $tags): void\n    {\n        $tags->add(1);\n        $tags->get(0);\n    }\n}\n";

    let issues =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]);
    let sharp_messages: Vec<&str> = issues.iter().map(|issue| issue.message.as_str()).collect();
    assert_eq!(
        sharp_messages,
        [
            "Invalid argument type for argument #1 of `Set<string>.add`: expected `string`, but found `int`.",
            "Invalid argument type for argument #1 of `Set<Status>.contains`: expected `Status`, but found `string`.",
            "Method `get` does not exist on `Set<string>`.",
            "Invalid argument type for argument #1 of `Tally.mark`: expected `Set<Status>`, but found `Map<string, int>`.",
        ]
    );
    for issue in &issues {
        let text = format!("{issue:?}");
        assert!(!text.contains("SetMethods"), "{text}");
    }
    assert_eq!(
        messages(("src/Demo/Tally.php", php), &[]),
        [
            "Invalid argument type for argument #1 of `Set<string>.add`: expected `string`, but found `int(1)`.",
            "Method `get` does not exist on `Set<string>`.",
        ]
    );
}

/// A `Set` has no index: its elements are tested with `contains` and added with `add`, so an index read or write is an
/// error. PHP keeps reading and writing an array's keys.
#[test]
fn an_index_of_a_set_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public string read(Set<string> tags)\n    {\n        tags[\"vip\"] = \"vip\";\n        return tags[0];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @param array<string, string> $tags */\n    public function read(array $tags): string\n    {\n        $tags['vip'] = 'vip';\n        return $tags['vip'];\n    }\n}\n";

    assert_eq!(
        refusals(("src/Demo/Tags.sharp", sharp), &[]),
        [
            "7:9 invalid-array-access A `Set<string>` has no index. | Test an element with `set.contains(x)`, add one with `set.add(x)`, or read each one with `for (const x of set)`.",
            "8:16 invalid-array-access A `Set<string>` has no index. | Test an element with `set.contains(x)`, add one with `set.add(x)`, or read each one with `for (const x of set)`.",
        ]
    );
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// A `Set` is a collection, which `==` cannot compare by value yet. PHP keeps comparing its arrays.
#[test]
fn equality_of_a_set_is_not_supported_yet() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool same(Set<int> items, Set<int> others) => items == others;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    /** @param array<int, int> $items @param array<int, int> $others */\n    public static function same(array $items, array $others): bool { return $items == $others; }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[]),
        [
            "5:65 not-supported-yet `==` on a collection is not supported yet, so it cannot compare `Set<int>` with `Set<int>`. | Compare the elements one by one."
        ]
    );
}

/// Spec section 12 converts a list passed where a `Set` is expected, so a `List` or a list literal is accepted as an
/// argument, and the receiving method makes it a `Set`. A `Map` is never a `Set`, and a `Set` is never a `Map` or a
/// `List`. A `List` is converted only where it is passed, so a typed local takes no `List`. A plain PHP caller passes a
/// list too.
#[test]
fn a_list_is_passed_where_a_set_is_expected_and_a_map_is_not() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public void take(Set<string> tags)\n    {\n    }\n\n    public void keep(Map<string, string> named, List<string> listed)\n    {\n    }\n\n    public void run(List<string> names, Map<string, string> named, Set<string> tags)\n    {\n        this.take(names);\n        this.take([\"vip\", \"new\"]);\n        this.take(tags);\n        this.take(named);\n        this.keep(tags, names);\n        this.keep(named, tags);\n        Set<string> fromList = names;\n        Set<string> fromMap = named;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Caller\n{\n    public function run(Tags $tags): void\n    {\n        $tags->take(['vip', 'new']);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tags.sharp", sharp), &[]),
        [
            "18:19 invalid-argument",
            "19:19 invalid-argument",
            "20:26 invalid-argument",
            "21:32 invalid-local-assignment-value",
            "22:31 invalid-local-assignment-value",
        ]
    );
    assert_eq!(issues(("src/Demo/Caller.php", php), &[("src/Demo/Tags.sharp", sharp)]), Vec::<String>::new());
}

/// A list becomes a `Set` only in the body of the PHP# method or lambda that declares the parameter `Set`, so only a
/// call of one takes a list there, each element of a variadic one too, and a lambda a local holds is one. Neither
/// `Sharp\ListMethods.add` of a `List<Set<string>>`, whose `Set` comes from its template, nor a call through a
/// `Function` value runs such a body, so each takes only a `Set`. A plain PHP caller still passes a list to a PHP#
/// method.
#[test]
fn a_list_is_passed_as_a_set_only_to_a_body_that_makes_it_one() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public void take(Set<string> tags)\n    {\n    }\n\n    public void many(Set<string> ...tags)\n    {\n    }\n\n    public void run(List<string> names, List<Set<string>> all, Function<bool(Set<string>)> check)\n    {\n        this.take(names);\n        this.many(names, [\"vip\"]);\n        all.add([\"vip\"]);\n        all.add(names);\n        check([\"vip\"]);\n        const typed = (Set<string> tags) => tags.contains(\"vip\");\n        typed([\"vip\"]);\n        Function<bool(Set<string>)> inferred = tags => tags.contains(\"vip\");\n        inferred(names);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Caller\n{\n    public function run(Tags $tags): void\n    {\n        $tags->take(['vip']);\n        $tags->many(['vip'], ['new']);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tags.sharp", sharp), &[]),
        ["17:17 invalid-argument", "18:17 invalid-argument", "19:15 invalid-argument", "23:18 invalid-argument",]
    );
    assert_eq!(issues(("src/Demo/Caller.php", php), &[("src/Demo/Tags.sharp", sharp)]), Vec::<String>::new());
}

/// `Sharp\Set` is the engine's class that makes each `Set` parameter a `Set`, which only the lowering calls, as
/// `Sharp\Collection` is the class each collection method runs on. Neither is a class PHP# code imports and calls, so
/// `Set.from` is an unknown method, as `\Sharp\Set::from` is in PHP.
#[test]
fn the_engines_set_class_is_not_a_class_sharp_code_calls() {
    let sharp = "namespace Demo;\n\nimport Sharp.Set;\n\nclass Tags\n{\n    public int count(List<string> tags)\n    {\n        const made = Set.from(tags);\n        return count(made);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @param list<string> $tags */\n    public function count(array $tags): int\n    {\n        $made = \\Sharp\\Set::from($tags);\n        return count($made);\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), ["9:26 non-existent-method", "10:22 mixed-argument"]);
    assert_eq!(
        issues(("src/Demo/Tags.php", php), &[]),
        ["10:29 non-existent-method", "10:9 mixed-assignment", "11:22 mixed-argument"]
    );
}

/// A `match` gives the value of its arm, so a list literal there is a `List`, which a place declared `Set` refuses.
#[test]
fn a_list_literal_in_a_match_arm_where_a_set_is_declared_is_refused() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public Set<string> pick(bool flag)\n    {\n        Set<string> tags = match (flag) {\n            true => [\"a\"],\n            default => [\"b\"],\n        };\n        return tags;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @return array<string, string> */\n    public function pick(bool $flag): array\n    {\n        $tags = match ($flag) {\n            true => ['a' => 'a'],\n            default => ['b' => 'b'],\n        };\n        return $tags;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), ["7:28 invalid-local-assignment-value"]);
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// `??` gives either side, so a list literal on its right is a `List`, which a place declared `Set` refuses.
#[test]
fn a_list_literal_after_null_coalescing_where_a_set_is_declared_is_refused() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public Set<string> pick(Set<string>? maybe)\n    {\n        Set<string> tags = maybe ?? [\"a\"];\n        return tags;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /**\n     * @param array<string, string>|null $maybe\n     * @return array<string, string>\n     */\n    public function pick(?array $maybe): array\n    {\n        $tags = $maybe ?? ['a' => 'a'];\n        return $tags;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), ["7:28 invalid-local-assignment-value"]);
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// `?:` gives either branch, so a list literal there is a `List`, which a place declared `Set` refuses, and never
/// compiles into a list held as a `Set`.
#[test]
fn a_list_literal_in_a_conditional_where_a_set_is_declared_is_refused() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public Set<string> pick(bool flag)\n    {\n        Set<string> tags = flag ? [\"a\"] : [\"b\"];\n        return tags;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @return array<string, string> */\n    public function pick(bool $flag): array\n    {\n        $tags = $flag ? ['a' => 'a'] : ['b' => 'b'];\n        return $tags;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), ["7:28 invalid-local-assignment-value"]);
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// Only a literal whose own place is declared `Set` is a `Set`, so a list literal inside a `List` of `Set`s is a
/// `List`, which the place refuses.
#[test]
fn a_list_literal_inside_a_list_of_sets_is_refused() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public List<Set<string>> all()\n    {\n        List<Set<string>> all = [[\"a\"]];\n        return all;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @return list<array<string, string>> */\n    public function all(): array\n    {\n        $all = [['a' => 'a']];\n        return $all;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), ["7:33 invalid-local-assignment-value"]);
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// A list literal that is a value of a `Map` literal of `Set`s is a `List`, which the place refuses.
#[test]
fn a_list_literal_as_a_value_in_a_map_of_sets_is_refused() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public Map<string, Set<string>> named()\n    {\n        Map<string, Set<string>> named = [\"x\": [\"a\"]];\n        return named;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @return array<string, array<string, string>> */\n    public function named(): array\n    {\n        $named = ['x' => ['a' => 'a']];\n        return $named;\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Tags.sharp", sharp), &[]), ["7:42 invalid-local-assignment-value"]);
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// A list literal assigned to an entry of a `Map` of `Set`s is a `List`, which the `Map` refuses, whether a property,
/// a parameter or a local holds it. Each keeps its `Set`s, so a later `get` never reads a list as a `Set`.
#[test]
fn a_list_literal_assigned_to_an_entry_of_a_map_of_sets_is_refused() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    private Map<string, Set<string>> named = [:];\n\n    public bool put(Map<string, Set<string>> given)\n    {\n        this.named[\"x\"] = [\"a\"];\n        given[\"x\"] = [\"a\"];\n        Map<string, Set<string>> local = [:];\n        local[\"x\"] = [\"a\"];\n        return local.get(\"x\")?.contains(\"a\") ?? false;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @var array<string, array<string, string>> */\n    private array $named = [];\n\n    /** @param array<string, array<string, string>> $given */\n    public function put(array $given): bool\n    {\n        $this->named['x'] = ['a' => 'a'];\n        $given['x'] = ['a' => 'a'];\n        $local = [];\n        $local['x'] = ['a' => 'a'];\n        return isset($local['x']['a']);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tags.sharp", sharp), &[]),
        [
            "9:9 invalid-property-assignment-value",
            "10:9 invalid-local-assignment-value",
            "12:9 invalid-local-assignment-value",
        ]
    );
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// A PHP# local, parameter or `field` is the collection it is declared as, so an element written into it must fit
/// that type, as one written into a property must, and the place keeps its type. PHP keeps widening an array.
#[test]
fn an_element_written_into_a_typed_local_or_parameter_must_fit_its_type() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    public int put(Map<string, int> counts)\n    {\n        counts[\"x\"] = \"text\";\n        Map<string, int> local = [:];\n        local[\"y\"] = 2;\n        local[\"z\"] = \"text\";\n        return count(counts) + count(local);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tally\n{\n    /** @param array<string, int> $counts */\n    public function put(array $counts): int\n    {\n        $counts['x'] = 'text';\n        $local = [];\n        $local['y'] = 2;\n        $local['z'] = 'text';\n        return count($counts) + count($local);\n    }\n}\n";

    assert_eq!(
        refusals(("src/Demo/Tally.sharp", sharp), &[]),
        [
            "7:9 invalid-local-assignment-value Invalid assignment to `counts`: it is declared as `Map<string, int>`. | Write a value of `Map<string, int>`'s element type, or change the type `counts` is declared with.",
            "10:9 invalid-local-assignment-value Invalid assignment to `local`: it is declared as `Map<string, int>`. | Write a value of `Map<string, int>`'s element type, or change the type `local` is declared with.",
        ]
    );
    assert_eq!(issues(("src/Demo/Tally.php", php), &[]), Vec::<String>::new());
}

/// A value written into a `Map` takes only a literal of the collection its value type declares, so a `Map` of `Map`s
/// refuses `[]` and takes `[:]` and `["x": 1]`, whether a parameter, a property or a local holds it. PHP has one empty
/// array, and its array of arrays takes `[]`.
#[test]
fn an_empty_list_literal_written_as_a_value_of_a_map_of_maps_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    private Map<string, Map<string, int>> named = [:];\n\n    public int put(Map<string, Map<string, int>> counts)\n    {\n        counts[\"a\"] = [];\n        counts[\"a\"] = [:];\n        counts[\"a\"] = [\"x\": 1];\n        this.named[\"a\"] = [];\n        Map<string, Map<string, int>> local = [:];\n        local[\"a\"] = [];\n        return count(counts) + count(local);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tally\n{\n    /** @var array<string, array<string, int>> */\n    private array $named = [];\n\n    /** @param array<string, array<string, int>> $counts */\n    public function put(array $counts): int\n    {\n        $counts['a'] = [];\n        $counts['a'] = ['x' => 1];\n        $this->named['a'] = [];\n        $local = [];\n        $local['a'] = [];\n        return count($counts) + count($local);\n    }\n}\n";

    let message = "invalid-array-element `[]` is an empty List. An empty Map is written `[:]`.";
    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Tally.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [format!("9:23 {message}"), format!("12:27 {message}"), format!("14:22 {message}")]
    );
    assert_eq!(issues(("src/Demo/Tally.php", php), &[]), Vec::<String>::new());
}

/// A value written two levels into a `Map` of `Map`s of `Map`s takes only a literal of the collection its value type
/// declares there, so `deep["a"]["b"]` refuses `[]` and takes `[:]`, as `deep["a"]` does. PHP has one empty array.
#[test]
fn an_empty_list_literal_written_two_levels_into_a_map_of_maps_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    public int put()\n    {\n        Map<string, Map<string, Map<string, int>>> deep = [:];\n        deep[\"a\"][\"b\"] = [];\n        deep[\"a\"][\"b\"] = [:];\n        deep[\"a\"] = [:];\n        return count(deep);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tally\n{\n    public function put(): int\n    {\n        /** @var array<string, array<string, array<string, int>>> $deep */\n        $deep = [];\n        $deep['a']['b'] = [];\n        $deep['a'] = [];\n        return count($deep);\n    }\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Tally.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["8:26 invalid-array-element `[]` is an empty List. An empty Map is written `[:]`."]
    );
    assert_eq!(issues(("src/Demo/Tally.php", php), &[]), Vec::<String>::new());
}

/// A value written into a `Map` takes only a literal of the collection its value type declares, so a `Map` of `List`s
/// and a `Map` of `Set`s refuse `[:]`. PHP has one empty array, and its array of arrays takes `[]`.
#[test]
fn an_empty_map_literal_written_as_a_value_of_a_map_of_lists_or_of_sets_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    public int put(Map<string, List<int>> lists, Map<string, Set<int>> sets)\n    {\n        lists[\"a\"] = [:];\n        lists[\"a\"] = [];\n        lists[\"a\"] = [1];\n        sets[\"a\"] = [:];\n        return count(lists) + count(sets);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tally\n{\n    /**\n     * @param array<string, list<int>> $lists\n     * @param array<string, array<int, int>> $sets\n     */\n    public function put(array $lists, array $sets): int\n    {\n        $lists['a'] = [];\n        $lists['a'] = [1];\n        $sets['a'] = [];\n        return count($lists) + count($sets);\n    }\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Tally.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        [
            "7:22 invalid-array-element `[:]` is an empty Map. An empty List is written `[]`.",
            "10:21 invalid-array-element `[:]` is an empty Map. An empty Set is written `[]`.",
        ]
    );
    assert_eq!(issues(("src/Demo/Tally.php", php), &[]), Vec::<String>::new());
}

/// Pin: a `List` is never written by index, so `[:]` written by index into a `List` of `List`s is refused once, by
/// that rule, and its literal adds no second issue. PHP writes an array by index.
#[test]
fn a_map_literal_written_by_index_into_a_list_of_lists_is_refused_once_by_the_list_write_rule() {
    let sharp = "namespace Demo;\n\nclass Grid\n{\n    public int fill(List<List<int>> rows)\n    {\n        rows[0] = [:];\n        return count(rows);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Grid\n{\n    /** @param list<list<int>> $rows */\n    public function fill(array $rows): int\n    {\n        $rows[0] = [];\n        return count($rows);\n    }\n}\n";

    assert_eq!(
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Grid.sharp", sharp), &[])
            .iter()
            .map(|issue| format!("{} {}", located(sharp, issue), issue.message))
            .collect::<Vec<_>>(),
        ["7:9 invalid-array-access A `List` is not written by index, because a write could leave a gap in its keys."]
    );
    assert_eq!(issues(("src/Demo/Grid.php", php), &[]), Vec::<String>::new());
}

/// Plain PHP receives a `Set` as the array it runs as, keyed by each element or its backing value, so a `Set` passes
/// where plain PHP takes an `array`, and to `count` and `in_array`.
#[test]
fn a_set_passes_where_plain_php_takes_an_array() {
    let sharp = "namespace Demo;\n\nimport Lib.Sink;\nimport Lib.Status;\n\nclass Tally\n{\n    public int total(Set<string> tags, Set<Status> statuses)\n    {\n        return count(tags) + Sink.plain(tags) + Sink.plain(statuses) + (in_array(\"vip\", tags, true) ? 1 : 0);\n    }\n}\n";
    let sink = "<?php\n\nnamespace Lib;\n\nfinal class Sink\n{\n    public static function plain(array $values): int\n    {\n        return count($values);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS), ("src/Lib/Sink.php", sink)]),
        Vec::<String>::new()
    );
}

/// A loop over a `Set` reads its elements, typed or not. A `Set` has no keys of its own, so `[k, v]` is refused, as
/// on a `List`. PHP keeps reading an array's keys.
#[test]
fn a_loop_over_a_set_reads_its_elements_and_no_keys() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Tally\n{\n    public int read(Set<string> tags, Set<Status> statuses)\n    {\n        let total = 0;\n        for (const tag of tags) {\n            total += strlen(tag);\n        }\n        for (const Status status of statuses) {\n            total += strlen(status.value);\n        }\n        for (const [key, tag] of tags) {\n        }\n        return total;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tally\n{\n    /** @param array<string, string> $tags */\n    public function read(array $tags): int\n    {\n        $total = 0;\n        foreach ($tags as $key => $tag) {\n            $total += strlen($key) + strlen($tag);\n        }\n        return $total;\n    }\n}\n";

    assert_eq!(
        refusals(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        [
            "16:34 invalid-iterator `for (const [k, v] of x)` reads the keys of a `Map`, and this is a `Set`. | Loop over the elements alone, as in `for (const x of set)`."
        ]
    );
    assert_eq!(issues(("src/Demo/Tally.php", php), &[]), Vec::<String>::new());
}

/// Spec section 12: the declared type makes a list literal a `Set`, at a typed local, an assignment, a property's
/// initial value, a parameter's default and a return, so each takes the literal and checks its elements.
#[test]
fn a_list_literal_where_a_set_is_declared_is_a_set() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Tags\n{\n    public Set<string> all = [];\n    public Set<Status> open = [Status.Active];\n\n    public Set<string> run(Set<string> extra = [\"new\"])\n    {\n        Set<string> tags = [\"vip\", \"new\"];\n        tags = [\"old\"];\n        this.all = [\"a\", \"b\"];\n        Set<string> wrong = [\"vip\", 1];\n        return [\"x\"];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tags.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        ["15:29 invalid-local-assignment-value"]
    );
}

/// A spread copies a `List`'s values or a `Map`'s entries, and a `Set` is neither, so a `.sharp` literal refuses its
/// spread. PHP keeps spreading the array a `Set` runs as.
#[test]
fn a_spread_of_a_set_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Tags\n{\n    public List<string> all(Set<string> tags)\n    {\n        return [...tags];\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Tags\n{\n    /** @param array<string, string> $tags @return array<string, string> */\n    public function all(array $tags): array\n    {\n        return [...$tags];\n    }\n}\n";

    assert_eq!(
        refusals(("src/Demo/Tags.sharp", sharp), &[])[0],
        "7:17 invalid-array-element Cannot spread a value of type `Set<string>`: PHP# spreads a `List` or a `Map`. | Spread a `List`, such as a `list<int>` from plain PHP."
    );
    assert_eq!(issues(("src/Demo/Tags.php", php), &[]), Vec::<String>::new());
}

/// A `List` spread appends, so a literal of values and `List` spreads is a `List`, a plain PHP `list<int>` included.
#[test]
fn a_list_spread_appends_into_a_list_literal() {
    let sharp = "namespace Demo;\n\nimport Lib.Prices;\n\nclass Report\n{\n    public List<int> join(List<int> open, List<int> closed)\n    {\n        List<int> all = [...open, 0, ...closed, ...Prices.listed()];\n        List<string> names = [...open];\n        return all;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Prices.php", PRICES)]),
        ["10:30 invalid-local-assignment-value"]
    );
}

/// A `Map` spread copies the entries with their keys, which needs the literal's type when it runs, so it waits for
/// typed compilation. A plain PHP `array<string, int>` is a `Map`.
#[test]
fn a_map_spread_keeps_every_key_of_the_map() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public Map<string, int> merge(Map<string, int> defaults, Map<string, int> overrides)\n    {\n        return [...defaults, \"root\": 0, ...overrides];\n    }\n\n    public Map<int, string> byId(Map<int, string> first, Map<int, string> second)\n    {\n        return [...first, ...second];\n    }\n}\n";
    let (issues, artifacts) =
        analyze_with_artifacts(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let type_at = |text: &str| {
        let start = sharp.find(text).unwrap() as u32;
        artifacts.expression_types.get(&(start, start + text.len() as u32)).map(|r#type| r#type.get_id().to_string())
    };

    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(
        type_at("[...defaults, \"root\": 0, ...overrides]").as_deref(),
        Some("array{'root': int, ...<string, int>}")
    );
    assert_eq!(type_at("[...first, ...second]").as_deref(), Some("array<int, string>"));
}

/// PHP renumbers the int keys a spread brings in, so a PHP literal spreading an int-keyed array keeps its list type.
#[test]
fn a_php_spread_of_an_int_keyed_array_renumbers_its_keys() {
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    /**\n     * @param array<int, string> $first\n     * @param array<int, string> $second\n     * @return list<string>\n     */\n    public function byId(array $first, array $second): array\n    {\n        return [...$first, ...$second];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
}

/// A literal is one collection, so a `List`'s values and a `Map`'s entries never share one.
#[test]
fn a_list_and_a_map_in_one_literal_is_an_error() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public void mix(List<int> open, Map<string, int> defaults)\n    {\n        const keyed = [...open, \"root\": 0];\n        const both = [...open, ...defaults];\n        const valued = [...defaults, 5];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        ["7:33 invalid-array-element", "8:32 invalid-array-element", "9:38 invalid-array-element"]
    );
    assert_eq!(
        messages(("src/Demo/Report.sharp", sharp), &[])[0],
        "A literal cannot hold a `List`'s values and a `Map`'s entries together."
    );

    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    /**\n     * @param list<int> $open\n     * @param array<string, int> $defaults\n     */\n    public function mix(array $open, array $defaults): array\n    {\n        return [[...$open, 'root' => 0], [...$open, ...$defaults], [...$defaults, 5]];\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
}

/// Only a `List` or a `Map` spreads into a literal. A value that may not be iterable keeps PHP's own error alone.
#[test]
fn a_spread_of_a_value_that_is_neither_a_list_nor_a_map_is_an_error() {
    let sharp = "namespace Demo;\n\nimport Lib.Prices;\n\nclass Report\n{\n    public void spread(int number)\n    {\n        const streamed = [...Prices.stream()];\n        const numbered = [...number];\n    }\n}\n";

    assert_eq!(
        messages(("src/Demo/Report.sharp", sharp), &[("src/Lib/Prices.php", PRICES)]),
        [
            "Cannot spread a value of type `Iterable<int>`: PHP# spreads a `List` or a `Map`.",
            "Cannot use spread operator on non-iterable type `int`.",
        ]
    );
}

/// The checker's types reach the running program, inferred ones too (decision 029), so the analysis records the type
/// arguments it inferred for each generic call and `new`, by the span of the call, one per template in declaration
/// order.
#[test]
fn the_inferred_type_arguments_of_a_generic_call_and_a_generic_new_are_recorded_by_span() {
    let sharp = "namespace Demo;\n\nimport Lib.Box;\nimport Lib.Pairs;\n\nclass Report\n{\n    public int run()\n    {\n        const pair = Pairs.of(5, \"tea\");\n        const box = new Box(2.5);\n        return count(pair) + (int)box.item;\n    }\n}\n";
    let library = "<?php\n\nnamespace Lib;\n\n/** @template T */\nfinal class Box\n{\n    /** @param T $item */\n    public function __construct(public mixed $item) {}\n}\n\nfinal class Pairs\n{\n    /**\n     * @template K\n     * @template V\n     *\n     * @param K $key\n     * @param V $value\n     *\n     * @return list<K|V>\n     */\n    public static function of(mixed $key, mixed $value): array\n    {\n        return [$key, $value];\n    }\n}\n";
    let call = sharp.find("Pairs.of").unwrap() as u32;
    let instantiation = sharp.find("new Box").unwrap() as u32;
    let end_of = |start: u32| start + sharp[start as usize..].find(')').unwrap() as u32 + 1;

    let (issues, artifacts) = analyze_with_artifacts(
        &PLUGIN_REGISTRY,
        settings(),
        ("src/Demo/Report.sharp", sharp),
        &[("src/Lib/Box.php", library)],
    );
    let type_arguments = |start: u32| -> Vec<String> {
        let span = (start, end_of(start));
        let recorded = artifacts.inferred_type_arguments.get(&span);
        let recorded = recorded.unwrap_or_else(|| panic!("{span:?} in {:?}", artifacts.inferred_type_arguments.keys()));

        recorded.iter().map(|argument| argument.get_id().to_string()).collect()
    };

    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(type_arguments(call), ["int", "string"]);
    assert_eq!(type_arguments(instantiation), ["float"]);
}

/// The lowering reads the type of a property read's receiver to tell a property from a method value, so the analysis
/// types the receiver when it already knows the property's type, as it does after a write or for a property of `this`.
#[test]
fn the_receiver_of_a_property_read_the_analysis_already_knows_is_typed() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public int count = 0;\n\n    public int total() => this.count + 1;\n\n    public int copy(Report other)\n    {\n        other.count = 2;\n        return other.count;\n    }\n}\n";
    let this = sharp.find("this.count").unwrap() as u32;
    let other = sharp.rfind("other.count").unwrap() as u32;

    let (issues, artifacts) =
        analyze_with_artifacts(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[]);
    let receiver = |start: u32, name: &str| {
        let span = (start, start + name.len() as u32);
        artifacts.expression_types.get(&span).map(|r#type| r#type.get_id().to_string())
    };

    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(receiver(this, "this"), Some("$this(Demo\\Report)".to_owned()));
    assert_eq!(receiver(other, "other"), Some("Demo\\Report".to_owned()));
}

/// The lowering reads or calls a member by its kind, so a receiver that can be several classes needs the member to be
/// one kind on all of them: a method on each, or a property on each. Each kind names its classes once.
#[test]
fn a_member_whose_kind_differs_across_the_receivers_classes_is_an_error() {
    let sharp = "namespace Demo;\n\nimport Lib.Invoice;\nimport Lib.Order;\nimport Lib.Quote;\n\nclass Report\n{\n    public int run(Order|Invoice doc, Order|Quote|Invoice three)\n    {\n        const Function<int()> total = doc.total;\n        const Function<int()> again = three.total;\n        return doc.total();\n    }\n}\n";
    let library = "<?php\n\nnamespace Lib;\n\nfinal class Order\n{\n    public function total(): int\n    {\n        return 1;\n    }\n}\n\nfinal class Quote\n{\n    public function total(): int\n    {\n        return 2;\n    }\n}\n\nfinal class Invoice\n{\n    /** @var \\Closure(): int */\n    public \\Closure $total;\n\n    public function __construct()\n    {\n        $this->total = fn (): int => 3;\n    }\n}\n";

    assert_eq!(
        messages(("src/Demo/Report.sharp", sharp), &[("src/Lib/Order.php", library)]),
        [
            "`doc.total` is a method on `Lib\\Order` but a property on `Lib\\Invoice`, so PHP# cannot tell how to read it.",
            "`three.total` is a method on `Lib\\Order` and `Lib\\Quote` but a property on `Lib\\Invoice`, so PHP# cannot tell how to read it.",
            "`doc.total()` calls a method on `Lib\\Order` but a function in a property on `Lib\\Invoice`, so PHP# cannot tell how to call it.",
        ]
    );
}

/// What the analysis recorded that each call of `calls` in the analyzed file runs, with `others` beside it.
fn recorded_callees(
    analyzed: (&'static str, &'static str),
    others: &[(&'static str, &'static str)],
    calls: &[&str],
) -> Vec<Vec<String>> {
    let (_, artifacts) = analyze_with_artifacts(&PLUGIN_REGISTRY, settings(), analyzed, others);

    calls
        .iter()
        .map(|call| {
            let start = analyzed.1.find(call).unwrap() as u32;
            let call = Span::dummy(start, start + call.len() as u32);

            artifacts.get_callees(&call).map(|callee| format!("{callee:?}")).collect()
        })
        .collect()
}

/// A call a PHP class's `__call` or `__callStatic` serves records that magic method, the class the call names, and the
/// name the call wrote, so the lowering reads the method the analysis checked the call against.
#[test]
fn a_call_a_magic_method_serves_records_the_magic_method_and_the_called_name() {
    let sharp = "namespace Demo;\n\nimport Lib.Bag;\nimport Lib.Calc;\n\nclass Report\n{\n    public int run(Bag bag, int extra)\n    {\n        Calc.remember(extra);\n        return strlen(gettype(bag.untagged()));\n    }\n}\n";
    let library = "<?php namespace Lib; final class Bag { public function __call(string $name, array $arguments): mixed { return 1; } } final class Calc { public static function __callStatic(string $name, array $arguments): mixed { return null; } }";

    assert_eq!(
        recorded_callees(
            ("src/Demo/Report.sharp", sharp),
            &[("src/Lib/Bag.php", library)],
            &["bag.untagged()", "Calc.remember(extra)"]
        ),
        [
            [r#"MagicMethod { callee: Method("Lib\\Bag", "__call"), class: "Lib\\Bag", method: "untagged" }"#],
            [r#"MagicMethod { callee: Method("lib\\calc", "__callstatic"), class: "Lib\\Calc", method: "remember" }"#],
        ]
    );
}

/// A call of a property holding a function records the property, whether its type is a PHP# `Function` or a PHP
/// `\Closure`, so the lowering calls the function the property holds.
#[test]
fn a_call_of_a_property_holding_a_function_records_the_property() {
    let sharp = "namespace Demo;\n\nimport Lib.Order;\n\nclass Report\n{\n    private Function<int(int)> scale;\n\n    public Report()\n    {\n        this.scale = n => n * 2;\n    }\n\n    public int run(Order order, int extra)\n    {\n        return this.scale(extra) + order.handler(extra);\n    }\n}\n";
    let library = "<?php namespace Lib; final class Order { /** @var \\Closure(int): int */ public \\Closure $handler; public function __construct() { $this->handler = fn (int $n): int => $n; } }";

    assert_eq!(
        recorded_callees(
            ("src/Demo/Report.sharp", sharp),
            &[("src/Lib/Order.php", library)],
            &["this.scale(extra)", "order.handler(extra)"]
        ),
        [
            [r#"Property { class: "demo\\report", property: "$scale" }"#],
            [r#"Property { class: "lib\\order", property: "$handler" }"#]
        ]
    );
}

/// The type arguments of the call in the analyzed file, with `library` beside it.
fn recorded_type_arguments(analyzed: (&'static str, &'static str), library: &'static str, call: &str) -> Vec<String> {
    let start = analyzed.1.find(call).unwrap() as u32;
    let span = (start, start + call.len() as u32);
    let (issues, artifacts) =
        analyze_with_artifacts(&PLUGIN_REGISTRY, settings(), analyzed, &[("src/Lib/Library.php", library)]);
    assert!(issues.is_empty(), "{issues:?}");

    let recorded = artifacts.inferred_type_arguments.get(&span);
    let recorded = recorded.unwrap_or_else(|| panic!("{span:?} in {:?}", artifacts.inferred_type_arguments.keys()));

    recorded.iter().map(|argument| argument.get_id().to_string()).collect()
}

/// A template no argument binds has no type argument the code chose, so it is `mixed`, whatever its constraint.
#[test]
fn a_call_template_no_argument_binds_records_mixed() {
    let sharp = "namespace Demo;\n\nimport Lib.Pairs;\n\nclass Report\n{\n    public int run()\n    {\n        const none = Pairs.none();\n        return count(none);\n    }\n}\n";
    let library = "<?php\n\nnamespace Lib;\n\nfinal class Pairs\n{\n    /**\n     * @template T of int\n     *\n     * @return list<T>\n     */\n    public static function none(): array\n    {\n        return [];\n    }\n}\n";

    assert_eq!(recorded_type_arguments(("src/Demo/Report.sharp", sharp), library, "Pairs.none()"), ["mixed"]);
}

/// A templated class without a constructor binds no template when it is created, so each type argument is `mixed`,
/// `SplObjectStorage`'s too, though the analyzer types its object with `never` arguments.
#[test]
fn new_on_a_templated_class_without_a_constructor_records_mixed() {
    let sharp = "namespace Demo;\n\nimport Lib.Bag;\n\nclass Report\n{\n    public Bag run()\n    {\n        return new Bag();\n    }\n}\n";
    let library = "<?php\n\nnamespace Lib;\n\n/**\n * @template K\n * @template V\n */\nfinal class Bag\n{\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfunction storage(): \\SplObjectStorage\n{\n    return new \\SplObjectStorage();\n}\n";

    assert_eq!(recorded_type_arguments(("src/Demo/Report.sharp", sharp), library, "new Bag()"), ["mixed", "mixed"]);
    assert_eq!(
        recorded_type_arguments(("src/Demo/storage.php", php), library, "new \\SplObjectStorage()"),
        ["mixed", "mixed"]
    );
}

/// A literal records its scalar type once, however many literals bind the template.
#[test]
fn a_template_bound_by_several_literals_records_their_scalar_type_once() {
    let sharp = "namespace Demo;\n\nimport Lib.Lists;\n\nclass Report\n{\n    public int run()\n    {\n        const sizes = Lists.of(5, 6);\n        const flags = Lists.of(true, false);\n        return count(sizes) + count(flags);\n    }\n}\n";
    let library = "<?php\n\nnamespace Lib;\n\nfinal class Lists\n{\n    /**\n     * @template T\n     *\n     * @param T ...$items\n     *\n     * @return list<T>\n     */\n    public static function of(mixed ...$items): array\n    {\n        return $items;\n    }\n}\n";
    let analyzed = ("src/Demo/Report.sharp", sharp);

    assert_eq!(recorded_type_arguments(analyzed, library, "Lists.of(5, 6)"), ["int"]);
    assert_eq!(recorded_type_arguments(analyzed, library, "Lists.of(true, false)"), ["bool"]);
}

/// A template only a callback binds gets the callback's return type, as every `map` does.
#[test]
fn a_template_only_a_callback_binds_records_the_callback_return_type() {
    let php = "<?php\n\nnamespace Demo;\n\n/**\n * @return list<string>\n */\nfunction labels(): array\n{\n    return \\Lib\\map([1, 2], fn (int $x): string => \"a\");\n}\n";
    let library = "<?php\n\nnamespace Lib;\n\n/**\n * @template T\n * @template R\n *\n * @param list<T> $items\n * @param callable(T): R $f\n *\n * @return list<R>\n */\nfunction map(array $items, callable $f): array\n{\n    return array_map($f, $items);\n}\n";

    assert_eq!(
        recorded_type_arguments(
            ("src/Demo/labels.php", php),
            library,
            "\\Lib\\map([1, 2], fn (int $x): string => \"a\")"
        ),
        ["int", "string"]
    );
}

/// The record widens a copy of each bound, so a call's return type keeps the literal it inferred, and plain PHP that
/// relies on it reports what it reported before.
#[test]
fn recording_type_arguments_leaves_the_issues_of_a_php_generic_call_unchanged() {
    let php = "<?php\n\nnamespace Demo;\n\n/**\n * @template T\n *\n * @param T $value\n *\n * @return T\n */\nfunction same(mixed $value): mixed\n{\n    return $value;\n}\n\n/**\n * @param 5 $five\n */\nfunction takeFive(int $five): void\n{\n}\n\ntakeFive(same(5));\ntakeFive(same(6));\n";

    assert_eq!(issues(("src/Demo/run.php", php), &[]), ["25:10 invalid-argument"]);
}

/// The standard library's `Text`, a static class with a native body, spec section 29.
const TEXT: (&str, &str) = (
    "library/Sharp/Text/Text.sharp",
    "namespace Sharp.Text;\n\npublic static class Text\n{\n    public static extern string slug(string title);\n}\n",
);

#[test]
fn an_extern_method_of_a_static_class_is_called_on_the_class_with_no_issues() {
    let code = "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page\n{\n    public string slug() => Text.slug(\"Hello\");\n}\n";

    assert_eq!(issues(("src/App/Page.sharp", code), &[TEXT]), Vec::<String>::new());
}

#[test]
fn new_on_a_static_class_is_an_error() {
    let code =
        "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Page\n{\n    public Text make() => new Text();\n}\n";

    assert_eq!(
        messages(("src/App/Page.sharp", code), &[TEXT]),
        ["`Text` is a static class, so it has no instances: call its members on the class."]
    );
    assert_eq!(issues(("src/App/Page.sharp", code), &[TEXT]), ["7:31 abstract-instantiation"]);
}

#[test]
fn a_class_that_extends_a_static_class_is_an_error() {
    let code = "namespace App;\n\nimport Sharp.Text.Text;\n\npublic class Slug : Text\n{\n}\n";

    assert_eq!(
        messages(("src/App/Slug.sharp", code), &[TEXT]),
        ["`Text` is a static class, so no class can extend it."]
    );
    assert_eq!(issues(("src/App/Slug.sharp", code), &[TEXT]), ["5:21 extend-final-class"]);
}

/// Semantics takes a well-formed `extern` method in any namespace, because the engine compiles the standard library
/// from `vendor/` without knowing it is vendored. The analyzer knows a project file, and refuses it there.
#[test]
fn an_extern_method_in_a_project_file_is_an_error_in_any_namespace() {
    for analyzed in [
        (
            "src/Sharp/Mine/Text.sharp",
            "namespace Sharp.Mine;\n\npublic static class Text\n{\n    public static extern string slug(string title);\n}\n",
        ),
        (
            "src/App/Text.sharp",
            "namespace App;\n\npublic static class Text\n{\n    public static extern string slug(string title);\n}\n",
        ),
    ] {
        assert_eq!(
            messages(analyzed, &[]),
            ["Only the standard library declares native bodies: give `slug` a body."],
            "{}",
            analyzed.0
        );
        assert_eq!(issues(analyzed, &[]), ["5:33 native-body-outside-library"], "{}", analyzed.0);
    }
}

/// An argument the parameter refuses is named as PHP# writes its type: `Any?`, `List<Any?>`, `int?`, a literal by its
/// type, and `Class<Dog>`. A `false` argument asks for no `int|false` parameter, which PHP# cannot write. The PHP twin
/// keeps Mago's text.
#[test]
fn a_refused_argument_names_its_types_as_sharp_writes_them() {
    let sharp = "namespace Demo;\n\npublic class Animal\n{\n}\n\npublic class Dog : Animal\n{\n}\n\npublic class Report\n{\n    public static int keep(int number) => number;\n\n    public static Dog pet(Dog dog) => dog;\n\n    public static List<int> counts(List<int> numbers) => numbers;\n\n    public static void run(Any? anything, Animal animal, List<Any?> values, int|string key, int? maybe)\n    {\n        Report.keep(anything);\n        Report.pet(animal);\n        Report.counts(values);\n        Report.keep(key);\n        Report.keep(null);\n        Report.keep(maybe);\n        Report.keep(false);\n        Report.keep(\"text\");\n        Report.keep(typeof(Dog));\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Animal\n{\n}\n\nclass Dog extends Animal\n{\n}\n\nclass Report\n{\n    public static function keep(int $number): int\n    {\n        return $number;\n    }\n\n    public static function pet(Dog $dog): Dog\n    {\n        return $dog;\n    }\n\n    /**\n     * @param list<int> $numbers\n     * @return list<int>\n     */\n    public static function counts(array $numbers): array\n    {\n        return $numbers;\n    }\n\n    /** @param list<mixed> $values */\n    public static function run(mixed $anything, Animal $animal, array $values, int|string $key, ?int $maybe): void\n    {\n        Report::keep($anything);\n        Report::pet($animal);\n        Report::counts($values);\n        Report::keep($key);\n        Report::keep(null);\n        Report::keep($maybe);\n        Report::keep(false);\n        Report::keep('text');\n        Report::keep(Dog::class);\n    }\n}\n";

    assert_eq!(
        worded(("src/Demo/Report.php", php), &[]),
        [
            "37:22 mixed-argument Invalid argument type for argument #1 of `Demo\\Report::keep`: expected `int`, but found `mixed`. | Argument has type `mixed` | Arguments to this method are incorrect | The type `mixed` is too general and does not match the expected type `int`. | Add specific type hints or assertions to the argument value.",
            "38:21 less-specific-argument Argument type mismatch for argument #1 of `Demo\\Report::pet`: expected `Demo\\Dog`, but provided type `Demo\\Animal` is less specific. | Provided type `Demo\\Animal` is too general. | Arguments to this method are incorrect | The provided type `Demo\\Animal` can be assigned to `Demo\\Dog`, but is wider (less specific). | Provide a value that more precisely matches `Demo\\Dog` or adjust the parameter type.",
            "39:24 less-specific-nested-argument-type Argument type mismatch for argument #1 of `Demo\\Report::counts`: expected `list<int>`, but provided type `list<mixed>` is less specific. | Provided type `list<mixed>` is too general due to nested `mixed`. | Arguments to this method are incorrect | The structure contains `mixed`, making it incompatible. | Provide a value that more precisely matches `list<int>` or adjust the parameter type.",
            "40:22 possibly-invalid-argument Possible argument type mismatch for argument #1 of `Demo\\Report::keep`: expected `int`, but possibly received `int|string`. | This might not be type `int` | Arguments to this method are incorrect | The provided type `int|string` overlaps with `int` but is not fully contained. | Ensure the argument always has the expected type using checks or assertions.",
            "41:22 null-argument Argument #1 of method `Demo\\Report::keep` is `null`, but parameter type `int` does not accept it. | This argument is `null` | Arguments to this method are incorrect | Provide a non-null value, or declare the parameter as nullable (e.g., `int|null`).",
            "42:22 possibly-null-argument Argument #1 of method `Demo\\Report::keep` is possibly `null`, but parameter type `int` does not accept it. | This argument of type `int|null` might be `null` | Arguments to this method are incorrect | Add a `null` check before this call to ensure the value is not `null`.",
            "43:22 false-argument Argument #1 of method `Demo\\Report::keep` is `false`, but parameter type `int` does not accept it. | This argument is `false` | Arguments to this method are incorrect | Provide a different value, or update the parameter type to accept false (e.g., `int|false`).",
            "44:22 invalid-argument Invalid argument type for argument #1 of `Demo\\Report::keep`: expected `int`, but found `string('text')`. | This has type `string('text')` | Arguments to this method are incorrect | The provided type `string('text')` is not compatible with the expected type `int`. | Change the argument value to match `int`, or update the parameter's type declaration.",
            "45:22 invalid-argument Invalid argument type for argument #1 of `Demo\\Report::keep`: expected `int`, but found `class-string('Demo\\Dog')`. | This has type `class-string('Demo\\Dog')` | Arguments to this method are incorrect | The provided type `class-string('Demo\\Dog')` is not compatible with the expected type `int`. | Change the argument value to match `int`, or update the parameter's type declaration.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Report.sharp", sharp), &[]),
        [
            "21:21 mixed-argument Invalid argument type for argument #1 of `Report.keep`: expected `int`, but found `Any?`. | Argument has type `Any?` | Arguments to this method are incorrect | The type `Any?` is too general and does not match the expected type `int`. | Add specific type hints or assertions to the argument value.",
            "22:20 less-specific-argument Argument type mismatch for argument #1 of `Report.pet`: expected `Dog`, but provided type `Animal` is less specific. | Provided type `Animal` is too general. | Arguments to this method are incorrect | The provided type `Animal` can be assigned to `Dog`, but is wider (less specific). | Provide a value that more precisely matches `Dog` or adjust the parameter type.",
            "23:23 less-specific-nested-argument-type Argument type mismatch for argument #1 of `Report.counts`: expected `List<int>`, but provided type `List<Any?>` is less specific. | Provided type `List<Any?>` is too general due to nested `Any?`. | Arguments to this method are incorrect | The structure contains `Any?`, making it incompatible. | Provide a value that more precisely matches `List<int>` or adjust the parameter type.",
            "24:21 possibly-invalid-argument Possible argument type mismatch for argument #1 of `Report.keep`: expected `int`, but possibly received `int|string`. | This might not be type `int` | Arguments to this method are incorrect | The provided type `int|string` overlaps with `int` but is not fully contained. | Ensure the argument always has the expected type using checks or assertions.",
            "25:21 null-argument Argument #1 of method `Report.keep` is `null`, but parameter type `int` does not accept it. | This argument is `null` | Arguments to this method are incorrect | Provide a non-null value, or declare the parameter as nullable (e.g., `int?`).",
            "26:21 possibly-null-argument Argument #1 of method `Report.keep` is possibly `null`, but parameter type `int` does not accept it. | This argument of type `int?` might be `null` | Arguments to this method are incorrect | Add a `null` check before this call to ensure the value is not `null`.",
            "27:21 false-argument Argument #1 of method `Report.keep` is `false`, but parameter type `int` does not accept it. | This argument is `false` | Arguments to this method are incorrect | Provide a different value.",
            "28:21 invalid-argument Invalid argument type for argument #1 of `Report.keep`: expected `int`, but found `string`. | This has type `string` | Arguments to this method are incorrect | The provided type `string` is not compatible with the expected type `int`. | Change the argument value to match `int`, or update the parameter's type declaration.",
            "29:21 invalid-argument Invalid argument type for argument #1 of `Report.keep`: expected `int`, but found `Class<Dog>`. | This has type `Class<Dog>` | Arguments to this method are incorrect | The provided type `Class<Dog>` is not compatible with the expected type `int`. | Change the argument value to match `int`, or update the parameter's type declaration.",
        ]
    );
}

/// A refused return value, a missing return, a computed property, a typed local, a property write and a list of
/// `Any?` returned as a `List<int>` name their types as PHP# writes them, and name the method as PHP# writes it. The
/// PHP twin keeps Mago's text.
#[test]
fn a_refused_value_names_its_types_as_sharp_writes_them() {
    let sharp = "namespace Demo;\n\npublic class Box\n{\n    public int count = 0;\n\n    public int mixed(Any? value) => value;\n\n    public int nullable(int? value) => value;\n\n    public int empty()\n    {\n        return;\n    }\n\n    public int first(bool found)\n    {\n        if (found) {\n            return 1;\n        }\n    }\n\n    public int total => \"text\";\n\n    public int written(string text)\n    {\n        this.count = text;\n        int local = text;\n        return local;\n    }\n\n    public List<int> counts(List<Any?> values) => values;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Box\n{\n    public int $count = 0;\n\n    public function mixed(mixed $value): int\n    {\n        return $value;\n    }\n\n    public function nullable(?int $value): int\n    {\n        return $value;\n    }\n\n    public function empty(): int\n    {\n        return;\n    }\n\n    public function first(bool $found): int\n    {\n        if ($found) {\n            return 1;\n        }\n    }\n\n    public int $total {\n        get => 'text';\n    }\n\n    public function written(string $text): int\n    {\n        $this->count = $text;\n        return 1;\n    }\n\n    /**\n     * @param list<mixed> $values\n     * @return list<int>\n     */\n    public function counts(array $values): array\n    {\n        return $values;\n    }\n}\n";

    assert_eq!(
        worded(("src/Demo/Box.php", php), &[]),
        [
            "11:16 mixed-return-statement Could not infer a precise return type for function `Demo\\Box::mixed`. Saw type `mixed`. | Type inferred as `mixed` here. | The analysis could not determine a specific type for the value returned here, resulting in `mixed`. This can happen with complex code paths or unannotated data. | Add specific type hints to variables, parameters, or properties involved in calculating the return value. Consider adding a specific return type declaration to the function signature to catch potential mismatches earlier.",
            "16:16 nullable-return-statement Function `Demo\\Box::nullable` is declared to return `int` but possibly returns a nullable value (inferred as `int|null`). | Nullable value returned here. | Return type declared as non-nullable `int` here. | The declared return type does not permit null, but the analysis indicates that 'null' or a nullable type could be returned from this path. | You can either change the return type declaration of `Demo\\Box::nullable` to be nullable (e.g., '?int'), or ensure that this function path always returns a non-null value.",
            "16:16 invalid-return-statement Invalid return type for function `Demo\\Box::nullable`: expected `int`, but found `int|null`. | This has type `int|null` | The type `int|null` returned here is not compatible with the declared return type `int`. | Change the return value to match `int`, or update the function's return type declaration.",
            "21:9 invalid-return-statement Function `Demo\\Box::empty` is declared to return `int` but no return value was specified. | No return value specified here. | Return type declared as `int` here. | The declared return type does not permit 'void', but the analysis indicates that this function path does not return a value. | You can either change the return type declaration of `Demo\\Box::empty` to be 'void', or ensure that this function path always returns a value.",
            "24:21 missing-return-statement Missing return statement in function `first` | This function is declared to return 'int'... | ...but this path can exit without returning a value. | A function that does not explicitly return a value will implicitly return `null`. | Add a `return` statement that provides a value of type 'int' to all paths, or change the function's return type to 'int|null' and return `null` explicitly.",
            "32:16 invalid-return-statement Property hook `Demo\\Box::$total::get` returns `string('text')` but property is typed as `int`. | Expression has type `string('text')`. | The get hook must return a value compatible with the property type `int`. | Change the returned expression to match the property type.",
            "37:24 invalid-property-assignment-value Invalid type for property `$count`: expected `int`, but got `string`. | This expression has type `string` | This property `$count` is declared with type `int` | The type `string` is not compatible with and cannot be assigned to `int`. | Change the assigned value to match the property's type, or update the property's type declaration.",
            "47:16 less-specific-nested-return-statement Returned type `list<mixed>` is less specific than the declared return type `list<int>` for function `Demo\\Box::counts` due to nested 'mixed'. | Returned value's type is too general here due to nested mixed | The analysis detected 'mixed' within the structure of the returned value, making the overall type less specific than what the function declared. | Ensure the structure returned by `Demo\\Box::counts` strictly adheres to the types specified in the `list<int>` return type declaration.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Box.sharp", sharp), &[]),
        [
            "7:37 mixed-return-statement Could not infer a precise return type for method `Box.mixed`. Saw type `Any?`. | Type inferred as `Any?` here. | The analysis could not determine a specific type for the value returned here, resulting in `Any?`. This can happen with complex code paths or unannotated data. | Add specific type hints to variables, parameters, or properties involved in calculating the return value. Consider adding a specific return type declaration to the method signature to catch potential mismatches earlier.",
            "9:40 nullable-return-statement Method `Box.nullable` is declared to return `int` but possibly returns a nullable value (inferred as `int?`). | Nullable value returned here. | Return type declared as non-nullable `int` here. | The declared return type does not permit null, but the analysis indicates that 'null' or a nullable type could be returned from this path. | You can either change the return type declaration of `Box.nullable` to be nullable (e.g., 'int?'), or ensure that this method path always returns a non-null value.",
            "9:40 invalid-return-statement Invalid return type for method `Box.nullable`: expected `int`, but found `int?`. | This has type `int?` | The type `int?` returned here is not compatible with the declared return type `int`. | Change the return value to match `int`, or update the method's return type declaration.",
            "13:9 invalid-return-statement Method `Box.empty` is declared to return `int` but no return value was specified. | No return value specified here. | Return type declared as `int` here. | The declared return type does not permit 'void', but the analysis indicates that this method path does not return a value. | You can either change the return type declaration of `Box.empty` to be 'void', or ensure that this method path always returns a value.",
            "16:16 missing-return-statement Missing return statement in method `Box.first` | This method is declared to return 'int'... | ...but this path can exit without returning a value. | A method that does not explicitly return a value will implicitly return `null`. | Add a `return` statement that provides a value of type 'int' to all paths, or change the method's return type to 'int?' and return `null` explicitly.",
            "23:25 invalid-return-statement Property hook `Box.total.get` returns `string` but property is typed as `int`. | Expression has type `string`. | The get hook must return a value compatible with the property type `int`. | Change the returned expression to match the property type.",
            "27:22 invalid-property-assignment-value Invalid type for property `Box.count`: expected `int`, but got `string`. | This expression has type `string` | This property `Box.count` is declared with type `int` | The type `string` is not compatible with and cannot be assigned to `int`. | Change the assigned value to match the property's type, or update the property's type declaration.",
            "28:21 invalid-local-assignment-value Invalid assignment to `local`: it is declared as `int`. | This value has type `string`. | `local` is declared as `int` here. | Assign a `int` value, or change the type `local` is declared with.",
            "32:51 less-specific-nested-return-statement Returned type `List<Any?>` is less specific than the declared return type `List<int>` for method `Box.counts` due to nested 'Any?'. | Returned value's type is too general here due to nested Any? | The analysis detected 'Any?' within the structure of the returned value, making the overall type less specific than what the method declared. | Ensure the structure returned by `Box.counts` strictly adheres to the types specified in the `List<int>` return type declaration.",
        ]
    );
}

/// An override that does not fit the member it overrides names both members and both types as PHP# writes them. The
/// PHP twin keeps Mago's text.
#[test]
fn an_override_names_its_members_and_types_as_sharp_writes_them() {
    let sharp = "namespace Demo;\n\npublic class Order\n{\n}\n\npublic class Line\n{\n}\n\npublic class Base\n{\n    public Order? item = null;\n\n    public virtual void put(Order item)\n    {\n    }\n\n    public virtual Order get(Order item) => item;\n}\n\npublic class OrderBase : Base\n{\n    public override Line? item = null;\n\n    public override void put(Line item)\n    {\n    }\n\n    public override Line get(Order item) => new Line();\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n}\n\nclass Line\n{\n}\n\nclass Base\n{\n    public ?Order $item = null;\n\n    public function put(Order $item): void\n    {\n    }\n\n    public function get(Order $item): Order\n    {\n        return $item;\n    }\n}\n\nclass OrderBase extends Base\n{\n    public ?Line $item = null;\n\n    public function put(Line $item): void\n    {\n    }\n\n    public function get(Order $item): Line\n    {\n        return new Line();\n    }\n}\n";

    assert_eq!(
        worded(("src/Demo/Base.php", php), &[]),
        [
            "31:21 incompatible-parameter-type Parameter `$item` of `Demo\\OrderBase::put()` expects type `Demo\\Line` but parent `Demo\\Base::put()` expects type `Demo\\Order` | Parameter `$item` expects type `Demo\\Line` but parent expects `Demo\\Order` | Parent method `Demo\\Base::put()` parameter defined here | In class `Demo\\OrderBase` | Parameter types must be contravariant: child must accept equal or wider types than parent. | Change the parameter type to be compatible with the parent method.",
            "35:21 incompatible-return-type Return type `Demo\\Line` of `Demo\\OrderBase::get()` is incompatible with parent return type `Demo\\Order` of `Demo\\Base::get()` | Returns type `Demo\\Line` but parent expects `Demo\\Order` | Parent method `Demo\\Base::get()` return type defined here | In class `Demo\\OrderBase` | Return types must be covariant: child must return equal or narrower types than parent. | Change the return type to be compatible with the parent method.",
            "29:12 incompatible-property-type Property `Demo\\OrderBase::$item` has an incompatible type declaration. | This type `Demo\\Line|null` is incompatible with the parent's type. | The parent property is defined with type `Demo\\Order|null` here. | PHP requires property types to be invariant, meaning the type declaration in a child class must be exactly the same as in the parent class. | Change the type of `$item` to `Demo\\Order|null` to match the parent property.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Base.sharp", sharp), &[]),
        [
            "26:26 incompatible-parameter-type Parameter `item` of `OrderBase.put` expects type `Line` but parent `Base.put` expects type `Order` | Parameter `item` expects type `Line` but parent expects `Order` | Parent method `Base.put` parameter defined here | In class `OrderBase` | Parameter types must be contravariant: child must accept equal or wider types than parent. | Change the parameter type to be compatible with the parent method.",
            "30:26 incompatible-return-type Return type `Line` of `OrderBase.get` is incompatible with parent return type `Order` of `Base.get` | Returns type `Line` but parent expects `Order` | Parent method `Base.get` return type defined here | In class `OrderBase` | Return types must be covariant: child must return equal or narrower types than parent. | Change the return type to be compatible with the parent method.",
            "24:21 incompatible-property-type Property `OrderBase.item` has an incompatible type declaration. | This type `Line?` is incompatible with the parent's type. | The parent property is defined with type `Order?` here. | PHP requires property types to be invariant, meaning the type declaration in a child class must be exactly the same as in the parent class. | Change the type of `item` to `Order?` to match the parent property.",
        ]
    );
}

/// A message names an accessor as C# does, `Box.total.get`, wherever the `get` returns a value of another type or
/// can end without returning. The PHP twin keeps Mago's `Demo\Box::$total::get`.
#[test]
fn a_message_names_an_accessor_as_sharp_writes_it() {
    let sharp = "namespace Demo;\n\npublic class Box\n{\n    public int total => \"text\";\n\n    public int open { get { if (this.ready()) { return 1; } } }\n\n    public int amount => this.raw();\n\n    public int size => this.maybe();\n\n    public int position => strpos(\"ab\", \"b\");\n\n    public bool ready() => true;\n\n    public Any? raw() => null;\n\n    public int? maybe() => null;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Box\n{\n    public int $total {\n        get => 'text';\n    }\n\n    public int $open {\n        get {\n            if ($this->ready()) {\n                return 1;\n            }\n        }\n    }\n\n    public int $amount {\n        get => $this->raw();\n    }\n\n    public int $size {\n        get => $this->maybe();\n    }\n\n    public int $position {\n        get => strpos('ab', 'b');\n    }\n\n    public function ready(): bool\n    {\n        return true;\n    }\n\n    public function raw(): mixed\n    {\n        return null;\n    }\n\n    public function maybe(): ?int\n    {\n        return null;\n    }\n}\n";

    assert_eq!(
        worded(("src/Demo/Box.php", php), &[]),
        [
            "8:16 invalid-return-statement Property hook `Demo\\Box::$total::get` returns `string('text')` but property is typed as `int`. | Expression has type `string('text')`. | The get hook must return a value compatible with the property type `int`. | Change the returned expression to match the property type.",
            "20:16 mixed-return-statement Could not infer a precise return type for property hook `Demo\\Box::$amount::get`. Saw type `mixed`. | Type inferred as `mixed` here. | The analysis could not determine a specific type for the value returned here. | Add specific type hints to variables or properties involved in calculating the return value.",
            "24:16 nullable-return-statement Property hook `Demo\\Box::$size::get` returns nullable value `int|null` but property type is `int`. | Nullable value returned here. | The property type does not permit null, but this expression could return null. | Ensure the hook always returns a non-null value, or change the property type to `?int`.",
            "28:16 falsable-return-statement Property hook `Demo\\Box::$position::get` returns falsable value `false|non-negative-int` but property type is `int`. | Potentially 'false' returned here. | The property type does not permit false, but this expression could return false. | Ensure the hook never returns false, or change the property type to `int|false`.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Box.sharp", sharp), &[]),
        [
            "5:25 invalid-return-statement Property hook `Box.total.get` returns `string` but property is typed as `int`. | Expression has type `string`. | The get hook must return a value compatible with the property type `int`. | Change the returned expression to match the property type.",
            "7:23 missing-return-statement Missing return statement in property hook `Box.open.get` | This property hook is declared to return 'int'... | ...but this path can exit without returning a value. | A property hook that does not explicitly return a value will implicitly return `null`. | Add a `return` statement that provides a value of type 'int' to all paths, or change the property hook's return type to 'int?' and return `null` explicitly.",
            "9:26 mixed-return-statement Could not infer a precise return type for property hook `Box.amount.get`. Saw type `Any?`. | Type inferred as `Any?` here. | The analysis could not determine a specific type for the value returned here. | Add specific type hints to variables or properties involved in calculating the return value.",
            "11:24 nullable-return-statement Property hook `Box.size.get` returns nullable value `int?` but property type is `int`. | Nullable value returned here. | The property type does not permit null, but this expression could return null. | Ensure the hook always returns a non-null value, or change the property type to `int?`.",
            "13:28 falsable-return-statement Property hook `Box.position.get` returns falsable value `bool|int` but property type is `int`. | Potentially `false` returned here. | The property type does not permit `false`, but this expression could return `false`. | Ensure the hook never returns `false`.",
        ]
    );
}

/// A plain PHP parent's abstract, final and by-reference hooks are named as PHP# names an accessor, `Priced.total.get`,
/// when a PHP# class misses or replaces them. The PHP twin keeps Mago's text.
#[test]
fn a_message_names_an_inherited_accessor_as_sharp_writes_it() {
    let library = "<?php\n\nnamespace Lib;\n\ninterface Priced\n{\n    public int $total { get; }\n}\n\nclass Counter\n{\n    public int $count = 0 {\n        final get => $this->count;\n    }\n}\n\ninterface Shared\n{\n    public array $items { &get; }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Counter;\nimport Lib.Priced;\nimport Lib.Shared;\n\npublic class Order : Priced\n{\n}\n\npublic class Tally : Counter\n{\n    public override int count { get => 1; }\n}\n\npublic class Bag : Shared\n{\n    public List<int> items { get => []; }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Counter;\nuse Lib\\Priced;\nuse Lib\\Shared;\n\nclass Order implements Priced\n{\n}\n\nclass Tally extends Counter\n{\n    public int $count {\n        get => 1;\n    }\n}\n\nclass Bag implements Shared\n{\n    public array $items {\n        get => [];\n    }\n}\n";
    let others = [("src/Lib/Library.php", library)];

    assert_eq!(
        worded(("src/Demo/Order.php", php), &others),
        [
            "9:7 unimplemented-abstract-property-hook Class `Demo\\Order` does not implement the abstract property hook `$total::get()`. | `Demo\\Order` is not abstract and must implement this hook | `Lib\\Priced::$total::get()` is defined as abstract here | When a concrete class extends an abstract class or implements an interface, it must provide an implementation for all inherited abstract property hooks. | You can either implement the `get` hook for property `$total` in `Demo\\Order`, or declare `Demo\\Order` as an abstract class.",
            "16:9 override-final-property-hook Cannot override final property hook `Lib\\Counter::$count::get()`. | Attempting to override final hook here | Hook `Lib\\Counter::$count::get()` is declared as final | Final property hooks cannot be overridden in child classes. | Remove the `get` hook from `Demo\\Tally::$count`, or remove the final modifier from the parent hook.",
            "23:9 incompatible-property-hook-signature Declaration of `Demo\\Bag::$items::get()` must be compatible with `& Lib\\Shared::$items::get()`. | This hook does not return by reference | Interface `Lib\\Shared` requires this hook to return by reference | When an interface declares a by-reference hook (`&get`), the implementing class must also return by reference. | Add `&` to the `get` hook declaration: `&get => ...`",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Order.sharp", sharp), &others),
        [
            "7:14 unimplemented-abstract-property-hook Class `Order` does not implement the abstract property hook `Priced.total.get`. | `Order` is not abstract and must implement this hook | `Priced.total.get` is defined as abstract here | When a concrete class extends an abstract class or implements an interface, it must provide an implementation for all inherited abstract property hooks. | You can either implement the `get` hook for property `total` in `Order`, or declare `Order` as an abstract class.",
            "13:33 override-final-property-hook Cannot override final property hook `Counter.count.get`. | Attempting to override final hook here | Hook `Counter.count.get` is declared as final | Final property hooks cannot be overridden in child classes. | Remove the `get` hook from `Tally.count`, or remove the final modifier from the parent hook.",
            "18:30 incompatible-property-hook-signature Declaration of `Bag.items.get` must be compatible with `& Shared.items.get`. | This hook does not return by reference | Interface `Shared` requires this hook to return by reference | When an interface declares a by-reference hook (`&get`), the implementing class must also return by reference. | Add `&` to the `get` hook declaration: `&get => ...`",
        ]
    );
}

/// An override whose parameter does not take the parent's type names the parameter as PHP# writes it, without `$`.
/// The PHP twin keeps Mago's `$item`.
#[test]
fn an_override_names_its_parameter_as_sharp_writes_it() {
    let sharp = "namespace Demo;\n\npublic class Order\n{\n}\n\npublic class Line\n{\n}\n\npublic class Base\n{\n    public virtual void put(Order item)\n    {\n    }\n}\n\npublic class Child : Base\n{\n    public override void put(Line item)\n    {\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n}\n\nclass Line\n{\n}\n\nclass Base\n{\n    public function put(Order $item): void\n    {\n    }\n}\n\nclass Child extends Base\n{\n    public function put(Line $item): void\n    {\n    }\n}\n";

    assert_eq!(
        worded(("src/Demo/Base.php", php), &[]),
        [
            "22:21 incompatible-parameter-type Parameter `$item` of `Demo\\Child::put()` expects type `Demo\\Line` but parent `Demo\\Base::put()` expects type `Demo\\Order` | Parameter `$item` expects type `Demo\\Line` but parent expects `Demo\\Order` | Parent method `Demo\\Base::put()` parameter defined here | In class `Demo\\Child` | Parameter types must be contravariant: child must accept equal or wider types than parent. | Change the parameter type to be compatible with the parent method.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Base.sharp", sharp), &[]),
        [
            "20:26 incompatible-parameter-type Parameter `item` of `Child.put` expects type `Line` but parent `Base.put` expects type `Order` | Parameter `item` expects type `Line` but parent expects `Order` | Parent method `Base.put` parameter defined here | In class `Child` | Parameter types must be contravariant: child must accept equal or wider types than parent. | Change the parameter type to be compatible with the parent method.",
        ]
    );
}

/// A plain PHP refinement is named by the PHP# type that holds it: a `non-empty-string` is a `string`, a
/// `positive-int` an `int`, a `class-string` a `Class<Object>` and an `object` an `Object`, spec section 24. The PHP
/// twin keeps Mago's text.
#[test]
fn a_refined_scalar_a_class_string_and_an_object_are_named_as_sharp_writes_them() {
    let values = "<?php\n\nnamespace Lib;\n\nfinal class Values\n{\n    /** @return non-empty-string */\n    public static function name(): string\n    {\n        return 'a';\n    }\n\n    /** @return positive-int */\n    public static function count(): int\n    {\n        return 1;\n    }\n\n    /** @return class-string */\n    public static function kind(): string\n    {\n        return self::class;\n    }\n\n    public static function thing(): object\n    {\n        return new self();\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Values;\n\npublic class Report\n{\n    public static bool keep(bool flag) => flag;\n\n    public static void run()\n    {\n        Report.keep(Values.name());\n        Report.keep(Values.count());\n        Report.keep(Values.kind());\n        Report.keep(Values.thing());\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Values;\n\nclass Report\n{\n    public static function keep(bool $flag): bool\n    {\n        return $flag;\n    }\n\n    public static function run(): void\n    {\n        Report::keep(Values::name());\n        Report::keep(Values::count());\n        Report::keep(Values::kind());\n        Report::keep(Values::thing());\n    }\n}\n";
    let others = [("src/Lib/Values.php", values)];

    assert_eq!(
        worded(("src/Demo/Report.php", php), &others),
        [
            "16:22 invalid-argument Invalid argument type for argument #1 of `Demo\\Report::keep`: expected `bool`, but found `non-empty-string`. | This has type `non-empty-string` | Arguments to this method are incorrect | The provided type `non-empty-string` is not compatible with the expected type `bool`. | Change the argument value to match `bool`, or update the parameter's type declaration.",
            "17:22 invalid-argument Invalid argument type for argument #1 of `Demo\\Report::keep`: expected `bool`, but found `positive-int`. | This has type `positive-int` | Arguments to this method are incorrect | The provided type `positive-int` is not compatible with the expected type `bool`. | Change the argument value to match `bool`, or update the parameter's type declaration.",
            "18:22 invalid-argument Invalid argument type for argument #1 of `Demo\\Report::keep`: expected `bool`, but found `class-string`. | This has type `class-string` | Arguments to this method are incorrect | The provided type `class-string` is not compatible with the expected type `bool`. | Change the argument value to match `bool`, or update the parameter's type declaration.",
            "19:22 invalid-argument Invalid argument type for argument #1 of `Demo\\Report::keep`: expected `bool`, but found `object`. | This has type `object` | Arguments to this method are incorrect | The provided type `object` is not compatible with the expected type `bool`. | Change the argument value to match `bool`, or update the parameter's type declaration.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Report.sharp", sharp), &others),
        [
            "11:21 invalid-argument Invalid argument type for argument #1 of `Report.keep`: expected `bool`, but found `string`. | This has type `string` | Arguments to this method are incorrect | The provided type `string` is not compatible with the expected type `bool`. | Change the argument value to match `bool`, or update the parameter's type declaration.",
            "12:21 invalid-argument Invalid argument type for argument #1 of `Report.keep`: expected `bool`, but found `int`. | This has type `int` | Arguments to this method are incorrect | The provided type `int` is not compatible with the expected type `bool`. | Change the argument value to match `bool`, or update the parameter's type declaration.",
            "13:21 invalid-argument Invalid argument type for argument #1 of `Report.keep`: expected `bool`, but found `Class<Object>`. | This has type `Class<Object>` | Arguments to this method are incorrect | The provided type `Class<Object>` is not compatible with the expected type `bool`. | Change the argument value to match `bool`, or update the parameter's type declaration.",
            "14:21 invalid-argument Invalid argument type for argument #1 of `Report.keep`: expected `bool`, but found `Object`. | This has type `Object` | Arguments to this method are incorrect | The provided type `Object` is not compatible with the expected type `bool`. | Change the argument value to match `bool`, or update the parameter's type declaration.",
        ]
    );
}

/// A plain PHP type that reaches PHP# is named by the PHP# type the spec gives it: an `array` is a
/// `Map<int|string, Any?>`, an `array-key` an `int|string`, and an `iterable` an `Iterable<…>` of its values, spec
/// sections 12 and 24. `numeric` and `scalar` have no PHP# name and keep Mago's. The PHP twin keeps Mago's text.
#[test]
fn a_plain_php_type_is_named_by_the_type_the_spec_gives_it() {
    let values = "<?php\n\nnamespace Lib;\n\nfinal class Values\n{\n    public static function rows(): array\n    {\n        return [];\n    }\n\n    /** @return array-key */\n    public static function key(): int|string\n    {\n        return 1;\n    }\n\n    /** @return iterable<int> */\n    public static function each(): iterable\n    {\n        return [];\n    }\n\n    /** @return numeric */\n    public static function amount(): int|float|string\n    {\n        return 1;\n    }\n\n    /** @return scalar */\n    public static function plain(): int|float|string|bool\n    {\n        return 1;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Values;\n\npublic class Report\n{\n    public static bool keep(bool flag) => flag;\n\n    public static void run()\n    {\n        Report.keep(Values.rows());\n        Report.keep(Values.key());\n        Report.keep(Values.each());\n        Report.keep(Values.amount());\n        Report.keep(Values.plain());\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Values;\n\nclass Report\n{\n    public static function keep(bool $flag): bool\n    {\n        return $flag;\n    }\n\n    public static function run(): void\n    {\n        Report::keep(Values::rows());\n        Report::keep(Values::key());\n        Report::keep(Values::each());\n        Report::keep(Values::amount());\n        Report::keep(Values::plain());\n    }\n}\n";
    let others = [("src/Lib/Values.php", values)];

    assert_eq!(
        messages(("src/Demo/Report.php", php), &others),
        [
            "Invalid argument type for argument #1 of `Demo\\Report::keep`: expected `bool`, but found `array<array-key, mixed>`.",
            "Invalid argument type for argument #1 of `Demo\\Report::keep`: expected `bool`, but found `array-key`.",
            "Invalid argument type for argument #1 of `Demo\\Report::keep`: expected `bool`, but found `iterable<mixed, int>`.",
            "Invalid argument type for argument #1 of `Demo\\Report::keep`: expected `bool`, but found `numeric`.",
            "Possible argument type mismatch for argument #1 of `Demo\\Report::keep`: expected `bool`, but possibly received `scalar`.",
        ]
    );
    assert_eq!(
        messages(("src/Demo/Report.sharp", sharp), &others),
        [
            "Invalid argument type for argument #1 of `Report.keep`: expected `bool`, but found `Map<int|string, Any?>`.",
            "Invalid argument type for argument #1 of `Report.keep`: expected `bool`, but found `int|string`.",
            "Invalid argument type for argument #1 of `Report.keep`: expected `bool`, but found `Iterable<int>`.",
            "Invalid argument type for argument #1 of `Report.keep`: expected `bool`, but found `numeric`.",
            "Possible argument type mismatch for argument #1 of `Report.keep`: expected `bool`, but possibly received `scalar`.",
        ]
    );
}

/// A method's return type error names the method as PHP# writes it, `Order.total`, calls it a method, and names the
/// returned literal by its type, as spec section 27's example of the engine's refusal shows. The PHP twin keeps
/// upstream's wording.
#[test]
fn a_return_type_error_names_the_method_as_sharp_writes_it() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public int total() => \"text\";\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public function total(): int\n    {\n        return \"text\";\n    }\n}\n";

    assert_eq!(
        worded(("src/Demo/Order.php", php), &[]),
        [
            "9:16 invalid-return-statement Invalid return type for function `Demo\\Order::total`: expected `int`, but found `string('text')`. | This has type `string('text')` | The type `string('text')` returned here is not compatible with the declared return type `int`. | Change the return value to match `int`, or update the function's return type declaration."
        ]
    );
    assert_eq!(
        messages(("src/Demo/Order.sharp", sharp), &[]),
        ["Invalid return type for method `Order.total`: expected `int`, but found `string`."]
    );
    assert_eq!(
        worded(("src/Demo/Order.sharp", sharp), &[]),
        [
            "5:27 invalid-return-statement Invalid return type for method `Order.total`: expected `int`, but found `string`. | This has type `string` | The type `string` returned here is not compatible with the declared return type `int`. | Change the return value to match `int`, or update the method's return type declaration."
        ]
    );
}

/// A property's type errors name the property as PHP# writes it, `Order.total`, at its default and where it is
/// written. The PHP twin keeps upstream's `Demo\Order::$count` and `$total`.
#[test]
fn a_property_type_error_names_the_property_as_sharp_writes_it() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public int total = 0;\n    private int count = \"x\";\n\n    public void change()\n    {\n        this.total = \"ten\";\n    }\n\n    public int read() => this.count;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public int $total = 0;\n    private int $count = \"x\";\n\n    public function change(): void\n    {\n        $this->total = \"ten\";\n    }\n\n    public function read(): int\n    {\n        return $this->count;\n    }\n}\n";

    assert_eq!(
        worded(("src/Demo/Order.php", php), &[]),
        [
            "8:26 invalid-property-default-value Default value for property `Demo\\Order::$count` is not assignable to its declared type. | This default value has type `string('x')` | Property is declared with type `int` | A property's default value must be assignable to the property's declared type. | Change the default value to match the declared type, or update the property type to accept the default.",
            "12:24 invalid-property-assignment-value Invalid type for property `$total`: expected `int`, but got `string('ten')`. | This expression has type `string('ten')` | This property `$total` is declared with type `int` | The type `string('ten')` is not compatible with and cannot be assigned to `int`. | Change the assigned value to match the property's type, or update the property's type declaration.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Order.sharp", sharp), &[]),
        [
            "6:25 invalid-property-default-value Default value for property `Order.count` is not assignable to its declared type. | This default value has type `string` | Property is declared with type `int` | A property's default value must be assignable to the property's declared type. | Change the default value to match the declared type, or update the property type to accept the default.",
            "10:22 invalid-property-assignment-value Invalid type for property `Order.total`: expected `int`, but got `string`. | This expression has type `string` | This property `Order.total` is declared with type `int` | The type `string` is not compatible with and cannot be assigned to `int`. | Change the assigned value to match the property's type, or update the property's type declaration.",
        ]
    );
}

/// A class constant and an enum case are named as PHP# writes them, `Order.LIMIT` and `Status.Active`, where their
/// value is checked and where a read names one that does not exist. The PHP twin keeps upstream's wording.
#[test]
fn a_constant_and_an_enum_case_are_named_as_sharp_writes_them() {
    let sharp = "namespace Demo;\n\nenum Status : string\n{\n    case Active = 1;\n}\n\nclass Order\n{\n    public const int LIMIT = \"x\";\n\n    public static Status first() => Status.Missing;\n\n    public static int limit() => Order.NOPE;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nenum Status: string\n{\n    case Active = 1;\n}\n\nclass Order\n{\n    public const int LIMIT = \"x\";\n\n    public static function first(): Status\n    {\n        return Status::Missing;\n    }\n\n    public static function limit(): int\n    {\n        return Order::NOPE;\n    }\n}\n";

    assert_eq!(
        worded(("src/Demo/Status.php", php), &[]),
        [
            "7:19 invalid-enum-case-value Invalid case value for `Demo\\Status::Active`. Expected `string`, but got `int(1)`. | This value has the type `int(1)` | Enum `Demo\\Status` is defined here with a `string` backing type | Ensure the case value is a literal string or a constant expression that resolves to a string.",
            "12:30 invalid-constant-value Value for constant `Demo\\Order::LIMIT` is not assignable to its declared type. | This value has type `string('x')` | Constant is declared with type `int` | A class constant's value must be assignable to its declared type. | Change the value to match the declared type, or update the declared type to accept the value.",
            "16:24 non-existent-class-constant Enum constant or case `Missing` does not exist. | Constant or case `Missing` not found in enum `Demo\\Status` | On this enum `Demo\\Status` | Check for typos or ensure `Missing` is defined in `Demo\\Status` or its ancestors/interfaces.",
            "16:16 never-return Cannot return value with type 'never' from this function. | This expression has type 'never'. | This return statement is effectively unreachable. | A 'never' return type indicates that a function is guaranteed to exit the script, throw an exception, or loop indefinitely. Code following a call to such a function is unreachable. | Since the preceding expression never returns, this 'return' statement cannot be reached. You can likely remove the 'return' keyword entirely.",
            "21:23 non-existent-class-constant Class-like constant `NOPE` does not exist. | Constant `NOPE` not found in `Demo\\Order` | On this class `Demo\\Order` | Check for typos or ensure `NOPE` is defined in `Demo\\Order` or its ancestors/interfaces.",
            "21:16 never-return Cannot return value with type 'never' from this function. | This expression has type 'never'. | This return statement is effectively unreachable. | A 'never' return type indicates that a function is guaranteed to exit the script, throw an exception, or loop indefinitely. Code following a call to such a function is unreachable. | Since the preceding expression never returns, this 'return' statement cannot be reached. You can likely remove the 'return' keyword entirely.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Status.sharp", sharp), &[]),
        [
            "5:19 invalid-enum-case-value Invalid case value for `Status.Active`. Expected `string`, but got `int`. | This value has the type `int` | Enum `Status` is defined here with a `string` backing type | Ensure the case value is a literal string or a constant expression that resolves to a string.",
            "10:30 invalid-constant-value Value for constant `Order.LIMIT` is not assignable to its declared type. | This value has type `string` | Constant is declared with type `int` | A class constant's value must be assignable to its declared type. | Change the value to match the declared type, or update the declared type to accept the value.",
            "12:44 non-existent-property `Status.Missing` does not exist. | This names no constant, case or static property | The enum `Status` has no constant, case or static property named `Missing`",
            "12:37 invalid-return-statement Invalid return type for method `Order.first`: expected `Status`, but found `null`. | This has type `null` | The type `null` returned here is not compatible with the declared return type `Status`. | Change the return value to match `Status`, or update the method's return type declaration.",
            "14:40 non-existent-property `Order.NOPE` does not exist. | This names no constant, case or static property | The class `Order` has no constant, case or static property named `NOPE`",
            "14:34 invalid-return-statement Invalid return type for method `Order.limit`: expected `int`, but found `null`. | This has type `null` | The type `null` returned here is not compatible with the declared return type `int`. | Change the return value to match `int`, or update the method's return type declaration.",
        ]
    );
}

/// A method or property a class does not have is named as PHP# writes it, `Order.missing`, on an instance, and
/// through the class. The PHP twin keeps upstream's wording.
#[test]
fn an_undefined_method_and_property_are_named_as_sharp_writes_them() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public int total = 0;\n}\n\nclass Report\n{\n    public static int run(Order order) => order.missing();\n\n    public static int read(Order order) => order.gone;\n\n    public static int call() => Order.nothing();\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public int $total = 0;\n}\n\nclass Report\n{\n    public static function run(Order $order): int\n    {\n        return $order->missing();\n    }\n\n    public static function read(Order $order): int\n    {\n        return $order->gone;\n    }\n\n    public static function call(): int\n    {\n        return Order::nothing();\n    }\n}\n";

    assert_eq!(
        messages(("src/Demo/Report.php", php), &[]),
        [
            "Method `missing` does not exist on type `Demo\\Order`.",
            "Could not infer a precise return type for function `Demo\\Report::run`. Saw type `mixed`.",
            "Property `$gone` does not exist on class `Demo\\Order`.",
            "Could not infer a precise return type for function `Demo\\Report::read`. Saw type `mixed`.",
            "Method `nothing` does not exist on type `Demo\\Order`.",
            "Could not infer a precise return type for function `Demo\\Report::call`. Saw type `mixed`.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Report.php", php), &[])[..3],
        [
            "14:24 non-existent-method Method `missing` does not exist on type `Demo\\Order`. | This method selection is invalid | This expression has type `Demo\\Order` | Ensure the `missing` method is defined in the `Demo\\Order` class-like.",
            "14:16 mixed-return-statement Could not infer a precise return type for function `Demo\\Report::run`. Saw type `mixed`. | Type inferred as `mixed` here. | The analysis could not determine a specific type for the value returned here, resulting in `mixed`. This can happen with complex code paths or unannotated data. | Add specific type hints to variables, parameters, or properties involved in calculating the return value. Consider adding a specific return type declaration to the function signature to catch potential mismatches earlier.",
            "19:24 non-existent-property Property `$gone` does not exist on class `Demo\\Order`. | Property not found here | On instance of `Demo\\Order` | The class `Demo\\Order` does not define the property `$gone`. | Define the property in the class or check for its existence before accessing it.",
        ]
    );
    assert_eq!(
        messages(("src/Demo/Report.sharp", sharp), &[]),
        [
            "Method `Order.missing` does not exist.",
            "Could not infer a precise return type for method `Report.run`. Saw type `Any?`.",
            "Property `Order.gone` does not exist.",
            "Could not infer a precise return type for method `Report.read`. Saw type `Any?`.",
            "Method `Order.nothing` does not exist.",
            "Could not infer a precise return type for method `Report.call`. Saw type `Any?`.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Report.sharp", sharp), &[])[..3],
        [
            "10:49 non-existent-method Method `Order.missing` does not exist. | This method selection is invalid | This expression has type `Order` | Ensure the method `Order.missing` is defined.",
            "10:43 mixed-return-statement Could not infer a precise return type for method `Report.run`. Saw type `Any?`. | Type inferred as `Any?` here. | The analysis could not determine a specific type for the value returned here, resulting in `Any?`. This can happen with complex code paths or unannotated data. | Add specific type hints to variables, parameters, or properties involved in calculating the return value. Consider adding a specific return type declaration to the method signature to catch potential mismatches earlier.",
            "12:50 non-existent-property Property `Order.gone` does not exist. | Property not found here | On instance of `Order` | The class `Order` does not define the property `gone`. | Define the property in the class or check for its existence before accessing it.",
        ]
    );
}

/// A value of a literal type is named by its general type, `string` and not `"a"`, where a message compares it with
/// the type it must have: an argument, a local's written type and a parameter's default. A type PHP declares as
/// literals, `'a'|'b'`, is still named as declared. The PHP twin keeps upstream's literal types.
#[test]
fn a_value_of_a_literal_type_is_named_by_its_general_type() {
    let mode = (
        "src/Lib/Mode.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Mode\n{\n    /** @param 'a'|'b' $mode */\n    public static function set(string $mode): void\n    {\n    }\n}\n",
    );
    let sharp = "namespace Demo;\n\nimport Lib.Mode;\n\npublic class Pick\n{\n    public const string NAME = \"a\";\n\n    public static void run(string text)\n    {\n        Mode.set(text);\n        Pick.take(Pick.NAME);\n        let count = 1;\n        count = \"many\";\n    }\n\n    public static void take(int value = \"none\")\n    {\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Mode;\n\nfinal class Pick\n{\n    public const string NAME = 'a';\n\n    public static function run(string $text): void\n    {\n        Mode::set($text);\n        Pick::take(Pick::NAME);\n    }\n\n    public static function take(int $value = 'none'): void\n    {\n    }\n}\n";

    assert_eq!(
        worded(("src/Demo/Pick.php", php), &[mode]),
        [
            "13:19 possibly-invalid-argument Possible argument type mismatch for argument #1 of `Lib\\Mode::set`: expected `string('a')|string('b')`, but possibly received `string`. | This might not be type `string('a')|string('b')` | Arguments to this method are incorrect | The provided type `string` overlaps with `string('a')|string('b')` but is not fully contained. | Ensure the argument always has the expected type using checks or assertions.",
            "14:20 invalid-argument Invalid argument type for argument #1 of `Demo\\Pick::take`: expected `int`, but found `string('a')`. | This has type `string('a')` | Arguments to this method are incorrect | The provided type `string('a')` is not compatible with the expected type `int`. | Change the argument value to match `int`, or update the parameter's type declaration.",
            "17:46 invalid-parameter-default-value Default value for parameter `$value` is not assignable to its declared type. | This default value has type `string('none')` | Parameter `$value` is declared with type `int` | A parameter's default value must be assignable to the parameter's declared type. | Change the default value to match the declared type, or widen the parameter type to accept the default.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Pick.sharp", sharp), &[mode]),
        [
            "11:18 possibly-invalid-argument Possible argument type mismatch for argument #1 of `Mode.set`: expected `\"a\"|\"b\"`, but possibly received `string`. | This might not be type `\"a\"|\"b\"` | Arguments to this method are incorrect | The provided type `string` overlaps with `\"a\"|\"b\"` but is not fully contained. | Ensure the argument always has the expected type using checks or assertions.",
            "12:19 invalid-argument Invalid argument type for argument #1 of `Pick.take`: expected `int`, but found `string`. | This has type `string` | Arguments to this method are incorrect | The provided type `string` is not compatible with the expected type `int`. | Change the argument value to match `int`, or update the parameter's type declaration.",
            "14:17 invalid-local-assignment-value Invalid assignment to `count`: it is declared as `int`. | This value has type `string`. | `count` is declared as `int` here. | Assign a `int` value, or change the type `count` is declared with.",
            "17:41 invalid-parameter-default-value Default value for parameter `value` is not assignable to its declared type. | This default value has type `string` | Parameter `value` is declared with type `int` | A parameter's default value must be assignable to the parameter's declared type. | Change the default value to match the declared type, or widen the parameter type to accept the default.",
        ]
    );
}

/// A literal value keeps its literal where the type it must have holds literals of its kind: `"up"` against
/// `"asc"|"desc"` names the value that fails. The PHP twin keeps upstream's wording.
#[test]
fn a_literal_value_keeps_its_literal_against_a_type_of_literals() {
    let sort = (
        "src/Lib/Sort.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Sort\n{\n    /** @param 'asc'|'desc' $direction */\n    public static function by(string $direction): void\n    {\n    }\n}\n",
    );
    let sharp = "namespace Demo;\n\nimport Lib.Sort;\n\npublic class Report\n{\n    public static void run()\n    {\n        Sort.by(\"up\");\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Sort;\n\nfinal class Report\n{\n    public static function run(): void\n    {\n        Sort::by('up');\n    }\n}\n";

    assert_eq!(
        messages(("src/Demo/Report.php", php), &[sort]),
        [
            "Invalid argument type for argument #1 of `Lib\\Sort::by`: expected `string('asc')|string('desc')`, but found `string('up')`."
        ]
    );
    assert_eq!(
        messages(("src/Demo/Report.sharp", sharp), &[sort]),
        ["Invalid argument type for argument #1 of `Sort.by`: expected `\"asc\"|\"desc\"`, but found `\"up\"`."]
    );
}

/// A constant of a PHP trait read through the trait is named as PHP# writes it, and its help names no `self::`,
/// which PHP# has no form for. The PHP twin keeps upstream's wording.
#[test]
fn a_trait_constant_read_through_the_trait_names_it_as_sharp_writes_it() {
    let flags =
        ("src/Lib/Flags.php", "<?php\n\nnamespace Lib;\n\ntrait Flags\n{\n    public const int LIMIT = 3;\n}\n");
    let sharp = "namespace Demo;\n\nimport Lib.Flags;\n\npublic class Report\n{\n    public static int run() => Flags.LIMIT;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Flags;\n\nfinal class Report\n{\n    public static function run(): int\n    {\n        return Flags::LIMIT;\n    }\n}\n";

    let refusals = |analyzed| -> Vec<String> {
        worded(analyzed, &[flags]).into_iter().filter(|line| line.contains(" direct-trait-constant-access")).collect()
    };

    assert_eq!(
        refusals(("src/Demo/Report.php", php)),
        [
            "11:16 direct-trait-constant-access Cannot access trait constant `Lib\\Flags::LIMIT` directly. | `Lib\\Flags` is a trait | Constant accessed here | Trait constants can only be accessed through classes that use the trait, or via self, static, or $this within the trait. | Access this constant through a class that uses `Lib\\Flags`, or use `self::LIMIT`, `static::LIMIT`, or `$this::LIMIT` instead."
        ]
    );
    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp)),
        [
            "7:32 direct-trait-constant-access Cannot access trait constant `Flags.LIMIT` directly. | `Flags` is a trait | Constant accessed here | Trait constants can only be accessed through classes that use the trait. | Access this constant through a class that uses `Flags`."
        ]
    );
}

/// A member a message reaches through access rules, a static call or a second write is named as PHP# writes it,
/// `Order.total`. The PHP twin keeps upstream's wording.
#[test]
fn a_member_access_refusal_names_the_member_as_sharp_writes_it() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public int count { get; }\n\n    private int secret() => 1;\n\n    public int total { get; private set; } = 0;\n\n    public Order()\n    {\n        this.count = 1;\n        this.count = 2;\n    }\n}\n\nclass Run\n{\n    public static int go(Order order)\n    {\n        order.total = 3;\n\n        return order.secret() + Order.secret();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Order\n{\n    public readonly int $count;\n\n    public private(set) int $total = 0;\n\n    public function __construct()\n    {\n        $this->count = 1;\n        $this->count = 2;\n    }\n\n    private function secret(): int\n    {\n        return 1;\n    }\n}\n\nfinal class Run\n{\n    public static function go(Order $order): int\n    {\n        $order->total = 3;\n\n        return $order->secret() + Order::secret();\n    }\n}\n";

    let refusals = |analyzed| -> Vec<String> {
        worded(analyzed, &[]).into_iter().filter(|line| line.contains(" invalid-")).collect()
    };

    assert_eq!(
        refusals(("src/Demo/Run.php", php)),
        [
            "14:16 invalid-property-write Cannot modify a readonly property after initialization. | This readonly property is already initialized | Write to `Demo\\Order::$count` occurs here | Property is defined as `readonly` here | Readonly properties may be initialized only once. Every later assignment throws an `Error` at runtime. | Remove this assignment or move the property's one-time initialization to this location.",
            "27:17 invalid-property-write Cannot write to private property `$total` on class `Demo\\Order`. | This member is private and cannot be accessed here | Invalid access occurs here, from within `demo\\run` | Member is defined as `private` here | Make the property `$total` writable (e.g., `public` or `public(set)`), or add a public setter method.",
            "29:24 invalid-method-access Cannot access private method `Demo\\Order::secret`. | This member is private and cannot be accessed here | Invalid access occurs here, from within `demo\\run` | Member is defined as `private` here | Change the visibility of method `secret` to `public`, or call it from an allowed scope.",
            "29:42 invalid-method-access Cannot access private method `Demo\\Order::secret`. | This member is private and cannot be accessed here | Invalid access occurs here, from within `demo\\run` | Member is defined as `private` here | Change the visibility of method `secret` to `public`, or call it from an allowed scope.",
            "29:42 invalid-static-method-access Cannot call non-static method `Demo\\Order::secret` statically. | This is a non-static method | To call this method, you must first create an instance of the class (e.g., `$obj = new MyClass(); $obj->method();`).",
        ]
    );
    assert_eq!(
        refusals(("src/Demo/Run.sharp", sharp)),
        [
            "14:14 invalid-property-write Cannot modify a readonly property after initialization. | This readonly property is already initialized | Write to `Order.count` occurs here | Property is defined as `readonly` here | Readonly properties may be initialized only once. Every later assignment throws an `Error` at runtime. | Remove this assignment or move the property's one-time initialization to this location.",
            "22:15 invalid-property-write Cannot write to private property `Order.total`. | This member is private and cannot be accessed here | Invalid access occurs here, from within `Run` | Member is defined as `private` here | Make the property `Order.total` writable (e.g., `public` or `public(set)`), or add a public setter method.",
            "24:22 invalid-method-access Cannot access private method `Order.secret`. | This member is private and cannot be accessed here | Invalid access occurs here, from within `Run` | Member is defined as `private` here | Change the visibility of method `secret` to `public`, or call it from an allowed scope.",
            "24:39 invalid-method-access Cannot access private method `Order.secret`. | This member is private and cannot be accessed here | Invalid access occurs here, from within `Run` | Member is defined as `private` here | Change the visibility of method `secret` to `public`, or call it from an allowed scope.",
            "24:39 invalid-static-method-access Cannot call non-static method `Order.secret` statically. | This is a non-static method | To call this method, you must first create an instance of the class (e.g., `const obj = new MyClass(); obj.method();`).",
        ]
    );
}

/// A property no constructor initializes is named as PHP# writes it, `total`, in its class `Order`. The PHP twin keeps
/// upstream's wording.
#[test]
fn an_uninitialized_property_and_its_class_are_named_as_sharp_writes_them() {
    let sharp = "namespace Demo;\n\nclass Ledger\n{\n    private int count;\n}\n\nclass Order\n{\n    private int total;\n\n    public Order()\n    {\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Ledger\n{\n    private int $count;\n}\n\nclass Order\n{\n    private int $total;\n\n    public function __construct()\n    {\n    }\n}\n";

    let uninitialized = |analyzed| -> Vec<String> {
        worded_with(Settings { check_property_initialization: true, ..settings() }, analyzed, &[])
            .into_iter()
            .filter(|line| line.contains(" missing-constructor") || line.contains(" uninitialized-property"))
            .collect()
    };

    assert_eq!(
        uninitialized(("src/Demo/Order.php", php)),
        [
            "5:7 missing-constructor Class `Demo\\Ledger` has typed properties without default values but no constructor to initialize them. | This class needs a constructor | This property needs initialization | Properties requiring initialization: $count | Add a constructor that initializes all typed properties, or provide default values.",
            "12:17 uninitialized-property Property `$total` is not initialized in the constructor of class `Demo\\Order`. | This property is not initialized | In class `Demo\\Order` | Typed properties without default values must be initialized in the constructor. | Initialize `$total` in the constructor, provide a default value, or make the type nullable.",
        ]
    );
    assert_eq!(
        uninitialized(("src/Demo/Order.sharp", sharp)),
        [
            "3:7 missing-constructor Class `Ledger` has typed properties without default values but no constructor to initialize them. | This class needs a constructor | This property needs initialization | Properties requiring initialization: count | Add a constructor that initializes all typed properties, or provide default values.",
            "10:17 uninitialized-property Property `total` is not initialized in the constructor of class `Order`. | This property is not initialized | In class `Order` | Typed properties without default values must be initialized in the constructor. | Initialize `total` in the constructor, provide a default value, or make the type nullable.",
        ]
    );
}

/// A misplaced or repeated attribute is named as PHP# writes it, `[Field]`. The attribute class keeps its PHP
/// `#[Attribute]` declaration in its own file, and the PHP twin keeps upstream's wording.
#[test]
fn a_misplaced_or_repeated_attribute_is_named_as_sharp_writes_it() {
    let field = "<?php\n\nnamespace Lib;\n\n#[\\Attribute(\\Attribute::TARGET_PROPERTY)]\nfinal class Field\n{\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Field;\n\n[Field]\nclass Report\n{\n    [Field]\n    [Field]\n    private int count = 0;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Field;\n\n#[Field]\nclass Report\n{\n    #[Field]\n    #[Field]\n    private int $count = 0;\n}\n";

    let attribute_issues = |analyzed| -> Vec<String> {
        worded(analyzed, &[("src/Lib/Field.php", field)])
            .into_iter()
            .filter(|line| line.contains(" invalid-attribute-target") || line.contains(" attribute-not-repeatable"))
            .collect()
    };

    assert_eq!(
        attribute_issues(("src/Demo/Report.php", php)),
        [
            "7:3 invalid-attribute-target Attribute `Lib\\Field` cannot be used on a class, interface, enum, or trait. | This attribute is not allowed here | `Lib\\Field` defined here | The definition of `Lib\\Field` restricts its use to the following targets: properties. | Remove the `#[Field]` attribute from this location, or update the `#[Attribute]` declaration on the `Lib\\Field` class to include `a class, interface, enum, or trait` as a valid target.",
            "11:7 attribute-not-repeatable Attribute `Lib\\Field` is not declared as repeatable and has already been used. | Duplicate use of non-repeatable attribute `Lib\\Field` | Attribute `Lib\\Field` was first used here | The attribute `Lib\\Field` is not declared with `Attribute::IS_REPEATABLE` in its `#[Attribute]` flags. Non-repeatable attributes can only be applied once to a given target (e.g., a class, method, property). | Remove this duplicate `Lib\\Field` attribute, or if multiple instances are intended and valid, modify the attribute class `Lib\\Field` to include `Attribute::IS_REPEATABLE` in its `#[Attribute]` declaration (e.g., `#[Attribute(Attribute::TARGET_ALL | Attribute::IS_REPEATABLE)]`).",
        ]
    );
    assert_eq!(
        attribute_issues(("src/Demo/Report.sharp", sharp)),
        [
            "5:2 invalid-attribute-target Attribute `Field` cannot be used on a class, interface, enum, or trait. | This attribute is not allowed here | `Field` defined here | The definition of `Field` restricts its use to the following targets: properties. | Remove the `[Field]` attribute from this location, or update the `#[Attribute]` declaration on the `Field` class to include `a class, interface, enum, or trait` as a valid target.",
            "9:6 attribute-not-repeatable Attribute `Field` is not declared as repeatable and has already been used. | Duplicate use of non-repeatable attribute `Field` | Attribute `Field` was first used here | The attribute `Field` is not declared with `Attribute::IS_REPEATABLE` in its `#[Attribute]` flags. Non-repeatable attributes can only be applied once to a given target (e.g., a class, method, property). | Remove this duplicate `Field` attribute, or if multiple instances are intended and valid, modify the attribute class `Field` to include `Attribute::IS_REPEATABLE` in its `#[Attribute]` declaration (e.g., `#[Attribute(Attribute::TARGET_ALL | Attribute::IS_REPEATABLE)]`).",
        ]
    );
}

/// An arithmetic message names its operand's type as PHP# writes it: an object by its short name, `Order`, and a
/// nullable `int` as `int?`. The PHP twin keeps upstream's `Demo\Order` and `int|null`.
#[test]
fn an_arithmetic_message_names_its_operand_type_as_sharp_writes_it() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public int twice(Order order) => order * 2;\n\n    public int next(int? count) => count + 1;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public function twice(Order $order): int\n    {\n        return $order * 2;\n    }\n\n    public function next(?int $count): int\n    {\n        return $count + 1;\n    }\n}\n";

    assert_eq!(
        worded(("src/Demo/Order.php", php), &[]),
        [
            "9:16 invalid-operand Invalid type for left operand. | Cannot perform arithmetic operation with non-numeric type Demo\\Order | The type(s) of the left operand are not compatible with this binary operation. | Ensure the left operand has a type suitable for this operation (e.g., number for arithmetic, string for concatenation).",
            "9:16 mixed-return-statement Could not infer a precise return type for function `Demo\\Order::twice`. Saw type `mixed`. | Type inferred as `mixed` here. | The analysis could not determine a specific type for the value returned here, resulting in `mixed`. This can happen with complex code paths or unannotated data. | Add specific type hints to variables, parameters, or properties involved in calculating the return value. Consider adding a specific return type declaration to the function signature to catch potential mismatches earlier.",
            "14:16 possibly-null-operand Left operand in arithmetic operation might be `null` (type `int|null`). | This might be `null`. | Performing arithmetic operations on `null` typically results in `0`. | Ensure the left operand is non-null before the operation, potentially using checks or assertions.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Order.sharp", sharp), &[]),
        [
            "5:38 invalid-operand `*` cannot apply to `Order` and `int`: `Order` declares no `operator *`. | This is `Order`. | This is `int`. | `*` on a class instance exists only where its class declares `operator *`. | Apply it to values the instances hold, such as their properties.",
            "5:38 invalid-return-statement Invalid return type for method `Order.twice`: expected `int`, but found `Order`. | This has type `Order` | The type `Order` returned here is not compatible with the declared return type `int`. | Change the return value to match `int`, or update the method's return type declaration.",
            "7:36 possibly-null-operand Left operand in arithmetic operation might be `null` (type `int?`). | This might be `null`. | Performing arithmetic operations on `null` typically results in `0`. | Ensure the left operand is non-null before the operation, potentially using checks or assertions.",
        ]
    );
}

/// A comparison message names its operand's type as PHP# writes it, `Order?` for a nullable object. The PHP twin
/// keeps upstream's `Demo\Order|null`.
#[test]
fn a_comparison_message_names_its_operand_type_as_sharp_writes_it() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public bool early(Order? order) => order < 1;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public function early(?Order $order): bool\n    {\n        return $order < 1;\n    }\n}\n";

    assert_eq!(
        worded(("src/Demo/Order.php", php), &[]),
        [
            "9:16 possibly-null-operand Left operand in `<` comparison might be `null` (type `Demo\\Order|null`). | This might be `null` | If this operand is `null` at runtime, PHP's specific comparison rules for `null` with `<` will apply. | Ensure this operand is non-null or that comparison with `null` is intended and handled safely.",
        ]
    );
    assert_eq!(
        worded(("src/Demo/Order.sharp", sharp), &[]),
        [
            "5:40 invalid-operand `<` cannot compare `Order?` with `int`: `Order` declares no `operator <=>`. | This is `Order?`. | This is `int`. | `<` on a class instance exists only where its class declares `operator <=>`. | Compare values the instances hold, such as their properties.",
        ]
    );
}

/// A type check the variable's type can never pass names that type as PHP# writes it, `string?`. The PHP twin keeps
/// upstream's `null|string`.
#[test]
fn an_impossible_type_check_names_the_variable_type_as_sharp_writes_it() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public bool known(string? code)\n    {\n        if (code is int) {\n            return true;\n        }\n\n        return false;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public function known(?string $code): bool\n    {\n        if (is_int($code)) {\n            return true;\n        }\n\n        return false;\n    }\n}\n";

    let type_of = |analyzed| -> Vec<String> {
        worded(analyzed, &[])
            .into_iter()
            .filter(|line| line.contains(" impossible-type-comparison "))
            .filter_map(|line| {
                line.split(" of type `").nth(1).and_then(|rest| rest.split('`').next()).map(str::to_owned)
            })
            .collect()
    };

    assert_eq!(type_of(("src/Demo/Order.php", php)), ["null|string"]);
    assert_eq!(type_of(("src/Demo/Order.sharp", sharp)), ["string?"]);
}

/// A loop over a value that may be null names its type as PHP# writes it, `List<int>?`, a loop over `null` names
/// PHP#'s null check `x != null` (spec section 19), and neither names PHP's `foreach` (spec section 17). The PHP twin
/// keeps upstream's `list<int>|null`, `foreach` and `$iterable !== null`.
#[test]
fn a_loop_message_names_its_type_and_null_check_as_sharp_writes_them() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public int total(List<int>? items)\n    {\n        int sum = 0;\n        for (const item of items) {\n            sum = sum + item;\n        }\n\n        return sum;\n    }\n\n    public int none()\n    {\n        int sum = 0;\n        for (const item of null) {\n            sum = sum + 1;\n        }\n\n        return sum;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    /**\n     * @param list<int>|null $items\n     */\n    public function total(?array $items): int\n    {\n        $sum = 0;\n        foreach ($items as $item) {\n            $sum = $sum + $item;\n        }\n\n        return $sum;\n    }\n\n    public function none(): int\n    {\n        $sum = 0;\n        foreach (null as $item) {\n            $sum = $sum + 1;\n        }\n\n        return $sum;\n    }\n}\n";
    let loops = |analyzed| -> Vec<String> {
        worded(analyzed, &[]).into_iter().filter(|line| line.contains("-iterator ")).collect()
    };

    assert_eq!(
        loops(("src/Demo/Order.php", php)),
        [
            "13:18 possibly-null-iterator Expression being iterated (type `list<int>|null`) might be `null` at runtime. | This might be `null` | This `foreach` might not be executed | If this expression is `null`, it will be treated as an empty array, and the loop body will not execute. | Consider checking for `null` before the loop if this is not intended.",
            "23:18 null-iterator Iterating over `null` in `foreach`. | This expression is `null` | This `foreach` will not be executed | In PHP, iterating over `null` with `foreach` behaves like iterating an empty array; the loop body will not execute | This can hide uninitialized variables or logic errors. | Ensure the expression is initialized to an array or a Traversable object. If `null` is a possible expected state, consider an explicit check before the loop (e.g., `if ($iterable !== null)`).",
        ]
    );
    assert_eq!(
        loops(("src/Demo/Order.sharp", sharp)),
        [
            "8:28 possibly-null-iterator Expression being iterated (type `List<int>?`) might be `null` at runtime. | This might be `null` | This loop might not be executed | If this expression is `null`, it will be treated as an empty list, and the loop body will not execute. | Consider checking for `null` before the loop if this is not intended.",
            "18:28 null-iterator Iterating over `null` in a loop. | This expression is `null` | This loop will not be executed | Iterating over `null` behaves like iterating an empty list; the loop body will not execute | This can hide uninitialized variables or logic errors. | Ensure the expression is initialized to an `Iterable<T>`. If `null` is a possible expected state, consider an explicit check before the loop (e.g., `if (iterable != null)`).",
        ]
    );
}

/// An unused field names the underscore prefix PHP# writes, `_`, and its fix inserts `_` before the name, since a
/// PHP# field has no `$`. The PHP twin keeps upstream's `$_` and inserts `_` after the `$`.
#[test]
fn an_unused_field_names_and_writes_the_underscore_prefix_of_the_file() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    private int total = 0;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    private int $total = 0;\n}\n";
    let fixed = |analyzed: (&'static str, &'static str)| -> Vec<String> {
        analyze(&PLUGIN_REGISTRY, settings(), analyzed, &[])
            .iter()
            .filter(|issue| issue.code.as_deref() == Some("unused-property"))
            .flat_map(|issue| issue.edits.values().flatten())
            .map(|edit| {
                let mut source = analyzed.1.to_owned();
                source.insert_str(edit.range.start as usize, &String::from_utf8_lossy(&edit.new_text));
                source.lines().find(|line| line.contains("total")).unwrap_or_default().trim().to_owned()
            })
            .collect()
    };
    let unused = |analyzed| -> Vec<String> {
        worded(analyzed, &[]).into_iter().filter(|line| line.contains(" unused-property ")).collect()
    };

    assert_eq!(
        unused(("src/Demo/Order.php", php)),
        [
            "7:17 unused-property Property `$total` is never used. | Property `$total` is declared here. | This property is declared but never read or written within the class. | Consider prefixing the property with an underscore (`$_`) to indicate that it is intentionally unused, or remove it if it is not needed."
        ]
    );
    assert_eq!(fixed(("src/Demo/Order.php", php)), ["private int $_total = 0;"]);
    assert_eq!(
        unused(("src/Demo/Order.sharp", sharp)),
        [
            "5:17 unused-property Property `total` is never used. | Property `total` is declared here. | This property is declared but never read or written within the class. | Consider prefixing the property with an underscore (`_`) to indicate that it is intentionally unused, or remove it if it is not needed."
        ]
    );
    assert_eq!(fixed(("src/Demo/Order.sharp", sharp)), ["private int _total = 0;"]);
}

/// An index read on a value that cannot take one names the value's type as PHP# writes it, and the null check as PHP#
/// writes it.
#[test]
fn an_index_read_message_names_its_type_and_null_check_as_sharp_writes_them() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public int pick(Order? o) => o[0];\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public function pick(?Order $o): int\n    {\n        return $o[0];\n    }\n}\n";
    let refused = |analyzed| -> Vec<String> {
        worded(analyzed, &[])
            .into_iter()
            .filter(|line| line.contains(" invalid-array-access ") || line.contains(" possibly-null-array-access "))
            .collect()
    };

    assert_eq!(
        refused(("src/Demo/Order.php", php)),
        [
            "9:16 invalid-array-access Cannot access array index on object `Demo\\Order` that does not implement `ArrayAccess`. | Object does not implement `ArrayAccess`. | Only objects implementing `ArrayAccess` can be accessed like arrays. | Ensure the object implements `ArrayAccess` before attempting to access it as an array.",
            "9:16 possibly-null-array-access Cannot perform array access on possibly `null` value. | The expression might be `null` here. | Attempting to read an array index on `null` will result in a runtime error. | Ensure the variable holds an array before accessing it, possibly by checking with `is_array()` or initializing it.",
        ]
    );
    assert_eq!(
        refused(("src/Demo/Order.sharp", sharp)),
        [
            "5:34 invalid-array-access Cannot access array index on object `Order` that does not implement `ArrayAccess`. | Object does not implement `ArrayAccess`. | Only objects implementing `ArrayAccess` can be accessed like arrays. | Ensure the object implements `ArrayAccess` before attempting to access it as an array.",
            "5:34 possibly-null-array-access Cannot perform array access on possibly `null` value. | The expression might be `null` here. | Attempting to read an array index on `null` will result in a runtime error. | Ensure the value is not `null` before reading an index, as in `if (value != null)`.",
        ]
    );
}

/// A `.sharp` file's docblock is only a comment: an inline `@psalm-trace` reports nothing and an inline `@var` asserts
/// nothing. The PHP twin keeps upstream's reports.
#[test]
fn an_inline_docblock_in_a_sharp_file_traces_and_asserts_nothing() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public int run(int? count)\n    {\n        /** @psalm-trace $count */\n        let total = 1;\n        /** @var string $total */\n        let other = total;\n        return 1;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public function run(?int $count): int\n    {\n        /** @psalm-trace $count */\n        $total = 1;\n        /** @var string $total */\n        $other = $total;\n        return 1;\n    }\n}\n";
    let docblock = |analyzed| -> Vec<String> {
        worded(analyzed, &[])
            .into_iter()
            .filter(|line| line.contains(" psalm-trace ") || line.contains(" docblock-type-mismatch "))
            .collect()
    };

    assert_eq!(
        docblock(("src/Demo/Order.php", php)),
        [
            "9:26 psalm-trace Trace: Type of `$count` is `int|null` | Type is: `int|null` | Spotted a `@psalm-trace` tag! While this works for compatibility, Mago has a more powerful way to inspect types. | For more flexible debugging, try using `Mago\\inspect()` directly in your code. It can inspect any expression, not just variables (e.g., `Mago\\inspect($foo->bar());`).",
            "11:18 docblock-type-mismatch Docblock type mismatch for variable `$total`. | This docblock asserts the type should be `string`, but it was previously defined as `int(1)`. | The type of the variable defined in the docblock does not match the previously defined type. | Change the docblock type to match `int(1)`, or update the variable definition to a compatible type `string`.",
        ]
    );
    assert_eq!(docblock(("src/Demo/Order.sharp", sharp)), Vec::<String>::new());
}

/// A `.sharp` file's docblock changes no declared type: `@var` on a field, and `@template`, `@param` and `@return` on
/// a method. The PHP twin reads them as upstream does.
#[test]
fn a_docblock_in_a_sharp_file_changes_no_declared_type() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    /** @var int */\n    public string total = \"\";\n\n    /**\n     * @template T\n     * @param T label\n     * @return int\n     */\n    public string name(string label) => label;\n\n    public string read() => this.total;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    /** @var int */\n    public string $total = \"\";\n\n    /**\n     * @template T\n     * @param T $label\n     * @return int\n     */\n    public function name(string $label): string\n    {\n        return $label;\n    }\n\n    public function read(): string\n    {\n        return $this->total;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Order.php", php), &[]),
        [
            "8:12 docblock-type-mismatch",
            "8:28 invalid-property-default-value",
            "15:42 docblock-type-mismatch",
            "15:26 docblock-type-mismatch",
            "17:16 less-specific-nested-return-statement",
            "22:16 invalid-return-statement",
        ]
    );
    assert_eq!(issues(("src/Demo/Order.sharp", sharp), &[]), Vec::<String>::new());
}

/// A read or call through a value that may be null names PHP#'s null-safe operator `?.` and its null check
/// `x != null`, spec sections 14.4 and 19, and its fix writes `?.` into the file. The PHP twin keeps upstream's `?->`,
/// `$obj !== null` and its `?->` fix.
#[test]
fn a_possibly_null_member_access_names_and_writes_the_null_safe_operator_of_the_file() {
    let box_class = (
        "src/Lib/Box.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Box\n{\n    public int $value = 0;\n\n    public function count(): int\n    {\n        return 1;\n    }\n}\n",
    );
    let sharp = "namespace Demo;\n\nimport Lib.Box;\n\nclass Report\n{\n    public static int? read(Box? box) => box.value;\n\n    public static int? count(Box? box) => box.count();\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Box;\n\nclass Report\n{\n    public static function read(?Box $box): ?int\n    {\n        return $box->value;\n    }\n\n    public static function count(?Box $box): ?int\n    {\n        return $box->count();\n    }\n}\n";
    let fixes = |analyzed| -> Vec<String> {
        analyze(&PLUGIN_REGISTRY, settings(), analyzed, &[box_class])
            .iter()
            .flat_map(|issue| issue.edits.values().flatten())
            .map(|edit| String::from_utf8_lossy(&edit.new_text).into_owned())
            .collect()
    };

    assert_eq!(
        worded(("src/Demo/Report.php", php), &[box_class]),
        [
            "11:16 possibly-null-property-access Attempting to access a property on a possibly `null` value. | This expression can be `null` here | If this expression is `null` at runtime, PHP will raise a warning and the property access will result in `null`. | Use the nullsafe operator (`?->`) to safely access the property, or add a check to ensure the value is not `null` (e.g., `if ($obj !== null)`).",
            "16:16 possible-method-access-on-null Attempting to call a method on `null`. | This expression can be `null` | Use the nullsafe operator (`?->`) if `null` is an expected value.",
            "16:16 mixed-return-statement Could not infer a precise return type for function `Demo\\Report::count`. Saw type `mixed`. | Type inferred as `mixed` here. | The analysis could not determine a specific type for the value returned here, resulting in `mixed`. This can happen with complex code paths or unannotated data. | Add specific type hints to variables, parameters, or properties involved in calculating the return value. Consider adding a specific return type declaration to the function signature to catch potential mismatches earlier.",
        ]
    );
    assert_eq!(fixes(("src/Demo/Report.php", php)), ["?->"]);
    assert_eq!(
        worded(("src/Demo/Report.sharp", sharp), &[box_class]),
        [
            "7:42 possibly-null-property-access Attempting to access a property on a possibly `null` value. | This expression can be `null` here | If this expression is `null` at runtime, PHP will raise a warning and the property access will result in `null`. | Use the nullsafe operator (`?.`) to safely access the property, or add a check to ensure the value is not `null` (e.g., `if (obj != null)`).",
            "9:43 possible-method-access-on-null Attempting to call a method on `null`. | This expression can be `null` | Use the nullsafe operator (`?.`) if `null` is an expected value.",
            "9:43 mixed-return-statement Could not infer a precise return type for method `Report.count`. Saw type `Any?`. | Type inferred as `Any?` here. | The analysis could not determine a specific type for the value returned here, resulting in `Any?`. This can happen with complex code paths or unannotated data. | Add specific type hints to variables, parameters, or properties involved in calculating the return value. Consider adding a specific return type declaration to the method signature to catch potential mismatches earlier.",
        ]
    );
    assert_eq!(fixes(("src/Demo/Report.sharp", sharp)), ["?."]);
}

/// A plugin message in a `.sharp` file writes PHP# code, `intdiv(num, 0)` and `divisor != 0` (spec section 19), and
/// names a parameter without `$`. The PHP twin keeps upstream's text.
#[test]
fn a_plugin_message_writes_the_code_and_parameter_names_of_the_file() {
    let sharp = "namespace Demo;\n\nimport SessionHandlerInterface;\n\nclass Order\n{\n    public int half(int total) => intdiv(total, 0);\n\n    public bool save(SessionHandlerInterface handler) => session_set_save_handler(handler, true, 1);\n\n    public bool open() => session_set_save_handler(() => true);\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse SessionHandlerInterface;\n\nclass Order\n{\n    public function half(int $total): int\n    {\n        return intdiv($total, 0);\n    }\n\n    public function save(SessionHandlerInterface $handler): bool\n    {\n        return session_set_save_handler($handler, true, 1);\n    }\n\n    public function open(): bool\n    {\n        return session_set_save_handler(fn() => true);\n    }\n}\n";
    let plugin = |analyzed| -> Vec<String> {
        worded(analyzed, &[])
            .into_iter()
            .filter(|line| line.contains(" invalid-operand ") || line.contains("-arguments "))
            .collect()
    };

    assert_eq!(
        plugin(("src/Demo/Order.php", php)),
        [
            "11:31 invalid-operand Call to `intdiv()` with a zero divisor. | This divisor is zero | In this `intdiv()` call | `intdiv($num, 0)` throws `DivisionByZeroError` at runtime. | Guard the call with `$divisor !== 0` or restrict the divisor's type to exclude zero.",
            "16:57 too-many-arguments Too many arguments provided for function `session_set_save_handler`. | Unexpected argument provided here | For this function call | When the first argument is a `SessionHandlerInterface`, `session_set_save_handler()` expects at most 2 arguments, but received 3. | Remove the extra arguments. The object form only accepts the handler and an optional `$register_shutdown` boolean.",
            "21:40 too-few-arguments Too few arguments provided for function `session_set_save_handler`. | Only 1 argument(s) provided | For this function call | The callable form of `session_set_save_handler()` requires at least 6 arguments (`$open`, `$close`, `$read`, `$write`, `$destroy`, `$gc`), but only 1 were provided. | Provide all 6 required callback arguments, or pass a `SessionHandlerInterface` object instead.",
        ]
    );
    assert_eq!(
        plugin(("src/Demo/Order.sharp", sharp)),
        [
            "7:49 invalid-operand Call to `intdiv()` with a zero divisor. | This divisor is zero | In this `intdiv()` call | `intdiv(num, 0)` throws `DivisionByZeroError` at runtime. | Guard the call with `divisor != 0` or restrict the divisor's type to exclude zero.",
            "9:98 too-many-arguments Too many arguments provided for function `session_set_save_handler`. | Unexpected argument provided here | For this function call | When the first argument is a `SessionHandlerInterface`, `session_set_save_handler()` expects at most 2 arguments, but received 3. | Remove the extra arguments. The object form only accepts the handler and an optional `register_shutdown` boolean.",
            "11:51 too-few-arguments Too few arguments provided for function `session_set_save_handler`. | Only 1 argument(s) provided | For this function call | The callable form of `session_set_save_handler()` requires at least 6 arguments (`open`, `close`, `read`, `write`, `destroy`, `gc`), but only 1 were provided. | Provide all 6 required callback arguments, or pass a `SessionHandlerInterface` object instead.",
        ]
    );
}

/// A message names a variable or a parameter as the file writes it: `code` in a `.sharp` file, as a property is named
/// without `$`. The PHP twin keeps upstream's `$code`.
#[test]
fn a_message_names_a_variable_as_sharp_writes_it() {
    let sharp = "namespace Demo;\n\nclass Order\n{\n    public bool known(string code) => code is string;\n\n    public bool counted(int count) => count is string;\n\n    public int take(int amount) => amount;\n\n    public int total() => this.take(cost: 1);\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public function known(string $code): bool\n    {\n        return is_string($code);\n    }\n\n    public function counted(int $count): bool\n    {\n        return is_string($count);\n    }\n\n    public function take(int $amount): int\n    {\n        return $amount;\n    }\n\n    public function total(): int\n    {\n        return $this->take(cost: 1);\n    }\n}\n";
    let named = |analyzed| -> Vec<String> {
        worded(analyzed, &[])
            .into_iter()
            .filter(|line| line.contains("-type-comparison ") || line.contains("named-argument "))
            .collect()
    };

    assert_eq!(
        named(("src/Demo/Order.php", php)),
        [
            "9:16 redundant-type-comparison Redundant type assertion: `$code` is already `string`. | Argument `$code` already has type `string` | The assertion against `string` always holds because `$code` is `string`. | Consider removing this assertion or replacing it with `default` if used in a `match` arm.",
            "14:16 impossible-type-comparison Impossible type assertion: `$count` of type `int` can never be `string`. | Argument `$count` has type `int` | The assertion expects `$count` to be `string`, but no value of type `int` can satisfy this. | Check that the correct variable is being passed, or update the assertion type.",
            "24:28 invalid-named-argument Invalid named argument `$cost` for method `Demo\\Order::take` | Unknown argument name `$cost` | Call to method is here | Available parameters are: `amount`.",
        ]
    );
    assert_eq!(
        named(("src/Demo/Order.sharp", sharp)),
        [
            "5:47 redundant-type-comparison Redundant type assertion: `code` is already `string`. | Argument `code` already has type `string` | The assertion against `string` always holds because `code` is `string`. | Consider removing this assertion or replacing it with `default` if used in a `match` arm.",
            "7:48 impossible-type-comparison Impossible type assertion: `count` of type `int` can never be `string`. | Argument `count` has type `int` | The assertion expects `count` to be `string`, but no value of type `int` can satisfy this. | Check that the correct variable is being passed, or update the assertion type.",
            "11:37 invalid-named-argument Invalid named argument `cost` for method `Order.take` | Unknown argument name `cost` | Call to method is here | Available parameters are: `amount`.",
        ]
    );
}

/// A type the PHP twin prints from one atomic keeps upstream's text: a template type is `'T.demo\fail() extends
/// mixed`, never wrapped in the parentheses a union of several types puts around it.
#[test]
fn a_php_message_names_a_template_type_without_union_parentheses() {
    let php = "<?php\n\nnamespace Demo;\n\n/**\n * @template T\n * @param T $value\n */\nfunction fail($value): void\n{\n    throw $value;\n}\n";

    assert_eq!(
        worded(("src/Demo/Fail.php", php), &[])
            .into_iter()
            .filter(|line| line.contains(" invalid-throw "))
            .collect::<Vec<_>>(),
        [
            "11:5 invalid-throw Cannot throw type `'T.demo\\fail() extends mixed` because it is not an instance of Throwable. | This has type `'T.demo\\fail() extends mixed`, not `Throwable` | Only objects that implement the `Throwable` interface (like `Exception` or `Error`) can be thrown. | Ensure the value being thrown is an instance of `Exception`, `Error`, or a subclass thereof."
        ]
    );
}

/// A class whose short name another class-like shares is named by its full dotted name in a `.sharp` file, so the two
/// stay apart: `App.Orders.Order` and `Billing.Order`. The PHP twin keeps upstream's full names.
#[test]
fn classes_that_share_a_short_name_are_named_by_their_full_name() {
    let app_order = ("src/App/Orders/Order.php", "<?php\n\nnamespace App\\Orders;\n\nclass Order\n{\n}\n");
    let billing_order = ("src/Billing/Order.php", "<?php\n\nnamespace Billing;\n\nclass Order\n{\n}\n");
    let ledger = (
        "src/Billing/Ledger.php",
        "<?php\n\nnamespace Billing;\n\nclass Ledger\n{\n    public function last(): Order\n    {\n        return new Order();\n    }\n}\n",
    );
    let sharp = "namespace App.Orders;\n\nimport Billing.Ledger;\n\nclass Shop\n{\n    public Order last(Ledger ledger) => ledger.last();\n}\n";
    let php = "<?php\n\nnamespace App\\Orders;\n\nuse Billing\\Ledger;\n\nclass Shop\n{\n    public function last(Ledger $ledger): Order\n    {\n        return $ledger->last();\n    }\n}\n";
    let others = [app_order, billing_order, ledger];
    let returned = |analyzed| -> Vec<String> {
        worded(analyzed, &others).into_iter().filter(|line| line.contains(" invalid-return-statement ")).collect()
    };

    assert_eq!(
        returned(("src/App/Orders/Shop.php", php)),
        [
            "11:16 invalid-return-statement Invalid return type for function `App\\Orders\\Shop::last`: expected `App\\Orders\\Order`, but found `Billing\\Order`. | This has type `Billing\\Order` | The type `Billing\\Order` returned here is not compatible with the declared return type `App\\Orders\\Order`. | Change the return value to match `App\\Orders\\Order`, or update the function's return type declaration."
        ]
    );
    assert_eq!(
        returned(("src/App/Orders/Shop.sharp", sharp)),
        [
            "7:41 invalid-return-statement Invalid return type for method `Shop.last`: expected `App.Orders.Order`, but found `Billing.Order`. | This has type `Billing.Order` | The type `Billing.Order` returned here is not compatible with the declared return type `App.Orders.Order`. | Change the return value to match `App.Orders.Order`, or update the method's return type declaration."
        ]
    );
}

/// A class the file imports is named by the name its import gives it, the alias or the last segment as written, even
/// when another class shares its short name: `Order` and `BillingOrder`, never the `App.Orders.Order` the file never
/// wrote. The PHP twin keeps upstream's full names.
#[test]
fn a_class_the_file_imports_is_named_by_the_name_its_import_gives_it() {
    let app_order = ("src/App/Orders/Order.php", "<?php\n\nnamespace App\\Orders;\n\nclass Order\n{\n}\n");
    let billing_order = ("src/Billing/Order.php", "<?php\n\nnamespace Billing;\n\nclass Order\n{\n}\n");
    let ledger = (
        "src/Billing/Ledger.php",
        "<?php\n\nnamespace Billing;\n\nclass Ledger\n{\n    public function last(): Order\n    {\n        return new Order();\n    }\n}\n",
    );
    let sharp = "namespace App.Shop;\n\nimport App.Orders.Order;\nimport Billing.Order as BillingOrder;\nimport Billing.Ledger;\n\nclass Shop\n{\n    public Order last(Ledger ledger) => ledger.last();\n}\n";
    let php = "<?php\n\nnamespace App\\Shop;\n\nuse App\\Orders\\Order;\nuse Billing\\Order as BillingOrder;\nuse Billing\\Ledger;\n\nclass Shop\n{\n    public function last(Ledger $ledger): Order\n    {\n        return $ledger->last();\n    }\n}\n";
    let others = [app_order, billing_order, ledger];
    let returned = |analyzed| -> Vec<String> {
        worded(analyzed, &others).into_iter().filter(|line| line.contains(" invalid-return-statement ")).collect()
    };

    assert_eq!(
        returned(("src/App/Shop/Shop.php", php)),
        [
            "13:16 invalid-return-statement Invalid return type for function `App\\Shop\\Shop::last`: expected `App\\Orders\\Order`, but found `Billing\\Order`. | This has type `Billing\\Order` | The type `Billing\\Order` returned here is not compatible with the declared return type `App\\Orders\\Order`. | Change the return value to match `App\\Orders\\Order`, or update the function's return type declaration."
        ]
    );
    assert_eq!(
        returned(("src/App/Shop/Shop.sharp", sharp)),
        [
            "9:41 invalid-return-statement Invalid return type for method `Shop.last`: expected `Order`, but found `BillingOrder`. | This has type `BillingOrder` | The type `BillingOrder` returned here is not compatible with the declared return type `Order`. | Change the return value to match `Order`, or update the method's return type declaration."
        ]
    );
}

/// PHP's built-in class-likes don't count toward a shared short name: a `.sharp` file reaches `Dom\Node` only through
/// an import, so the project's own `Node` keeps its short name. The PHP twin keeps upstream's full names.
#[test]
fn a_class_that_shares_its_short_name_only_with_a_built_in_class_keeps_its_short_name() {
    let node = ("src/App/Graph/Node.php", "<?php\n\nnamespace App\\Graph;\n\nclass Node\n{\n}\n");
    let edge = ("src/App/Graph/Edge.php", "<?php\n\nnamespace App\\Graph;\n\nclass Edge\n{\n}\n");
    let sharp = "namespace App.Graph;\n\nclass Walk\n{\n    public Node first(Edge edge) => edge;\n}\n";
    let php = "<?php\n\nnamespace App\\Graph;\n\nclass Walk\n{\n    public function first(Edge $edge): Node\n    {\n        return $edge;\n    }\n}\n";
    let returned = |analyzed| -> Vec<String> {
        worded(analyzed, &[node, edge]).into_iter().filter(|line| line.contains(" invalid-return-statement ")).collect()
    };

    assert_eq!(
        returned(("src/App/Graph/Walk.php", php)),
        [
            "9:16 invalid-return-statement Invalid return type for function `App\\Graph\\Walk::first`: expected `App\\Graph\\Node`, but found `App\\Graph\\Edge`. | This has type `App\\Graph\\Edge` | The type `App\\Graph\\Edge` returned here is not compatible with the declared return type `App\\Graph\\Node`. | Change the return value to match `App\\Graph\\Node`, or update the function's return type declaration."
        ]
    );
    assert_eq!(
        returned(("src/App/Graph/Walk.sharp", sharp)),
        [
            "5:37 invalid-return-statement Invalid return type for method `Walk.first`: expected `Node`, but found `Edge`. | This has type `Edge` | The type `Edge` returned here is not compatible with the declared return type `Node`. | Change the return value to match `Node`, or update the method's return type declaration."
        ]
    );
}

/// A `.sharp` file reaches the standard library's `Sharp.Environment` with no import, so it shares its short name with
/// the project's own `App.Ops.Environment` and a message names it by its full dotted name. The file imports
/// `App.Ops.Environment`, so a message names that class `Environment`, as the import does. The PHP twin keeps
/// upstream's full names.
#[test]
fn a_sharp_prelude_class_that_shares_its_short_name_is_named_by_its_full_name() {
    let environment = ("src/App/Ops/Environment.php", "<?php\n\nnamespace App\\Ops;\n\nclass Environment\n{\n}\n");
    let shell = (
        "src/Lib/Shell.php",
        "<?php\n\nnamespace Lib;\n\nclass Shell\n{\n    public function environment(): \\Sharp\\Environment\n    {\n        return new \\Sharp\\Environment();\n    }\n}\n",
    );
    let sharp = "namespace App.Jobs;\n\nimport App.Ops.Environment;\nimport Lib.Shell;\n\nclass Job\n{\n    public Environment current(Shell shell) => shell.environment();\n}\n";
    let php = "<?php\n\nnamespace App\\Jobs;\n\nuse App\\Ops\\Environment;\nuse Lib\\Shell;\n\nclass Job\n{\n    public function current(Shell $shell): Environment\n    {\n        return $shell->environment();\n    }\n}\n";
    let returned = |analyzed| -> Vec<String> {
        worded(analyzed, &[environment, shell])
            .into_iter()
            .filter(|line| line.contains(" invalid-return-statement "))
            .collect()
    };

    assert_eq!(
        returned(("src/App/Jobs/Job.php", php)),
        [
            "12:16 invalid-return-statement Invalid return type for function `App\\Jobs\\Job::current`: expected `App\\Ops\\Environment`, but found `Sharp\\Environment`. | This has type `Sharp\\Environment` | The type `Sharp\\Environment` returned here is not compatible with the declared return type `App\\Ops\\Environment`. | Change the return value to match `App\\Ops\\Environment`, or update the function's return type declaration."
        ]
    );
    assert_eq!(
        returned(("src/App/Jobs/Job.sharp", sharp)),
        [
            "8:48 invalid-return-statement Invalid return type for method `Job.current`: expected `Environment`, but found `Sharp.Environment`. | This has type `Sharp.Environment` | The type `Sharp.Environment` returned here is not compatible with the declared return type `Environment`. | Change the return value to match `Environment`, or update the method's return type declaration."
        ]
    );
}

/// A message names a static property read as the file writes it: `Order.count` in a `.sharp` file, with the class
/// named by the same short-name rule as every other class in a message. The PHP twin keeps upstream's
/// `App\Shop\Order::$count`.
#[test]
fn a_message_names_a_static_property_as_sharp_writes_it() {
    let sharp = "namespace App.Shop;\n\nclass Order\n{\n    public static int count = 0;\n\n    public bool counted() => is_string(Order.count);\n}\n";
    let php = "<?php\n\nnamespace App\\Shop;\n\nclass Order\n{\n    public static int $count = 0;\n\n    public function counted(): bool\n    {\n        return is_string(Order::$count);\n    }\n}\n";
    let compared = |analyzed| -> Vec<String> {
        worded(analyzed, &[]).into_iter().filter(|line| line.contains("-type-comparison ")).collect()
    };

    assert_eq!(
        compared(("src/App/Shop/Order.php", php)),
        [
            "11:16 impossible-type-comparison Impossible type assertion: `App\\Shop\\Order::$count` of type `int` can never be `string`. | Argument `App\\Shop\\Order::$count` has type `int` | The assertion expects `App\\Shop\\Order::$count` to be `string`, but no value of type `int` can satisfy this. | Check that the correct variable is being passed, or update the assertion type."
        ]
    );
    assert_eq!(
        compared(("src/App/Shop/Order.sharp", sharp)),
        [
            "7:30 impossible-type-comparison Impossible type assertion: `Order.count` of type `int` can never be `string`. | Argument `Order.count` has type `int` | The assertion expects `Order.count` to be `string`, but no value of type `int` can satisfy this. | Check that the correct variable is being passed, or update the assertion type."
        ]
    );
}

/// Code a help tells the developer to write names an enum by the short name its file binds, as PHP# refuses a full
/// name in code (spec section 23). Prose keeps the dotted name that tells two enums of one short name apart. The PHP
/// twin keeps upstream's text.
#[test]
fn an_enum_instantiation_help_writes_the_name_the_file_binds() {
    let app_status =
        ("src/App/Orders/Status.php", "<?php\n\nnamespace App\\Orders;\n\nenum Status\n{\n    case Open;\n}\n");
    let billing_status =
        ("src/Billing/Status.php", "<?php\n\nnamespace Billing;\n\nenum Status\n{\n    case Due;\n}\n");
    let sharp = "namespace App.Orders;\n\nclass Shop\n{\n    public Status open() => new Status();\n}\n";
    let php = "<?php\n\nnamespace App\\Orders;\n\nclass Shop\n{\n    public function open(): Status\n    {\n        return new Status();\n    }\n}\n";
    let refused = |analyzed| -> Vec<String> {
        worded(analyzed, &[app_status, billing_status])
            .into_iter()
            .filter(|line| line.contains(" enum-instantiation "))
            .collect()
    };

    assert_eq!(
        refused(("src/App/Orders/Shop.php", php)),
        [
            "9:20 enum-instantiation Enum `App\\Orders\\Status` cannot be instantiated with `new`. | Attempting to instantiate an enum with `new` | Enum instances are created by accessing their cases directly (e.g., `MyEnum::CaseName`). | Use `App\\Orders\\Status::CASE_NAME` to get an enum case instance, or `App\\Orders\\Status::cases()` to get all cases."
        ]
    );
    assert_eq!(
        refused(("src/App/Orders/Shop.sharp", sharp)),
        [
            "5:33 enum-instantiation Enum `App.Orders.Status` cannot be instantiated with `new`. | Attempting to instantiate an enum with `new` | Enum instances are created by accessing their cases directly (e.g., `MyEnum.CaseName`). | Use `Status.CASE_NAME` to get an enum case instance, or `Status.cases()` to get all cases."
        ]
    );
}

/// A `match` that misses cases of an enum its file doesn't import names the import the missing arms need.
#[test]
fn a_match_that_misses_cases_of_an_unimported_enum_names_the_import() {
    let sharp = "namespace Demo;\n\nimport Lib.Ticket;\n\nclass Report\n{\n    public static string state(Ticket ticket) => match (ticket.status()) {\n    };\n}\n";

    let issues =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[("src/Lib/Status.php", TICKETS)]);

    assert_eq!(
        written_errors(sharp, &issues),
        [
            "match match-not-exhaustive This `match` misses `Status.Open`, `Status.Closed` and `Status.Archived`. Add `import Lib.Status;` to the file.",
            "match (ticket.status()) {\n    } empty-match-expression Match expression cannot be empty.",
        ]
    );
}

/// An import under another name binds its enum, so the missing arms are written with that name and need no import.
#[test]
fn a_match_that_misses_cases_of_an_enum_imported_under_another_name_writes_them_with_that_name() {
    let sharp = "namespace Demo;\n\nimport Lib.Ticket;\nimport Lib.Status as State;\n\nclass Report\n{\n    public static string state(Ticket ticket) => match (ticket.status()) {\n    };\n}\n";

    let issues =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Report.sharp", sharp), &[("src/Lib/Status.php", TICKETS)]);

    assert_eq!(
        written_errors(sharp, &issues),
        [
            "match match-not-exhaustive This `match` misses `State.Open`, `State.Closed` and `State.Archived`.",
            "match (ticket.status()) {\n    } empty-match-expression Match expression cannot be empty.",
        ]
    );
}

/// An import of the enum's short name would clash with the file's import of another enum, so the missing arms are
/// written with the alias the import needs.
#[test]
fn a_match_that_misses_cases_of_an_enum_whose_short_name_the_file_imports_writes_them_with_an_alias() {
    let billing_status =
        ("src/Billing/Status.php", "<?php\n\nnamespace Billing;\n\nenum Status\n{\n    case Due;\n}\n");
    let sharp = "namespace Demo;\n\nimport Lib.Ticket;\nimport Billing.Status;\n\nclass Report\n{\n    public static string state(Ticket ticket) => match (ticket.status()) {\n    };\n}\n";

    let issues = analyze(
        &PLUGIN_REGISTRY,
        settings(),
        ("src/Demo/Report.sharp", sharp),
        &[("src/Lib/Status.php", TICKETS), billing_status],
    );

    assert_eq!(
        written_errors(sharp, &issues),
        [
            "match match-not-exhaustive This `match` misses `LibStatus.Open`, `LibStatus.Closed` and `LibStatus.Archived`. Add `import Lib.Status as LibStatus;` to the file.",
            "match (ticket.status()) {\n    } empty-match-expression Match expression cannot be empty.",
        ]
    );
}

/// Iterating an enum value names the loop over its cases as the file writes it: the enum's bound short name, and the
/// import when the file doesn't bind it. The PHP twin keeps upstream's text.
#[test]
fn an_enum_iteration_help_writes_the_name_the_file_binds_and_its_import() {
    let billing_status =
        ("src/Billing/Status.php", "<?php\n\nnamespace Billing;\n\nenum Status\n{\n    case Due;\n}\n");
    let sharp = "namespace Demo;\n\nimport Lib.Ticket;\n\nclass Report\n{\n    public static void fields(Ticket ticket)\n    {\n        for (const field of ticket.status()) {\n        }\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Ticket;\n\nclass Report\n{\n    public static function fields(Ticket $ticket): void\n    {\n        foreach ($ticket->status() as $field) {\n        }\n    }\n}\n";
    let iterated = |analyzed| -> Vec<String> {
        worded(analyzed, &[("src/Lib/Status.php", TICKETS), billing_status])
            .into_iter()
            .filter(|line| line.contains(" enum-iteration "))
            .collect()
    };

    assert_eq!(
        iterated(("src/Demo/Report.php", php)),
        [
            "11:18 enum-iteration Iterating directly over the enum enum `Lib\\Status`. This will yield its public properties. | This enum instance is being iterated directly | PHP allows iterating an enum case instance like an object, which exposes its public properties: `name` (string). | This is different from iterating through all defined cases of the `Lib\\Status` enum using `Lib\\Status::cases()`, where each item would be an enum case instance itself. | If you only need the properties of this specific instance, consider accessing them directly (e.g., `$instance->name`) for better clarity, unless iterating its few properties is explicitly intended. | If your goal is to loop through all defined cases of the `Lib\\Status` enum, use `Lib\\Status::cases()` instead (e.g., `foreach (Lib\\Status::cases() as $case)`)."
        ]
    );
    assert_eq!(
        iterated(("src/Demo/Report.sharp", sharp)),
        [
            "9:29 enum-iteration Iterating directly over the enum enum `Lib.Status`. This will yield its public properties. | This enum instance is being iterated directly | PHP allows iterating an enum case instance like an object, which exposes its public properties: `name` (string). | This is different from iterating through all defined cases of the `Lib.Status` enum using `Status.cases()`, where each item would be an enum case instance itself. | If you only need the properties of this specific instance, consider accessing them directly (e.g., `instance.name`) for better clarity, unless iterating its few properties is explicitly intended. | If your goal is to loop through all defined cases of the `Lib.Status` enum, use `Status.cases()` instead (e.g., `for (const case of Status.cases())`). Add `import Lib.Status;` to the file."
        ]
    );
}

const MONEY_OPERATORS: (&str, &str) = (
    "src/App/Money.sharp",
    "namespace App;\n\npublic class Money\n{\n    public int cents { get; }\n\n    public Money(int cents)\n    {\n        this.cents = cents;\n    }\n\n    public int hash() => this.cents;\n\n    public static bool operator ==(Money a, Money b) => a.cents == b.cents;\n\n    public static Money operator +(Money a, Money b) => new Money(a.cents + b.cents);\n\n    public static Money operator -(Money a)\n    {\n        return new Money(-a.cents);\n    }\n\n    public static int operator <=>(Money a, Money b) => a.cents <=> b.cents;\n}\n",
);

const ORDER_OF_MONEY: (&str, &str) = ("src/App/Order.sharp", "namespace App;\n\npublic class Order : Money\n{\n}\n");

/// Each operator runs as a public static method named after .NET's operator method, which a subclass inherits.
#[test]
fn an_operator_is_the_static_method_it_runs_as_and_a_subclass_inherits_it() {
    let php = "<?php\n\nnamespace App;\n\nfinal class Ledger\n{\n    public static function add(Order $a, Order $b): Money\n    {\n        return Order::op_Addition($a, $b);\n    }\n\n    public static function negate(Order $a): Money\n    {\n        return Order::op_UnaryNegation($a);\n    }\n\n    public static function same(Order $a, Order $b): bool\n    {\n        return Order::op_Equality($a, $b);\n    }\n\n    public static function less(Order $a, Order $b): Money\n    {\n        return Order::op_Subtraction($a, $b);\n    }\n}\n";

    assert_eq!(
        issues(("src/App/Ledger.php", php), &[MONEY_OPERATORS, ORDER_OF_MONEY]),
        ["24:23 non-existent-method", "24:16 mixed-return-statement"]
    );
}

#[test]
fn an_operator_body_is_checked_as_its_static_method_body() {
    let wrong = "namespace App;\n\npublic class Money\n{\n    public int cents { get; }\n\n    public Money(int cents)\n    {\n        this.cents = cents;\n    }\n\n    public static Money operator +(Money a, Money b) => a.cents + b.cents;\n}\n";

    assert_eq!(issues(MONEY_OPERATORS, &[]), Vec::<String>::new());
    assert_eq!(issues(("src/App/Money.sharp", wrong), &[]), ["12:57 invalid-return-statement"]);
}

/// Equal values hash alike, so a class that declares `operator ==` declares `public int hash()` or inherits it.
#[test]
fn equality_needs_a_public_int_hash_in_the_class_or_a_parent() {
    let missing =
        "namespace App;\n\npublic class Money\n{\n    public static bool operator ==(Money a, Money b) => true;\n}\n";
    let protected = "namespace App;\n\npublic class Money\n{\n    protected int hash() => 1;\n\n    public static bool operator ==(Money a, Money b) => true;\n}\n";
    let string = "namespace App;\n\npublic class Money\n{\n    public string hash() => \"\";\n\n    public static bool operator ==(Money a, Money b) => true;\n}\n";
    let entity = ("src/App/Entity.sharp", "namespace App;\n\npublic class Entity\n{\n    public int hash() => 1;\n}\n");
    let inherited = "namespace App;\n\npublic class Order : Entity\n{\n    public static bool operator ==(Order a, Order b) => true;\n}\n";

    assert_eq!(
        messages(("src/App/Money.sharp", missing), &[]),
        [
            "`Money` declares `operator ==` without `public int hash()`: declare it in `Money` or a parent, so equal values hash alike."
        ]
    );
    assert_eq!(issues(("src/App/Money.sharp", missing), &[]), ["5:24 unimplemented-abstract-method"]);
    assert_eq!(issues(("src/App/Money.sharp", protected), &[]), ["7:24 unimplemented-abstract-method"]);
    assert_eq!(issues(("src/App/Money.sharp", string), &[]), ["7:24 unimplemented-abstract-method"]);
    assert_eq!(issues(("src/App/Order.sharp", inherited), &[entity]), Vec::<String>::new());
}

/// A subclass inherits its parent's operators, which are static, and PHP# has no overloading by parameter types, so
/// it cannot declare them again, as C# cannot override an operator.
#[test]
fn a_subclass_cannot_declare_an_operator_its_parent_declares() {
    let order = "namespace App;\n\npublic class Order : Money\n{\n    public static Order operator +(Order a, Order b) => a;\n\n    public static Order operator -(Order a) => a;\n}\n";

    assert_eq!(
        messages(("src/App/Order.sharp", order), &[MONEY_OPERATORS]),
        [
            "`Order` cannot declare `operator +`: it inherits it from `Money`.",
            "`Order` cannot declare unary `operator -`: it inherits it from `Money`."
        ]
    );
    assert_eq!(
        issues(("src/App/Order.sharp", order), &[MONEY_OPERATORS]),
        ["5:25 override-final-method", "7:25 override-final-method"]
    );
}

/// PHP keeps its own rule: a subclass redeclares a static method its parent declares.
#[test]
fn a_php_subclass_keeps_redeclaring_a_static_method_named_like_an_operator() {
    let money = (
        "src/App/Money.php",
        "<?php\n\nnamespace App;\n\nclass Money\n{\n    public static function op_Addition(Money $a, Money $b): Money\n    {\n        return $a;\n    }\n}\n",
    );
    let order = "<?php\n\nnamespace App;\n\nfinal class Order extends Money\n{\n    public static function op_Addition(Money $a, Money $b): Money\n    {\n        return $b;\n    }\n}\n";

    assert_eq!(issues(("src/App/Order.php", order), &[money]), Vec::<String>::new());
}

#[test]
fn a_php_class_keeps_its_static_op_equality_without_a_hash() {
    let php = "<?php\n\nnamespace App;\n\nfinal class Money\n{\n    public static function op_Equality(?Money $a, ?Money $b): bool\n    {\n        return $a === $b;\n    }\n}\n";

    assert_eq!(issues(("src/App/Money.php", php), &[]), Vec::<String>::new());
}

/// Spec section 19: an operator a class declares or inherits runs where it is used, checked as the static call it runs
/// as. `==` lifts a nullable side, `== null` runs no operator, and a compound assignment assigns the operator's result.
/// PHP keeps its own operators on objects.
#[test]
fn a_declared_operator_runs_where_it_is_used() {
    let sharp = "namespace App;\n\npublic class Ledger\n{\n    public Money total { get; set; }\n\n    public Ledger(Money total)\n    {\n        this.total = total;\n    }\n\n    public bool same(Money a, Money b) => a == b;\n\n    public bool differ(Money a, Money? b) => a != b;\n\n    public bool less(Money a, Money b) => a < b;\n\n    public bool most(Money a, Order b) => a >= b;\n\n    public int compare(Money a, Money b) => a <=> b;\n\n    public Money sum(Money a, Money b) => a + b;\n\n    public Money negated(Order a) => -a;\n\n    public bool missing(Money? a) => a == null;\n\n    public void add(Money price)\n    {\n        this.total += price;\n    }\n}\n";
    let php = "<?php\n\nnamespace App;\n\nfinal class Ledger\n{\n    public function __construct(public Money $total)\n    {\n    }\n\n    public function same(Money $a, Money $b): bool { return $a == $b; }\n\n    public function differ(Money $a, ?Money $b): bool { return $a != $b; }\n\n    public function less(Money $a, Money $b): bool { return $a < $b; }\n\n    public function most(Money $a, Order $b): bool { return $a >= $b; }\n\n    public function compare(Money $a, Money $b): int { return $a <=> $b; }\n\n    public function sum(Money $a, Money $b): Money { return $a + $b; }\n\n    public function negated(Order $a): Money { return -$a; }\n\n    public function missing(?Money $a): bool { return $a == null; }\n\n    public function add(Money $price): void { $this->total += $price; }\n}\n";

    assert_eq!(
        issues(("src/App/Ledger.php", php), &[MONEY_OPERATORS, ORDER_OF_MONEY]),
        [
            "13:70 possibly-null-operand",
            "21:61 invalid-operand",
            "21:66 invalid-operand",
            "21:61 mixed-return-statement",
            "23:56 invalid-operand",
            "23:55 never-return",
            "25:55 possibly-null-operand",
            "25:61 null-operand",
            "27:47 invalid-operand",
            "27:63 invalid-operand",
            "27:63 mixed-property-type-coercion",
        ]
    );
    assert_eq!(issues(("src/App/Ledger.sharp", sharp), &[MONEY_OPERATORS, ORDER_OF_MONEY]), Vec::<String>::new());
}

const DATABASE_ENTITY: (&str, &str) = (
    "src/App/DatabaseEntity.sharp",
    "namespace App;\n\npublic class DatabaseEntity\n{\n    public int id { get; }\n\n    public DatabaseEntity(int id)\n    {\n        this.id = id;\n    }\n\n    public int hash() => this.id;\n\n    public static bool operator ==(DatabaseEntity a, DatabaseEntity b) => a.id == b.id;\n}\n",
);

const ORDER_OF_DATABASE_ENTITY: (&str, &str) =
    ("src/App/Order.sharp", "namespace App;\n\npublic class Order : DatabaseEntity\n{\n}\n");

/// `Order` compares with the `operator ==` it inherits from `DatabaseEntity`, as `Order::op_Equality`.
#[test]
fn equality_of_a_subclass_runs_the_operator_its_parent_declares() {
    let sharp = "namespace App;\n\npublic class Sync\n{\n    public bool unchanged(Order order, Order other) => order == other;\n\n    public bool moved(Order order, Order? other) => order != other;\n}\n";
    let php = "<?php\n\nnamespace App;\n\nfinal class Sync\n{\n    public function unchanged(Order $order, Order $other): bool { return $order == $other; }\n\n    public function moved(Order $order, ?Order $other): bool { return $order != $other; }\n}\n";

    assert_eq!(
        issues(("src/App/Sync.php", php), &[DATABASE_ENTITY, ORDER_OF_DATABASE_ENTITY]),
        ["9:81 possibly-null-operand"]
    );
    assert_eq!(
        issues(("src/App/Sync.sharp", sharp), &[DATABASE_ENTITY, ORDER_OF_DATABASE_ENTITY]),
        Vec::<String>::new()
    );
}

/// An operator a class neither declares nor inherits is refused, naming the class and the operator. PHP orders and
/// adds objects by its own rules.
#[test]
fn an_operator_on_an_instance_whose_class_declares_none_is_refused_by_name() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool a(Cart cart, Cart other) => cart < other;\n\n    public static int b(Cart cart, Cart other) => cart <=> other;\n\n    public static Cart c(Cart cart, Cart other) => cart + other;\n\n    public static Cart d(Cart cart) => -cart;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function a(Cart $cart, Cart $other): bool { return $cart < $other; }\n\n    public static function b(Cart $cart, Cart $other): int { return $cart <=> $other; }\n\n    public static function c(Cart $cart, Cart $other): Cart { return $cart + $other; }\n\n    public static function d(Cart $cart): Cart { return -$cart; }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.php", php), &[("src/Demo/Cart.php", CART)]),
        [
            "11:70 invalid-operand",
            "11:78 invalid-operand",
            "11:70 mixed-return-statement",
            "13:58 invalid-operand",
            "13:57 never-return",
        ]
    );
    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[("src/Demo/Cart.php", CART)]),
        [
            "5:52 invalid-operand `<` cannot compare `Cart` with `Cart`: `Cart` declares no `operator <=>`. | Compare values the instances hold, such as their properties.",
            "7:51 invalid-operand `<=>` cannot compare `Cart` with `Cart`: `Cart` declares no `operator <=>`. | Compare values the instances hold, such as their properties.",
            "9:52 invalid-operand `+` cannot apply to `Cart` and `Cart`: `Cart` declares no `operator +`. | Apply it to values the instances hold, such as their properties.",
            "11:41 invalid-operand Unary `-` cannot apply to `Cart`: `Cart` declares no unary `operator -`. | Apply it to a value the instance holds, such as a property.",
        ]
    );
}

/// A value that may be an instance compares by its class's operator, so a `Cart|int`, whose `Cart` declares none, is
/// refused naming `Cart`. PHP compares it by its own rules.
#[test]
fn comparing_a_value_that_may_be_an_instance_names_its_class() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static bool a(Cart|int extra) => extra >= 1;\n\n    public static bool b(Cart|int extra) => extra == 1;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function a(Cart|int $extra): bool { return $extra >= 1; }\n\n    public static function b(Cart|int $extra): bool { return $extra == 1; }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[("src/Demo/Cart.php", CART)]), Vec::<String>::new());
    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[("src/Demo/Cart.php", CART)]),
        [
            "5:45 invalid-operand `>=` cannot compare `int|Cart` with `int`: `Cart` declares no `operator <=>`. | Compare values the instances hold, such as their properties.",
            "7:45 invalid-operand `==` cannot compare `int|Cart` with `int`: `Cart` declares no `operator ==`. | Use `===` to test whether both sides are the same object.",
        ]
    );
}

/// `money * 2` runs `operator *`, which `Money` does not declare. `money == 5` would run `Money::op_Equality(money, 5)`,
/// which takes no `int`, so it is refused as a comparison of two types that never match.
#[test]
fn an_undeclared_operator_and_a_wrong_operand_are_refused() {
    let sharp = "namespace App;\n\npublic class Pricing\n{\n    public Money doubled(Money money) => money * 2;\n\n    public bool five(Money money) => money == 5;\n}\n";

    assert_eq!(
        refusals(("src/App/Pricing.sharp", sharp), &[MONEY_OPERATORS]),
        [
            "5:42 invalid-operand `*` cannot apply to `Money` and `int`: `Money` declares no `operator *`. | Apply it to values the instances hold, such as their properties.",
            "7:38 invalid-operand `==` cannot compare `Money` with `int`. | Convert one side so both sides have the same type.",
        ]
    );
}

/// An operand the declared operator does not take is refused in PHP# words, before the static call it would run as is
/// checked: `money < 5` would pass an `int` to `Money::op_Comparison`. PHP compares and adds objects by its own rules.
#[test]
fn an_operand_the_declared_operator_does_not_take_is_refused_by_its_php_sharp_types() {
    let sharp = "namespace App;\n\npublic class Pricing\n{\n    public bool a(Money money) => money != \"5\";\n\n    public bool b(Money money) => money < 5;\n\n    public int c(Money money) => 5 <=> money;\n\n    public Money d(Money money) => money + 5;\n\n    public bool e(Money money, Order order) => money < order && money + order == order;\n}\n";
    let php = "<?php\n\nnamespace App;\n\nfinal class Pricing\n{\n    public function a(Money $money): bool { return $money != \"5\"; }\n\n    public function b(Money $money): bool { return $money < 5; }\n\n    public function c(Money $money): int { return 5 <=> $money; }\n\n    public function e(Money $money, Order $order): bool { return $money < $order && $money == $order; }\n}\n";

    assert_eq!(issues(("src/App/Pricing.php", php), &[MONEY_OPERATORS, AUDITED_ORDER]), Vec::<String>::new());
    assert_eq!(
        refusals(("src/App/Pricing.sharp", sharp), &[MONEY_OPERATORS, AUDITED_ORDER]),
        [
            "5:35 invalid-operand `!=` cannot compare `Money` with `string`. | Convert one side so both sides have the same type.",
            "7:35 invalid-operand `<` cannot compare `Money` with `int`. | Convert one side so both sides have the same type.",
            "9:34 invalid-operand `<=>` cannot compare `int` with `Money`. | Convert one side so both sides have the same type.",
            "11:36 invalid-operand `+` cannot apply to `Money` and `int`: `Money` declares `operator +` on `Money` and `Money`. | Apply it to values of the types the operator takes.",
        ]
    );
}

/// Decision 046 lifts only `==` and `!=` over null, so ordering a nullable instance is refused until it is tested. An
/// operand the operator never takes is named first, as testing for `null` would not let it order.
#[test]
fn ordering_a_nullable_instance_is_refused_until_it_is_tested_for_null() {
    let sharp = "namespace App;\n\npublic class Range\n{\n    public bool below(Money? low, Money high) => low < high;\n\n    public int compare(Money low, Money? high) => low <=> high;\n\n    public bool tested(Money? low, Money high) => low != null && low < high;\n\n    public bool five(Money? low) => low < 5;\n}\n";

    assert_eq!(
        refusals(("src/App/Range.sharp", sharp), &[MONEY_OPERATORS]),
        [
            "5:50 invalid-operand `<` cannot compare `Money?` with `Money`: only `==` and `!=` take `null`, so test the value for `null` first. | Test it with `!= null` before the comparison.",
            "7:51 invalid-operand `<=>` cannot compare `Money` with `Money?`: only `==` and `!=` take `null`, so test the value for `null` first. | Test it with `!= null` before the comparison.",
            "11:37 invalid-operand `<` cannot compare `Money?` with `int`. | Convert one side so both sides have the same type.",
        ]
    );
}

const AUDITED_ORDER: (&str, &str) =
    ("src/App/Order.sharp", "namespace App;\n\npublic class Order : Money\n{\n    public int items() => 1;\n}\n");

/// `==` on instances runs the class's `operator ==`, which is no identity test, so it narrows nothing: `money` stays a
/// `Money` where it equals an `Order`.
#[test]
fn equality_of_instances_narrows_nothing() {
    let sharp = "namespace App;\n\npublic class Audit\n{\n    public int items(Money money, Order order)\n    {\n        if (money == order) {\n            return money.items();\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace App;\n\nfinal class Audit\n{\n    public function items(Money $money, Order $order): int\n    {\n        if ($money == $order) {\n            return $money->items();\n        }\n        return 0;\n    }\n}\n";

    assert_eq!(issues(("src/App/Audit.php", php), &[MONEY_OPERATORS, AUDITED_ORDER]), Vec::<String>::new());
    assert_eq!(
        issues(("src/App/Audit.sharp", sharp), &[MONEY_OPERATORS, AUDITED_ORDER]),
        ["8:26 non-existent-method", "8:20 mixed-return-statement"]
    );
}

/// The lowered `op_Equality` takes nullable parameters, so a plain PHP caller passes `null` and gets the lifted answer.
/// A PHP# caller keeps the parameter types `operator ==` declares.
#[test]
fn a_php_caller_passes_null_to_op_equality_and_a_sharp_caller_does_not() {
    let php = "<?php\n\nnamespace App;\n\nfinal class Check\n{\n    public static function same(Money $money, ?Money $other): bool\n    {\n        return Money::op_Equality($money, null) || Money::op_Equality($money, $other);\n    }\n}\n";
    let sharp = "namespace App;\n\npublic class Check\n{\n    public static bool same(Money money, Money? other) => Money.op_Equality(money, null) || Money.op_Equality(money, other);\n}\n";

    assert_eq!(issues(("src/App/Check.php", php), &[MONEY_OPERATORS]), Vec::<String>::new());
    assert_eq!(
        issues(("src/App/Check.sharp", sharp), &[MONEY_OPERATORS]),
        ["5:84 null-argument", "5:118 possibly-null-argument"]
    );
}

/// Both parameters of the lowered `op_Equality` take `null`, so a plain PHP caller may pass it on the left too.
#[test]
fn a_php_caller_passes_null_as_the_left_operand_of_op_equality() {
    let php = "<?php\n\nnamespace App;\n\nfinal class Check\n{\n    public static function missing(Money $money): bool\n    {\n        return Money::op_Equality(null, $money);\n    }\n}\n";

    assert_eq!(issues(("src/App/Check.php", php), &[MONEY_OPERATORS]), Vec::<String>::new());
}

/// A PHP caller checks its arguments against the PHP method a PHP# method runs as, whose `List<int>` and
/// `Map<string, int>` parameters keep their element types, so a wrongly typed list or map is still reported.
#[test]
fn a_php_caller_keeps_the_element_types_of_a_sharp_methods_collection_parameters() {
    let tally = (
        "src/Demo/Tally.sharp",
        "namespace Demo;\n\npublic class Tally\n{\n    public static int total(List<int> items, Map<string, int> counts) => 0;\n}\n",
    );
    let php = "<?php\n\nnamespace Demo;\n\nfinal class Report\n{\n    public static function run(): int\n    {\n        return Tally::total(['a'], [1 => 'x']);\n    }\n}\n";

    assert_eq!(
        messages(("src/Demo/Report.php", php), &[tally]),
        [
            "Possible argument type mismatch for argument #1 of `Demo\\Tally::total`: expected `list<int>`, but possibly received `list{string('a')}`.",
            "Possible argument type mismatch for argument #2 of `Demo\\Tally::total`: expected `array<string, int>`, but possibly received `array{1: string('x')}`.",
        ]
    );
}

/// `<=>` orders two numbers as PHP does and two strings by their bytes. A string against a number is refused as `<`
/// refuses it. PHP keeps its loose `<=>`.
#[test]
fn spaceship_orders_numbers_and_strings_and_refuses_a_string_against_a_number() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int a(int x, int y) => x <=> y;\n\n    public static int b(float x, int y) => x <=> y;\n\n    public static int c(string x, string y) => x <=> y;\n\n    public static int d(string x, int y) => x <=> y;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function a(int $x, int $y): int { return $x <=> $y; }\n\n    public static function b(float $x, int $y): int { return $x <=> $y; }\n\n    public static function c(string $x, string $y): int { return $x <=> $y; }\n\n    public static function d(string $x, int $y): int { return $x <=> $y; }\n}\n";

    assert_eq!(issues(("src/Demo/Report.php", php), &[]), Vec::<String>::new());
    assert_eq!(
        refusals(("src/Demo/Report.sharp", sharp), &[]),
        [
            "11:45 invalid-operand `<=>` cannot compare `string` with `int`. | Convert one side so both sides have the same type."
        ]
    );
}

const STRIPE: (&str, &str) = (
    "vendor/stripe/StripeClient.php",
    "<?php\n\nnamespace Stripe;\n\nclass StripeClient\n{\n    public function charges(): object\n    {\n        return $this;\n    }\n}\n",
);

const NOW: (&str, &str) = ("src/helpers.php", "<?php\n\nfunction now(): int\n{\n    return time();\n}\n");

const CLOCK_STUB: (&str, &str) = ("app/Stubs/Clock.sharp", "namespace App.Stubs;\n\nextern now uses Clock;\n");

/// Analyzes every file of `files` against the codebase they declare together, and returns the codebase and the effect
/// summaries of every body they declare.
fn summarized(files: &[(&'static str, &'static str)]) -> (CodebaseMetadata, Vec<EffectSummary>) {
    let Prelude { mut database, mut metadata, mut symbol_references } = PRELUDE.clone();
    let file_ids: Vec<_> = files
        .iter()
        .map(|(name, code)| {
            database.add(File::ephemeral(Cow::Borrowed(name.as_bytes()), Cow::Borrowed(code.as_bytes())))
        })
        .collect();

    let arena = LocalArena::new();
    let mut programs = Vec::new();
    for file_id in &file_ids {
        let file = database.get_ref(file_id).expect("file was just added");
        let program = parse_file(&arena, file);
        assert!(!program.has_errors(), "{} did not parse: {:?}", String::from_utf8_lossy(&file.name), program.errors);

        let names = NameResolver::new(&arena).resolve(program);
        metadata.extend(scan_program(&arena, file, program, &names, settings().version));
        programs.push((file, program, names));
    }

    populate_codebase(&mut metadata, &mut symbol_references, WordSet::default(), HashSet::default());

    let mut summaries = Vec::new();
    for (file, program, names) in &programs {
        let mut result = AnalysisResult::new(symbol_references.clone());
        let mut artifacts = Analyzer::new(&arena, file, names, &metadata, &PLUGIN_REGISTRY, settings())
            .analyze_with_artifacts(program, &mut result)
            .expect("analysis succeeds");
        summaries.append(&mut artifacts.effect_summaries);
    }

    (metadata, summaries)
}

/// Every issue of the effect rules over `files`, as `file:line:column code: message`, then ` Help: ` and its help when
/// it has one.
fn effect_issues(files: &[(&'static str, &'static str)]) -> Vec<String> {
    let (codebase, summaries) = summarized(files);

    Effects::solve(&codebase, &summaries)
        .issues(&codebase)
        .into_iter()
        .map(|issue| {
            let file_id = issue.primary_span().expect("a primary span").file_id;
            let (name, code) = files
                .iter()
                .find(|(name, _)| FileId::new(name.as_bytes()) == file_id)
                .expect("the issue is in one of the files");
            let help = issue.help.as_deref().map(|help| format!(" Help: {help}")).unwrap_or_default();

            format!("{name}:{}: {}{help}", located(code, &issue), issue.message)
        })
        .collect()
}

/// Each class, method or function has at most one `extern` declaration in the whole project, spec section 29. The
/// second one, in file order, is the error, and it points at the first.
#[test]
fn a_second_extern_of_a_target_in_another_file_is_refused_on_the_second() {
    let first = (
        "app/Stubs/Payments.sharp",
        "namespace App.Stubs;\n\nimport Stripe.StripeClient;\n\nextern StripeClient uses Http;\n",
    );
    let second =
        ("app/Stubs/Stripe.sharp", "namespace App.Stubs;\n\nimport Stripe.StripeClient;\n\nextern StripeClient;\n");

    assert_eq!(issues(first, &[second, STRIPE]), Vec::<String>::new());
    let refused = analyze(&PLUGIN_REGISTRY, settings(), second, &[first, STRIPE]);
    let reported: Vec<String> = refused
        .iter()
        .map(|issue| {
            let secondary: Vec<String> = issue
                .annotations
                .iter()
                .filter(|annotation| !annotation.kind.is_primary())
                .map(|annotation| {
                    format!("{} {}", annotation.span.start.offset, annotation.message.as_deref().unwrap_or(""))
                })
                .collect();

            format!("{} {} {secondary:?}", located(second.1, issue), issue.message)
        })
        .collect();
    let first_offset = first.1.find("extern").unwrap();
    assert_eq!(
        reported,
        [format!(
            "5:1 duplicate-extern `StripeClient` already has an `extern` declaration. [\"{first_offset} First declared here.\"]"
        )]
    );
}

/// Spec section 23: a rename changes the name, not what is imported, so an `extern` written with the new name declares
/// the same class and section 29's one-declaration rule counts it once.
#[test]
fn an_extern_written_with_a_renamed_import_declares_the_same_target() {
    let first = (
        "app/Stubs/Payments.sharp",
        "namespace App.Stubs;\n\nimport Stripe.StripeClient;\n\nextern StripeClient uses Http;\n",
    );
    let second =
        ("app/Stubs/Stripe.sharp", "namespace App.Stubs;\n\nimport Stripe.StripeClient as Client;\n\nextern Client;\n");

    let refused: Vec<String> = analyze(&PLUGIN_REGISTRY, settings(), second, &[first, STRIPE])
        .iter()
        .map(|issue| format!("{} {}", located(second.1, issue), issue.message))
        .collect();

    assert_eq!(refused, ["5:1 duplicate-extern `Client` already has an `extern` declaration."]);
}

/// An `extern` target that names no class, method or function is reported with the code a call of it would get.
#[test]
fn an_extern_target_that_names_nothing_is_a_non_existent_class_method_or_function() {
    let stub = (
        "app/Stubs/Stripe.sharp",
        "namespace App.Stubs;\n\nimport Stripe.StripeClient;\nimport Stripe.Missing;\n\nextern Missing;\nextern StripeClient.refund;\nextern nowhere;\n",
    );

    assert_eq!(
        issues(stub, &[STRIPE]),
        ["6:8 non-existent-class-like", "7:8 non-existent-method", "8:8 non-existent-function"]
    );
}

/// PHP# infers the effects of PHP# code, so an `extern` names only plain PHP.
#[test]
fn an_extern_of_a_php_sharp_class_is_refused() {
    let order = ("app/Shop/Order.sharp", "namespace App.Shop;\n\npublic class Order\n{\n}\n");
    let stub = ("app/Stubs/Shop.sharp", "namespace App.Stubs;\n\nimport App.Shop.Order;\n\nextern Order;\n");

    assert_eq!(messages(stub, &[order]), ["`Order` is PHP# code, so it needs no `extern`: PHP# infers its effects."]);
    assert_eq!(issues(stub, &[order]), ["5:8 extern-on-sharp"]);
}

#[test]
fn an_extern_of_a_plain_php_class_is_accepted() {
    let stub =
        ("app/Stubs/Stripe.sharp", "namespace App.Stubs;\n\nimport Stripe.StripeClient;\n\nextern StripeClient;\n");

    assert_eq!(issues(stub, &[STRIPE]), Vec::<String>::new());
}

const LABEL: (&str, &str) = (
    "app/Shop/Label.sharp",
    "namespace App.Shop;\n\npublic class Label\n{\n    public Label(private string name) { }\n\n    public string text => trim(this.name);\n}\n",
);

#[test]
fn a_getter_calling_a_function_an_extern_declares_pure_passes() {
    let stub = ("app/Stubs/Text.sharp", "namespace App.Stubs;\n\nextern trim;\n");

    assert_eq!(effect_issues(&[LABEL, stub]), Vec::<String>::new());
}

#[test]
fn a_getter_calling_a_function_with_no_extern_is_refused_and_names_the_missing_declaration() {
    assert_eq!(
        effect_issues(&[LABEL]),
        [
            "app/Shop/Label.sharp:7:27 impure-getter: Getter `text` calls `trim`, which has no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern trim;` when it has no effect, or name its effects after `uses`."
        ]
    );
}

const MAGIC: (&str, &str) = (
    "src/Lib/Magic.php",
    "<?php\n\nnamespace Lib;\n\nfinal class Bag\n{\n    /** @param list<mixed> $arguments */\n    public function __call(string $name, array $arguments): int\n    {\n        return 0;\n    }\n}\n\nfinal class Codes\n{\n    /** @param list<mixed> $arguments */\n    public static function __callStatic(string $name, array $arguments): int\n    {\n        return 0;\n    }\n}\n",
);

const SHELF: (&str, &str) = (
    "app/Shop/Shelf.sharp",
    "namespace App.Shop;\n\nimport Lib.Bag;\nimport Lib.Codes;\n\npublic class Shelf\n{\n    public Shelf(private Bag bag) { }\n\n    public int count => this.bag.size();\n\n    public int code => Codes.next();\n}\n",
);

/// A method no class declares runs the class's `__call` or `__callStatic`, so its call has the effects the `extern` on
/// that magic method declares, and an unknown effect without one, as any call of a plain PHP method does.
#[test]
fn a_getter_calling_a_method_a_magic_method_serves_has_the_effects_of_the_magic_method() {
    let pure = (
        "app/Stubs/Magic.sharp",
        "namespace App.Stubs;\n\nimport Lib.Bag;\nimport Lib.Codes;\n\nextern Bag.__call;\nextern Codes.__callStatic;\n",
    );
    let timed = (
        "app/Stubs/Magic.sharp",
        "namespace App.Stubs;\n\nimport Lib.Bag;\nimport Lib.Codes;\n\nextern Bag.__call uses Clock;\nextern Codes.__callStatic;\n",
    );

    assert_eq!(
        effect_issues(&[SHELF, MAGIC]),
        [
            "app/Shop/Shelf.sharp:10:25 impure-getter: Getter `count` calls `Bag.__call`, which has no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern Bag.__call;` when it has no effect, or name its effects after `uses`.",
            "app/Shop/Shelf.sharp:12:24 impure-getter: Getter `code` calls `Codes.__callStatic`, which has no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern Codes.__callStatic;` when it has no effect, or name its effects after `uses`.",
        ]
    );
    assert_eq!(issues(pure, &[MAGIC]), Vec::<String>::new());
    assert_eq!(effect_issues(&[SHELF, MAGIC, pure]), Vec::<String>::new());
    assert_eq!(
        effect_issues(&[SHELF, MAGIC, timed]),
        [
            "app/Shop/Shelf.sharp:10:25 impure-getter: Getter `count` calls `Bag.__call`, which has the effect `Clock`. Getters must be pure."
        ]
    );
}

/// Spec section 29: code without a body is pure unless it says `uses`, function types included, so calling a property
/// a PHP# class declares with a `Function` type has no effect.
#[test]
fn a_getter_calling_a_property_holding_a_function_type_passes() {
    let pricing = (
        "app/Shop/Pricing.sharp",
        "namespace App.Shop;\n\npublic class Pricing\n{\n    public Pricing(private Function<int(int)> rate) { }\n\n    public int price => this.rate(2);\n}\n",
    );

    assert_eq!(effect_issues(&[pricing]), Vec::<String>::new());
}

const HOLDER: (&str, &str) = (
    "src/Lib/Holder.php",
    "<?php\n\nnamespace Lib;\n\nfinal class Formatter\n{\n    public function __invoke(int $amount): int\n    {\n        return $amount;\n    }\n}\n\nfinal class Holder\n{\n    /** @var \\Closure(int): int */\n    public \\Closure $closure;\n\n    /** @var callable(int): int */\n    public $callback;\n\n    public Formatter $format;\n}\n",
);

/// A plain PHP property holding a closure or a callable declares no effect, and an `extern` on its class declares the
/// class's own members, not the code the property holds, so calling it always has an unknown effect. An object with
/// `__invoke` has the effects of its `__invoke`.
#[test]
fn a_getter_calling_a_plain_php_property_holding_a_function_has_an_unknown_effect() {
    let holder = HOLDER;
    let till = (
        "app/Shop/Till.sharp",
        "namespace App.Shop;\n\nimport Lib.Holder;\n\npublic class Till\n{\n    public Till(private Holder holder) { }\n\n    public int closed => this.holder.closure(2);\n\n    public int called => this.holder.callback(2);\n\n    public int formatted => this.holder.format(2);\n}\n",
    );
    let class_extern = ("app/Stubs/Holder.sharp", "namespace App.Stubs;\n\nimport Lib.Holder;\n\nextern Holder;\n");
    let refused = [
        "app/Shop/Till.sharp:9:26 impure-getter: Getter `closed` calls `Holder.closure`, which has no `extern` declaration. Getters must be pure. Help: Property `Holder.closure` holds plain PHP code, which no `extern` can declare. Call it outside the getter, or through a method of `Holder` that an `extern` declares.",
        "app/Shop/Till.sharp:11:26 impure-getter: Getter `called` calls `Holder.callback`, which has no `extern` declaration. Getters must be pure. Help: Property `Holder.callback` holds plain PHP code, which no `extern` can declare. Call it outside the getter, or through a method of `Holder` that an `extern` declares.",
        "app/Shop/Till.sharp:13:29 impure-getter: Getter `formatted` calls `Formatter.__invoke`, which has no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern Formatter.__invoke;` when it has no effect, or name its effects after `uses`. Add `import Lib.Formatter;` to the file.",
    ];

    assert_eq!(effect_issues(&[till, holder]), refused);
    assert_eq!(effect_issues(&[till, holder, class_extern]), refused);
}

/// A law calling a plain PHP property that holds a closure is refused as a getter is, and its help names the law.
#[test]
fn a_law_calling_a_plain_php_property_holding_a_closure_has_an_unknown_effect() {
    let rule = (
        "app/Shop/Rule.sharp",
        "namespace App.Shop;\n\nimport Lib.Holder;\n\npublic class Rule\n{\n    law positive(Holder holder) => holder.closure(2) > 0;\n}\n",
    );

    assert_eq!(
        effect_issues(&[rule, HOLDER]),
        [
            "app/Shop/Rule.sharp:7:36 impure-law: Law `positive` calls `Holder.closure`, which has no `extern` declaration. Laws hold only over pure code. Help: Property `Holder.closure` holds plain PHP code, which no `extern` can declare. Call it outside the law, or through a method of `Holder` that an `extern` declares."
        ]
    );
}

#[test]
fn a_getter_reaching_a_method_that_calls_an_extern_with_an_effect_in_another_file_is_refused() {
    let order =
        ("app/Shop/Order.sharp", "namespace App.Shop;\n\npublic class Order\n{\n    public int price() => now();\n}\n");
    let cart = (
        "app/Shop/Cart.sharp",
        "namespace App.Shop;\n\npublic class Cart\n{\n    public Cart(private Order order) { }\n\n    public int total => this.order.price();\n}\n",
    );

    assert_eq!(
        effect_issues(&[cart, order, CLOCK_STUB, NOW]),
        [
            "app/Shop/Cart.sharp:7:25 impure-getter: Getter `total` reaches `Order.price`, which calls `now` with the effect `Clock`. Getters must be pure."
        ]
    );
}

#[test]
fn a_getter_writing_a_field_of_this_is_refused() {
    let counter = (
        "app/Shop/Counter.sharp",
        "namespace App.Shop;\n\npublic class Counter\n{\n    private int count = 0;\n\n    public int next { get { this.count = this.count + 1; return this.count; } }\n}\n",
    );

    assert_eq!(
        effect_issues(&[counter]),
        ["app/Shop/Counter.sharp:7:29 impure-getter: Getter `next` changes `this.count`. Getters must be pure."]
    );
}

#[test]
fn a_getter_building_and_changing_a_new_object_passes() {
    let builder = (
        "app/Shop/Builder.sharp",
        "namespace App.Shop;\n\npublic class Builder\n{\n    private string text = \"\";\n\n    public void add(string part)\n    {\n        this.text = this.text + part;\n    }\n\n    public string build() => this.text;\n}\n",
    );
    let page = (
        "app/Shop/Page.sharp",
        "namespace App.Shop;\n\npublic class Page\n{\n    public string title\n    {\n        get\n        {\n            let builder = new Builder();\n            builder.add(\"Shop\");\n            return builder.build();\n        }\n    }\n}\n",
    );

    assert_eq!(effect_issues(&[page, builder]), Vec::<String>::new());
    let (codebase, summaries) = summarized(&[page, builder]);
    let add = FunctionLikeIdentifier::Method(ascii_lowercase_word(b"App\\Shop\\Builder"), ascii_lowercase_word(b"add"));
    assert_eq!(
        Effects::solve(&codebase, &summaries).impurity(&codebase, &add).as_deref(),
        Some("changes `this.text`"),
        "the getter calls a method that changes its object, and the object is the getter's own"
    );
}

#[test]
fn a_getter_calling_a_virtual_method_whose_override_has_an_effect_is_refused() {
    let rates = (
        "app/Shop/Rate.sharp",
        "namespace App.Shop;\n\npublic class Rate\n{\n    public virtual int value() => 1;\n}\n\npublic class LiveRate : Rate\n{\n    public override int value() => now();\n}\n",
    );
    let quote = (
        "app/Shop/Quote.sharp",
        "namespace App.Shop;\n\npublic class Quote\n{\n    public Quote(private Rate rate) { }\n\n    public int amount => this.rate.value();\n}\n",
    );

    assert_eq!(
        effect_issues(&[quote, rates, CLOCK_STUB, NOW]),
        [
            "app/Shop/Quote.sharp:7:26 impure-getter: Getter `amount` reaches `LiveRate.value`, which calls `now` with the effect `Clock`. Getters must be pure."
        ]
    );
}

/// A collection method has the effects of the lambda passed to it, as `uses f` will declare, and the lambda is part of
/// the getter, so the error points into it.
#[test]
fn a_getter_whose_list_map_lambda_calls_an_undeclared_function_is_refused() {
    let names = (
        "app/Shop/Names.sharp",
        "namespace App.Shop;\n\npublic class Names\n{\n    public Names(private List<string> all) { }\n\n    public List<string> loud => this.all.map(name => strtoupper(name));\n}\n",
    );

    assert_eq!(
        effect_issues(&[names]),
        [
            "app/Shop/Names.sharp:7:54 impure-getter: Getter `loud` calls `strtoupper`, which has no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern strtoupper;` when it has no effect, or name its effects after `uses`."
        ]
    );
}

#[test]
fn a_getter_reading_the_environment_is_refused_with_the_effect_environment() {
    let deploy = (
        "app/Ops/Deploy.sharp",
        "namespace App.Ops;\n\npublic class Deploy\n{\n    public Deploy(private Environment environment) { }\n\n    public string region => this.environment.variable(\"AWS_REGION\") ?? \"us-east-1\";\n}\n",
    );

    assert_eq!(
        effect_issues(&[deploy]),
        [
            "app/Ops/Deploy.sharp:7:29 impure-getter: Getter `region` calls `Environment.variable`, which has the effect `Environment`. Getters must be pure."
        ]
    );
}

/// Two methods that call each other form one strongly connected part, which the solve iterates to a fixpoint instead
/// of following forever. `impurity` answers for a method as it does for a getter.
#[test]
fn a_recursive_pair_of_methods_solves_without_looping() {
    let tree = (
        "app/Shop/Tree.sharp",
        "namespace App.Shop;\n\npublic class Tree\n{\n    public int even(int n) => n == 0 ? 0 : this.odd(n - 1);\n\n    public int odd(int n) => n == 0 ? now() : this.even(n - 1);\n\n    public int depth => this.even(4);\n}\n",
    );
    let files = [tree, CLOCK_STUB, NOW];

    assert_eq!(
        effect_issues(&files),
        [
            "app/Shop/Tree.sharp:9:25 impure-getter: Getter `depth` reaches `Tree.odd`, which calls `now` with the effect `Clock`. Getters must be pure."
        ]
    );
    let (codebase, summaries) = summarized(&files);
    let effects = Effects::solve(&codebase, &summaries);
    let even = FunctionLikeIdentifier::Method(ascii_lowercase_word(b"App\\Shop\\Tree"), ascii_lowercase_word(b"even"));
    assert_eq!(
        effects.impurity(&codebase, &even).as_deref(),
        Some("reaches `Tree.odd`, which calls `now` with the effect `Clock`")
    );
}

/// An operator on an instance runs the operator its class declares (spec section 19), so a getter that applies one
/// reaches that operator's body, as a getter that calls a method reaches the method's.
#[test]
fn a_getter_applying_an_operator_whose_body_has_an_effect_is_refused() {
    let money = (
        "app/Shop/Money.sharp",
        "namespace App.Shop;\n\npublic class Money\n{\n    public Money(public int cents) { }\n\n    public int hash() => this.cents;\n\n    public static Money operator +(Money a, Money b) => new Money(a.cents + b.cents + now());\n\n    public static Money operator -(Money a) => new Money(now() - a.cents);\n\n    public static bool operator ==(Money a, Money b) => a.cents == b.cents + now();\n\n    public static int operator <=>(Money a, Money b) => a.cents - b.cents + now();\n}\n",
    );
    let cart = (
        "app/Shop/Cart.sharp",
        "namespace App.Shop;\n\npublic class Cart\n{\n    public Cart(private Money price, private Money tax) { }\n\n    public Money total => this.price + this.tax;\n\n    public Money refund => -this.price;\n\n    public bool even => this.price == this.tax;\n\n    public bool cheaper => this.price < this.tax;\n\n    public Money doubled\n    {\n        get\n        {\n            let sum = this.price;\n            sum += this.price;\n            return sum;\n        }\n    }\n}\n",
    );

    assert_eq!(
        effect_issues(&[cart, money, CLOCK_STUB, NOW]),
        [
            "app/Shop/Cart.sharp:7:27 impure-getter: Getter `total` reaches `Money.operator +`, which calls `now` with the effect `Clock`. Getters must be pure.",
            "app/Shop/Cart.sharp:9:28 impure-getter: Getter `refund` reaches `Money.operator -`, which calls `now` with the effect `Clock`. Getters must be pure.",
            "app/Shop/Cart.sharp:11:25 impure-getter: Getter `even` reaches `Money.operator ==`, which calls `now` with the effect `Clock`. Getters must be pure.",
            "app/Shop/Cart.sharp:13:28 impure-getter: Getter `cheaper` reaches `Money.operator <=>`, which calls `now` with the effect `Clock`. Getters must be pure.",
            "app/Shop/Cart.sharp:20:13 impure-getter: Getter `doubled` reaches `Money.operator +`, which calls `now` with the effect `Clock`. Getters must be pure.",
        ]
    );
}

/// Spec section 28's `Money`, whose law holds over its pure `add`.
const LAWFUL_MONEY: (&str, &str) = (
    "app/Shared/Money.sharp",
    "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n    public Money add(Money other) => new Money(this.amount + other.amount, this.currency);\n\n    law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n}\n",
);

/// Spec section 28: a law is a `bool` expression over typed parameters, analyzed as a static method's body.
#[test]
fn a_law_over_pure_code_reports_no_issue() {
    assert_eq!(issues(LAWFUL_MONEY, &[]), Vec::<String>::new());
    assert_eq!(effect_issues(&[LAWFUL_MONEY]), Vec::<String>::new());
}

/// Spec section 28's state machine: a law covers every state and event through the enum's pure transition method.
#[test]
fn a_law_over_an_enum_transition_reports_no_issue() {
    let payment = (
        "app/Shop/Payment.sharp",
        "namespace App.Shop;\n\npublic enum Payment\n{\n    case Captured;\n    case Returned;\n}\n",
    );
    let status = (
        "app/Shop/Status.sharp",
        "namespace App.Shop;\n\npublic enum Status : string\n{\n    case Open = \"open\";\n    case Paid = \"paid\";\n    case Refunded = \"refunded\";\n\n    public Status after(Payment e) => match (this) {\n        Status.Open => e == Payment.Captured ? Status.Paid : Status.Open,\n        Status.Paid => e == Payment.Returned ? Status.Refunded : Status.Paid,\n        Status.Refunded => Status.Refunded,\n    };\n\n    law refundedIsFinal(Payment e) => Status.Refunded.after(e) == Status.Refunded;\n}\n",
    );

    assert_eq!(issues(status, &[payment]), Vec::<String>::new());
    assert_eq!(effect_issues(&[status, payment]), Vec::<String>::new());
}

const GATEWAY: (&str, &str) = (
    "src/Billing/Gateway.php",
    "<?php\n\nnamespace Billing;\n\nfinal class Gateway\n{\n    public static function charge(int $amount): bool\n    {\n        return $amount > 0;\n    }\n}\n",
);

/// Spec section 28: laws hold only over pure code, so a law that calls code with an effect is refused on the call.
#[test]
fn a_law_calling_plain_php_with_an_effect_is_refused_on_the_call() {
    let stub = (
        "app/Stubs/Billing.sharp",
        "namespace App.Stubs;\n\nimport Billing.Gateway;\n\nextern Gateway.charge uses Http;\n",
    );
    let refund = (
        "app/Shop/Refund.sharp",
        "namespace App.Shop;\n\nimport Billing.Gateway;\n\npublic class Refund\n{\n    law refundAllowed(int amount) => Gateway.charge(amount);\n}\n",
    );

    assert_eq!(
        effect_issues(&[refund, stub, GATEWAY]),
        [
            "app/Shop/Refund.sharp:7:38 impure-law: Law `refundAllowed` calls `Gateway.charge`, which has the effect `Http`. Laws hold only over pure code."
        ]
    );
}

/// A law that reaches an effect through PHP# code in another file names the method it reaches.
#[test]
fn a_law_reaching_a_method_with_an_effect_in_another_file_is_refused() {
    let timer = (
        "src/Lib/Timer.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Timer\n{\n    public static function now(): int\n    {\n        return time();\n    }\n}\n",
    );
    let stub = ("app/Stubs/Timer.sharp", "namespace App.Stubs;\n\nimport Lib.Timer;\n\nextern Timer.now uses Clock;\n");
    let order = (
        "app/Shop/Order.sharp",
        "namespace App.Shop;\n\nimport Lib.Timer;\n\npublic class Order\n{\n    public int total() => Timer.now();\n}\n",
    );
    let checkout = (
        "app/Shop/Checkout.sharp",
        "namespace App.Shop;\n\npublic class Checkout\n{\n    law totalMatches(Order order) => order.total() == order.total();\n}\n",
    );

    assert_eq!(
        effect_issues(&[checkout, order, stub, timer]),
        [
            "app/Shop/Checkout.sharp:5:38 impure-law: Law `totalMatches` reaches `Order.total`, which calls `Timer.now` with the effect `Clock`. Laws hold only over pure code."
        ]
    );
}

/// A law that calls plain PHP with no `extern` names the declaration it lacks, as the getter rule does.
#[test]
fn a_law_calling_plain_php_with_no_extern_names_the_missing_declaration() {
    let refund = (
        "app/Shop/Refund.sharp",
        "namespace App.Shop;\n\nimport Billing.Gateway;\n\npublic class Refund\n{\n    law refundAllowed(int amount) => Gateway.charge(amount);\n}\n",
    );

    assert_eq!(
        effect_issues(&[refund, GATEWAY]),
        [
            "app/Shop/Refund.sharp:7:38 impure-law: Law `refundAllowed` calls `Gateway.charge`, which has no `extern` declaration. Laws hold only over pure code. Help: Declare it in a .sharp file: `extern Gateway.charge;` when it has no effect, or name its effects after `uses`."
        ]
    );
}

const APP_CLOCK: (&str, &str) = (
    "src/App/Clock.php",
    "<?php\n\nnamespace App;\n\nfinal class Clock\n{\n    public function now(): int\n    {\n        return 0;\n    }\n}\n",
);

const VENDOR_CLOCK: (&str, &str) = (
    "src/Vendor/Clock.php",
    "<?php\n\nnamespace Vendor;\n\nfinal class Clock\n{\n    /** @var \\Closure(int): int */\n    public \\Closure $tick;\n\n    public function now(): int\n    {\n        return time();\n    }\n}\n",
);

/// A PHP# class that hands out a `Vendor\Clock`, so a getter in its namespace reaches the clock with no import.
const CLOCK_SOURCE: (&str, &str) = (
    "app/Shop/Source.sharp",
    "namespace App.Shop;\n\nimport Vendor.Clock;\n\npublic class Source\n{\n    public Source(private Clock time) { }\n\n    public Clock clock() => this.time;\n}\n",
);

/// An effects message names a class as every other message does: by its full name when another class shares its short
/// name.
#[test]
fn a_getter_calling_a_method_of_a_class_that_shares_its_short_name_names_the_class_in_full() {
    let timer = (
        "app/Shop/Timer.sharp",
        "namespace App.Shop;\n\npublic class Timer\n{\n    public Timer(private Source source) { }\n\n    public int started => this.source.clock().now();\n}\n",
    );

    assert_eq!(
        effect_issues(&[timer, CLOCK_SOURCE, VENDOR_CLOCK, APP_CLOCK]),
        [
            "app/Shop/Timer.sharp:7:27 impure-getter: Getter `started` calls `Vendor.Clock.now`, which has no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern Clock.now;` when it has no effect, or name its effects after `uses`. Add `import Vendor.Clock;` to the file."
        ]
    );
}

/// The help for a plain PHP property names its class by the same rule as the message.
#[test]
fn a_getter_calling_a_plain_php_property_of_a_class_that_shares_its_short_name_names_the_class_in_full() {
    let timer = (
        "app/Shop/Timer.sharp",
        "namespace App.Shop;\n\npublic class Timer\n{\n    public Timer(private Source source) { }\n\n    public int ticked => this.source.clock().tick(2);\n}\n",
    );

    assert_eq!(
        effect_issues(&[timer, CLOCK_SOURCE, VENDOR_CLOCK, APP_CLOCK]),
        [
            "app/Shop/Timer.sharp:7:26 impure-getter: Getter `ticked` calls `Vendor.Clock.tick`, which has no `extern` declaration. Getters must be pure. Help: Property `Vendor.Clock.tick` holds plain PHP code, which no `extern` can declare. Call it outside the getter, or through a method of `Vendor.Clock` that an `extern` declares."
        ]
    );
}

/// An effects message names a class the file imports by the name its import gives it.
#[test]
fn a_getter_calling_a_method_of_a_class_imported_under_another_name_names_the_class_by_that_name() {
    let timer = (
        "app/Shop/Timer.sharp",
        "namespace App.Shop;\n\nimport Vendor.Clock as VendorClock;\n\npublic class Timer\n{\n    public Timer(private VendorClock clock) { }\n\n    public int started => this.clock.now();\n}\n",
    );

    assert_eq!(
        effect_issues(&[timer, VENDOR_CLOCK, APP_CLOCK]),
        [
            "app/Shop/Timer.sharp:9:27 impure-getter: Getter `started` calls `VendorClock.now`, which has no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern VendorClock.now;` when it has no effect, or name its effects after `uses`."
        ]
    );
}

/// The PHP# method a getter reaches is named by the same rule as a plain PHP class.
#[test]
fn a_getter_reaching_a_method_of_a_php_sharp_class_that_shares_its_short_name_names_the_class_in_full() {
    let shop_order = ("app/Shop/Order.sharp", "namespace App.Shop;\n\npublic class Order\n{\n}\n");
    let billing_order = (
        "app/Billing/Order.sharp",
        "namespace App.Billing;\n\npublic class Order\n{\n    public int price() => now();\n}\n",
    );
    let cart = (
        "app/Billing/Cart.sharp",
        "namespace App.Billing;\n\npublic class Cart\n{\n    public Cart(private Order order) { }\n\n    public int total => this.order.price();\n}\n",
    );

    assert_eq!(
        effect_issues(&[cart, billing_order, shop_order, CLOCK_STUB, NOW]),
        [
            "app/Billing/Cart.sharp:7:25 impure-getter: Getter `total` reaches `App.Billing.Order.price`, which calls `now` with the effect `Clock`. Getters must be pure."
        ]
    );
}

/// The message is the getter's, so it names a class as the getter's file does, whatever the file of the method that
/// makes the call imports.
#[test]
fn a_getter_reaching_a_call_in_another_file_names_the_class_as_the_getter_file_does() {
    let order = (
        "app/Shop/Order.sharp",
        "namespace App.Shop;\n\nimport Vendor.Clock as VendorClock;\n\npublic class Order\n{\n    public Order(private VendorClock clock) { }\n\n    public int stamp() => this.clock.now();\n}\n",
    );
    let cart = (
        "app/Shop/Cart.sharp",
        "namespace App.Shop;\n\npublic class Cart\n{\n    public Cart(private Order order) { }\n\n    public int stamped => this.order.stamp();\n}\n",
    );

    assert_eq!(
        effect_issues(&[cart, order, VENDOR_CLOCK, APP_CLOCK]),
        [
            "app/Shop/Cart.sharp:7:27 impure-getter: Getter `stamped` reaches `Order.stamp`, which calls `Vendor.Clock.now` with no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern Clock.now;` when it has no effect, or name its effects after `uses`. Add `import Vendor.Clock;` to the file."
        ]
    );
}

const METER: (&str, &str) = (
    "src/Lib/Meter.php",
    "<?php\n\nnamespace Lib;\n\nfinal class Meter\n{\n    public function read(): int\n    {\n        return 0;\n    }\n}\n",
);

/// The `extern` the help writes compiles only where its class is bound, so the help names the import a class from
/// another namespace needs, even when no other class shares its short name.
#[test]
fn a_getter_calling_a_class_its_file_does_not_import_names_the_import_the_extern_needs() {
    let gauge = (
        "app/Shop/Gauge.sharp",
        "namespace App.Shop;\n\nimport Lib.Meter;\n\npublic class Gauge\n{\n    public Gauge(private Meter source) { }\n\n    public Meter meter() => this.source;\n}\n",
    );
    let panel = (
        "app/Shop/Panel.sharp",
        "namespace App.Shop;\n\npublic class Panel\n{\n    public Panel(private Gauge gauge) { }\n\n    public int level => this.gauge.meter().read();\n}\n",
    );

    assert_eq!(
        effect_issues(&[panel, gauge, METER]),
        [
            "app/Shop/Panel.sharp:7:25 impure-getter: Getter `level` calls `Meter.read`, which has no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern Meter.read;` when it has no effect, or name its effects after `uses`. Add `import Lib.Meter;` to the file."
        ]
    );
}

/// A class of the file's own namespace is bound without an import, so the help names none.
#[test]
fn a_getter_calling_a_class_of_its_own_namespace_names_no_import() {
    let printer = (
        "src/App/Shop/Printer.php",
        "<?php\n\nnamespace App\\Shop;\n\nfinal class Printer\n{\n    public function print(): int\n    {\n        return 0;\n    }\n}\n",
    );
    let kiosk = (
        "app/Shop/Kiosk.sharp",
        "namespace App.Shop;\n\npublic class Kiosk\n{\n    public Kiosk(private Printer printer) { }\n\n    public int printed => this.printer.print();\n}\n",
    );

    assert_eq!(
        effect_issues(&[kiosk, printer]),
        [
            "app/Shop/Kiosk.sharp:7:27 impure-getter: Getter `printed` calls `Printer.print`, which has no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern Printer.print;` when it has no effect, or name its effects after `uses`."
        ]
    );
}

/// An import of the class's short name would clash with the file's import of another class, and `Clock` in the
/// `extern` would name that other class. So the help imports the class under the alias its full name gives, and the
/// `extern` uses the alias. Following the help clears the error.
#[test]
fn a_getter_calling_a_class_whose_short_name_its_file_imports_names_an_aliased_import() {
    let timer = (
        "app/Shop/Timer.sharp",
        "namespace App.Shop;\n\nimport App.Clock;\n\npublic class Timer\n{\n    public Timer(private Source source, private Clock clock) { }\n\n    public int started => this.source.clock().now();\n}\n",
    );
    let declared = (
        "app/Shop/Timer.sharp",
        "namespace App.Shop;\n\nimport App.Clock;\nimport Vendor.Clock as VendorClock;\n\nextern VendorClock.now;\n\npublic class Timer\n{\n    public Timer(private Source source, private Clock clock) { }\n\n    public int started => this.source.clock().now();\n}\n",
    );

    assert_eq!(
        effect_issues(&[timer, CLOCK_SOURCE, VENDOR_CLOCK, APP_CLOCK]),
        [
            "app/Shop/Timer.sharp:9:27 impure-getter: Getter `started` calls `Vendor.Clock.now`, which has no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern VendorClock.now;` when it has no effect, or name its effects after `uses`. Add `import Vendor.Clock as VendorClock;` to the file."
        ]
    );
    assert_eq!(effect_issues(&[declared, CLOCK_SOURCE, VENDOR_CLOCK, APP_CLOCK]), Vec::<String>::new());
}

/// A plain PHP namespace may start with a lowercase letter, and the alias is a PHP# type name, so it starts each part
/// with a capital.
#[test]
fn an_aliased_import_capitalizes_each_part_of_a_lowercase_namespace() {
    let clock = (
        "src/vendor/Clock.php",
        "<?php\n\nnamespace vendor;\n\nfinal class Clock\n{\n    public function now(): int\n    {\n        return time();\n    }\n}\n",
    );
    let source = (
        "app/Shop/Source.sharp",
        "namespace App.Shop;\n\nimport vendor.Clock;\n\npublic class Source\n{\n    public Source(private Clock time) { }\n\n    public Clock clock() => this.time;\n}\n",
    );
    let timer = (
        "app/Shop/Timer.sharp",
        "namespace App.Shop;\n\nimport App.Clock;\n\npublic class Timer\n{\n    public Timer(private Source source, private Clock clock) { }\n\n    public int started => this.source.clock().now();\n}\n",
    );

    assert_eq!(
        effect_issues(&[timer, source, clock, APP_CLOCK]),
        [
            "app/Shop/Timer.sharp:9:27 impure-getter: Getter `started` calls `vendor.Clock.now`, which has no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern VendorClock.now;` when it has no effect, or name its effects after `uses`. Add `import vendor.Clock as VendorClock;` to the file."
        ]
    );
}

/// When the file binds the alias too, the help numbers it, as C#'s Roslyn numbers a name it generates, from 1 up to
/// the first name the file leaves free.
#[test]
fn an_aliased_import_whose_alias_the_file_binds_takes_the_first_free_number() {
    let timer = (
        "app/Shop/Timer.sharp",
        "namespace App.Shop;\n\nimport App.Clock;\nimport Lib.VendorClock;\n\npublic class Timer\n{\n    public Timer(private Source source, private Clock clock) { }\n\n    public int started => this.source.clock().now();\n}\n",
    );

    assert_eq!(
        effect_issues(&[timer, CLOCK_SOURCE, VENDOR_CLOCK, APP_CLOCK]),
        [
            "app/Shop/Timer.sharp:10:27 impure-getter: Getter `started` calls `Vendor.Clock.now`, which has no `extern` declaration. Getters must be pure. Help: Declare it in a .sharp file: `extern VendorClock1.now;` when it has no effect, or name its effects after `uses`. Add `import Vendor.Clock as VendorClock1;` to the file."
        ]
    );
}

/// A law states a fact, so its body is a `bool`.
#[test]
fn a_law_whose_body_is_not_a_bool_is_an_invalid_return_statement_of_the_law() {
    let count = (
        "app/Shared/Count.sharp",
        "namespace App.Shared;\n\npublic class Count\n{\n    law alwaysPositive(int a) => a + 1;\n}\n",
    );

    assert_eq!(issues(count, &[]), ["5:34 invalid-return-statement"]);
    let refused = analyze(&PLUGIN_REGISTRY, settings(), count, &[]);
    assert_eq!(
        refused.iter().map(|issue| (issue.message.as_str(), issue.help.as_deref())).collect::<Vec<_>>(),
        [(
            "Invalid return type for law `Count.alwaysPositive`: expected `bool`, but found `int`.",
            Some("A law states a fact, so its body is a `bool`.")
        )]
    );
}

/// A law's parameter without a type is refused as every PHP# parameter without one is.
#[test]
fn a_law_parameter_without_a_type_is_refused_as_any_php_sharp_parameter_is() {
    let count = (
        "app/Shared/Count.sharp",
        "namespace App.Shared;\n\npublic class Count\n{\n    law reflexive(a) => a == a;\n}\n",
    );

    assert_eq!(messages(count, &[])[0], "A PHP# parameter needs a type, as in `int extra`.");
}

/// A law ranges over its parameters alone, so `this` is refused in it as in a static method.
#[test]
fn this_in_a_law_is_refused_as_in_a_static_method() {
    let law = (
        "app/Shared/Count.sharp",
        "namespace App.Shared;\n\npublic class Count\n{\n    private int total = 0;\n\n    law matches(int a) => this.total == a;\n}\n",
    );
    let static_method = (
        "app/Shared/Count.sharp",
        "namespace App.Shared;\n\npublic class Count\n{\n    private int total = 0;\n\n    public static bool matches(int a) => this.total == a;\n}\n",
    );

    assert_eq!(issues(law, &[]), ["7:27 undefined-variable"]);
    assert_eq!(issues(static_method, &[]), ["7:42 undefined-variable"]);
}

/// Spec section 28: a law is checked and never runs, so a call of it names no method.
#[test]
fn a_call_of_a_law_is_a_non_existent_method_that_names_the_law() {
    let ledger = (
        "app/Shared/Ledger.sharp",
        "namespace App.Shared;\n\npublic class Ledger\n{\n    public static bool check(Money a, Money b) => Money.addKeepsCurrency(a, b);\n}\n",
    );

    assert_eq!(codes(&issues(ledger, &[LAWFUL_MONEY])), ["non-existent-method", "mixed-return-statement"]);
    assert_eq!(messages(ledger, &[LAWFUL_MONEY])[0], "`Money.addKeepsCurrency` is a law, and a law is never called.");
}

/// A PHP caller of a law gets the same refusal, with the law named as PHP writes a static method.
#[test]
fn a_php_call_of_a_law_is_a_non_existent_method_that_names_the_law_as_php_writes_it() {
    let ledger = (
        "src/Shared/Ledger.php",
        "<?php\n\nnamespace App\\Shared;\n\nfinal class Ledger\n{\n    public static function check(Money $a, Money $b): bool\n    {\n        return Money::addKeepsCurrency($a, $b);\n    }\n}\n",
    );

    assert_eq!(codes(&issues(ledger, &[LAWFUL_MONEY]))[0], "non-existent-method");
    assert_eq!(
        messages(ledger, &[LAWFUL_MONEY])[0],
        "`App\\Shared\\Money::addKeepsCurrency` is a law, and a law is never called."
    );
}

#[test]
fn a_getter_applying_a_pure_operator_passes() {
    let money = (
        "app/Shop/Money.sharp",
        "namespace App.Shop;\n\npublic class Money\n{\n    public Money(public int cents) { }\n\n    public static Money operator +(Money a, Money b) => new Money(a.cents + b.cents);\n}\n",
    );
    let cart = (
        "app/Shop/Cart.sharp",
        "namespace App.Shop;\n\npublic class Cart\n{\n    public Cart(private Money price, private Money tax) { }\n\n    public Money total => this.price + this.tax;\n}\n",
    );

    assert_eq!(effect_issues(&[cart, money]), Vec::<String>::new());
}
