use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_fingerprint::FingerprintOptions;
use mago_fingerprint::Fingerprintable;

fn get_fingerprint(code: &'static [u8]) -> u64 {
    fingerprint_of(b"test.php", code)
}

/// The fingerprint of `code` as the file `name`, whose extension picks its dialect.
fn fingerprint_of(name: &'static [u8], code: &'static [u8]) -> u64 {
    let arena = LocalArena::new();
    let file = File::ephemeral(name.into(), code.into());
    let program = mago_syntax::parser::parse_file(&arena, &file);
    assert!(!program.has_errors(), "Parse failed: {:?}", program.errors);

    let resolved_names = mago_names::resolver::NameResolver::new(&arena).resolve(program);
    let options = FingerprintOptions::default();

    use foldhash::fast::FixedState;
    use std::hash::BuildHasher;
    use std::hash::Hasher;
    let mut hasher = FixedState::default().build_hasher();
    for statement in &program.statements {
        statement.fingerprint_with_hasher(&mut hasher, &resolved_names, &options);
    }
    hasher.finish()
}

#[test]
fn test_parameter_name_change() {
    let sig1 = get_fingerprint(b"<?php function foo($x) {}");
    let sig2 = get_fingerprint(b"<?php function foo($y) {}");

    assert_ne!(sig1, sig2, "Parameter name change should change fingerprint");
}

#[test]
fn test_parameter_addition() {
    let sig1 = get_fingerprint(b"<?php function foo($x) {}");
    let sig2 = get_fingerprint(b"<?php function foo($x, $y) {}");

    assert_ne!(sig1, sig2, "Adding parameter should change fingerprint");
}

#[test]
fn test_parameter_removal() {
    let sig1 = get_fingerprint(b"<?php function foo($x, $y) {}");
    let sig2 = get_fingerprint(b"<?php function foo($x) {}");

    assert_ne!(sig1, sig2, "Removing parameter should change fingerprint");
}

#[test]
fn test_parameter_order_change() {
    let sig1 = get_fingerprint(b"<?php function foo($x, $y) {}");
    let sig2 = get_fingerprint(b"<?php function foo($y, $x) {}");

    assert_ne!(sig1, sig2, "Parameter order should change fingerprint");
}

#[test]
fn test_parameter_default_value_addition() {
    let sig1 = get_fingerprint(b"<?php function foo($x) {}");
    let sig2 = get_fingerprint(b"<?php function foo($x = 1) {}");

    assert_ne!(sig1, sig2, "Adding default value should change fingerprint");
}

#[test]
fn test_parameter_default_value_change() {
    let sig1 = get_fingerprint(b"<?php function foo($x = 1) {}");
    let sig2 = get_fingerprint(b"<?php function foo($x = 2) {}");

    assert_ne!(sig1, sig2, "Changing default value should change fingerprint");
}

#[test]
fn test_parameter_type_addition() {
    let sig1 = get_fingerprint(b"<?php function foo($x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(int $x) {}");

    assert_ne!(sig1, sig2, "Adding parameter type should change fingerprint");
}

#[test]
fn test_parameter_type_change() {
    let sig1 = get_fingerprint(b"<?php function foo(int $x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(string $x) {}");

    assert_ne!(sig1, sig2, "Parameter type change should change fingerprint");
}

#[test]
fn test_parameter_type_removal() {
    let sig1 = get_fingerprint(b"<?php function foo(int $x) {}");
    let sig2 = get_fingerprint(b"<?php function foo($x) {}");

    assert_ne!(sig1, sig2, "Removing parameter type should change fingerprint");
}

#[test]
fn test_parameter_nullable_type() {
    let sig1 = get_fingerprint(b"<?php function foo(string $x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(?string $x) {}");

    assert_ne!(sig1, sig2, "Making parameter nullable should change fingerprint");
}

#[test]
fn test_parameter_union_type() {
    let sig1 = get_fingerprint(b"<?php function foo(string $x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(string|int $x) {}");

    assert_ne!(sig1, sig2, "Adding union type should change fingerprint");
}

#[test]
fn test_parameter_intersection_type() {
    let sig1 = get_fingerprint(b"<?php function foo(A $x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(A&B $x) {}");

    assert_ne!(sig1, sig2, "Adding intersection type should change fingerprint");
}

#[test]
fn test_parameter_array_type() {
    let sig1 = get_fingerprint(b"<?php function foo($x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(array $x) {}");

    assert_ne!(sig1, sig2, "Adding array type should change fingerprint");
}

#[test]
fn test_parameter_callable_type() {
    let sig1 = get_fingerprint(b"<?php function foo($x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(callable $x) {}");

    assert_ne!(sig1, sig2, "Adding callable type should change fingerprint");
}

#[test]
fn test_return_type_addition() {
    let sig1 = get_fingerprint(b"<?php function foo() {}");
    let sig2 = get_fingerprint(b"<?php function foo(): int {}");

    assert_ne!(sig1, sig2, "Adding return type should change fingerprint");
}

#[test]
fn test_return_type_change() {
    let sig1 = get_fingerprint(b"<?php function foo(): int {}");
    let sig2 = get_fingerprint(b"<?php function foo(): string {}");

    assert_ne!(sig1, sig2, "Return type change should change fingerprint");
}

#[test]
fn test_return_type_removal() {
    let sig1 = get_fingerprint(b"<?php function foo(): int {}");
    let sig2 = get_fingerprint(b"<?php function foo() {}");

    assert_ne!(sig1, sig2, "Removing return type should change fingerprint");
}

#[test]
fn test_return_type_nullable() {
    let sig1 = get_fingerprint(b"<?php function foo(): string {}");
    let sig2 = get_fingerprint(b"<?php function foo(): ?string {}");

    assert_ne!(sig1, sig2, "Making return type nullable should change fingerprint");
}

#[test]
fn test_return_type_void() {
    let sig1 = get_fingerprint(b"<?php function foo() {}");
    let sig2 = get_fingerprint(b"<?php function foo(): void {}");

    assert_ne!(sig1, sig2, "Adding void return type should change fingerprint");
}

#[test]
fn test_return_type_never() {
    let sig1 = get_fingerprint(b"<?php function foo(): void {}");
    let sig2 = get_fingerprint(b"<?php function foo(): never {}");

    assert_ne!(sig1, sig2, "Changing to never return type should change fingerprint");
}

#[test]
fn test_return_type_union() {
    let sig1 = get_fingerprint(b"<?php function foo(): int {}");
    let sig2 = get_fingerprint(b"<?php function foo(): int|string {}");

    assert_ne!(sig1, sig2, "Adding union return type should change fingerprint");
}

#[test]
fn test_return_type_mixed() {
    let sig1 = get_fingerprint(b"<?php function foo() {}");
    let sig2 = get_fingerprint(b"<?php function foo(): mixed {}");

    assert_ne!(sig1, sig2, "Adding mixed return type should change fingerprint");
}

#[test]
fn test_parameter_by_ref_addition() {
    let sig1 = get_fingerprint(b"<?php function foo($x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(&$x) {}");

    assert_ne!(sig1, sig2, "Making parameter by-ref should change fingerprint");
}

#[test]
fn test_parameter_by_ref_removal() {
    let sig1 = get_fingerprint(b"<?php function foo(&$x) {}");
    let sig2 = get_fingerprint(b"<?php function foo($x) {}");

    assert_ne!(sig1, sig2, "Removing by-ref should change fingerprint");
}

#[test]
fn test_multiple_by_ref_parameters() {
    let sig1 = get_fingerprint(b"<?php function foo(&$x, $y) {}");
    let sig2 = get_fingerprint(b"<?php function foo($x, &$y) {}");

    assert_ne!(sig1, sig2, "By-ref position should change fingerprint");
}

#[test]
fn test_by_ref_with_type() {
    let sig1 = get_fingerprint(b"<?php function foo(int $x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(int &$x) {}");

    assert_ne!(sig1, sig2, "By-ref with type should change fingerprint");
}

#[test]
fn test_variadic_parameter_addition() {
    let sig1 = get_fingerprint(b"<?php function foo($x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(...$x) {}");

    assert_ne!(sig1, sig2, "Making parameter variadic should change fingerprint");
}

#[test]
fn test_variadic_with_type() {
    let sig1 = get_fingerprint(b"<?php function foo(...$x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(int ...$x) {}");

    assert_ne!(sig1, sig2, "Adding type to variadic should change fingerprint");
}

#[test]
fn test_variadic_by_ref() {
    let sig1 = get_fingerprint(b"<?php function foo(...$x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(&...$x) {}");

    assert_ne!(sig1, sig2, "Making variadic by-ref should change fingerprint");
}

#[test]
fn test_method_visibility_public_to_private() {
    let sig1 = get_fingerprint(b"<?php class A { public function foo() {} }");
    let sig2 = get_fingerprint(b"<?php class A { private function foo() {} }");

    assert_ne!(sig1, sig2, "Changing method visibility should change fingerprint");
}

#[test]
fn test_method_visibility_public_to_protected() {
    let sig1 = get_fingerprint(b"<?php class A { public function foo() {} }");
    let sig2 = get_fingerprint(b"<?php class A { protected function foo() {} }");

    assert_ne!(sig1, sig2, "Changing method visibility should change fingerprint");
}

#[test]
fn test_method_static_addition() {
    let sig1 = get_fingerprint(b"<?php class A { public function foo() {} }");
    let sig2 = get_fingerprint(b"<?php class A { public static function foo() {} }");

    assert_ne!(sig1, sig2, "Making method static should change fingerprint");
}

#[test]
fn test_method_static_removal() {
    let sig1 = get_fingerprint(b"<?php class A { public static function foo() {} }");
    let sig2 = get_fingerprint(b"<?php class A { public function foo() {} }");

    assert_ne!(sig1, sig2, "Removing static should change fingerprint");
}

#[test]
fn test_method_abstract_addition() {
    let sig1 = get_fingerprint(b"<?php abstract class A { public function foo() {} }");
    let sig2 = get_fingerprint(b"<?php abstract class A { abstract public function foo(); }");

    assert_ne!(sig1, sig2, "Making method abstract should change fingerprint");
}

#[test]
fn test_method_final_addition() {
    let sig1 = get_fingerprint(b"<?php class A { public function foo() {} }");
    let sig2 = get_fingerprint(b"<?php class A { final public function foo() {} }");

    assert_ne!(sig1, sig2, "Making method final should change fingerprint");
}

#[test]
fn test_property_type_addition() {
    let sig1 = get_fingerprint(b"<?php class A { public $x; }");
    let sig2 = get_fingerprint(b"<?php class A { public int $x; }");

    assert_ne!(sig1, sig2, "Adding property type should change fingerprint");
}

#[test]
fn test_property_type_change() {
    let sig1 = get_fingerprint(b"<?php class A { public int $x; }");
    let sig2 = get_fingerprint(b"<?php class A { public string $x; }");

    assert_ne!(sig1, sig2, "Property type change should change fingerprint");
}

#[test]
fn test_property_nullable_type() {
    let sig1 = get_fingerprint(b"<?php class A { public string $x; }");
    let sig2 = get_fingerprint(b"<?php class A { public ?string $x; }");

    assert_ne!(sig1, sig2, "Making property nullable should change fingerprint");
}

#[test]
fn test_property_readonly_addition() {
    let sig1 = get_fingerprint(b"<?php class A { public string $x; }");
    let sig2 = get_fingerprint(b"<?php class A { public readonly string $x; }");

    assert_ne!(sig1, sig2, "Making property readonly should change fingerprint");
}

#[test]
fn test_property_static_addition() {
    let sig1 = get_fingerprint(b"<?php class A { public $x; }");
    let sig2 = get_fingerprint(b"<?php class A { public static $x; }");

    assert_ne!(sig1, sig2, "Making property static should change fingerprint");
}

#[test]
fn test_property_visibility_change() {
    let sig1 = get_fingerprint(b"<?php class A { public $x; }");
    let sig2 = get_fingerprint(b"<?php class A { private $x; }");

    assert_ne!(sig1, sig2, "Changing property visibility should change fingerprint");
}

#[test]
fn test_property_default_value() {
    let sig1 = get_fingerprint(b"<?php class A { public $x; }");
    let sig2 = get_fingerprint(b"<?php class A { public $x = 1; }");

    assert_ne!(sig1, sig2, "Adding property default value should change fingerprint");
}

#[test]
fn test_constructor_property_promotion() {
    let sig1 = get_fingerprint(b"<?php class A { public function __construct($x) {} }");
    let sig2 = get_fingerprint(b"<?php class A { public function __construct(public $x) {} }");

    assert_ne!(sig1, sig2, "Property promotion should change fingerprint");
}

#[test]
fn test_constructor_property_promotion_type() {
    let sig1 = get_fingerprint(b"<?php class A { public function __construct(public $x) {} }");
    let sig2 = get_fingerprint(b"<?php class A { public function __construct(public int $x) {} }");

    assert_ne!(sig1, sig2, "Promoted property type should change fingerprint");
}

#[test]
fn test_constructor_property_promotion_readonly() {
    let sig1 = get_fingerprint(b"<?php class A { public function __construct(public $x) {} }");
    let sig2 = get_fingerprint(b"<?php class A { public function __construct(public readonly $x) {} }");

    assert_ne!(sig1, sig2, "Promoted readonly property should change fingerprint");
}

#[test]
fn test_constructor_property_promotion_visibility() {
    let sig1 = get_fingerprint(b"<?php class A { public function __construct(public $x) {} }");
    let sig2 = get_fingerprint(b"<?php class A { public function __construct(private $x) {} }");

    assert_ne!(sig1, sig2, "Promoted property visibility should change fingerprint");
}

#[test]
fn test_all_parameter_features() {
    let sig1 = get_fingerprint(b"<?php function foo($x) {}");
    let sig2 = get_fingerprint(b"<?php function foo(int|string &...$x = []) {}");

    assert_ne!(sig1, sig2, "Combined parameter features should change fingerprint");
}

#[test]
fn test_mixed_parameter_types() {
    let sig1 = get_fingerprint(b"<?php function foo($a, $b, $c) {}");
    let sig2 = get_fingerprint(b"<?php function foo(int $a, string $b, array $c) {}");

    assert_ne!(sig1, sig2, "Adding types to multiple params should change fingerprint");
}

#[test]
fn test_mixed_by_ref_parameters() {
    let sig1 = get_fingerprint(b"<?php function foo($a, $b, $c) {}");
    let sig2 = get_fingerprint(b"<?php function foo(&$a, $b, &$c) {}");

    assert_ne!(sig1, sig2, "Making some params by-ref should change fingerprint");
}

#[test]
fn test_mixed_default_values() {
    let sig1 = get_fingerprint(b"<?php function foo($a, $b, $c) {}");
    let sig2 = get_fingerprint(b"<?php function foo($a, $b = 1, $c = 2) {}");

    assert_ne!(sig1, sig2, "Adding some default values should change fingerprint");
}

#[test]
fn test_class_abstract_addition() {
    let sig1 = get_fingerprint(b"<?php class A {}");
    let sig2 = get_fingerprint(b"<?php abstract class A {}");

    assert_ne!(sig1, sig2, "Making class abstract should change fingerprint");
}

#[test]
fn test_class_final_addition() {
    let sig1 = get_fingerprint(b"<?php class A {}");
    let sig2 = get_fingerprint(b"<?php final class A {}");

    assert_ne!(sig1, sig2, "Making class final should change fingerprint");
}

#[test]
fn test_class_readonly_addition() {
    let sig1 = get_fingerprint(b"<?php class A {}");
    let sig2 = get_fingerprint(b"<?php readonly class A {}");

    assert_ne!(sig1, sig2, "Making class readonly should change fingerprint");
}

#[test]
fn test_void_vs_no_return_type() {
    let no_type = get_fingerprint(b"<?php function foo() {}");
    let void_type = get_fingerprint(b"<?php function foo(): void {}");

    assert_ne!(no_type, void_type, "Void return type should differ from no type");
}

#[test]
fn test_null_vs_nullable() {
    let nullable = get_fingerprint(b"<?php function foo(?int $x) {}");
    let union = get_fingerprint(b"<?php function foo(int|null $x) {}");

    assert_ne!(nullable, union, "?int and int|null are syntactically different");
}

#[test]
fn test_parameter_names_only_differ() {
    let sig1 = get_fingerprint(b"<?php function foo(int $x, string $y) {}");
    let sig2 = get_fingerprint(b"<?php function foo(int $a, string $b) {}");

    assert_ne!(sig1, sig2, "Parameter names should matter even with same types");
}

#[test]
fn test_property_name_order() {
    let sig1 = get_fingerprint(b"<?php class A { public $x; public $y; }");
    let sig2 = get_fingerprint(b"<?php class A { public $y; public $x; }");

    assert_ne!(sig1, sig2, "Property order should matter");
}

#[test]
fn test_method_order() {
    let sig1 = get_fingerprint(b"<?php class A { function foo() {} function bar() {} }");
    let sig2 = get_fingerprint(b"<?php class A { function bar() {} function foo() {} }");

    assert_ne!(sig1, sig2, "Method order should matter");
}

#[test]
fn a_function_type_effect_change_changes_the_fingerprint() {
    let field = |code| fingerprint_of(b"src/Checkout.sharp", code);
    let pure = field(b"class Checkout\n{\n    private Function<Charge(Cart)>? charge = null;\n}\n");
    let http = field(b"class Checkout\n{\n    private Function<Charge(Cart) uses Http>? charge = null;\n}\n");
    let mail = field(b"class Checkout\n{\n    private Function<Charge(Cart) uses Mail>? charge = null;\n}\n");

    assert_ne!(pure, http, "Adding an effect should change the fingerprint");
    assert_ne!(http, mail, "Changing an effect should change the fingerprint");
}

#[test]
fn a_super_argument_change_changes_the_fingerprint() {
    let constructor = |code| fingerprint_of(b"src/Order.sharp", code);
    let row = constructor(b"class Order : Entity\n{\n    public Order(Row row, Row other) : super(row) { }\n}\n");
    let other = constructor(b"class Order : Entity\n{\n    public Order(Row row, Row other) : super(other) { }\n}\n");

    assert_ne!(row, other, "Changing a `: super(…)` argument should change the fingerprint");
}

#[test]
fn an_emitted_event_change_changes_the_fingerprint() {
    let order = |code| fingerprint_of(b"src/Order.sharp", code);
    let paid = order(b"class Order\n{\n    public void run()\n    {\n        emit new OrderPaid(1);\n    }\n}\n");
    let refunded =
        order(b"class Order\n{\n    public void run()\n    {\n        emit new OrderRefunded(1);\n    }\n}\n");

    assert_ne!(paid, refunded, "Changing what `emit` raises should change the fingerprint");
}

#[test]
fn an_on_clause_change_changes_the_fingerprint() {
    let listener = |code| fingerprint_of(b"src/Notices.sharp", code);
    let plain = listener(b"class Notices\n{\n    public void send(OrderEvent e) { }\n}\n");
    let paid = listener(b"class Notices\n{\n    public void send(OrderEvent e) on OrderPaid { }\n}\n");
    let refunded =
        listener(b"class Notices\n{\n    public void send(OrderEvent e) on OrderPaid, OrderRefunded { }\n}\n");

    assert_ne!(plain, paid, "Adding an `on` clause should change the fingerprint");
    assert_ne!(paid, refunded, "Listing another event should change the fingerprint");
}

#[test]
fn a_method_uses_change_changes_the_fingerprint() {
    let rates = |code| fingerprint_of(b"src/Rates.sharp", code);
    let pure = rates(b"interface Rates\n{\n    int rate(int id);\n}\n");
    let http = rates(b"interface Rates\n{\n    int rate(int id) uses Http;\n}\n");
    let mail = rates(b"interface Rates\n{\n    int rate(int id) uses Mail;\n}\n");

    assert_ne!(pure, http, "Adding a `uses` should change the fingerprint");
    assert_ne!(http, mail, "Changing an effect in `uses` should change the fingerprint");
}

#[test]
fn an_extension_block_type_parameter_change_changes_the_fingerprint() {
    let lists = |code| fingerprint_of(b"src/Lists.sharp", code);
    let open = lists(b"public static class Lists\n{\n    extension<T>(List<T> items)\n    {\n        public int size() => 0;\n    }\n}\n");
    let bounded =
        lists(b"public static class Lists\n{\n    extension<T : Order>(List<T> items)\n    {\n        public int size() => 0;\n    }\n}\n");

    assert_ne!(open, bounded, "Bounding an extension block's type parameter should change the fingerprint");
}

#[test]
fn a_foreign_modifier_change_changes_the_fingerprint() {
    let payments = |code| fingerprint_of(b"src/Payments.sharp", code);
    let plain = payments(b"public class Payments\n{\n}\n");
    let foreign = payments(b"public foreign class Payments\n{\n}\n");

    assert_ne!(plain, foreign, "Marking a class `foreign` should change the fingerprint");
}

/// Each PHP# form changes the fingerprint when its own part changes, so an edit to it reaches the analysis cache. Each
/// row is a member of a class, before and after the edit.
#[test]
fn an_edit_to_each_php_sharp_form_changes_the_fingerprint() {
    let rows: [(&str, &[u8], &[u8]); 26] = [
        ("an `is` type", b"bool run(Any? v) => v is int;", b"bool run(Any? v) => v is string;"),
        ("an `is` name", b"bool run(Any? v) => v is Order o;", b"bool run(Any? v) => v is Order p;"),
        ("an `as` type", b"Order? run(Any? v) => v as Order;", b"Invoice? run(Any? v) => v as Invoice;"),
        ("a value pattern", b"bool run(Any? v) => v is 200;", b"bool run(Any? v) => v is 300;"),
        ("a comparison pattern", b"bool run(Any? v) => v is < 10;", b"bool run(Any? v) => v is > 10;"),
        ("a `not` pattern", b"bool run(Any? v) => v is int;", b"bool run(Any? v) => v is not int;"),
        ("`and` or `or`", b"bool run(int v) => v is > 1 and < 9;", b"bool run(int v) => v is > 1 or < 9;"),
        (
            "a pattern's grouping",
            b"bool run(int v) => v is (> 1 or < 0) and < 9;",
            b"bool run(int v) => v is > 1 or (< 0 and < 9);",
        ),
        (
            "a properties pattern",
            b"bool run(Any? v) => v is { status: 200 };",
            b"bool run(Any? v) => v is { code: 200 };",
        ),
        (
            "a type pattern's properties",
            b"bool run(Any? v) => v is Order { total: 1 };",
            b"bool run(Any? v) => v is Order { total: 2 };",
        ),
        (
            "a list pattern's rest",
            b"bool run(List<int> v) => v is [int first, ...];",
            b"bool run(List<int> v) => v is [..., int first];",
        ),
        (
            "a named rest's type",
            b"bool run(List<int> v) => v is [int a, ...List<int> rest];",
            b"bool run(List<int> v) => v is [int a, ...List<string> rest];",
        ),
        (
            "a case's fields",
            b"bool run(PaymentResult v) => v is PaymentResult.Paid(string id);",
            b"bool run(PaymentResult v) => v is PaymentResult.Paid(int id);",
        ),
        (
            "a `match` arm",
            b"string run(Any? v) => match (v) { int i => \"a\", default => \"b\" };",
            b"string run(Any? v) => match (v) { string i => \"a\", default => \"b\" };",
        ),
        (
            "a `match` guard",
            b"string run(Any? v) => match (v) { int i => \"a\", default => \"b\" };",
            b"string run(Any? v) => match (v) { int i when i > 0 => \"a\", default => \"b\" };",
        ),
        (
            "a `match` statement's block arm",
            b"void run(Any? v) { match (v) { int i => { this.a(); }, default => { } } }",
            b"void run(Any? v) { match (v) { int i => { this.b(); }, default => { } } }",
        ),
        ("a `typeof`", b"Class<Any> run() => typeof(Order);", b"Class<Any> run() => typeof(Invoice);"),
        ("a `with`", b"Point run(Point p) => p with { x: 1 };", b"Point run(Point p) => p with { x: 2 };"),
        (
            "a class value's type arguments",
            b"int run() => Counter<Order>.count;",
            b"int run() => Counter<Invoice>.count;",
        ),
        (
            "a `for … of` body",
            b"void run(List<int> lines) { for (const line of lines) { this.a(); } }",
            b"void run(List<int> lines) { for (const line of lines) { this.b(); } }",
        ),
        (
            "a local declaration",
            b"int run() { let total = 1; return total; }",
            b"int run() { let total = 2; return total; }",
        ),
        ("a `new`'s type arguments", b"Any run() => new Box<Order>();", b"Any run() => new Box<Invoice>();"),
        ("a `new`'s initializer", b"Point run() => new Point { x: 1 };", b"Point run() => new Point { x: 2 };"),
        (
            "a method call's type arguments",
            b"Any run(string s) => Json.decode<Order>(s);",
            b"Any run(string s) => Json.decode<Invoice>(s);",
        ),
        (
            "a null-safe method call's type arguments",
            b"Any run(Repo? r) => r?.find<Order>(1);",
            b"Any run(Repo? r) => r?.find<Invoice>(1);",
        ),
        (
            "a `?? return`",
            b"int run(int? v) { int n = v ?? return 0; return n; }",
            b"int run(int? v) { int n = v ?? return 1; return n; }",
        ),
    ];

    for (form, before, after) in rows {
        let member = |member: &[u8]| {
            let code = [b"class Shows\n{\n    public ".as_slice(), member, b"\n}\n"].concat();

            fingerprint_of(b"src/Shows.sharp", code.leak())
        };

        assert_ne!(member(before), member(after), "An edit to {form} should change the fingerprint");
    }
}

/// An interface's modifiers, `sealed` among them, change the fingerprint, as an `extern` declaration's effects do.
#[test]
fn a_sealed_interface_and_an_extern_change_the_fingerprint() {
    let shape = |code| fingerprint_of(b"src/Shape.sharp", code);
    let open = shape(b"public interface Shape\n{\n}\n");
    let internal = shape(b"internal interface Shape\n{\n}\n");
    let sealed = shape(b"public sealed interface Shape\n{\n}\n");
    let clock = shape(b"extern Carbon.now uses Clock;\n");
    let http = shape(b"extern Carbon.now uses Http;\n");

    assert_ne!(open, internal, "Making an interface `internal` should change the fingerprint");
    assert_ne!(open, sealed, "Sealing an interface should change the fingerprint");
    assert_ne!(clock, http, "Changing an `extern` declaration's effects should change the fingerprint");
}

/// A generic struct's type parameters change the fingerprint, as a generic class's do: their names, their count and
/// their bounds.
#[test]
fn a_struct_type_parameter_changes_the_fingerprint() {
    let pair = |code| fingerprint_of(b"src/Pair.sharp", code);
    let one = pair(b"public struct Pair<TFirst>\n{\n}\n");

    for (form, edited) in [
        ("a type parameter's name", b"public struct Pair<TSecond>\n{\n}\n".as_slice()),
        ("one more type parameter", b"public struct Pair<TFirst, TSecond>\n{\n}\n"),
        ("a bound", b"public struct Pair<TFirst : Order>\n{\n}\n"),
        ("no type parameter", b"public struct Pair\n{\n}\n"),
    ] {
        assert_ne!(one, pair(edited), "An edit to {form} should change the fingerprint");
    }
}
