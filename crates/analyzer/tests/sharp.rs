#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;

use foldhash::HashSet;

use mago_allocator::LocalArena;
use mago_analyzer::Analyzer;
use mago_analyzer::analysis_result::AnalysisResult;
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
use mago_codex::populator::populate_codebase;
use mago_codex::scanner::scan_program;
use mago_database::DatabaseReader;
use mago_database::file::File;
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
    let code = analyzed.1;

    analyze(registry, settings, analyzed, others)
        .iter()
        .map(|issue| {
            let offset = issue.primary_span().expect("a primary span").start.offset as usize;
            let line = code[..offset].matches('\n').count() + 1;
            let column = offset - code[..offset].rfind('\n').map_or(0, |newline| newline + 1) + 1;

            format!("{line}:{column} {}", issue.code.as_deref().unwrap_or("none"))
        })
        .collect()
}

/// Analyzes `analyzed` together with `others` under `settings` and the plugins of `registry`, and returns its issues.
fn analyze(
    registry: &PluginRegistry,
    settings: Settings,
    analyzed: (&'static str, &'static str),
    others: &[(&'static str, &'static str)],
) -> Vec<Issue> {
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
    Analyzer::new(&arena, file, names, &metadata, registry, settings)
        .analyze(program, &mut result)
        .expect("analysis succeeds");

    // The analyzed file's parse errors come first, as `mago analyze` reports them beside the analysis.
    program.errors.iter().map(Issue::from).chain(result.issues).collect()
}

#[test]
fn adding_a_mixed_operand_reports_mixed_operand_as_in_php() {
    let any = "<?php\n\nnamespace Lib;\n\nfinal class Any\n{\n    public static function value(): mixed\n    {\n        return 1;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Any;\n\nclass Report\n{\n    public static int total()\n    {\n        return Any.value() + 1;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Any;\n\nclass Report\n{\n    public static function total(): int\n    {\n        return Any::value() + 1;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Any.php", any)]);
    let php_issues = issues(("src/Demo/Report.php", php), &[("src/Lib/Any.php", any)]);

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
    for class in ["Sharp\\ListMethods", "Sharp\\MapMethods"] {
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
/// checks its arguments against it.
#[test]
fn a_let_local_takes_any_value_of_its_first_values_general_type() {
    let sharp = "namespace Demo;\n\nclass Tally\n{\n    public int run()\n    {\n        let total = 0;\n        total = 5;\n        let sizes = [5];\n        sizes = [];\n        sizes.add(6);\n        sizes.add(\"six\");\n        let rows = [[1]];\n        rows = [[2, 3], []];\n        let prices = [\"a\": 1];\n        prices = [\"b\": 2, \"c\": 3];\n        prices = [1.5];\n        return total + count(sizes) + count(rows) + count(prices);\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[]),
        ["12:19 invalid-argument", "17:18 invalid-local-assignment-value"]
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
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? first, int? second)\n    {\n        if (first !== null || second !== null) {\n            return first ?? second;\n        }\n        return 0;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function total(?int $first, ?int $second): int\n    {\n        if ($first !== null || $second !== null) {\n            return $first ?? $second;\n        }\n        return 0;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues(("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, Vec::<String>::new());
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
}

#[test]
fn nullable_values_flow_in_and_out_with_no_issues() {
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static Calc? find(int? id, Calc? fallback = null)\n    {\n        if (id === null) {\n            return null;\n        }\n        return fallback;\n    }\n\n    public static int? total()\n    {\n        const found = Report.find(null);\n        if (found === null) {\n            return null;\n        }\n        return found.add(1, 2);\n    }\n}\n";

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
            "9:15 mixed-assignment",
            "10:16 nullable-return-statement",
            "10:16 invalid-return-statement",
        ]
    );
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
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
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int total(int? extra)\n    {\n        for (int step = \"one\"; step < 3; step++) {\n        }\n        for (int? found = null; found === null; ) {\n            found = extra;\n        }\n        for (int count = 0; count < 3; count++) {\n            count = 1.5;\n        }\n        return 0;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[]),
        ["7:25 invalid-local-assignment-value", "13:21 invalid-local-assignment-value"]
    );
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
    let sharp = "namespace Demo;\n\nimport Lib.Box;\n\nclass Report\n{\n    public static int total(Box box, Box other)\n    {\n        if (other.count() !== null) {\n            box.value = 1;\n            return other.count();\n        }\n        return 0;\n    }\n}\n";
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
                "A property that replaces the untyped PHP property `Lib\\Record::$label` is not supported yet."
                    .to_owned(),
                "Rename the property, or give the PHP property a type.".to_owned()
            ),
            (
                "A property that replaces the untyped PHP property `Lib\\Record::$code` is not supported yet."
                    .to_owned(),
                "Rename the property, or give the PHP property a type.".to_owned()
            ),
        ]
    );
}

/// A property over a plain PHP parent's attributes has no storage. PHP# has no conversion from the `mixed` that
/// `getAttribute` returns to the property's type yet: spec section 21 writes it `as`, which is not built. So the `get`
/// body reports `mixed` as the PHP twin's hook does.
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
/// has no storage, so its `get` still reports `mixed` until spec section 21's `as` is built. One whose body uses
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
    let registry = "<?php\n\nnamespace Lib;\n\nenum Order: string\n{\n    case Ascending = 'asc';\n}\n\nfinal class Registry\n{\n    public const int VERSION = 2;\n    public static string $label = 'registry';\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Order;\nimport Lib.Registry;\n\nclass Members\n{\n    public static int version()\n    {\n        return Registry.VERSION;\n    }\n\n    public static Order order()\n    {\n        return Order.Ascending;\n    }\n\n    public static int label()\n    {\n        return Registry.label;\n    }\n\n    public static int missing()\n    {\n        return Registry.missing;\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Order;\nuse Lib\\Registry;\n\nclass Members\n{\n    public static function version(): int\n    {\n        return Registry::VERSION;\n    }\n\n    public static function order(): Order\n    {\n        return Order::Ascending;\n    }\n\n    public static function label(): int\n    {\n        return Registry::$label;\n    }\n\n    public static function missing(): int\n    {\n        return Registry::$missing;\n    }\n}\n";

    let sharp_issues = issues(("src/Demo/Members.sharp", sharp), &[("src/Lib/Registry.php", registry)]);
    let php_issues = issues(("src/Demo/Members.php", php), &[("src/Lib/Registry.php", registry)]);

    assert_eq!(codes(&sharp_issues), codes(&php_issues), "{sharp_issues:?} {php_issues:?}");
    assert_eq!(
        sharp_issues,
        ["20:16 invalid-return-statement", "25:25 non-existent-property", "25:16 invalid-return-statement"]
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

/// A header with two classes, a trait, a missing name, a final class or an interface whose method the class lacks
/// reports what its PHP twin's `extends` and `implements` report.
#[test]
fn a_header_reports_what_extends_and_implements_report_in_php() {
    let library = "<?php\n\nnamespace Lib;\n\ninterface Named\n{\n    public function name(): string;\n}\n\nclass Entity\n{\n}\n\nclass Other\n{\n}\n\ntrait Mixin\n{\n}\n\nfinal class Sealed\n{\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Entity;\nimport Lib.Mixin;\nimport Lib.Named;\nimport Lib.Other;\nimport Lib.Sealed;\n\npublic class Twice : Entity, Other\n{\n}\n\npublic class Blend : Mixin\n{\n}\n\npublic class Lost : Missing\n{\n}\n\npublic class Closed : Sealed\n{\n}\n\npublic class Partial : Named\n{\n}\n\npublic interface Wide : Entity\n{\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nuse Lib\\Entity;\nuse Lib\\Mixin;\nuse Lib\\Named;\nuse Lib\\Other;\nuse Lib\\Sealed;\n\nclass Twice extends Entity implements Other\n{\n}\n\nclass Blend implements Mixin\n{\n}\n\nclass Lost implements Missing\n{\n}\n\nclass Closed extends Sealed\n{\n}\n\nclass Partial implements Named\n{\n}\n\ninterface Wide extends Entity\n{\n}\n";
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
    assert_eq!(missing.message, "Missing `override` modifier on overriding method `Demo\\Child::size`.");
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
            "The override `Demo\\Order::$fillable` is `public`, but `Lib\\Model::$fillable` is `protected`.",
        ),
        (
            "invalid-property-default-value",
            "Default value for property `Demo\\Order::table` is not assignable to its declared type.",
        ),
        (
            "incompatible-property-type",
            "The override `Demo\\Order::$timestamps` has type `int`, which does not fit `bool`, the type of `Lib\\Model::$timestamps`.",
        ),
        ("incompatible-property-type", "Property `Demo\\Order::$appends` has an incompatible type declaration."),
        ("missing-override-attribute", "Missing `override` modifier on overriding field `Demo\\Order::$with`."),
        ("invalid-override-attribute", "Invalid `override` modifier on `Demo\\Order::$missing`."),
        ("invalid-override-attribute", "Invalid `override` modifier on `Demo\\Order::$secret`."),
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
            "The override `Demo\\Order::$table` has type `int`, which does not fit `null|string`, the type of `Lib\\Model::$table`.",
        )]
    );
}

/// A PHP# parent's property is overridden as a property, spec section 6.1, which is not supported yet. A field that
/// replaces one without `override` keeps PHP's own checks.
#[test]
fn overriding_a_sharp_property_is_not_supported_yet() {
    let sharp = "namespace Demo;\n\npublic class Base\n{\n    protected string label = \"base\";\n    protected int size = 1;\n}\n\npublic class Child : Base\n{\n    protected override string label = \"child\";\n    protected int size = 2;\n}\n";

    let issues = analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Base.sharp", sharp), &[]);

    assert_eq!(
        issues
            .iter()
            .map(|issue| (issue.code.as_deref().unwrap_or("none"), issue.message.as_str()))
            .collect::<Vec<_>>(),
        [("not-supported-yet", "Overriding the PHP# property `Demo\\Base::$label` is not supported yet.")]
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
        "Parameter #1 of `Demo\\RenamedReport::resize()` is named `w` but parent `Lib\\Report::resize()` names it `width`"
    );
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
    assert_eq!(invalid.message, "Cannot use `super` as the current type (`Demo\\Tag`) does not have a parent class.");
}

/// The checker refuses a member of `typeof(X)` once, so the analyzer adds no issue on the refused read, its chain, or
/// the value it gives.
#[test]
fn reading_a_member_of_typeof_adds_no_issue() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public string label()\n    {\n        return typeof(Report).name;\n    }\n\n    public string first()\n    {\n        return typeof(Report).name.first;\n    }\n\n    public string called()\n    {\n        return typeof(Report).name();\n    }\n}\n";

    assert_eq!(issues(("src/Demo/Report.sharp", sharp), &[]), Vec::<String>::new());
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
    let any = "<?php\n\nnamespace Lib;\n\nfinal class Any\n{\n    public static function value(): mixed\n    {\n        return 1;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Any;\n\nclass Report\n{\n    public static int pick(int count, string name, bool? maybe)\n    {\n        const a = count ? 1 : 2;\n        const b = name ? 1 : 2;\n        const c = maybe ? 1 : 2;\n        const d = Any.value() ? 1 : 2;\n        const e = count ?: 2;\n        return a + b + c + d + e;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Any.php", any)]),
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
    let any = "<?php\n\nnamespace Lib;\n\nfinal class Any\n{\n    public static function value(): mixed\n    {\n        return 1;\n    }\n}\n";
    let sharp = "namespace Demo;\n\nimport Lib.Any;\n\nclass Report\n{\n    public static string pick(string name, bool flag, int? maybe)\n    {\n        const a = (int)name;\n        const b = (float)flag;\n        const c = (string)name;\n        const d = (int)maybe;\n        const e = (string)Any.value();\n        return `${a}${b}${c}${d}${e}`;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Any.php", any)]),
        [
            "9:19 invalid-operand",
            "10:19 invalid-operand",
            "11:19 invalid-operand",
            "12:19 invalid-operand",
            "13:19 invalid-operand",
        ]
    );
}

#[test]
fn int_and_float_parse_have_the_types_of_the_sharp_library_classes() {
    let sharp = "namespace Demo;\n\nclass Report\n{\n    public static int count(string text, int? fallback)\n    {\n        const price = Float.parse(text) + (Float.tryParse(fallback) ?? 0.0);\n        const count = Int.parse(text) + (Int.tryParse(null) ?? 0);\n        return price > 1.0 ? count : Int.tryParse(text);\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function count(string $text, ?int $fallback): int\n    {\n        $price = \\Sharp\\Float::parse($text) + (\\Sharp\\Float::tryParse($fallback) ?? 0.0);\n        $count = \\Sharp\\Int::parse($text) + (\\Sharp\\Int::tryParse(null) ?? 0);\n        return $price > 1.0 ? $count : \\Sharp\\Int::tryParse($text);\n    }\n}\n";

    // `check_throws` skips `.sharp` files, so the PHP twin is analyzed without it.
    let sharp_issues = issues(("src/Demo/Report.sharp", sharp), &[]);
    let php_issues = issues_with(Settings { check_throws: false, ..settings() }, ("src/Demo/Report.php", php), &[]);

    assert_eq!(sharp_issues, ["9:16 nullable-return-statement", "9:16 invalid-return-statement"]);
    assert_eq!(codes(&sharp_issues), codes(&php_issues));
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
            "string impossible-type-comparison Impossible type assertion: `$count` of type `int` can never be `string`.",
            "\"none\" impossible-type-comparison This pattern never matches the value it tests.",
            "Square impossible-type-comparison This pattern never matches the value it tests.",
            "Square impossible-type-comparison This pattern never matches the value it tests.",
            "\"none\" impossible-type-comparison This pattern never matches the value it tests.",
        ]
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
    let sharp = "namespace Demo;\n\nimport Lib.Limits;\nimport Lib.Status;\n\nclass Report\n{\n    public static string label(Status status) => match (status) {\n        Status.Open => \"open\",\n        Status.Closed => \"closed\",\n    };\n\n    public static void close(Status? status)\n    {\n        match (status) {\n            Status.Open when status !== null => {},\n            Status.Closed => {},\n        }\n    }\n\n    public static int level(int count) => match (count) {\n        Limits.LOW => 1,\n        Limits.HIGH => 2,\n    };\n}\n";

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
    let sharp = "namespace Demo;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int|string key = 1;\n\n    public int|string find(int|Calc id)\n    {\n        int|string found = this.key;\n        if (found === 1) {\n            return 1.5;\n        }\n        return id;\n    }\n}\n";
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
    assert_eq!(issue.message, "`Demo\\Child::sum()` must declare parameter `values` variadic like `Demo\\Base::sum()`");
}

#[test]
fn a_backed_enum_and_a_class_using_it_have_the_issues_of_their_php_twins() {
    let sharp = "namespace Demo;\n\nenum Status : string\n{\n    case Active = \"a\";\n    case Paused = \"p\";\n\n    public string label()\n    {\n        return this.name + \": \" + this.value;\n    }\n\n    public static Status fallback()\n    {\n        return Status.from(\"a\");\n    }\n}\n\nclass Report\n{\n    private Status status;\n\n    public Report(Status status)\n    {\n        this.status = status;\n    }\n\n    public string describe(string code)\n    {\n        const found = Status.tryFrom(code);\n        if (found === null || count(Status.cases()) < 2) {\n            return this.status.label();\n        }\n        return found.value + found.name;\n    }\n\n    public static Report make()\n    {\n        return new Report(Status.fallback());\n    }\n}\n";
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
    let sharp = "namespace Demo;\n\nimport Lib.Registry;\n\nenum Status : int\n{\n    case Active = Registry.VERSION;\n    case Paused = Registry.count;\n\n    public static Status first() => Status.Active;\n}\n\nclass Report\n{\n    public static bool run(Status status = Status.Active)\n    {\n        return status === Status.first();\n    }\n}\n";
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
    let sharp = "namespace Demo;\n\nimport Lib.Registry;\n\npublic enum Status : string\n{\n    case Active = \"a\";\n    case Paused = Registry.PAUSED;\n\n    public const Status Default = Status.Active;\n\n    public bool active() => this === Status.Active;\n\n    public string label() => this.name;\n}\n\nclass Report\n{\n    public string run(Status status = Status.Active)\n    {\n        if (status === Status.Active) {\n            return Status.Active.label();\n        }\n        return Status.Default.label();\n    }\n}\n";
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
            "Could not infer a precise return type for function `App\\Tenant\\Report::run`. Saw type `mixed`.",
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
            "Could not infer a precise return type for function `App\\Tenant\\Report::run`. Saw type `mixed`.",
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
    let sharp = "namespace Demo;\n\nenum Status : string\n{\n    case Active = \"a\";\n    case Paused = \"p\";\n\n    public static Status fallback() => Status.Active;\n}\n\nclass Report\n{\n    public int run(List<string> codes)\n    {\n        const Function<Status(string)> parse = Status.from;\n        const Function<Status()> fallback = Status.fallback;\n        List<Status> found = codes.map(parse);\n        List<Status> active = found.filter(s => s === Status.Active || s === fallback());\n        List<int> wrong = codes.map(Status.from);\n        return count(active) + count(wrong);\n    }\n}\n";

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

/// `for (const [k, v] of x)` reads the keys of a `Map`. A `List`'s indexes come from `entries()`, as spec section 12
/// writes. PHP stores an all-digit string key as an `int`, so the key of a `Map<string, V>` reads back as
/// `int|string`.
#[test]
fn a_key_and_value_loop_reads_a_map_and_its_keys_as_php_stores_them() {
    let sharp = "namespace Demo;\n\nclass Loops\n{\n    public int run(List<int> sizes, Map<string, int> counts)\n    {\n        let total = 0;\n        for (const [index, size] of sizes) {\n            total += index + size;\n        }\n        for (const [name, count] of counts) {\n            total += strlen(name) + count;\n        }\n        return total;\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Loops.sharp", sharp), &[]),
        ["8:37 invalid-iterator", "12:29 possibly-invalid-argument"]
    );
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
            "Invalid argument type for argument #1 of `List<Demo\\Line>.add`: expected `Demo\\Line`, but found `int(1)`.",
            "Invalid argument type for argument #1 of `List<int>.set`: expected `int`, but found `string('a')`.",
            "Method `add` does not exist on `Map<string, int>`.",
            "Method `delete` does not exist on `List<int>`.",
            "Invalid argument type for argument #1 of `Map<string, int>.get`: expected `string`, but found `float(1.5)`.",
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
        issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        [
            "8:12 template-constraint-violation",
            "10:12 template-constraint-violation",
            "10:33 template-constraint-violation",
            "12:9 template-constraint-violation"
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

/// The engine holds a backed enum key as its backing value, and the loop reads it back as the case only through a
/// written key type that names the enum. A written type that cannot hold the case is refused once, as a typed local's.
#[test]
fn a_loop_over_a_map_keyed_by_a_backed_enum_needs_the_key_type_written() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Tally\n{\n    public void read(Map<Status, int> counts)\n    {\n        for (const [status, n] of counts) {\n        }\n        for (const [Status? status, int n] of counts) {\n        }\n        for (const [string status, int n] of counts) {\n        }\n        for (const [Status status, string n] of counts) {\n        }\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        [
            "9:21 invalid-foreach-key",
            "11:21 invalid-foreach-key",
            "13:28 invalid-local-assignment-value",
            "15:43 invalid-local-assignment-value",
        ]
    );
}

/// The message writes the loop over again with the enum's name, the value's type and the collection.
#[test]
fn a_backed_key_without_its_type_names_the_loop_that_compiles() {
    let sharp = "namespace Demo;\n\nimport Lib.Status;\n\nclass Tally\n{\n    public void read(Map<Status, int> counts)\n    {\n        for (const [status, n] of counts) {\n        }\n    }\n}\n";

    let issue =
        analyze(&PLUGIN_REGISTRY, settings(), ("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)])
            .remove(0);

    assert_eq!(issue.message, "A `Status` key needs its type written: `for (const [Status status, int n] of counts)`.");
}

/// A written key or value type checks as a typed local's does. Spec section 12 reads a `Map<string, V>` key back as a
/// `string`, even the key `"5"` PHP stores as an int, so a key written `string` reads back through `(string)`. An
/// unwritten key stays `int|string` until typed compilation casts it, and so does a key written `string?`, which
/// nothing casts.
#[test]
fn a_written_loop_variable_type_checks_as_a_typed_local_does() {
    let sharp = "namespace Demo;\n\nimport Lib.Line;\n\nclass Tally\n{\n    public void read(Map<string, int> stock, List<Line> lines)\n    {\n        for (const [int|string sku, int n] of stock) {\n        }\n        for (const [string sku, int n] of stock) {\n            this.reserve(sku);\n        }\n        for (const [sku, n] of stock) {\n            this.reserve(sku);\n        }\n        for (const [string? sku, int n] of stock) {\n        }\n        for (const Line line of lines) {\n        }\n        for (const int line of lines) {\n        }\n    }\n\n    private void reserve(string sku)\n    {\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Tally.sharp", sharp), &[("src/Lib/Status.php", STATUS)]),
        [
            "15:26 possibly-invalid-argument",
            "17:29 invalid-local-assignment-value",
            "21:24 invalid-local-assignment-value",
        ]
    );
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
fn a_map_spread_is_not_supported_yet() {
    let sharp = "namespace Demo;\n\nimport Lib.Prices;\n\nclass Report\n{\n    public void merge(Map<string, int> defaults, Map<string, int> overrides)\n    {\n        const merged = [...defaults, ...overrides];\n        const rooted = [...defaults, \"root\": 0];\n        const named = [...Prices.named()];\n    }\n}\n";

    assert_eq!(
        issues(("src/Demo/Report.sharp", sharp), &[("src/Lib/Prices.php", PRICES)]),
        ["9:25 not-supported-yet", "10:25 not-supported-yet", "11:24 not-supported-yet"]
    );
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
            "Cannot spread a value of type `iterable<int, int>`: PHP# spreads a `List` or a `Map`.",
            "Cannot use spread operator on non-iterable type `int`.",
        ]
    );
}
