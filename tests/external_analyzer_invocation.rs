#![allow(clippy::expect_used, clippy::missing_panics_doc)]

use std::borrow::Cow;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use mago_analyzer::external::ExternalAnalyzer;
use mago_analyzer::external::ExternalAnalyzerHandle;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings;
use mago_database::Database;
use mago_database::file::File;
use mago_database::file::FileType;
use mago_extension::WorkerCommand;
use mago_extension::WorkerPool;
use mago_extension::WorkerPoolOptions;
use mago_orchestrator::service::incremental_analysis::IncrementalAnalysisService;
use mago_php_version::PHPVersion;
use mago_syntax::settings::ParserSettings;

use crate::common;

const SOURCE: &str = r"<?php

declare(strict_types=1);

/** @template T */
class Builder
{
    /** @return T */
    public function first(): mixed
    {
        return null;
    }
}

/** @property string $magic */
class BaseModel
{
    public function __get(string $property): mixed
    {
        return null;
    }

    public function __set(string $property, mixed $value): void {}

    public static function query(): mixed
    {
        return null;
    }

    public function newQuery(): mixed
    {
        return null;
    }

    public static function echoArgument(string $value): mixed
    {
        return null;
    }

    public static function acceptString(array $value): mixed
    {
        return null;
    }

    public static function late(): mixed
    {
        return null;
    }

    public static function magicValues(): mixed
    {
        return null;
    }

    public static function callLate(): mixed
    {
        return static::late();
    }

    public function passthrough(): object
    {
        return $this;
    }
}

final class User extends BaseModel {}

class Relation
{
    public function __call(string $name, array $arguments): mixed
    {
        return null;
    }
}

class HasMany extends Relation {}

final class CustomRelation extends HasMany {}

interface ExternalInterface {}

final class ExternalResult
{
    public function path(): string
    {
        return '';
    }
}

final class ExternalService {}

final class ExternalStaticService {}

final class ExternalAssertions
{
    public static function isString(mixed $value): bool
    {
        return true;
    }
}

interface ReceiverMarker
{
    public function marked(): string;
}

final class ReceiverWidening
{
    public function willImplement(string $interface): self
    {
        return $this;
    }
}

final class Attribute
{
    public const int TARGET_METHOD = 4;

    public function __construct(public int $flags = 0) {}
}

#[Attribute(Attribute::TARGET_METHOD)]
final class FrameworkInitializer {}

class FrameworkTestCase
{
    protected ExternalResult $framework;

    #[FrameworkInitializer]
    protected function setUp(): void
    {
        $this->framework = new ExternalResult();
    }

    public function getFramework(): ExternalResult
    {
        return $this->framework;
    }
}

final class ManagedTestCase extends FrameworkTestCase
{
    public ExternalResult $application;

    private ExternalResult $native;

    private ExternalResult $lifecycle;

    public function __construct()
    {
        $this->native = new ExternalResult();
    }

    public function getApplication(): ExternalResult
    {
        return $this->application;
    }

    public function getNative(): ExternalResult
    {
        return $this->native;
    }

    #[FrameworkInitializer]
    protected function setUp(): void
    {
        parent::setUp();
        $this->lifecycle = new ExternalResult();
    }

    public function getLifecycle(): ExternalResult
    {
        return $this->lifecycle;
    }
}

interface Marker {}

class DynamicProxy
{
    public function __call(string $name, array $arguments): mixed
    {
        return null;
    }
}

class DynamicFacade
{
    public static function __callStatic(string $name, array $arguments): mixed
    {
        return null;
    }
}

class ClosureCommand
{
    public function comment(string $message): void {}
}

class Artisan
{
    public static function command(string $signature, Closure $callback): void {}
}

function external_function(): mixed
{
    return null;
}

function external_assert_string(mixed $value): void {}

function collect(array $value): mixed
{
    return null;
}

/** @param Builder<User> $_builder */
function take_user_builder(Builder $_builder): void {}

function take_user(User $_user): void {}

function take_string(string $_value): void {}

function take_int(int $_value): void {}

function take_external_result(ExternalResult $_result): void {}

function take_intersection(BaseModel&Marker $_model): void {}

function take_widened_receiver(ReceiverWidening&ReceiverMarker $_receiver): void {}

/** @param array<int, string> $_values */
function take_string_map(array $_values): void {}

/** @param list<Builder<string>> $_builders */
function take_string_builders(array $_builders): void {}

take_string(external_function());
/** @var mixed $asserted */
$asserted = 'asserted';
external_assert_string($asserted);
take_string($asserted);
/** @var mixed $conditional */
$conditional = 'conditional';
if (ExternalAssertions::isString($conditional)) {
    take_string($conditional);
}
$receiver = new ReceiverWidening();
$receiver->willImplement(ReceiverMarker::class);
take_widened_receiver($receiver);
take_string($receiver->marked());
take_string(collect('function'));
Artisan::command('inspire', function (): void {
    $this->comment('Be inspired!');
});
take_user_builder(User::query());
take_string(User::echoArgument('value'));
take_string(User::acceptString('method'));
take_string_builders(User::magicValues());
take_string((new DynamicProxy())->dynamic('instance'));
take_string((new DynamicProxy())->acceptString('dynamic-signature'));
take_string_map(DynamicFacade::dynamic('static'));
take_string((new CustomRelation())->where());
take_string((new ExternalService())->resolve('instance-provider'));
take_string(ExternalStaticService::resolve('static-provider'));
User::callLate();
(new ManagedTestCase())->getApplication();
(new ManagedTestCase())->getNative();
(new ManagedTestCase())->getFramework();
(new ManagedTestCase())->getLifecycle();

function use_external_interface(ExternalInterface $service): void
{
    take_external_result($service->fetch());
}

function get_external_path(?ExternalInterface $service): ?string
{
    return $service?->fetch()->path();
}

$user = new User();
take_user_builder($user->newQuery());
take_string($user->name);
$user->name = 123;
take_string($user->name);
take_int($user->id);
take_user($user->self);
$user->secret = 'token';
// @mago-expect analysis:invalid-property-write
$user->id = 'forbidden';
// @mago-expect analysis:invalid-property-read
// @mago-expect analysis:no-value
echo $user->secret;

/** @var Builder<User> $builder */
$builder = new Builder();
take_user($builder->first());

/** @param BaseModel&Marker $model */
function inspect_intersection(BaseModel $model): void
{
    take_intersection($model->passthrough());
}
";

const NO_METHOD_PROVIDER_SOURCE: &str = r"<?php

declare(strict_types=1);

class BaseModel
{
    public static function query(): mixed
    {
        return null;
    }
}

final class User extends BaseModel {}

User::query();
";

const UNDOCUMENTED_METHOD_SOURCE: &str = r"<?php

declare(strict_types=1);

final class DynamicProxy
{
    /** @return static */
    public function __call(string $name, array $arguments): mixed
    {
        return $this;
    }
}

final class DynamicFacade
{
    /** @return static */
    public static function __callStatic(string $name, array $arguments): mixed
    {
        return new static();
    }
}

function take_proxy(DynamicProxy $_proxy): void {}

function take_facade(DynamicFacade $_facade): void {}

take_proxy((new DynamicProxy())->dynamicCall('instance'));
take_facade(DynamicFacade::dynamicCall('static'));
";

/// A model whose `__call` the fixture's forwarding provider forwards to a query, whose own `__call` answers the query.
const FORWARDED_METHOD_SOURCE: &str = r"<?php

declare(strict_types=1);

final class ForwardedQuery
{
    /** @return static */
    public function __call(string $name, array $arguments): mixed
    {
        return $this;
    }
}

final class ForwardingModel
{
    public function __call(string $name, array $arguments): mixed
    {
        return null;
    }
}

function take_query(ForwardedQuery $_query): void {}

take_query((new ForwardingModel())->whereKey('paid'));
";

const MISSING_METHOD_SOURCE: &str = r"<?php

declare(strict_types=1);

interface DeclinedContract {}

function test(DeclinedContract $contract): void
{
    $contract->missing();
}
";

const DECLINED_PROPERTY_INITIALIZATION_SOURCE: &str = r"<?php

declare(strict_types=1);

final class ExternalResult {}

class FrameworkTestCase {}

final class ManagedTestCase extends FrameworkTestCase
{
    public ExternalResult $application;

    public function __construct() {}

    public function getApplication(): ExternalResult
    {
        return $this->application;
    }
}

(new ManagedTestCase())->getApplication();
";

const ISSUE_FILTER_SOURCE: &str = r"<?php

declare(strict_types=1);

// issue-filter-batch-proof
function known_function(): void {}

filtered_missing();
retained_missing();
";

const EFFECTIVE_SIGNATURE_DIAGNOSTIC_SOURCE: &str = r"<?php

declare(strict_types=1);

final class DynamicProxy
{
    public function __call(string $name, array $arguments): mixed
    {
        return null;
    }
}

(new DynamicProxy())->knownMethod('not-an-int');
(new DynamicProxy())->knownMethod(null);
";

#[test]
fn external_providers_receive_complete_invocation_context() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    let observation = analyze_with_fixture("src/invocation.php", SOURCE, &[])?;
    assert!(
        observation.issues.is_empty(),
        "complete external invocation context should preserve precise types: {:#?}",
        observation.issues
    );

    let mut invocations = observation.invocations;
    invocations.sort_unstable();
    assert_eq!(
        invocations,
        [
            "argument-handle",
            "class-initializer-frameworktestcase",
            "class-initializer-managedtestcase",
            "closure-this-return",
            "closure-this-signature",
            "complete-composite",
            "dynamic-instance",
            "dynamic-signature",
            "dynamic-signature-return",
            "dynamic-static",
            "function",
            "function-assertion",
            "function-signature",
            "function-signature-return",
            "generic",
            "instance",
            "intersection",
            "late-static",
            "method-assertion",
            "method-signature",
            "method-signature-return",
            "missing-class-return",
            "missing-class-signature",
            "missing-interface-return",
            "missing-interface-return",
            "missing-interface-signature",
            "missing-interface-signature",
            "missing-static-return",
            "missing-static-signature",
            "property-id-read",
            "property-id-write",
            "property-initialization",
            "property-name-read",
            "property-name-write",
            "property-secret-read",
            "property-secret-write",
            "property-self-read",
            "receiver-assertion",
            "relation-subclass",
            "static"
        ]
    );

    Ok(())
}

#[test]
fn analyzer_without_method_providers_sends_no_method_request() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    let observation =
        analyze_with_fixture("src/invocation.php", NO_METHOD_PROVIDER_SOURCE, &["MAGO_INVOCATION_FUNCTION_ONLY"])?;
    assert!(observation.issues.is_empty(), "native method analysis should remain unchanged: {:#?}", observation.issues);
    assert!(observation.invocations.is_empty(), "a host without method providers must not receive method requests");

    Ok(())
}

#[test]
fn declined_dynamic_methods_preserve_non_documented_method_diagnostics() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    let observation =
        analyze_with_fixture("src/invocation.php", UNDOCUMENTED_METHOD_SOURCE, &["MAGO_INVOCATION_FUNCTION_ONLY"])?;
    assert_eq!(
        observation.issues,
        [
            (
                Some("non-documented-method".to_owned()),
                "Ambiguous method call to `dynamicCall` on class `DynamicProxy`.".to_owned()
            ),
            (
                Some("non-documented-method".to_owned()),
                "Ambiguous method call to `dynamicCall` on class `DynamicFacade`.".to_owned()
            ),
        ]
    );
    assert!(observation.invocations.is_empty(), "a host without method providers must receive no dynamic requests");

    Ok(())
}

/// A call a provider forwards to an object that has no such method runs that object's `__call`, which types the call,
/// as a call written on that object does, and is as ambiguous as that call.
#[test]
fn a_forwarded_call_reaches_the_magic_method_of_the_object_it_is_forwarded_to() -> Result<(), Box<dyn std::error::Error>>
{
    if !php_sdk_is_available() {
        return Ok(());
    }

    let observation =
        analyze_with_fixture("src/invocation.php", FORWARDED_METHOD_SOURCE, &["MAGO_INVOCATION_FUNCTION_ONLY"])?;
    // `take_query` takes the `static` the query's `__call` answers, so only the ambiguity is reported.
    assert_eq!(
        observation.issues,
        [(
            Some("non-documented-method".to_owned()),
            "Ambiguous method call to `whereKey` on class `ForwardedQuery`.".to_owned()
        )]
    );
    assert_eq!(observation.invocations, ["forwarded-ForwardingModel::whereKey"]);

    Ok(())
}

#[test]
fn declined_missing_methods_preserve_non_existent_method_diagnostics() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    let observation = analyze_with_fixture("src/invocation.php", MISSING_METHOD_SOURCE, &[])?;
    assert_eq!(
        observation.issues,
        [(
            Some("non-existent-method".to_owned()),
            "Method `missing` does not exist on type `DeclinedContract`.".to_owned(),
        )]
    );
    assert!(observation.invocations.is_empty(), "a declining provider must not establish the missing method");

    Ok(())
}

#[test]
fn declined_property_initialization_preserves_uninitialized_diagnostic() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    let observation = analyze_with_fixture(
        "src/invocation.php",
        DECLINED_PROPERTY_INITIALIZATION_SOURCE,
        &["MAGO_INVOCATION_DECLINE_PROPERTY_INITIALIZATION"],
    )?;
    assert_eq!(observation.issues.len(), 1, "a declining provider must preserve the native diagnostic");
    assert_eq!(observation.issues[0].0.as_deref(), Some("uninitialized-property"));
    assert!(observation.invocations.is_empty(), "a declining provider must not claim initialization");

    Ok(())
}

#[test]
fn external_issue_filters_batch_and_remove_selected_native_diagnostics() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    let observation = analyze_with_fixture(
        "src/invocation.php",
        ISSUE_FILTER_SOURCE,
        &["MAGO_INVOCATION_FUNCTION_ONLY", "MAGO_INVOCATION_ISSUE_FILTER"],
    )?;
    assert_eq!(observation.issues.len(), 1, "only the explicitly filtered native issue should be suppressed");
    assert_eq!(observation.issues[0].0.as_deref(), Some("non-existent-function"));
    assert!(observation.issues[0].1.contains("`retained_missing`"));
    assert_eq!(observation.invocations.len(), 2, "both issues from the file should reach the worker batch");
    assert!(observation.invocations.iter().all(|entry| entry.starts_with("issue-filter-")));

    Ok(())
}

#[test]
fn effective_signatures_name_the_logical_callable_and_parameter_in_diagnostics()
-> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    let observation = analyze_with_fixture("src/invocation.php", EFFECTIVE_SIGNATURE_DIAGNOSTIC_SOURCE, &[])?;
    assert_eq!(
        observation.issues,
        [(
            Some("invalid-argument".to_owned()),
            "Invalid argument type for `$a` of `Subject::knownMethod`: expected `int`, but found `string('not-an-int')`."
                .to_owned(),
        ), (
            Some("null-argument".to_owned()),
            "Argument `$a` of method `Subject::knownMethod` is `null`, but parameter type `int` does not accept it."
                .to_owned(),
        )]
    );
    assert_eq!(
        observation.invocations,
        ["diagnostic-signature", "diagnostic-return", "diagnostic-signature", "diagnostic-return"]
    );

    Ok(())
}

/// `Demo\Calc`, the PHP# class whose static `make` the hook tests call, in a file of its own.
const CALC: (&str, &str) = (
    "src/Calc.sharp",
    "namespace Demo;\n\nclass Calc\n{\n    public static Calc make(Calc calc)\n    {\n        return calc;\n    }\n\n    public int add(int value)\n    {\n        return value;\n    }\n}\n",
);

/// `Demo\Report`, whose `total` calls `Calc.make(calc).add(1)`.
const REPORT: (&str, &str) = (
    "src/Report.sharp",
    "namespace Demo;\n\nclass Report\n{\n    public static int total(Calc calc)\n    {\n        return Calc.make(calc).add(1);\n    }\n}\n",
);

#[test]
fn node_hooks_see_a_sharp_static_call_as_static_as_in_php() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    const PHP: &str = "<?php\n\nnamespace Demo;\n\nclass Calc\n{\n    public static function make(Calc $calc): Calc\n    {\n        return $calc;\n    }\n\n    public function add(int $value): int\n    {\n        return $value;\n    }\n}\n\nclass Report\n{\n    public static function total(Calc $calc): int\n    {\n        return Calc::make($calc)->add(1);\n    }\n}\n";

    for files in [&[CALC, REPORT][..], &[("src/Report.php", PHP)]] {
        let observation = analyze_files_with_fixture(files, &["MAGO_INVOCATION_CALL_KINDS"])?;

        assert_eq!(observation.issues, [], "{files:?}");
        assert_eq!(
            observation.invocations,
            ["call-kind-add-instance", "receiver-call-kind-make-static", "call-kind-make-static"],
            "{files:?}"
        );
    }

    Ok(())
}

/// `Tools.largest(…)` calls the function `largest` a library declares in PHP's root namespace beside its classes of
/// `Tools`, spec section 23, as `largest(…)` does in PHP, so it reaches the hooks that target function calls, and only
/// them, as a Roslyn analyzer registered for invocations of a kind receives each call the semantic model reads as that
/// kind.
#[test]
fn node_hooks_see_a_sharp_call_of_a_global_function_as_a_function_call_as_in_php()
-> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    const LARGEST: (&str, &str) = (
        "src/Tools/largest.php",
        "<?php\n\nfunction largest(int $a, int $b): int\n{\n    return $a > $b ? $a : $b;\n}\n",
    );
    const TALLY: (&str, &str) = ("src/Tools/Tally.php", "<?php\n\nnamespace Tools;\n\nfinal class Tally\n{\n}\n");
    const EXTERNS: (&str, &str) = ("src/Externs.sharp", "namespace Demo;\n\nextern namespace Tools for largest;\n");
    const SHARP: (&str, &str) = (
        "src/Report.sharp",
        "namespace Demo;\n\nimport Tools;\n\nclass Report\n{\n    public static int total(Calc calc)\n    {\n        return Tools.largest(calc.add(1), 2);\n    }\n}\n",
    );
    const PHP: (&str, &str) = (
        "src/Report.php",
        "<?php\n\nnamespace Demo;\n\nclass Calc\n{\n    public function add(int $value): int\n    {\n        return $value;\n    }\n}\n\nclass Report\n{\n    public static function total(Calc $calc): int\n    {\n        return largest($calc->add(1), 2);\n    }\n}\n",
    );

    for files in [&[LARGEST, TALLY, EXTERNS, CALC, SHARP][..], &[LARGEST, PHP]] {
        let functions = analyze_files_with_fixture(files, &["MAGO_INVOCATION_FUNCTION_CALL_KINDS"])?;
        let methods = analyze_files_with_fixture(files, &["MAGO_INVOCATION_CALL_KINDS"])?;

        assert_eq!(functions.issues, [], "{files:?}");
        assert_eq!(functions.invocations, ["call-kind-largest-function"], "{files:?}");
        assert_eq!(methods.invocations, ["call-kind-add-instance"], "{files:?}");
    }

    Ok(())
}

/// A host reads each PHP# call as the PHP that means it: `Self.cap(3)` calls the enclosing class's static method, as
/// `static::cap(3)` does, `PaymentResult.Declined(reason: …)` builds the enum case, as `new PaymentResult\Declined(…)`
/// does, so it reaches no method hook, and `twice(1)` calls the function value a local holds, which names no function,
/// as `$twice(1)` does.
#[test]
fn node_hooks_see_a_sharp_call_of_self_a_case_and_a_local_as_the_php_that_means_it()
-> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    const RESULT: (&str, &str) = (
        "src/PaymentResult.sharp",
        "namespace Demo;\n\npublic enum PaymentResult\n{\n    case Paid(string transactionId);\n    case Declined(string reason);\n}\n",
    );
    const SHARP: (&str, &str) = (
        "src/Report.sharp",
        "namespace Demo;\n\nclass Report\n{\n    public static int cap(int x) => x;\n\n    public static int total()\n    {\n        const twice = (int x) => x * 2;\n        return Self.cap(3) + twice(1);\n    }\n\n    public static PaymentResult decline() => PaymentResult.Declined(reason: \"expired\");\n}\n",
    );
    const PHP: (&str, &str) = (
        "src/Report.php",
        "<?php\n\nnamespace Demo;\n\nclass Report\n{\n    public static function cap(int $x): int\n    {\n        return $x;\n    }\n\n    public static function total(): int\n    {\n        $twice = fn (int $x): int => $x * 2;\n        return static::cap(3) + $twice(1);\n    }\n\n    public static function decline(): PaymentResult\n    {\n        return new PaymentResult\\Declined(reason: 'expired');\n    }\n}\n",
    );

    for files in [&[RESULT, SHARP][..], &[RESULT, PHP]] {
        let methods = analyze_files_with_fixture(files, &["MAGO_INVOCATION_CALL_KINDS"])?;
        let functions = analyze_files_with_fixture(files, &["MAGO_INVOCATION_FUNCTION_CALL_KINDS"])?;

        assert_eq!(methods.invocations, ["call-kind-cap-static"], "{files:?}");
        assert_eq!(functions.invocations, ["call-kind-unnamed-function"], "{files:?}");
    }

    Ok(())
}

/// A host's node hooks read each PHP# call form, its type arguments apart from its arguments: an instance call
/// `calc.wrap<int>(1)`, a null-safe one `maybe?.wrap<int>(2)`, a static one `Calc.pair<int>(3)`, a call on a call
/// `Calc.pair<int>(3).first<int>()`, a null-safe call of a function value `doubler?.(4)`, and `new Box<int>(5)`, which no
/// SDK view reads. Each passes its type arguments to the call as written, where 0.6.10's view of a call that passes any
/// refused it with "A call node has no argument list." and stopped the analysis of the file.
#[test]
fn node_hooks_read_each_sharp_call_form_and_its_type_arguments() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    const BOX: (&str, &str) =
        ("src/Box.sharp", "namespace Demo;\n\npublic class Box<T>\n{\n    public Box(public T item { get; }) { }\n}\n");
    const CALC: (&str, &str) = (
        "src/Calc.sharp",
        "namespace Demo;\n\npublic class Calc\n{\n    public int wrap<TItem>(TItem item) => 1;\n\n    public static Calc pair<TItem>(TItem item) => new Calc();\n\n    public TItem? first<TItem>() => null;\n}\n",
    );
    const REPORT: (&str, &str) = (
        "src/Report.sharp",
        "namespace Demo;\n\npublic class Report\n{\n    public static int total(Calc calc, Calc? maybe, Function<int(int)>? doubler)\n    {\n        const wrapped = calc.wrap<int>(1);\n        const maybeWrapped = maybe?.wrap<int>(2);\n        const first = Calc.pair<int>(3).first<int>();\n        const doubled = doubler?.(4);\n        const box = new Box<int>(5);\n        return wrapped + (maybeWrapped ?? 0) + (first ?? 0) + (doubled ?? 0) + box.item;\n    }\n}\n",
    );

    let observation = analyze_files_with_fixture(&[BOX, CALC, REPORT], &["MAGO_INVOCATION_EVERY_CALL_KIND"])?;

    assert_eq!(observation.issues, []);
    assert_eq!(
        observation.invocations,
        [
            "call-kind-wrap-instance",
            "type-arguments-wrap<int>(1)",
            "call-kind-wrap-instance",
            "type-arguments-wrap<int>(1)",
            "call-kind-first-instance",
            "receiver-call-kind-pair-static",
            "type-arguments-first<int>(0)",
            "call-kind-pair-static",
            "type-arguments-pair<int>(1)",
            "call-kind-unnamed-function",
            "Instantiation-hook-new Box<int>(5)",
            "Instantiation-hook-new Calc()",
        ]
    );

    Ok(())
}

/// `Pdf.render(…)` after `import Vendor.Pdf;` calls the plain PHP function `Vendor\Pdf\render`, spec section 23, where
/// no class is named `Vendor\Pdf`, as `render(…)` does in PHP after `use function Vendor\Pdf\render;`. A host reads it
/// as that function's call, named at `render`, as Roslyn's semantic model gives a call's symbol.
#[test]
fn node_hooks_see_a_sharp_call_of_a_namespace_function_as_a_call_of_that_function()
-> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    const PDF: (&str, &str) = (
        "src/pdf.php",
        "<?php\n\nnamespace Vendor\\Pdf;\n\nfunction render(string $html): string\n{\n    return $html;\n}\n",
    );
    const SHARP: (&str, &str) = (
        "src/Report.sharp",
        "namespace Demo;\n\nimport Vendor.Pdf;\n\nclass Report\n{\n    public static string total() => Pdf.render(\"x\");\n}\n",
    );
    const PHP: (&str, &str) = (
        "src/Report.php",
        "<?php\n\nnamespace Demo;\n\nuse function Vendor\\Pdf\\render;\n\nclass Report\n{\n    public static function total(): string\n    {\n        return render('x');\n    }\n}\n",
    );

    let named = analyze_files_with_fixture(
        &[PDF, SHARP],
        &["MAGO_INVOCATION_FUNCTION_CALL_KINDS", "MAGO_INVOCATION_FUNCTION_NAMES"],
    )?;
    assert_eq!(named.issues, []);
    assert_eq!(named.invocations, ["call-kind-render-function", "function-Vendor\\Pdf\\render"]);

    for files in [&[PDF, SHARP][..], &[PDF, PHP]] {
        let functions = analyze_files_with_fixture(files, &["MAGO_INVOCATION_FUNCTION_CALL_KINDS"])?;
        let methods = analyze_files_with_fixture(files, &["MAGO_INVOCATION_CALL_KINDS"])?;

        assert_eq!(functions.invocations, ["call-kind-render-function"], "{files:?}");
        assert_eq!(methods.invocations, [] as [&str; 0], "{files:?}");
    }

    Ok(())
}

/// A method search names each attribute as the hook writes it, and the host keys it as the codebase does, spec section
/// 24: the PHP# `Marked` by its exact name, and the plain PHP `Tagged` in any case. So a search for either finds the
/// method that carries it.
#[test]
fn a_method_search_finds_a_method_by_a_sharp_attribute_as_by_a_plain_one() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    // The analysis here runs without PHP's stubs, so neither class is marked `Attribute`, and each use is reported as
    // one: a method search reads the name of each attribute a method carries, whatever class it names.
    const MARKED: (&str, &str) = ("src/Probe/Marked.sharp", "namespace Probe;\n\npublic class Marked\n{\n}\n");
    const TAGGED: (&str, &str) = ("src/Probe/Tagged.php", "<?php\n\nnamespace Probe;\n\nfinal class Tagged\n{\n}\n");
    const HOLDER: (&str, &str) = (
        "src/Probe/Holder.php",
        "<?php\n\nnamespace Probe;\n\nfinal class Holder\n{\n    #[Marked]\n    public function markedMethod(): void\n    {\n    }\n\n    #[Tagged]\n    public function taggedMethod(): void\n    {\n        $this->markedMethod();\n    }\n}\n",
    );

    let observation = analyze_files_with_fixture(&[MARKED, TAGGED, HOLDER], &["MAGO_INVOCATION_ATTRIBUTED_METHODS"])?;

    assert_eq!(
        observation.issues.iter().map(|(code, _)| code.as_deref()).collect::<Vec<_>>(),
        [Some("class-not-marked-as-attribute"); 2]
    );
    assert_eq!(
        observation.invocations,
        ["attributed-Probe\\Marked-markedMethod", "attributed-Probe\\Tagged-taggedMethod"]
    );

    Ok(())
}

#[test]
fn a_hook_that_targets_static_calls_receives_a_sharp_static_call() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    const PHP: &str = "<?php\n\nnamespace Demo;\n\nclass Calc\n{\n    public static function make(Calc $calc): Calc\n    {\n        return $calc;\n    }\n\n    public function add(int $value): int\n    {\n        return $value;\n    }\n}\n\nclass Report\n{\n    public static function total(Calc $calc): int\n    {\n        return Calc::make($calc)->add(1);\n    }\n}\n";

    for files in [&[CALC, REPORT][..], &[("src/Report.php", PHP)]] {
        let observation = analyze_files_with_fixture(files, &["MAGO_INVOCATION_STATIC_CALL_KINDS"])?;

        assert_eq!(observation.issues, [], "{files:?}");
        assert_eq!(observation.invocations, ["call-kind-make-static"], "{files:?}");
    }

    Ok(())
}

#[test]
fn a_method_call_hook_receives_a_sharp_static_call() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    const SHARP: [(&str, &str); 2] = [
        (
            "src/Calc.sharp",
            "namespace Demo;\n\nclass Calc\n{\n    public static Calc make(Calc calc)\n    {\n        return calc;\n    }\n}\n",
        ),
        (
            "src/Report.sharp",
            "namespace Demo;\n\nclass Report\n{\n    public static Calc total(Calc calc)\n    {\n        return Calc.make(calc);\n    }\n}\n",
        ),
    ];
    const PHP: &str = "<?php\n\nnamespace Demo;\n\nclass Calc\n{\n    public static function make(Calc $calc): Calc\n    {\n        return $calc;\n    }\n}\n\nclass Report\n{\n    public static function total(Calc $calc): Calc\n    {\n        return Calc::make($calc);\n    }\n}\n";

    for files in [&SHARP[..], &[("src/Report.php", PHP)]] {
        let observation = analyze_files_with_fixture(files, &["MAGO_INVOCATION_METHOD_CALL_HOOK"])?;

        assert_eq!(observation.issues, [], "{files:?}");
        assert_eq!(observation.invocations, ["method-call-hook-make-static"], "{files:?}");
    }

    Ok(())
}

#[test]
fn hooks_without_the_subtree_split_php_method_and_static_calls() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    const PHP: &str = "<?php\n\nnamespace Demo;\n\nclass Calc\n{\n    public static function make(Calc $calc): Calc\n    {\n        return $calc;\n    }\n\n    public function add(int $value): int\n    {\n        return $value;\n    }\n}\n\nclass Report\n{\n    public static function total(Calc $calc): int\n    {\n        return Calc::make($calc)->add(1);\n    }\n}\n";

    let observation = analyze_with_fixture("src/Report.php", PHP, &["MAGO_INVOCATION_BARE_CALL_HOOKS"])?;

    assert_eq!(observation.issues, []);
    assert_eq!(
        observation.invocations,
        ["MethodCall-hook-Calc::make($calc)->add(1)", "StaticMethodCall-hook-Calc::make($calc)"]
    );

    Ok(())
}

#[test]
fn hooks_without_the_subtree_split_sharp_method_and_static_calls() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    let observation = analyze_files_with_fixture(&[CALC, REPORT], &["MAGO_INVOCATION_BARE_CALL_HOOKS"])?;

    assert_eq!(observation.issues, []);
    assert_eq!(
        observation.invocations,
        [
            "MethodCall-hook-Calc.make(calc).add(1)",
            "MethodCall-hook-Calc.make(calc)",
            "StaticMethodCall-hook-Calc.make(calc)"
        ]
    );

    Ok(())
}

/// A provider of the PHP# `Demo\Box::total` provides only `total`, and a hook of `Demo\Box::Total` hooks only `Total`:
/// the twins are two methods, spec section 24.
#[test]
fn a_provider_and_a_hook_of_a_sharp_method_reach_it_and_not_its_twin() -> Result<(), Box<dyn std::error::Error>> {
    if !php_sdk_is_available() {
        return Ok(());
    }

    const BOX: (&str, &str) = (
        "src/Box.sharp",
        "namespace Demo;\n\npublic class Box\n{\n    public int total() => 1;\n\n    public int Total() => 2;\n}\n",
    );
    const SHELF: (&str, &str) = (
        "src/Shelf.sharp",
        "namespace Demo;\n\npublic class Shelf\n{\n    public static int count(Box box) => box.total() + box.Total();\n}\n",
    );

    let observation =
        analyze_files_with_fixture(&[BOX, SHELF], &["MAGO_INVOCATION_FUNCTION_ONLY", "MAGO_INVOCATION_TWINS"])?;

    assert_eq!(
        observation.issues,
        [(Some("method-name-case".to_owned()), "Method name `Total` should be in camel case.".to_owned())]
    );
    assert_eq!(observation.invocations, ["twin-provided-total", "twin-hook-box.Total()"]);

    Ok(())
}

/// Analyzes `source`, saved as `file`, with the invocation fixture worker. Each name in `switches` is a fixture
/// environment variable set to `1`.
fn analyze_with_fixture(
    file: &'static str,
    source: &str,
    switches: &[&str],
) -> Result<AnalysisObservation, Box<dyn std::error::Error>> {
    analyze_files_with_fixture(&[(file, source)], switches)
}

/// Analyzes `files`, each a path and its source, as one project with the invocation fixture worker, as
/// [`analyze_with_fixture`] does one file. A `.sharp` file declares one type, spec section 27, so a PHP# project of two
/// classes is two files.
fn analyze_files_with_fixture(
    files: &[(&'static str, &str)],
    switches: &[&str],
) -> Result<AnalysisObservation, Box<dyn std::error::Error>> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temporary = tempfile::tempdir()?;
    let audit = temporary.path().join("invocations.txt");
    std::fs::write(&audit, [])?;
    let mut command = WorkerCommand::new("php")
        .with_argument(repository.join("composer/tests/Sdk/Fixtures/analyzer-invocation-worker.php"))
        .with_current_directory(repository)
        .with_environment("MAGO_INVOCATION_AUDIT_LOG", &audit);
    for switch in switches {
        command = command.with_environment(switch, "1");
    }
    let pool = WorkerPool::spawn(command, NonZeroUsize::MIN, WorkerPoolOptions::default())?;
    let external = ExternalAnalyzer::initialize([Arc::new(pool)], PHPVersion::PHP85, &[], false)?;
    let mut registry = PluginRegistry::with_library_providers();
    registry.set_external_analyzer(Arc::new(ExternalAnalyzerHandle::ready(external)));

    let configuration = common::database_configuration("/invocation-proof", vec![Cow::Borrowed(b"src")]);
    let mut database = Database::new(configuration);
    for (file, source) in files {
        database.add(File::new(
            Cow::Borrowed(file.as_bytes()),
            FileType::Host,
            None,
            Cow::Owned(source.as_bytes().to_vec()),
        ));
    }

    let mut settings = Settings::new(PHPVersion::PHP85);
    settings.find_unused_parameters = false;
    settings.check_property_initialization = true;
    let mut service = IncrementalAnalysisService::new(
        database.read_only(),
        Default::default,
        settings,
        ParserSettings::default(),
        Arc::new(registry),
    );
    let result = service.analyze()?;
    let issues = result.issues.iter().map(|issue| (issue.code.clone(), issue.message.clone())).collect::<Vec<_>>();
    let invocations = std::fs::read_to_string(audit)?.lines().map(str::to_owned).collect::<Vec<_>>();

    Ok(AnalysisObservation { issues, invocations })
}

fn php_sdk_is_available() -> bool {
    common::php_sdk_is_available(Path::new(env!("CARGO_MANIFEST_DIR")), "the external analyzer invocation tests")
}

struct AnalysisObservation {
    issues: Vec<(Option<String>, String)>,
    invocations: Vec<String>,
}
