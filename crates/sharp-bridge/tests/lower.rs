//! Each test lowers a PHP# snippet and compares the node tree with the `zend_ast` that php-src's
//! `zend_language_parser.y` builds for the equivalent PHP, quoted above each test.
//!
//! A tree prints one node per line: its kind without `SHARP_AST_`, its attr in brackets when it is not 0, a ZVAL's
//! value, and a declaration's name and lines. `null` is a null child.

#![allow(clippy::panic, clippy::expect_used, clippy::use_debug)]

mod common;

use std::borrow::Cow;
use std::fmt::Write;
use std::path::Path;
use std::thread;

use indoc::indoc;

use mago_database::file::File;
use mago_database::file::FileType;
use mago_sharp_bridge::InlineForms;
use mago_sharp_bridge::Unit;
use mago_sharp_bridge::inline_forms;
use mago_sharp_bridge::lower;
use mago_sharp_bridge::sharp_kind;
use mago_sharp_bridge::sharp_node;
use mago_sharp_bridge::sharp_str;
use mago_sharp_bridge::sharp_value;
use mago_sharp_bridge::unit::encode;

/// A file the checker accepted and its lowered unit, or the checker's refusal.
struct Lowered(Result<Unit, Vec<String>>);

impl Lowered {
    fn new(code: &str) -> Self {
        Self::with(code, &[])
    }

    /// Lowers `code` as the file at `path`, which decides whether it is the standard library's.
    fn named(path: &str, code: &str) -> Self {
        Self(common::checked(path, code, &[], lower))
    }

    /// Lowers `code` beside the `library` files, each a path and its code, which declare what `code` uses.
    fn with(code: &str, library: &[(&str, &str)]) -> Self {
        Self(common::checked("src/Report.sharp", code, library, lower))
    }

    /// Lowers `code` beside the `library` files and the standard library files `standard`, inlining the forms the
    /// standard library files give.
    fn inlining(code: &str, library: &[(&str, &str)], standard: &[(&str, &str)]) -> Self {
        let declarations: Vec<(&str, &str)> = library.iter().chain(standard).copied().collect();

        Self(common::checked_inlining("src/Report.sharp", code, &declarations, &common::library_forms(standard), lower))
    }

    fn unit(&self) -> Option<&Unit> {
        self.0.as_ref().ok()
    }

    /// The unit's nodes, none when the checker refused the file.
    fn nodes(&self) -> &[sharp_node] {
        self.unit().map_or(&[], Unit::nodes)
    }

    fn children(&self) -> &[u32] {
        self.unit().map_or(&[], Unit::children)
    }

    fn root(&self) -> u32 {
        self.0.as_ref().unwrap_or_else(|refusal| panic!("the source lowers: {refusal:#?}")).root()
    }

    /// Every refusal, as `line:column severity message`.
    fn diagnostics(&self) -> Vec<String> {
        self.0.as_ref().err().cloned().unwrap_or_default()
    }

    fn tree(&self) -> String {
        assert_eq!(self.diagnostics(), Vec::<String>::new(), "the source lowers");

        self.render(self.root())
    }

    /// The statements of the method `run`.
    fn body(&self) -> String {
        self.render(self.child(self.method_named("run"), 2))
    }

    /// The method PHP declares as `name`.
    fn declared(&self, name: &str) -> String {
        self.render(self.method_named(name))
    }

    fn method_named(&self, name: &str) -> u32 {
        assert_eq!(self.diagnostics(), Vec::<String>::new(), "the source lowers");
        let method = self
            .nodes()
            .iter()
            .position(|node| node.kind == sharp_kind::SHARP_AST_METHOD && self.text(node.text) == name)
            .unwrap_or_else(|| panic!("the source declares `{name}`"));

        method as u32
    }

    fn child(&self, node: u32, index: u32) -> u32 {
        self.children()[(self.nodes()[node as usize].first_child + index) as usize]
    }

    fn render(&self, node: u32) -> String {
        let mut tree = String::new();
        self.render_into(node, 0, &mut tree);

        tree
    }

    fn render_into(&self, index: u32, depth: usize, tree: &mut String) {
        tree.push_str(&"  ".repeat(depth));
        if index == u32::MAX {
            tree.push_str("null\n");

            return;
        }

        let node = &self.nodes()[index as usize];
        tree.push_str(format!("{:?}", node.kind).trim_start_matches("SHARP_AST_"));
        if node.attr != 0 {
            let _ = write!(tree, " [{}]", node.attr);
        }

        match node.kind {
            sharp_kind::SHARP_AST_ZVAL => {
                let _ = match node.value {
                    sharp_value::SHARP_NULL => write!(tree, " null"),
                    sharp_value::SHARP_FALSE => write!(tree, " false"),
                    sharp_value::SHARP_TRUE => write!(tree, " true"),
                    sharp_value::SHARP_LONG => write!(tree, " {}", node.long_value),
                    sharp_value::SHARP_DOUBLE => write!(tree, " {:?}", node.double_value),
                    sharp_value::SHARP_STRING => write!(tree, " {:?}", self.text(node.text)),
                };
            }
            sharp_kind::SHARP_AST_CLASS
            | sharp_kind::SHARP_AST_METHOD
            | sharp_kind::SHARP_AST_PROPERTY_HOOK
            | sharp_kind::SHARP_AST_CLOSURE
            | sharp_kind::SHARP_AST_ARROW_FUNC => {
                let _ = write!(tree, " {:?} @{}-{}", self.text(node.text), node.line, node.end_line);
            }
            _ => {}
        }

        tree.push('\n');
        for child in 0..node.child_count {
            self.render_into(self.child(index, child), depth + 1, tree);
        }
    }

    fn text(&self, text: sharp_str) -> String {
        String::from_utf8_lossy(self.unit().expect("the source lowers").text(text)).into_owned()
    }
}

/// The signature `method` declares `run` with.
const RUN: &str = "int run(int extra)";

/// `Sharp\Text` at the standard library's path, with methods each inlining rule takes or leaves.
const TEXT: (&str, &str) = (
    "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text.sharp",
    "namespace Sharp;\n\npublic class Text\n{\n    public static string shout(string text) => strtoupper(text);\n\n    public static string padded(string text, int width = 8) => str_pad(trim(text), width);\n\n    public static string twice(string text) => str_repeat(text, strlen(text));\n\n    public static string trimmed(string text)\n    {\n        const trimmed = trim(text);\n        return trimmed;\n    }\n\n    public static Text make() => new Text();\n\n    public string label() => \"text\";\n\n    public string wrapped(string format) => sprintf(format, this.label());\n\n    public string secret(string text) => this.hidden(text);\n\n    private string hidden(string text) => strtolower(text);\n\n    public static string quiet(string text) => Text.muted(text);\n\n    protected static string muted(string text) => strtolower(text);\n\n    public virtual string echoed(string text) => this.wrapped(text);\n\n    public static string again(string text) => Self.shout(text);\n}\n",
);

/// `Lib\Loud`, a project file with a method the inlining rule would take in the standard library.
const LOUD: (&str, &str) = (
    "src/Lib/Loud.sharp",
    "namespace Lib;\n\npublic class Loud\n{\n    public static string shout(string text) => strtoupper(text);\n}\n",
);

/// The statements of `run`, declared with `signature` and holding `statements` in a class that imports `Sharp.Text`
/// and `Lib.Loud`, lowered with the inline forms of `TEXT`.
fn inlined_body(signature: &str, statements: &str) -> String {
    let code = format!(
        "namespace App.Tenant;\n\nimport Lib.Loud;\nimport Sharp.Text;\n\nclass Report\n{{\n    public {signature}\n    {{\n{statements}    }}\n}}\n"
    );

    Lowered::inlining(&code, &[LOUD], &[TEXT]).body()
}

/// A file declaring the method `run` with `body`. Its body starts on line 9.
fn method(body: &str) -> String {
    method_with(RUN, body)
}

/// A file declaring a method with `signature` and `body` in the class `Report`. Its body starts on line 9.
fn method_with(signature: &str, body: &str) -> String {
    method_in("Report", signature, body)
}

/// A file declaring a method with `signature` and `body` in the class `header`. Its body starts on line 9.
fn method_in(header: &str, signature: &str, body: &str) -> String {
    format!(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass {header}\n{{\n    public {signature}\n    {{\n{body}    }}\n}}\n"
    )
}

fn body(statements: &str) -> String {
    body_in(RUN, statements, &[])
}

/// The statements of `run`, declared with `signature` and holding `statements`, lowered beside the `library` files.
fn body_in(signature: &str, statements: &str, library: &[(&str, &str)]) -> String {
    Lowered::with(&method_with(signature, statements), library).body()
}

/// The statements of `run`, declared with `signature` and holding `statements`, in `Report : Base`, where `base`
/// declares `App\Tenant\Base` in PHP.
fn child_body(signature: &str, statements: &str, base: &str) -> String {
    Lowered::with(&method_in("Report : Base", signature, statements), &[("src/App/Tenant/Base.php", base)]).body()
}

/// ```php
/// $base = 2; $base = 3;
/// ```
#[test]
fn a_const_local_reassigned_returns_one_compile_error_and_no_nodes() {
    let lowered = Lowered::new(&method("        const base = 2;\n        base = 3;\n        return base;\n"));

    assert_eq!(lowered.diagnostics(), ["10:9 compile error: Cannot assign to `base`: it is declared with `const`."]);
    assert_eq!(lowered.nodes().len(), 0);
    assert_eq!(lowered.children().len(), 0);
}

#[test]
fn php_syntax_returns_a_parse_error_and_no_nodes() {
    let lowered = Lowered::new(&method("        return this->run(1);\n"));

    assert_eq!(lowered.diagnostics(), ["9:20 parse error: `->` is PHP syntax: PHP# writes member access with `.`"]);
    assert_eq!(lowered.nodes().len(), 0);
}

#[test]
fn a_construct_outside_the_slice_returns_its_not_supported_error() {
    let lowered = Lowered::new(&method(
        "        switch (extra) {\n            default: return 1;\n        }\n        return 1;\n",
    ));

    assert_eq!(lowered.diagnostics(), ["9:9 compile error: This statement is not supported yet in PHP#."]);
}

/// The lowering takes only a checked program, so an enum the slice refuses lowers no node of the enum.
#[test]
fn an_enum_the_slice_refuses_returns_its_error_and_no_nodes() {
    let lowered = Lowered::new("enum Status\n{\n    case Active;\n\n    public status() {}\n}\n");

    assert_eq!(
        lowered.diagnostics(),
        ["5:12 compile error: An enum has no constructor: its cases are its only values."]
    );
    assert_eq!(lowered.nodes().len(), 0);
}

#[test]
fn a_method_without_a_body_returns_the_checker_error() {
    let lowered = Lowered::new("class Report\n{\n    public int run();\n}\n");

    assert_eq!(
        lowered.diagnostics(),
        [
            "3:21 compile error: Non-Abstract method `Report.run` must have a concrete body.",
            "1:7 compile error: Class `Report` does not implement the abstract method `run`."
        ]
    );
}

/// Each body nests one chain 100,000 levels deep: a sum, a call chain, a null-safe call chain, a `??` chain and a
/// `??=` chain. With the namespace, the class and the `return`, that is far past the 512 levels the parser allows.
/// The parser builds a sum or a call chain in a loop and finds it too deep at its innermost term, the first one
/// written. It builds `??` and `??=` by recursing to the right and stops at the term 510 levels into the chain.
#[test]
fn a_file_nested_100_000_levels_deep_returns_the_depth_error_and_no_nodes() {
    let bodies = [
        (format!("        return {};\n", vec!["extra"; 100_000].join(" + ")), 16),
        (format!("        return this{};\n", ".total()".repeat(100_000)), 16),
        (format!("        return this{};\n", "?.total()".repeat(100_000)), 16),
        (format!("        return {};\n", vec!["extra"; 100_000].join(" ?? ")), 16 + 509 * "extra ?? ".len()),
        (format!("        return {};\n", vec!["extra"; 100_000].join(" ??= ")), 16 + 509 * "extra ??= ".len()),
    ];

    for (body, column) in bodies {
        let lowered = Lowered::new(&method(&body));

        assert_eq!(
            lowered.diagnostics(),
            [format!("9:{column} parse error: PHP# nests statements, expressions and types at most 512 levels deep.")]
        );
        assert_eq!(lowered.nodes().len(), 0);
    }
}

/// A union nests its types to the right, and each type is a level, so a 100,000-member return type is too deep at
/// its member 509: the namespace, the class and 509 unions are around it.
#[test]
fn a_union_of_100_000_types_returns_the_depth_error_and_no_nodes() {
    let union = (0..100_000).map(|index| format!("A{index}")).collect::<Vec<_>>().join("|");
    let lowered = Lowered::new(&format!(
        "namespace App.Tenant;\n\nclass Report\n{{\n    public {union} run()\n    {{\n        return null;\n    }}\n}}\n"
    ));
    let column = "    public ".len() + union.find("A509|").expect("the union has member 509") + 1;

    assert_eq!(
        lowered.diagnostics(),
        [format!("5:{column} parse error: PHP# nests statements, expressions and types at most 512 levels deep.")]
    );
    assert_eq!(lowered.nodes().len(), 0);
}

/// A 509-term sum or `??` chain in a method nests its innermost term 512 levels deep, the most the checker allows, and
/// so do `this` with a chain of 508 calls, null-safe after the first, and a 510-term sum as a field's initial value. Each is checked on a thread
/// with the 8 MiB stack `mago` gives its smallest worker, then lowers from a stack far smaller than the recursion needs,
/// which proves `ensure_sufficient_stack` guards every recursive path of `lower` whatever stack its caller starts on.
#[test]
fn the_deepest_file_the_checker_accepts_lowers_from_a_small_stack() {
    let codes = [
        method(&format!("        return {};\n", vec!["extra"; 509].join(" + "))),
        method_with("int run(int? extra)", &format!("        return {} ?? 0;\n", vec!["extra"; 508].join(" ?? "))),
        method_in(
            "Report : Base",
            RUN,
            &format!("        this.total(extra){};\n        return extra;\n", "?.total(extra)".repeat(507)),
        ),
        format!(
            "namespace App.Tenant;\n\nclass Report\n{{\n    private int total = {};\n}}\n",
            vec!["1"; 510].join(" + ")
        ),
    ];
    let base = (
        "src/App/Tenant/Base.php",
        "<?php namespace App\\Tenant; class Base { public function total(int $amount): ?static { return $this; } }",
    );

    for code in codes {
        let node_count = thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                common::checked("src/Report.sharp", &code, &[base], |checked| {
                    stacker::grow(128 * 1024, || lower(checked).nodes().len())
                })
            })
            .expect("the thread starts")
            .join()
            .expect("the check and the lowering finish");

        assert!(node_count.as_ref().is_ok_and(|&count| count > 509 * 2), "{node_count:?} nodes");
    }
}

/// The Zend scanner ends a line at `\n`, `\r\n` and a lone `\r`, and stops on the line after the last line ending.
#[test]
fn the_root_end_line_is_the_last_line_the_zend_scanner_counts() {
    let end_lines: Vec<u32> = [
        "class Report {}",
        "class Report {}\n",
        "class Report {}\r\n",
        "class Report {}\r",
        "class Report\r\n{\r}\n\n",
    ]
    .iter()
    .map(|code| {
        let lowered = Lowered::new(code);
        assert_eq!(lowered.diagnostics(), Vec::<String>::new());

        lowered.nodes()[lowered.root() as usize].end_line
    })
    .collect();

    assert_eq!(end_lines, [1, 2, 2, 2, 5]);
}

/// Node lines, declaration lines and diagnostic lines count a lone `\r` as a line ending too.
#[test]
fn every_line_is_the_line_the_zend_scanner_counts() {
    let class = Lowered::new("class Report\r\n{\r}\n\n");
    let root = &class.nodes()[class.root() as usize];
    let declaration = &class.nodes()[class.child(class.root(), 1) as usize];
    assert_eq!((declaration.line, declaration.end_line, root.end_line), (1, 3, 5));

    let method = Lowered::new("class Report\r{\r    public int run()\r    {\r        return 1;\r    }\r}\r");
    let lines: Vec<(String, u32, u32)> = method
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_METHOD || node.kind == sharp_kind::SHARP_AST_RETURN)
        .map(|node| (format!("{:?}", node.kind), node.line, node.end_line))
        .collect();
    assert_eq!(lines, [("SHARP_AST_RETURN".to_owned(), 5, 0), ("SHARP_AST_METHOD".to_owned(), 3, 6)]);

    let refused = Lowered::new("class Report\r{\r    public void run() { echo 1; }\r}\r");
    assert_eq!(refused.diagnostics(), ["3:25 compile error: PHP# has no `echo`: write `printf` or `fwrite`."]);
}

/// ```php
/// <?php
/// declare(strict_types=1);
/// namespace App\Tenant;
/// use Lib\Calc;
/// class Report
/// {
/// }
/// ```
///
/// The `use` is not lowered: every class name in the tree is fully qualified.
#[test]
fn a_file_declares_strict_types_then_its_namespace_and_classes() {
    let lowered = Lowered::new("namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n}\n");

    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              NAMESPACE
                ZVAL "App\\Tenant"
                null
              CLASS "Report" @5-7
                null
                null
                STMT_LIST
                null
                null
        "#}
    );
}

/// An `extern` declaration only tells the checker what plain PHP does, so the engine gets the same tree without it.
#[test]
fn a_file_with_extern_declarations_lowers_to_the_nodes_it_lowers_to_without_them() {
    let library = [("vendor/acme/Mailer.php", "<?php\n\nnamespace Acme;\n\nclass Mailer\n{\n}\n")];
    let with = Lowered::with(
        "namespace App.Tenant;\n\nimport Acme.Mailer;\n\nextern Mailer uses Mail, Http;\nextern trim;\n\nclass Report\n{\n}\n",
        &library,
    );
    let without = Lowered::with("namespace App.Tenant;\n\nimport Acme.Mailer;\n\n\n\n\nclass Report\n{\n}\n", &library);

    assert_eq!(with.tree(), without.tree());
}

/// A law, spec section 28, is checked and never runs, so the engine gets the same tree without it.
#[test]
fn a_class_or_an_enum_with_a_law_lowers_to_the_nodes_it_lowers_to_without_it() {
    let with = Lowered::new(
        "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }) { }\n\n    law amountIsItself(Money a) => a.amount == a.amount;\n}\n\npublic enum Status\n{\n    case Open;\n\n    law openIsOpen(Status s) => Status.Open == Status.Open;\n}\n",
    );
    let without = Lowered::new(
        "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }) { }\n\n\n}\n\npublic enum Status\n{\n    case Open;\n\n\n}\n",
    );

    assert_eq!(with.tree(), without.tree());
}

/// ```php
/// <?php
/// declare(strict_types=1);
/// namespace App\Tenant;
/// use Sharp\Text\Regex as Rx;
/// class Report
/// {
///     public function run(string $text): bool
///     {
///         return Rx::matches("/a/", $text);
///     }
/// }
/// ```
///
/// php-src parses the `use` into `USE [ZEND_SYMBOL_CLASS]` holding `USE_ELEM`, `ZVAL "Sharp\Text\Regex"` and
/// `ZVAL "Rx"`, and compiles `Rx` to the class it names. The `use` is not lowered: the call names
/// `Sharp\Text\Regex` in full, as `Regex.matches(…)` under a plain import does.
#[test]
fn a_call_through_a_renamed_import_lowers_to_the_call_through_the_plain_import() {
    let library = [(
        "src/Sharp/Text/Regex.php",
        "<?php namespace Sharp\\Text; final class Regex { public static function matches(string $pattern, string $text): bool { return true; } }",
    )];
    let renamed = Lowered::with(
        "namespace App.Tenant;\n\nimport Sharp.Text.Regex as Rx;\n\nclass Report\n{\n    public bool run(string text)\n    {\n        return Rx.matches(\"/a/\", text);\n    }\n}\n",
        &library,
    );
    let plain = Lowered::with(
        "namespace App.Tenant;\n\nimport Sharp.Text.Regex;\n\nclass Report\n{\n    public bool run(string text)\n    {\n        return Regex.matches(\"/a/\", text);\n    }\n}\n",
        &library,
    );

    assert_eq!(
        renamed.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "Sharp\\Text\\Regex"
                  ZVAL "matches"
                  ARG_LIST
                    ZVAL "/a/"
                    VAR
                      ZVAL "text"
        "#}
    );
    assert_eq!(renamed.tree(), plain.tree());
}

/// ```php
/// <?php
/// declare(strict_types=1);
/// class Report {}
/// ```
#[test]
fn a_file_without_a_namespace_declares_its_classes_globally() {
    let lowered = Lowered::new("class Report {}\n");

    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              CLASS "Report" @1-1
                null
                null
                STMT_LIST
                null
                null
        "#}
    );
}

/// ```php
/// public function run(int $extra): int
/// {
///     return $extra;
/// }
/// ```
///
/// `[1]` on the method is `ZEND_ACC_PUBLIC`, and `[1]` on `int` is `ZEND_NAME_NOT_FQ`.
#[test]
fn a_method_is_a_public_function_with_its_return_type_after_its_parameters() {
    let lowered = Lowered::new(&method("        return extra;\n"));
    let class = lowered.child(lowered.root(), 2);

    assert_eq!(
        lowered.render(class),
        indoc! {r#"
            CLASS "Report" @5-11
              null
              null
              STMT_LIST
                METHOD [1] "run" @7-10
                  PARAM_LIST
                    PARAM
                      ZVAL [1] "int"
                      ZVAL "extra"
                      null
                      null
                      null
                      null
                  null
                  STMT_LIST
                    RETURN
                      VAR
                        ZVAL "extra"
                  ZVAL [1] "int"
                  null
              null
              null
        "#}
    );
}

/// ```php
/// private int $count = 0;
/// protected \Lib\Calc $calc;
/// ```
///
/// `[4]` and `[2]` are `ZEND_ACC_PRIVATE` and `ZEND_ACC_PROTECTED`.
#[test]
fn a_field_is_a_property_group_of_one_property() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int count = 0;\n    protected Calc calc;\n}\n",
        &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")],
    );
    let class = lowered.child(lowered.root(), 2);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [4]
                ZVAL [1] "int"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "count"
                    ZVAL 0
                    null
                    null
                null
              PROP_GROUP [2]
                ZVAL "Lib\\Calc"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "calc"
                    null
                    null
                    null
                null
        "#}
    );
}

/// ```php
/// public private(set) int $views = 0;
/// public string $name;
/// public readonly int $id;
/// protected protected(set) int $total;   // written `public int total { get; protected set; }`
/// private readonly int $hidden;
/// ```
///
/// An auto-property is a property with asymmetric visibility. `[4097]` is `ZEND_ACC_PUBLIC | ZEND_ACC_PRIVATE_SET`,
/// and `[2049]` is `ZEND_ACC_PUBLIC | ZEND_ACC_PROTECTED_SET`. A get-only property is `readonly`, so `[129]` is
/// `ZEND_ACC_PUBLIC | ZEND_ACC_READONLY` and `[132]` is `ZEND_ACC_PRIVATE | ZEND_ACC_READONLY`.
#[test]
fn an_auto_property_is_a_property_with_its_set_visibility() {
    let lowered = Lowered::new(
        "class Report\n{\n    public int views { get; private set; } = 0;\n    public string name { get; set; }\n    public int id { get; }\n    public int total { get; protected set; }\n    private int hidden { get; }\n}\n",
    );
    let groups: Vec<(u32, String)> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_PROP_GROUP)
        .map(|(index, node)| (node.attr, lowered.render(lowered.child(index as u32, 1))))
        .collect();

    assert_eq!(
        groups,
        [
            (4097, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"views\"\n    ZVAL 0\n    null\n    null\n".to_owned()),
            (1, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"name\"\n    null\n    null\n    null\n".to_owned()),
            (129, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"id\"\n    null\n    null\n    null\n".to_owned()),
            (2049, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"total\"\n    null\n    null\n    null\n".to_owned()),
            (132, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"hidden\"\n    null\n    null\n    null\n".to_owned()),
        ]
    );
}

/// ```php
/// public function __construct(int $start)
/// {
///     $this->count = $start;
/// }
/// ```
///
/// The constructor has no return type, and its first line is its name's, where PHP writes `function`.
#[test]
fn the_constructor_is_a_public_function_named_construct() {
    let lowered = Lowered::new(
        "class Report\n{\n    private int count;\n\n    public\n    Report(int start)\n    {\n        this.count = start;\n    }\n}\n",
    );
    let constructor =
        lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_METHOD).expect("a constructor");

    assert_eq!(
        lowered.render(constructor as u32),
        indoc! {r#"
            METHOD [1] "__construct" @6-9
              PARAM_LIST
                PARAM
                  ZVAL [1] "int"
                  ZVAL "start"
                  null
                  null
                  null
                  null
              null
              STMT_LIST
                ASSIGN
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "count"
                  VAR
                    ZVAL "start"
              null
              null
        "#}
    );
}

/// ```php
/// public function total(): int { return $this->count + 1; }
/// public function touch(): void { $this->save(); }
/// public function __construct(int $count) { $this->count = $count; }
/// ```
///
/// An expression body returns its expression, and runs it as a statement in a `void` method and the constructor.
#[test]
fn an_expression_body_is_the_body_that_returns_its_expression() {
    let lowered = Lowered::with(
        "class Report : Base\n{\n    private int count = 0;\n\n    public int total() => this.count + 1;\n\n    public void touch() => this.save();\n\n    public Report(int count) => this.count = count;\n}\n",
        &[("src/Base.php", "<?php class Base { public function save(): void {} }")],
    );
    let bodies: Vec<String> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_METHOD)
        .map(|(index, _)| lowered.render(lowered.child(index as u32, 2)))
        .collect();

    assert_eq!(
        bodies,
        [
            indoc! {r#"
                STMT_LIST
                  RETURN
                    BINARY_OP [1]
                      PROP
                        VAR
                          ZVAL "this"
                        ZVAL "count"
                      ZVAL 1
            "#},
            indoc! {r#"
                STMT_LIST
                  METHOD_CALL
                    VAR
                      ZVAL "this"
                    ZVAL "save"
                    ARG_LIST
            "#},
            indoc! {r#"
                STMT_LIST
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "count"
                    VAR
                      ZVAL "count"
            "#},
        ]
    );
}

/// ```php
/// public string $slug { get => strtolower($this->name); }
/// ```
///
/// A computed property is a property with one `get` hook whose body is the short body php-src's grammar builds for
/// `get => expr;`. The hook's lines are the arrow's.
#[test]
fn a_computed_property_is_a_property_with_a_short_get_hook() {
    let lowered = Lowered::new(
        "class Report\n{\n    private string name = \"\";\n\n    public string slug => strtolower(this.name);\n}\n",
    );
    let class = lowered.child(lowered.root(), 1);

    assert_eq!(
        lowered.render(lowered.child(lowered.child(class, 2), 1)),
        indoc! {r#"
            PROP_GROUP [1]
              ZVAL [1] "string"
              PROP_DECL
                PROP_ELEM
                  ZVAL "slug"
                  null
                  null
                  STMT_LIST
                    PROPERTY_HOOK "get" @5-5
                      null
                      null
                      PROPERTY_HOOK_SHORT_BODY
                        CALL
                          ZVAL "strtolower"
                          ARG_LIST
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "name"
                      null
                      null
              null
        "#}
    );
}

/// The `PROP_GROUP` of each property a class declares, rendered.
fn property_groups(lowered: &Lowered) -> Vec<String> {
    assert_eq!(lowered.diagnostics(), Vec::<String>::new(), "the source lowers");

    lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_PROP_GROUP)
        .map(|(index, _)| lowered.render(index as u32))
        .collect()
}

/// ```php
/// public string $name { get => $this->name; set { $this->name = $value; } }
/// public private(set) int $count { set { $this->count = $value + 1; } }
/// public int $total { get { return $this->count; } }
/// public int $size { get => $this->size; set { $this->resize($value); } }
/// public protected(set) int $kept = 3 { get => $this->kept * 2; }
/// protected int $guarded { get => $this->guarded; }
/// ```
///
/// Each accessor body is a hook: `get => expr;` keeps PHP's short body, and a block or `set => expr;` is a statement
/// list, because PHP stores the result of a short `set` body. `field` is `$this->name` and `value` is `$value`. An auto
/// accessor beside a body that uses `field` is PHP's backing store, so it lowers to nothing, and otherwise it is the
/// hook over the storage, so the property stays backed. A public get-only property whose body uses `field` takes
/// `protected(set)`, the set visibility of `readonly`, which PHP refuses on a hooked property. A protected one takes
/// none, because PHP drops a set visibility equal to the property's own: `[4097]` is
/// `ZEND_ACC_PUBLIC | ZEND_ACC_PRIVATE_SET`, `[2049]` is `ZEND_ACC_PUBLIC | ZEND_ACC_PROTECTED_SET` and `[2]` is
/// `ZEND_ACC_PROTECTED`.
#[test]
fn accessor_bodies_are_property_hooks() {
    let lowered = Lowered::new(
        "class Report\n{\n    public string name { get => field; set => field = value; }\n    public int count { get; private set => field = value + 1; }\n    public int total { get { return this.count; } }\n    public int size { get; set => this.resize(value); }\n    public int kept { get => field * 2; } = 3;\n    protected int guarded { get => field; }\n\n    public void resize(int to)\n    {\n    }\n}\n",
    );

    assert_eq!(
        property_groups(&lowered),
        [
            indoc! {r#"
                PROP_GROUP [1]
                  ZVAL [1] "string"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "name"
                      null
                      null
                      STMT_LIST
                        PROPERTY_HOOK "get" @3-3
                          null
                          null
                          PROPERTY_HOOK_SHORT_BODY
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "name"
                          null
                          null
                        PROPERTY_HOOK "set" @3-3
                          null
                          null
                          STMT_LIST
                            ASSIGN
                              PROP
                                VAR
                                  ZVAL "this"
                                ZVAL "name"
                              VAR
                                ZVAL "value"
                          null
                          null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [4097]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "count"
                      null
                      null
                      STMT_LIST
                        PROPERTY_HOOK "set" @4-4
                          null
                          null
                          STMT_LIST
                            ASSIGN
                              PROP
                                VAR
                                  ZVAL "this"
                                ZVAL "count"
                              BINARY_OP [1]
                                VAR
                                  ZVAL "value"
                                ZVAL 1
                          null
                          null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [1]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "total"
                      null
                      null
                      STMT_LIST
                        PROPERTY_HOOK "get" @5-5
                          null
                          null
                          STMT_LIST
                            RETURN
                              PROP
                                VAR
                                  ZVAL "this"
                                ZVAL "count"
                          null
                          null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [1]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "size"
                      null
                      null
                      STMT_LIST
                        PROPERTY_HOOK "get" @6-6
                          null
                          null
                          PROPERTY_HOOK_SHORT_BODY
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "size"
                          null
                          null
                        PROPERTY_HOOK "set" @6-6
                          null
                          null
                          STMT_LIST
                            METHOD_CALL
                              VAR
                                ZVAL "this"
                              ZVAL "resize"
                              ARG_LIST
                                VAR
                                  ZVAL "value"
                          null
                          null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [2049]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "kept"
                      ZVAL 3
                      null
                      STMT_LIST
                        PROPERTY_HOOK "get" @7-7
                          null
                          null
                          PROPERTY_HOOK_SHORT_BODY
                            BINARY_OP [3]
                              PROP
                                VAR
                                  ZVAL "this"
                                ZVAL "kept"
                              ZVAL 2
                          null
                          null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [2]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "guarded"
                      null
                      null
                      STMT_LIST
                        PROPERTY_HOOK "get" @8-8
                          null
                          null
                          PROPERTY_HOOK_SHORT_BODY
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "guarded"
                          null
                          null
                  null
            "#},
        ]
    );
}

/// ```php
/// public protected(set) int $a { get { if ($this->ready()) { return $this->a; } return 0; } }
/// ```
///
/// `field` in a nested block keeps a property backed, as php-src finds `$this->a` there, so the get-only property
/// takes `protected(set)`. `field` only in a lambda leaves a property virtual, and the checker refuses the lambda, so
/// the file lowers to the error and no nodes.
#[test]
fn field_in_a_nested_block_keeps_a_property_backed_and_field_only_in_a_lambda_is_refused() {
    let nested = Lowered::new(
        "class Report\n{\n    public int a { get { if (this.ready()) { return field; } return 0; } }\n\n    public bool ready()\n    {\n        return true;\n    }\n}\n",
    );
    let lambda =
        Lowered::new("class Report\n{\n    public int b { get { const read = () => field; return read(); } }\n}\n");

    assert_eq!(
        property_groups(&nested),
        [indoc! {r#"
            PROP_GROUP [2049]
              ZVAL [1] "int"
              PROP_DECL
                PROP_ELEM
                  ZVAL "a"
                  null
                  null
                  STMT_LIST
                    PROPERTY_HOOK "get" @3-3
                      null
                      null
                      STMT_LIST
                        IF
                          IF_ELEM
                            METHOD_CALL
                              VAR
                                ZVAL "this"
                              ZVAL "ready"
                              ARG_LIST
                            STMT_LIST
                              RETURN
                                PROP
                                  VAR
                                    ZVAL "this"
                                  ZVAL "a"
                        RETURN
                          ZVAL 0
                      null
                      null
              null
        "#}]
    );
    assert_eq!(
        lambda.diagnostics(),
        [
            "3:45 compile error: `field` cannot be used in a lambda: PHP would call the accessor again instead of reading the storage.",
            "3:59 compile error: Could not infer a precise return type for property hook `Report.b.get`. Saw type `Any?`."
        ]
    );
    assert_eq!(lambda.nodes().len(), 0);
}

/// ```php
/// public int $size { get => $this->size; set { $this->size = $value; } }
/// ```
///
/// An auto accessor beside a body that does not use `field` is the hook over the storage, so PHP sees a backed
/// property, as C# does.
#[test]
fn an_auto_accessor_beside_a_body_without_field_is_the_hook_over_the_storage() {
    let lowered = Lowered::new(
        "class Report\n{\n    public int size { get => this.read(); set; }\n\n    public int read()\n    {\n        return 1;\n    }\n}\n",
    );

    assert_eq!(
        property_groups(&lowered),
        [indoc! {r#"
            PROP_GROUP [1]
              ZVAL [1] "int"
              PROP_DECL
                PROP_ELEM
                  ZVAL "size"
                  null
                  null
                  STMT_LIST
                    PROPERTY_HOOK "get" @3-3
                      null
                      null
                      PROPERTY_HOOK_SHORT_BODY
                        METHOD_CALL
                          VAR
                            ZVAL "this"
                          ZVAL "read"
                          ARG_LIST
                      null
                      null
                    PROPERTY_HOOK "set" @3-3
                      null
                      null
                      STMT_LIST
                        ASSIGN
                          PROP
                            VAR
                              ZVAL "this"
                            ZVAL "size"
                          VAR
                            ZVAL "value"
                      null
                      null
              null
        "#}]
    );
}

/// ```php
/// public function __construct(
///     public string $title { get => $this->title; set { $this->title = \trim($value); } },
///     public protected(set) int $id { get => $this->id; },
/// ) {}
/// ```
///
/// A property declared on a constructor parameter carries its hooks as the parameter's last child.
#[test]
fn a_promoted_property_with_accessor_bodies_is_a_promoted_parameter_with_hooks() {
    let lowered = Lowered::new(
        "class Report\n{\n    public Report(public string title { get => field; set => field = trim(value); }, public int id { get => field; })\n    {\n    }\n}\n",
    );
    let parameters: Vec<String> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_PARAM)
        .map(|(index, _)| lowered.render(index as u32))
        .collect();

    assert_eq!(
        parameters,
        [
            indoc! {r#"
                PARAM [1]
                  ZVAL [1] "string"
                  ZVAL "title"
                  null
                  null
                  null
                  STMT_LIST
                    PROPERTY_HOOK "get" @3-3
                      null
                      null
                      PROPERTY_HOOK_SHORT_BODY
                        PROP
                          VAR
                            ZVAL "this"
                          ZVAL "title"
                      null
                      null
                    PROPERTY_HOOK "set" @3-3
                      null
                      null
                      STMT_LIST
                        ASSIGN
                          PROP
                            VAR
                              ZVAL "this"
                            ZVAL "title"
                          CALL
                            ZVAL "trim"
                            ARG_LIST
                              VAR
                                ZVAL "value"
                      null
                      null
            "#},
            indoc! {r#"
                PARAM [2049]
                  ZVAL [1] "int"
                  ZVAL "id"
                  null
                  null
                  null
                  STMT_LIST
                    PROPERTY_HOOK "get" @3-3
                      null
                      null
                      PROPERTY_HOOK_SHORT_BODY
                        PROP
                          VAR
                            ZVAL "this"
                          ZVAL "id"
                      null
                      null
            "#},
        ]
    );
}

/// ```php
/// public int $price = 0 { set { if ($value < 0) { throw new \InvalidArgumentException("negative"); } $this->price = $value; } }
/// public Address $shipping { get => $this->getAttribute("shipping"); set { $this->setAttribute("shipping", $value); } }
/// ```
///
/// A `set` block that validates `value` and writes `field` is PHP's `set` hook over the storage, beside which `get;`
/// is the backing store. Accessors over a plain PHP parent's methods never use `field`, so the property is virtual.
#[test]
fn a_validating_set_and_a_property_over_a_parents_methods_are_their_php_hooks() {
    let lowered = Lowered::with(
        "import Lib.Model;\nimport InvalidArgumentException;\n\nclass Order : Model\n{\n    public int price { get; set { if (value < 0) { throw new InvalidArgumentException(\"negative\"); } field = value; } } = 0;\n    public Address shipping { get => this.getAttribute(\"shipping\"); set => this.setAttribute(\"shipping\", value); }\n}\n\nclass Address\n{\n}\n",
        &[common::MODEL],
    );

    assert_eq!(
        property_groups(&lowered),
        [
            indoc! {r#"
                PROP_GROUP [1]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "price"
                      ZVAL 0
                      null
                      STMT_LIST
                        PROPERTY_HOOK "set" @6-6
                          null
                          null
                          STMT_LIST
                            IF
                              IF_ELEM
                                BINARY_OP [20]
                                  VAR
                                    ZVAL "value"
                                  ZVAL 0
                                STMT_LIST
                                  THROW
                                    NEW
                                      ZVAL "InvalidArgumentException"
                                      ARG_LIST
                                        ZVAL "negative"
                            ASSIGN
                              PROP
                                VAR
                                  ZVAL "this"
                                ZVAL "price"
                              VAR
                                ZVAL "value"
                          null
                          null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [1]
                  ZVAL "Address"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "shipping"
                      null
                      null
                      STMT_LIST
                        PROPERTY_HOOK "get" @7-7
                          null
                          null
                          PROPERTY_HOOK_SHORT_BODY
                            METHOD_CALL
                              VAR
                                ZVAL "this"
                              ZVAL "getAttribute"
                              ARG_LIST
                                ZVAL "shipping"
                          null
                          null
                        PROPERTY_HOOK "set" @7-7
                          null
                          null
                          STMT_LIST
                            METHOD_CALL
                              VAR
                                ZVAL "this"
                              ZVAL "setAttribute"
                              ARG_LIST
                                ZVAL "shipping"
                                VAR
                                  ZVAL "value"
                          null
                          null
                  null
            "#},
        ]
    );
}

/// The PHP twin:
///
/// ```php
/// public ?Address $shipping { get => $this->getAttribute("shipping"); set { $this->setAttribute("shipping", $value); } }
/// public ?string $note = null { get => $this->note; set { $this->note = $value; } }
/// public ?string $summary { get => $this->note; }
/// ```
///
/// A nullable property with accessor bodies takes the nullable type any `T?` lowers to. One whose body uses `field`
/// starts as null, so it takes the default `null`, as a settable auto-property does. One without storage is virtual,
/// and PHP refuses a default on a virtual property, so it takes none, get-only or not. `[256]` is `ZEND_TYPE_NULLABLE`
/// on a class name, and `[257]` is `ZEND_NAME_NOT_FQ | ZEND_TYPE_NULLABLE`.
#[test]
fn nullable_properties_with_accessor_bodies_are_their_php_hooks() {
    let lowered = Lowered::with(
        "import Lib.Model;\n\nclass Order : Model\n{\n    public Address? shipping { get => this.getAttribute(\"shipping\"); set => this.setAttribute(\"shipping\", value); }\n    public string? note { get => field; set => field = value; }\n    public string? summary { get => this.note; }\n}\n\nclass Address\n{\n}\n",
        &[common::MODEL],
    );

    assert_eq!(
        property_groups(&lowered),
        [
            indoc! {r#"
                PROP_GROUP [1]
                  ZVAL [256] "Address"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "shipping"
                      null
                      null
                      STMT_LIST
                        PROPERTY_HOOK "get" @5-5
                          null
                          null
                          PROPERTY_HOOK_SHORT_BODY
                            METHOD_CALL
                              VAR
                                ZVAL "this"
                              ZVAL "getAttribute"
                              ARG_LIST
                                ZVAL "shipping"
                          null
                          null
                        PROPERTY_HOOK "set" @5-5
                          null
                          null
                          STMT_LIST
                            METHOD_CALL
                              VAR
                                ZVAL "this"
                              ZVAL "setAttribute"
                              ARG_LIST
                                ZVAL "shipping"
                                VAR
                                  ZVAL "value"
                          null
                          null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [1]
                  ZVAL [257] "string"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "note"
                      ZVAL null
                      null
                      STMT_LIST
                        PROPERTY_HOOK "get" @6-6
                          null
                          null
                          PROPERTY_HOOK_SHORT_BODY
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "note"
                          null
                          null
                        PROPERTY_HOOK "set" @6-6
                          null
                          null
                          STMT_LIST
                            ASSIGN
                              PROP
                                VAR
                                  ZVAL "this"
                                ZVAL "note"
                              VAR
                                ZVAL "value"
                          null
                          null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [1]
                  ZVAL [257] "string"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "summary"
                      null
                      null
                      STMT_LIST
                        PROPERTY_HOOK "get" @7-7
                          null
                          null
                          PROPERTY_HOOK_SHORT_BODY
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "note"
                          null
                          null
                  null
            "#},
        ]
    );
}

/// ```php
/// public function __construct(private int $count, public readonly int $id, protected string $name, int $extra) {}
/// ```
///
/// A parameter that declares a member carries the member's flags: `[4]` is `ZEND_ACC_PRIVATE`, `[129]` is
/// `ZEND_ACC_PUBLIC | ZEND_ACC_READONLY`, and `[2]` is `ZEND_ACC_PROTECTED`.
#[test]
fn a_constructor_parameter_with_an_access_modifier_is_a_promoted_parameter() {
    let lowered = Lowered::new(
        "class Report\n{\n    public Report(private int count, public int id { get; }, protected string name { get; set; }, int extra) {}\n}\n",
    );
    let parameters: Vec<(String, u32)> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_PARAM)
        .map(|(index, node)| {
            assert_eq!(lowered.child(index as u32, 5), u32::MAX, "a promoted parameter has no hooks");

            (lowered.text(lowered.nodes()[lowered.child(index as u32, 1) as usize].text), node.attr)
        })
        .collect();

    assert_eq!(
        parameters,
        [("count".to_owned(), 4), ("id".to_owned(), 129), ("name".to_owned(), 2), ("extra".to_owned(), 0)]
    );
}

/// ```php
/// private \Lib\Calc $calc;
/// public int $total = 2;
/// public function __construct(int $start)
/// {
///     $this->calc = new \Lib\Calc(1);
///     $this->total = $start;
/// }
/// ```
///
/// A constant initial value is the property's default. Any other runs at the start of the constructor.
#[test]
fn a_non_constant_initial_value_runs_at_the_start_of_the_constructor() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private Calc calc = new Calc(1);\n    public int total { get; set; } = 1 + 1;\n\n    public Report(int start)\n    {\n        this.total = start;\n    }\n}\n",
        &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc { public function __construct(int $start) {} }")],
    );
    let class = lowered.child(lowered.root(), 2);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [4]
                ZVAL "Lib\\Calc"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "calc"
                    null
                    null
                    null
                null
              PROP_GROUP [1]
                ZVAL [1] "int"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "total"
                    BINARY_OP [1]
                      ZVAL 1
                      ZVAL 1
                    null
                    null
                null
              METHOD [1] "__construct" @10-13
                PARAM_LIST
                  PARAM
                    ZVAL [1] "int"
                    ZVAL "start"
                    null
                    null
                    null
                    null
                null
                STMT_LIST
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "calc"
                    NEW
                      ZVAL "Lib\\Calc"
                      ARG_LIST
                        ZVAL 1
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "total"
                    VAR
                      ZVAL "start"
                null
                null
        "#}
    );
}

/// ```php
/// private \Lib\Calc $calc;
/// public function __construct()
/// {
///     $this->calc = new \Lib\Calc(1);
/// }
/// ```
///
/// A class with a non-constant initial value and no constructor gets a public one, which spans the class.
#[test]
fn a_class_with_a_non_constant_initial_value_and_no_constructor_gets_one() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private Calc calc = new Calc(1);\n}\n",
        &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc { public function __construct(int $start) {} }")],
    );
    let class = lowered.child(lowered.root(), 2);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [4]
                ZVAL "Lib\\Calc"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "calc"
                    null
                    null
                    null
                null
              METHOD [1] "__construct" @5-8
                PARAM_LIST
                null
                STMT_LIST
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "calc"
                    NEW
                      ZVAL "Lib\\Calc"
                      ARG_LIST
                        ZVAL 1
                null
                null
        "#}
    );
}

/// ```php
/// public readonly string $code;
/// public function __construct()
/// {
///     $this->code = 'none';
/// }
/// ```
///
/// PHP takes no default on a `readonly` property, so a get-only property's initial value runs in the constructor even
/// when it is constant.
#[test]
fn a_get_only_property_gets_its_initial_value_in_the_constructor() {
    let lowered = Lowered::new("class Report\n{\n    public string code { get; } = \"none\";\n}\n");
    let class = lowered.child(lowered.root(), 1);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [129]
                ZVAL [1] "string"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "code"
                    null
                    null
                    null
                null
              METHOD [1] "__construct" @1-4
                PARAM_LIST
                null
                STMT_LIST
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "code"
                    ZVAL "none"
                null
                null
        "#}
    );
}

/// ```php
/// private ?int $total = null;
/// public ?\Lib\Calc $owner = null;
/// private int|string|null $key = null;
/// private ?int $start = 1;
/// public readonly ?int $limit;
///
/// public function __construct()
/// {
///     $this->limit = null;
/// }
/// ```
///
/// A field or a settable auto-property of a nullable type without an initial value starts as null, so it takes the
/// default `null` on the line of its name. A get-only property is `readonly`, which takes no default, so its initial
/// value runs in the constructor. `[257]` is `ZEND_NAME_NOT_FQ | ZEND_TYPE_NULLABLE`, and `[256]` is
/// `ZEND_TYPE_NULLABLE` on a class name.
#[test]
fn a_nullable_field_or_settable_auto_property_without_an_initial_value_defaults_to_null() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int? total;\n    public Calc? owner { get; set; }\n    private (int|string)? key;\n    private int? start = 1;\n    public int? limit { get; } = null;\n}\n",
        &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")],
    );
    let class = lowered.child(lowered.root(), 2);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [4]
                ZVAL [257] "int"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "total"
                    ZVAL null
                    null
                    null
                null
              PROP_GROUP [1]
                ZVAL [256] "Lib\\Calc"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "owner"
                    ZVAL null
                    null
                    null
                null
              PROP_GROUP [4]
                TYPE_UNION
                  ZVAL [1] "int"
                  ZVAL [1] "string"
                  ZVAL [1] "null"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "key"
                    ZVAL null
                    null
                    null
                null
              PROP_GROUP [4]
                ZVAL [257] "int"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "start"
                    ZVAL 1
                    null
                    null
                null
              PROP_GROUP [129]
                ZVAL [257] "int"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "limit"
                    null
                    null
                    null
                null
              METHOD [1] "__construct" @5-12
                PARAM_LIST
                null
                STMT_LIST
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "limit"
                    ZVAL null
                null
                null
        "#}
    );
    let members = lowered.child(class, 2);
    let default_lines = [0, 1, 2].map(|member| {
        let element = lowered.child(lowered.child(lowered.child(members, member), 1), 0);
        lowered.nodes()[lowered.child(element, 1) as usize].line
    });
    assert_eq!(default_lines, [7, 8, 9]);
}

/// ```php
/// public static function make(): void {}
/// private function hide() {}
/// protected function share() {}
/// ```
///
/// The flags are `ZEND_ACC_PUBLIC | ZEND_ACC_STATIC`, `ZEND_ACC_PRIVATE` and `ZEND_ACC_PROTECTED`.
#[test]
fn method_modifiers_become_the_method_flags() {
    let lowered = Lowered::new(
        "class Report\n{\n    public static void make() {}\n    private void hide() {}\n    protected void share() {}\n}\n",
    );
    let methods: Vec<(String, u32)> = lowered
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_METHOD)
        .map(|node| (lowered.text(node.text), node.attr))
        .collect();

    assert_eq!(methods, [("make".to_owned(), 17), ("hide".to_owned(), 4), ("share".to_owned(), 2)]);
}

/// ```php
/// public function make(\Lib\Calc $other, float $rate = 1.5, int $loud = PHP_DEBUG): \Lib\Calc {}
/// ```
///
/// A class type is its full name with `ZEND_NAME_FQ`, which is 0.
#[test]
fn parameters_carry_their_type_name_and_default() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public Calc make(Calc other, float rate = 1.5, int loud = PHP_DEBUG) { return other; }\n}\n",
        &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")],
    );
    let method = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_METHOD).expect("a method");

    assert_eq!(
        lowered.render(lowered.child(method as u32, 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                ZVAL "Lib\\Calc"
                ZVAL "other"
                null
                null
                null
                null
              PARAM
                ZVAL [1] "float"
                ZVAL "rate"
                ZVAL 1.5
                null
                null
                null
              PARAM
                ZVAL [1] "int"
                ZVAL "loud"
                CONST
                  ZVAL [1] "PHP_DEBUG"
                null
                null
                null
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(method as u32, 3)),
        indoc! {r#"
            ZVAL "Lib\\Calc"
        "#}
    );
}

/// ```php
/// public function find(?int $id, ?\Lib\Calc $other = null): ?\Lib\Calc { return null; }
/// ```
///
/// `[256]` is `ZEND_TYPE_NULLABLE`, which php-src's grammar adds to the type's attr, and `[257]` adds it to
/// `ZEND_NAME_NOT_FQ`.
#[test]
fn a_nullable_type_is_its_type_with_the_nullable_flag() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public Calc? find(int? id, Calc? other = null) { return null; }\n}\n",
        &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")],
    );
    let method = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_METHOD).expect("a method");

    assert_eq!(
        lowered.render(lowered.child(method as u32, 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                ZVAL [257] "int"
                ZVAL "id"
                null
                null
                null
                null
              PARAM
                ZVAL [256] "Lib\\Calc"
                ZVAL "other"
                ZVAL null
                null
                null
                null
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(method as u32, 3)),
        indoc! {r#"
            ZVAL [256] "Lib\\Calc"
        "#}
    );
}

/// ```php
/// public function group(array $items, ?array $sizes = ['a' => 1]): ?array { return null; }
/// ```
///
/// A `List` or `Map` is a PHP array, so its type is `array`: a `TYPE` with `IS_ARRAY`, which is 7, as php-src's
/// grammar builds it, and `[263]` adds `ZEND_TYPE_NULLABLE`.
#[test]
fn a_list_or_map_type_is_the_array_type() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public Map<string, List<int>>? group(List<int> items, Map<string, int>? sizes = [\"a\": 1]) { return null; }\n}\n",
    );
    let method = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_METHOD).expect("a method");

    assert_eq!(
        lowered.render(lowered.child(method as u32, 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                TYPE [7]
                ZVAL "items"
                null
                null
                null
                null
              PARAM
                TYPE [263]
                ZVAL "sizes"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL 1
                    ZVAL "a"
                null
                null
                null
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(method as u32, 3)),
        indoc! {"
            TYPE [263]
        "}
    );
}

/// ```php
/// public function group(array $sizes, array $calcs, array $keys): ?array { return null; }
/// ```
///
/// A nullable type argument lowers as any other, so each collection is `array`: a `TYPE` with `IS_ARRAY`, which is
/// 7, and `[263]` adds `ZEND_TYPE_NULLABLE` to the collection itself.
#[test]
fn a_collection_with_a_nullable_type_argument_is_the_array_type() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public Map<string, Any?>? group(List<int?> sizes, Map<string, List<Calc?>> calcs, List<(int|string)?> keys) { return null; }\n}\n",
        &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")],
    );
    let method = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_METHOD).expect("a method");

    assert_eq!(
        lowered.render(lowered.child(method as u32, 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                TYPE [7]
                ZVAL "sizes"
                null
                null
                null
                null
              PARAM
                TYPE [7]
                ZVAL "calcs"
                null
                null
                null
                null
              PARAM
                TYPE [7]
                ZVAL "keys"
                null
                null
                null
                null
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(method as u32, 3)),
        indoc! {"
            TYPE [263]
        "}
    );
}

/// ```php
/// $counts = [\Lib\Calc::Active->value => 1];
/// $counts[\Lib\Calc::Closed->value] = 2;
/// return $counts[\Lib\Calc::Active->value] ?? 0;
/// ```
///
/// A `Map` keyed by a backed enum holds each key as its backing value, so a case going in as a key, in a literal or
/// an index, is its `->value`.
#[test]
fn a_backed_enum_key_goes_into_a_map_as_its_backing_value() {
    assert_eq!(
        body_in(
            RUN,
            "        Map<Calc, int> counts = [Calc.Active: 1];\n        counts[Calc.Closed] = 2;\n        return counts[Calc.Active] ?? 0;\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "counts"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL 1
                    PROP
                      CLASS_CONST
                        ZVAL "Lib\\Calc"
                        ZVAL "Active"
                      ZVAL "value"
              ASSIGN
                DIM
                  VAR
                    ZVAL "counts"
                  PROP
                    CLASS_CONST
                      ZVAL "Lib\\Calc"
                      ZVAL "Closed"
                    ZVAL "value"
                ZVAL 2
              RETURN
                COALESCE
                  DIM
                    VAR
                      ZVAL "counts"
                    PROP
                      CLASS_CONST
                        ZVAL "Lib\\Calc"
                        ZVAL "Active"
                      ZVAL "value"
                  ZVAL 0
        "#}
    );
}

/// ```php
/// return \array_replace($defaults, [\Lib\Calc::Active->value => 1]);
/// ```
///
/// The entries a `Map` spread's `\array_replace` takes put a backed enum key in as its backing value too.
#[test]
fn a_backed_enum_key_beside_a_map_spread_goes_in_as_its_backing_value() {
    assert_eq!(
        body_in(
            "Map<Calc, int> run(Map<Calc, int> defaults)",
            "        return [...defaults, Calc.Active: 1];\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "array_replace"
                  ARG_LIST
                    VAR
                      ZVAL "defaults"
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL 1
                        PROP
                          CLASS_CONST
                            ZVAL "Lib\\Calc"
                            ZVAL "Active"
                          ZVAL "value"
        "#}
    );
}

/// ```php
/// return $counts->get($status->value);
/// ```
///
/// `Map.get` declares its parameter as the `Map`'s key type, so a case going in as its key is its `->value`.
#[test]
fn a_backed_enum_key_goes_into_map_get_as_its_backing_value() {
    assert_eq!(
        body_in(
            "int? run(Map<Calc, int> counts, Calc status)",
            "        return counts.get(status);\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                METHOD_CALL
                  VAR
                    ZVAL "counts"
                  ZVAL "get"
                  ARG_LIST
                    PROP
                      VAR
                        ZVAL "status"
                      ZVAL "value"
        "#}
    );
}

/// ```php
/// $counts->delete($status->value);
/// $counts->delete(key: \Lib\Calc::Closed->value);
/// ```
///
/// `Map.delete` declares its parameter as the `Map`'s key type, so a case going in as its key is its `->value`,
/// passed by position or by name.
#[test]
fn a_backed_enum_key_goes_into_map_delete_as_its_backing_value() {
    assert_eq!(
        body_in(
            "void run(Map<Calc, int> counts, Calc status)",
            "        counts.delete(status);\n        counts.delete(key: Calc.Closed);\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              METHOD_CALL
                VAR
                  ZVAL "counts"
                ZVAL "delete"
                ARG_LIST
                  PROP
                    VAR
                      ZVAL "status"
                    ZVAL "value"
              METHOD_CALL
                VAR
                  ZVAL "counts"
                ZVAL "delete"
                ARG_LIST
                  NAMED_ARG
                    ZVAL "key"
                    PROP
                      CLASS_CONST
                        ZVAL "Lib\\Calc"
                        ZVAL "Closed"
                      ZVAL "value"
        "#}
    );
}

/// ```php
/// return $counts?->get($status->value);
/// ```
///
/// A null-safe call of `Map.get` puts a case in as its `->value` too.
#[test]
fn a_backed_enum_key_goes_into_a_null_safe_map_get_as_its_backing_value() {
    assert_eq!(
        body_in(
            "int? run(Map<Calc, int>? counts, Calc status)",
            "        return counts?.get(status);\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                NULLSAFE_METHOD_CALL
                  VAR
                    ZVAL "counts"
                  ZVAL "get"
                  ARG_LIST
                    PROP
                      VAR
                        ZVAL "status"
                      ZVAL "value"
        "#}
    );
}

/// ```php
/// $standings->add($status);
/// ```
///
/// `List.add` declares its parameter as the `List`'s element type, not a key type, so a case goes in as the case.
#[test]
fn a_backed_enum_value_goes_into_list_add_as_its_case() {
    assert_eq!(
        body_in(
            "void run(List<Calc> standings, Calc status)",
            "        standings.add(status);\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              METHOD_CALL
                VAR
                  ZVAL "standings"
                ZVAL "add"
                ARG_LIST
                  VAR
                    ZVAL "status"
        "#}
    );
}

/// ```php
/// $tags = ["vip" => "vip", "new" => "new"];
/// $states = [\Lib\Calc::Active->value => \Lib\Calc::Active, $status->value => $status];
/// $names = [($element#1 = \strtoupper($name)) => $element#1];
/// ```
///
/// Spec section 12: a `Set` holds each element under its key, the element or a backed enum's `->value`. PHP reads an
/// element's key before its value, so an element that runs something goes in through a hidden variable its key
/// assigns, and runs once.
#[test]
fn a_set_literal_holds_each_element_under_its_key() {
    assert_eq!(
        body_in(
            "void run(Calc status, string name)",
            "        Set<string> tags = [\"vip\", \"new\"];\n        Set<Calc> states = [Calc.Active, status];\n        Set<string> names = [strtoupper(name)];\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "tags"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "vip"
                    ZVAL "vip"
                  ARRAY_ELEM
                    ZVAL "new"
                    ZVAL "new"
              ASSIGN
                VAR
                  ZVAL "states"
                ARRAY [3]
                  ARRAY_ELEM
                    CLASS_CONST
                      ZVAL "Lib\\Calc"
                      ZVAL "Active"
                    PROP
                      CLASS_CONST
                        ZVAL "Lib\\Calc"
                        ZVAL "Active"
                      ZVAL "value"
                  ARRAY_ELEM
                    VAR
                      ZVAL "status"
                    PROP
                      VAR
                        ZVAL "status"
                      ZVAL "value"
              ASSIGN
                VAR
                  ZVAL "names"
                ARRAY [3]
                  ARRAY_ELEM
                    VAR
                      ZVAL "element#1"
                    ASSIGN
                      VAR
                        ZVAL "element#1"
                      CALL
                        ZVAL "strtoupper"
                        ARG_LIST
                          VAR
                            ZVAL "name"
        "#}
    );
}

/// ```php
/// $tags = [];
/// $tags = ["b" => "b"];
/// ```
///
/// A list literal assigned to a local declared `Set` is that `Set`, each element under its key.
#[test]
fn a_list_literal_assigned_to_a_set_local_is_a_set() {
    assert_eq!(
        body_in("void run(bool flag)", "        Set<string> tags = [];\n        tags = [\"b\"];\n", &[]),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "tags"
                ARRAY [3]
              ASSIGN
                VAR
                  ZVAL "tags"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "b"
                    ZVAL "b"
        "#}
    );
}

/// ```php
/// $branch = [];
/// if ($flag) { $branch = ["d" => "d"]; } else { $branch = ["e" => "e"]; }
/// ```
///
/// A list literal assigned in either branch of an `if` to a local declared `Set` is that `Set`.
#[test]
fn a_list_literal_assigned_in_each_branch_of_an_if_to_a_set_local_is_a_set() {
    assert_eq!(
        body_in(
            "void run(bool flag)",
            "        Set<string> branch = [];\n        if (flag) {\n            branch = [\"d\"];\n        } else {\n            branch = [\"e\"];\n        }\n",
            &[],
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "branch"
                ARRAY [3]
              IF
                IF_ELEM
                  VAR
                    ZVAL "flag"
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "branch"
                      ARRAY [3]
                        ARRAY_ELEM
                          ZVAL "d"
                          ZVAL "d"
                IF_ELEM
                  null
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "branch"
                      ARRAY [3]
                        ARRAY_ELEM
                          ZVAL "e"
                          ZVAL "e"
        "#}
    );
}

/// ```php
/// $maybe = $maybe === null ? null : \Sharp\Set::from($maybe);
/// $maybe ??= ["c" => "c"];
/// ```
///
/// A list literal that `??=` assigns to a nullable `Set` is that `Set`.
#[test]
fn a_list_literal_that_coalescing_assigns_to_a_nullable_set_is_a_set() {
    assert_eq!(
        body_in("void run(Set<string>? maybe)", "        maybe ??= [\"c\"];\n", &[]),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "maybe"
                CONDITIONAL
                  BINARY_OP [16]
                    VAR
                      ZVAL "maybe"
                    ZVAL null
                  ZVAL null
                  STATIC_CALL
                    ZVAL "Sharp\\Set"
                    ZVAL "from"
                    ARG_LIST
                      VAR
                        ZVAL "maybe"
              ASSIGN_COALESCE
                VAR
                  ZVAL "maybe"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "c"
                    ZVAL "c"
        "#}
    );
}

/// ```php
/// private array $tags = ["a" => "a"];
/// ```
///
/// A list literal that is the initial value of a property declared `Set` is that `Set`, its constant default.
#[test]
fn a_list_literal_initial_value_of_a_set_property_is_a_set() {
    assert_eq!(
        property_groups(&Lowered::new("class Report\n{\n    private Set<string> tags = [\"a\"];\n}\n")),
        [indoc! {r#"
            PROP_GROUP [4]
              TYPE [7]
              PROP_DECL
                PROP_ELEM
                  ZVAL "tags"
                  ARRAY [3]
                    ARRAY_ELEM
                      ZVAL "a"
                      ZVAL "a"
                  null
                  null
              null
        "#}]
    );
}

/// ```php
/// $this->tags = ["c" => "c"];
/// \Report::$shared = ["d" => "d"];
/// $this->hooked = ["e" => "e"];
/// ```
///
/// A list literal assigned to a field, a static field or a property with accessors declared `Set` is that `Set`.
#[test]
fn a_list_literal_assigned_to_a_set_property_is_a_set() {
    let lowered = Lowered::new(
        "class Report\n{\n    private Set<string> tags = [];\n    private static Set<string> shared = [];\n    public Set<string> hooked { get; set; } = [];\n\n    public void run()\n    {\n        this.tags = [\"c\"];\n        Report.shared = [\"d\"];\n        this.hooked = [\"e\"];\n    }\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "tags"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "c"
                    ZVAL "c"
              ASSIGN
                STATIC_PROP
                  ZVAL "Report"
                  ZVAL "shared"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "d"
                    ZVAL "d"
              ASSIGN
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "hooked"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "e"
                    ZVAL "e"
        "#}
    );
}

/// ```php
/// public function run(): array { return ["a" => "a"]; }
/// public function short(): array { return ["b" => "b"]; }
/// ```
///
/// A list literal a method declared to return a `Set` returns, from a block or as its expression body, is that `Set`.
#[test]
fn a_list_literal_returned_where_a_set_is_declared_is_a_set() {
    let lowered = Lowered::new(
        "class Report\n{\n    public Set<string> run()\n    {\n        return [\"a\"];\n    }\n\n    public Set<string> short() => [\"b\"];\n}\n",
    );
    let body = |name: &str| lowered.render(lowered.child(lowered.method_named(name), 2));

    assert_eq!(
        body("run"),
        indoc! {r#"
            STMT_LIST
              RETURN
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "a"
                    ZVAL "a"
        "#}
    );
    assert_eq!(
        body("short"),
        indoc! {r#"
            STMT_LIST
              RETURN
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "b"
                    ZVAL "b"
        "#}
    );
}

/// ```php
/// public function run(array $seed = ["a" => "a"]): int { $seed = \Sharp\Set::from($seed); return \count($seed); }
/// ```
///
/// A list literal that is the default of a parameter declared `Set` is that `Set`.
#[test]
fn a_list_literal_default_of_a_set_parameter_is_a_set() {
    let lowered = Lowered::new("class Report\n{\n    public int run(Set<string> seed = [\"a\"]) => seed.count();\n}\n");

    assert_eq!(
        lowered.render(lowered.child(lowered.method_named("run"), 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                TYPE [7]
                ZVAL "seed"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "a"
                    ZVAL "a"
                null
                null
                null
        "#}
    );
}

/// ```php
/// $tags = \Sharp\Set::from($tags);
/// $states = \Sharp\Set::from($states);
/// !isset($tags[$name]) && ($tags += [$name => $name]);
/// isset($tags[$element#1 = \strtoupper($name)]) && $tags->delete($element#1) === null;
/// !isset($states[$status->value]) && ($states += [$status->value => $status]);
/// isset($states[\Lib\Calc::Closed->value]) && $states->delete(\Lib\Calc::Closed->value) === null;
/// $tags = [];
/// return isset($tags[$name]);
/// ```
///
/// Until the engine runs `Set` methods (php-sharp #57), each method that reads or changes an element by its key is
/// the PHP that does it on the array: `add` and `remove` give whether they changed the `Set`, and `remove` runs
/// `Sharp\Collection`'s `delete`. An element that runs something goes in through a hidden variable, so it runs once.
#[test]
fn set_methods_read_and_change_each_element_under_its_key() {
    assert_eq!(
        body_in(
            "bool run(Set<string> tags, Set<Calc> states, Calc status, string name)",
            "        tags.add(name);\n        tags.remove(strtoupper(name));\n        states.add(status);\n        states.remove(Calc.Closed);\n        tags.clear();\n        return tags.contains(name);\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "tags"
                STATIC_CALL
                  ZVAL "Sharp\\Set"
                  ZVAL "from"
                  ARG_LIST
                    VAR
                      ZVAL "tags"
              ASSIGN
                VAR
                  ZVAL "states"
                STATIC_CALL
                  ZVAL "Sharp\\Set"
                  ZVAL "from"
                  ARG_LIST
                    VAR
                      ZVAL "states"
              AND
                UNARY_OP [14]
                  ISSET
                    DIM
                      VAR
                        ZVAL "tags"
                      VAR
                        ZVAL "name"
                ASSIGN_OP [1]
                  VAR
                    ZVAL "tags"
                  ARRAY [3]
                    ARRAY_ELEM
                      VAR
                        ZVAL "name"
                      VAR
                        ZVAL "name"
              AND
                ISSET
                  DIM
                    VAR
                      ZVAL "tags"
                    ASSIGN
                      VAR
                        ZVAL "element#1"
                      CALL
                        ZVAL "strtoupper"
                        ARG_LIST
                          VAR
                            ZVAL "name"
                BINARY_OP [16]
                  METHOD_CALL
                    VAR
                      ZVAL "tags"
                    ZVAL "delete"
                    ARG_LIST
                      VAR
                        ZVAL "element#1"
                  ZVAL null
              AND
                UNARY_OP [14]
                  ISSET
                    DIM
                      VAR
                        ZVAL "states"
                      PROP
                        VAR
                          ZVAL "status"
                        ZVAL "value"
                ASSIGN_OP [1]
                  VAR
                    ZVAL "states"
                  ARRAY [3]
                    ARRAY_ELEM
                      VAR
                        ZVAL "status"
                      PROP
                        VAR
                          ZVAL "status"
                        ZVAL "value"
              AND
                ISSET
                  DIM
                    VAR
                      ZVAL "states"
                    PROP
                      CLASS_CONST
                        ZVAL "Lib\\Calc"
                        ZVAL "Closed"
                      ZVAL "value"
                BINARY_OP [16]
                  METHOD_CALL
                    VAR
                      ZVAL "states"
                    ZVAL "delete"
                    ARG_LIST
                      PROP
                        CLASS_CONST
                          ZVAL "Lib\\Calc"
                          ZVAL "Closed"
                        ZVAL "value"
                  ZVAL null
              ASSIGN
                VAR
                  ZVAL "tags"
                ARRAY [3]
              RETURN
                ISSET
                  DIM
                    VAR
                      ZVAL "tags"
                    VAR
                      ZVAL "name"
        "#}
    );
}

/// ```php
/// $tags = \Sharp\Set::from($tags);
/// $count = \count($tags);
/// $kept = $tags->filterValues($keep);
/// $tags->any($keep);
/// return \array_values($tags);
/// ```
///
/// `count` and `toList` are PHP's functions on the array. `filter` keeps each element under its key, as
/// `Sharp\Collection`'s `filterValues` does, and any other `Set` method is the `Sharp\Collection` method of its name.
#[test]
fn set_count_to_list_and_filter_are_the_php_that_reads_the_array() {
    assert_eq!(
        body_in(
            "List<string> run(Set<string> tags, Function<bool(string)> keep)",
            "        const count = tags.count();\n        const kept = tags.filter(keep);\n        tags.any(keep);\n        return tags.toList();\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "tags"
                STATIC_CALL
                  ZVAL "Sharp\\Set"
                  ZVAL "from"
                  ARG_LIST
                    VAR
                      ZVAL "tags"
              ASSIGN
                VAR
                  ZVAL "count"
                CALL
                  ZVAL "count"
                  ARG_LIST
                    VAR
                      ZVAL "tags"
              ASSIGN
                VAR
                  ZVAL "kept"
                METHOD_CALL
                  VAR
                    ZVAL "tags"
                  ZVAL "filterValues"
                  ARG_LIST
                    VAR
                      ZVAL "keep"
              METHOD_CALL
                VAR
                  ZVAL "tags"
                ZVAL "any"
                ARG_LIST
                  VAR
                    ZVAL "keep"
              RETURN
                CALL
                  ZVAL "array_values"
                  ARG_LIST
                    VAR
                      ZVAL "tags"
        "#}
    );
}

/// ```php
/// !isset($this->tags["vip"]) && ($this->tags += ["vip" => "vip"]);
/// isset(($receiver#1 = $this->next())->tags["old"]) && $receiver#1->tags->delete("old") === null;
/// $this->next()->tags = [];
/// !isset(($receiver#1 = $this->names())["x"]) && ($receiver#1 += ["x" => "x"]);
/// return isset($this->names()["vip"]);
/// ```
///
/// A `Set` method changes a property through the property, which runs its hooks. A receiver read twice whose object
/// runs something goes through a hidden variable its first read assigns, as PHP refuses a write to an assignment. A
/// change of any other value changes a copy of it.
#[test]
fn a_set_method_changes_a_property_through_the_property() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public Set<string> tags { get => field; set => field = value; } = [];\n\n    public bool run()\n    {\n        this.tags.add(\"vip\");\n        this.next().tags.remove(\"old\");\n        this.next().tags.clear();\n        this.names().add(\"x\");\n        return this.names().contains(\"vip\");\n    }\n\n    private Report next() => this;\n\n    private Set<string> names() => this.tags;\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              AND
                UNARY_OP [14]
                  ISSET
                    DIM
                      PROP
                        VAR
                          ZVAL "this"
                        ZVAL "tags"
                      ZVAL "vip"
                ASSIGN_OP [1]
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "tags"
                  ARRAY [3]
                    ARRAY_ELEM
                      ZVAL "vip"
                      ZVAL "vip"
              AND
                ISSET
                  DIM
                    PROP
                      ASSIGN
                        VAR
                          ZVAL "receiver#1"
                        METHOD_CALL
                          VAR
                            ZVAL "this"
                          ZVAL "next"
                          ARG_LIST
                      ZVAL "tags"
                    ZVAL "old"
                BINARY_OP [16]
                  METHOD_CALL
                    PROP
                      VAR
                        ZVAL "receiver#1"
                      ZVAL "tags"
                    ZVAL "delete"
                    ARG_LIST
                      ZVAL "old"
                  ZVAL null
              ASSIGN
                PROP
                  METHOD_CALL
                    VAR
                      ZVAL "this"
                    ZVAL "next"
                    ARG_LIST
                  ZVAL "tags"
                ARRAY [3]
              AND
                UNARY_OP [14]
                  ISSET
                    DIM
                      ASSIGN
                        VAR
                          ZVAL "receiver#1"
                        METHOD_CALL
                          VAR
                            ZVAL "this"
                          ZVAL "names"
                          ARG_LIST
                      ZVAL "x"
                ASSIGN_OP [1]
                  VAR
                    ZVAL "receiver#1"
                  ARRAY [3]
                    ARRAY_ELEM
                      ZVAL "x"
                      ZVAL "x"
              RETURN
                ISSET
                  DIM
                    METHOD_CALL
                      VAR
                        ZVAL "this"
                      ZVAL "names"
                      ARG_LIST
                    ZVAL "vip"
        "#}
    );
}

/// ```php
/// $names = $names === null ? null : \Sharp\Set::from($names);
/// ($names === null) ? null : (!isset($names["vip"]) && ($names += ["vip" => "vip"]));
/// ($other?->tags === null) ? null : (isset($other->tags["old"]) && $other->tags->delete("old") === null);
/// return ($names === null) ? null : isset($names["vip"]);
/// ```
///
/// A null-safe `Set` method tests its receiver first, as its first read, and gives null for a null receiver. The
/// method's reads after the test read the receiver without `?->`.
#[test]
fn a_null_safe_set_method_tests_its_receiver_first() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    private Set<string>? tags = null;\n\n    public bool? run(Set<string>? names, Report? other)\n    {\n        names?.add(\"vip\");\n        other?.tags?.remove(\"old\");\n        return names?.contains(\"vip\");\n    }\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "names"
                CONDITIONAL
                  BINARY_OP [16]
                    VAR
                      ZVAL "names"
                    ZVAL null
                  ZVAL null
                  STATIC_CALL
                    ZVAL "Sharp\\Set"
                    ZVAL "from"
                    ARG_LIST
                      VAR
                        ZVAL "names"
              CONDITIONAL [1]
                BINARY_OP [16]
                  VAR
                    ZVAL "names"
                  ZVAL null
                ZVAL null
                AND
                  UNARY_OP [14]
                    ISSET
                      DIM
                        VAR
                          ZVAL "names"
                        ZVAL "vip"
                  ASSIGN_OP [1]
                    VAR
                      ZVAL "names"
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "vip"
                        ZVAL "vip"
              CONDITIONAL [1]
                BINARY_OP [16]
                  NULLSAFE_PROP
                    VAR
                      ZVAL "other"
                    ZVAL "tags"
                  ZVAL null
                ZVAL null
                AND
                  ISSET
                    DIM
                      PROP
                        VAR
                          ZVAL "other"
                        ZVAL "tags"
                      ZVAL "old"
                  BINARY_OP [16]
                    METHOD_CALL
                      PROP
                        VAR
                          ZVAL "other"
                        ZVAL "tags"
                      ZVAL "delete"
                      ARG_LIST
                        ZVAL "old"
                    ZVAL null
              RETURN
                CONDITIONAL [1]
                  BINARY_OP [16]
                    VAR
                      ZVAL "names"
                    ZVAL null
                  ZVAL null
                  ISSET
                    DIM
                      VAR
                        ZVAL "names"
                      ZVAL "vip"
        "#}
    );
}

/// ```php
/// public readonly array $tags;
/// public function __construct(array $tags, array $ids)
/// {
///     $this->tags = \Sharp\Set::from($tags);
///     $ids = \Sharp\Set::from($ids);
/// }
/// ```
///
/// Plain PHP and a `List` argument pass a `Set` parameter any array of its elements, so a method with a body makes
/// each `Set` parameter a `Set` first. PHP sets a promoted property before the body runs, and a get-only one only
/// once, so a promoted `Set` parameter is a plain parameter beside the property it declares, `[129]`
/// `ZEND_ACC_PUBLIC | ZEND_ACC_READONLY`, which the body sets to the `Set`. A method without a body has no
/// statements.
#[test]
fn a_set_parameter_becomes_a_set_before_the_body_runs() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\ninterface Tagged\n{\n    void keep(Set<string> kept);\n}\n\nclass Order\n{\n    public Order(public Set<string> tags { get; }, Set<int> ids) {}\n}\n",
    );
    let body = |name: &str| lowered.render(lowered.child(lowered.method_named(name), 2));

    assert_eq!(
        property_groups(&lowered),
        [indoc! {r#"
            PROP_GROUP [129]
              TYPE [7]
              PROP_DECL
                PROP_ELEM
                  ZVAL "tags"
                  null
                  null
                  null
              null
        "#}]
    );
    assert_eq!(
        lowered.render(lowered.child(lowered.method_named("__construct"), 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                TYPE [7]
                ZVAL "tags"
                null
                null
                null
                null
              PARAM
                TYPE [7]
                ZVAL "ids"
                null
                null
                null
                null
        "#}
    );
    assert_eq!(body("keep"), "null\n");
    assert_eq!(
        body("__construct"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "tags"
                STATIC_CALL
                  ZVAL "Sharp\\Set"
                  ZVAL "from"
                  ARG_LIST
                    VAR
                      ZVAL "tags"
              ASSIGN
                VAR
                  ZVAL "ids"
                STATIC_CALL
                  ZVAL "Sharp\\Set"
                  ZVAL "from"
                  ARG_LIST
                    VAR
                      ZVAL "ids"
        "#}
    );
}

/// ```php
/// $prefix = "v";
/// $check = function (array $tags) use ($prefix) { $tags = \Sharp\Set::from($tags); return isset($tags[$prefix]); };
/// $size = function (array $tags) { $tags = \Sharp\Set::from($tags); return \count($tags); };
/// return $check($extra) ? $size($extra) : 0;
/// ```
///
/// A call of a lambda that declares a parameter `Set` passes it a list too, so a lambda makes each `Set` parameter a
/// `Set` first, as a method does. PHP's `fn` has no statements before its expression, so a lambda with an expression
/// body and a `Set` parameter is a `CLOSURE` whose `use` list captures what the binder says it captures.
#[test]
fn a_lambda_with_a_set_parameter_is_a_closure_that_makes_it_a_set_first() {
    assert_eq!(
        body_in(
            "int run(List<string> extra)",
            "        const prefix = \"v\";\n        const check = (Set<string> tags) => tags.contains(prefix);\n        const size = (Set<string> tags) => {\n            return tags.count();\n        };\n        return check(extra) ? size(extra) : 0;\n",
            &[],
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "prefix"
                ZVAL "v"
              ASSIGN
                VAR
                  ZVAL "check"
                CLOSURE "" @10-10
                  PARAM_LIST
                    PARAM
                      TYPE [7]
                      ZVAL "tags"
                      null
                      null
                      null
                      null
                  CLOSURE_USES
                    ZVAL "prefix"
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "tags"
                      STATIC_CALL
                        ZVAL "Sharp\\Set"
                        ZVAL "from"
                        ARG_LIST
                          VAR
                            ZVAL "tags"
                    RETURN
                      ISSET
                        DIM
                          VAR
                            ZVAL "tags"
                          VAR
                            ZVAL "prefix"
                  null
                  null
              ASSIGN
                VAR
                  ZVAL "size"
                CLOSURE "" @11-13
                  PARAM_LIST
                    PARAM
                      TYPE [7]
                      ZVAL "tags"
                      null
                      null
                      null
                      null
                  null
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "tags"
                      STATIC_CALL
                        ZVAL "Sharp\\Set"
                        ZVAL "from"
                        ARG_LIST
                          VAR
                            ZVAL "tags"
                    RETURN
                      CALL
                        ZVAL "count"
                        ARG_LIST
                          VAR
                            ZVAL "tags"
                  null
                  null
              RETURN
                CONDITIONAL
                  CALL
                    VAR
                      ZVAL "check"
                    ARG_LIST
                      VAR
                        ZVAL "extra"
                  CALL
                    VAR
                      ZVAL "size"
                    ARG_LIST
                      VAR
                        ZVAL "extra"
                  ZVAL 0
        "#}
    );
}

/// ```php
/// public function many(array ...$groups): int { $groups = \array_map(\Sharp\Set::from(...), $groups); return \count($groups); }
/// public static function op_Addition(\App\Tenant\Order $order, array $tags): \App\Tenant\Order
/// {
///     $tags = \Sharp\Set::from($tags);
///     return $order;
/// }
/// public function maybe(?array ...$groups): int
/// {
///     $groups = \array_map(fn ($set) => $set === null ? null : \Sharp\Set::from($set), $groups);
///     return \count($groups);
/// }
/// ```
///
/// A variadic `Set` parameter takes a list for each of its elements, so each element becomes a `Set`, under its
/// position or its name, and a null element of a nullable one stays null. An operator is a method, and makes each
/// `Set` parameter a `Set` first too.
#[test]
fn a_variadic_or_operator_set_parameter_becomes_a_set_before_the_body_runs() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Order\n{\n    public int many(Set<string> ...groups) => count(groups);\n\n    public static Order operator +(Order order, Set<string> tags) => order;\n\n    public int maybe(Set<string>? ...groups) => count(groups);\n}\n",
    );
    let body = |name: &str| lowered.render(lowered.child(lowered.method_named(name), 2));

    assert_eq!(
        body("many"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "groups"
                CALL
                  ZVAL "array_map"
                  ARG_LIST
                    STATIC_CALL
                      ZVAL "Sharp\\Set"
                      ZVAL "from"
                      CALLABLE_CONVERT
                    VAR
                      ZVAL "groups"
              RETURN
                CALL
                  ZVAL "count"
                  ARG_LIST
                    VAR
                      ZVAL "groups"
        "#}
    );
    assert_eq!(
        body("op_Addition"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "tags"
                STATIC_CALL
                  ZVAL "Sharp\\Set"
                  ZVAL "from"
                  ARG_LIST
                    VAR
                      ZVAL "tags"
              RETURN
                VAR
                  ZVAL "order"
        "#}
    );
    assert_eq!(
        body("maybe"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "groups"
                CALL
                  ZVAL "array_map"
                  ARG_LIST
                    ARROW_FUNC "" @9-9
                      PARAM_LIST
                        PARAM
                          null
                          ZVAL "set"
                          null
                          null
                          null
                          null
                      null
                      CONDITIONAL
                        BINARY_OP [16]
                          VAR
                            ZVAL "set"
                          ZVAL null
                        ZVAL null
                        STATIC_CALL
                          ZVAL "Sharp\\Set"
                          ZVAL "from"
                          ARG_LIST
                            VAR
                              ZVAL "set"
                      null
                      null
                    VAR
                      ZVAL "groups"
              RETURN
                CALL
                  ZVAL "count"
                  ARG_LIST
                    VAR
                      ZVAL "groups"
        "#}
    );
}

/// ```php
/// public array $tags = [] { set { $this->tags = []; $this->tags->add("x"); } }
/// ```
///
/// `field = []` leaves the property a `List<string>`, so the checker checks `field.add` as `List.add`, and the add
/// lowers as that call.
#[test]
fn an_emptied_list_field_takes_add_as_the_list_method() {
    let lowered = Lowered::new(
        "class Product\n{\n    public List<string> tags { get; set { field = []; field.add(\"x\"); } } = [];\n}\n",
    );

    assert_eq!(
        property_groups(&lowered),
        [indoc! {r#"
            PROP_GROUP [1]
              TYPE [7]
              PROP_DECL
                PROP_ELEM
                  ZVAL "tags"
                  ARRAY [3]
                  null
                  STMT_LIST
                    PROPERTY_HOOK "set" @3-3
                      null
                      null
                      STMT_LIST
                        ASSIGN
                          PROP
                            VAR
                              ZVAL "this"
                            ZVAL "tags"
                          ARRAY [3]
                        METHOD_CALL
                          PROP
                            VAR
                              ZVAL "this"
                            ZVAL "tags"
                          ZVAL "add"
                          ARG_LIST
                            ZVAL "x"
                      null
                      null
              null
        "#}]
    );
}

/// ```php
/// $standings = [$status];
/// $standings = [];
/// $standings->add($status);
/// ```
///
/// `standings = []` leaves the local a `List<Calc>`, whose `add` takes an element, so a case goes in as the case.
#[test]
fn a_backed_enum_value_goes_into_an_emptied_list_add_as_its_case() {
    assert_eq!(
        body_in(
            "void run(Calc status)",
            "        List<Calc> standings = [status];\n        standings = [];\n        standings.add(status);\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "standings"
                ARRAY [3]
                  ARRAY_ELEM
                    VAR
                      ZVAL "status"
                    null
              ASSIGN
                VAR
                  ZVAL "standings"
                ARRAY [3]
              METHOD_CALL
                VAR
                  ZVAL "standings"
                ZVAL "add"
                ARG_LIST
                  VAR
                    ZVAL "status"
        "#}
    );
}

/// ```php
/// $counts = [$status->value => 1];
/// $counts = [];
/// return $counts->get($status->value);
/// ```
///
/// `counts = [:]` leaves the local a `Map<Calc, int>`, whose `get` takes a key, so a case goes in as its `->value`.
#[test]
fn a_backed_enum_key_goes_into_an_emptied_map_get_as_its_backing_value() {
    assert_eq!(
        body_in(
            "int? run(Calc status)",
            "        Map<Calc, int> counts = [status: 1];\n        counts = [:];\n        return counts.get(status);\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "counts"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL 1
                    PROP
                      VAR
                        ZVAL "status"
                      ZVAL "value"
              ASSIGN
                VAR
                  ZVAL "counts"
                ARRAY [3]
              RETURN
                METHOD_CALL
                  VAR
                    ZVAL "counts"
                  ZVAL "get"
                  ARG_LIST
                    PROP
                      VAR
                        ZVAL "status"
                      ZVAL "value"
        "#}
    );
}

/// ```php
/// $statuses = [];
/// foreach ($statuses as $status => $n) {
///     $status = \Lib\Calc::from($status);
///     {
///         $extra += $n;
///     }
/// }
/// return $extra;
/// ```
///
/// `statuses = [:]` leaves the parameter a `Map<Calc, int>`, so the loop reads each key back as its case.
#[test]
fn an_emptied_map_loop_reads_each_key_back_as_its_case() {
    assert_eq!(
        body_in(
            "int run(int extra, Map<Calc, int> statuses)",
            "        statuses = [:];\n        for (const [status, n] of statuses) {\n            extra += n;\n        }\n        return extra;\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "statuses"
                ARRAY [3]
              FOREACH
                VAR
                  ZVAL "statuses"
                VAR
                  ZVAL "n"
                VAR
                  ZVAL "status"
                STMT_LIST
                  ASSIGN
                    VAR
                      ZVAL "status"
                    STATIC_CALL
                      ZVAL "Lib\\Calc"
                      ZVAL "from"
                      ARG_LIST
                        VAR
                          ZVAL "status"
                  STMT_LIST
                    ASSIGN_OP [1]
                      VAR
                        ZVAL "extra"
                      VAR
                        ZVAL "n"
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// `sizes = []` leaves the parameter a `List<int>`, whose indexes come from `entries()`, so `[k, v]` is refused before
/// the lowering reads a key type.
#[test]
fn an_emptied_list_loop_by_key_and_value_is_refused_before_lowering() {
    let lowered = Lowered::with(
        &method_with(
            "int run(int extra, List<int> sizes)",
            "        sizes = [];\n        for (const [index, size] of sizes) {\n            extra += index + size;\n        }\n        return extra;\n",
        ),
        &[],
    );

    assert_eq!(
        lowered.diagnostics(),
        ["10:37 compile error: `for (const [k, v] of x)` reads the keys of a `Map`, and this is a `List`."]
    );
    assert_eq!(lowered.nodes().len(), 0);
}

/// ```php
/// public function apply(\Closure $step, ?\Closure $other = null): \Closure { return $step; }
/// ```
///
/// A function type runs as PHP's `\Closure`, so its type is the full name `Closure` with `ZEND_NAME_FQ`, which is 0.
#[test]
fn a_function_type_is_the_closure_class() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public Function<int(string, int)> apply(Function<int(string, int)> step, Function<void()>? other = null) { return step; }\n}\n",
    );
    let method = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_METHOD).expect("a method");

    assert_eq!(
        lowered.render(lowered.child(method as u32, 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                ZVAL "Closure"
                ZVAL "step"
                null
                null
                null
                null
              PARAM
                ZVAL [256] "Closure"
                ZVAL "other"
                ZVAL null
                null
                null
                null
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(method as u32, 3)),
        indoc! {r#"
            ZVAL "Closure"
        "#}
    );
}

/// ```php
/// $numbers = [1, $extra];
/// $named = ['a' => 1, 2 => $numbers[0]];
/// $empty = [];
/// $named['b'] = $numbers[0];
/// $this->sizes['a'] = ($this->sizes['a'] ?? 0) + 1;
/// return $numbers[1];
/// ```
///
/// A list or map literal is an `ARRAY` with `ZEND_ARRAY_SYNTAX_SHORT`, which is 3, of `ARRAY_ELEM`s that take the value
/// before the key. An index is a `DIM` of the value and the key, read or written as its place in the tree decides.
#[test]
fn literals_are_short_arrays_and_an_index_is_a_dim() {
    assert_eq!(
        child_body(
            RUN,
            "        List<int> numbers = [1, extra];\n        const named = [\"a\": 1, 2: numbers[0]];\n        const Map<string, int> empty = [:];\n        named[\"b\"] = numbers[0];\n        this.sizes[\"a\"] = (this.sizes[\"a\"] ?? 0) + 1;\n        return numbers[1];\n",
            "<?php namespace App\\Tenant; class Base { /** @var array<string, int> */ public array $sizes = ['a' => 0]; }"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "numbers"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL 1
                    null
                  ARRAY_ELEM
                    VAR
                      ZVAL "extra"
                    null
              ASSIGN
                VAR
                  ZVAL "named"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL 1
                    ZVAL "a"
                  ARRAY_ELEM
                    DIM
                      VAR
                        ZVAL "numbers"
                      ZVAL 0
                    ZVAL 2
              ASSIGN
                VAR
                  ZVAL "empty"
                ARRAY [3]
              ASSIGN
                DIM
                  VAR
                    ZVAL "named"
                  ZVAL "b"
                DIM
                  VAR
                    ZVAL "numbers"
                  ZVAL 0
              ASSIGN
                DIM
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "sizes"
                  ZVAL "a"
                BINARY_OP [1]
                  COALESCE
                    DIM
                      PROP
                        VAR
                          ZVAL "this"
                        ZVAL "sizes"
                      ZVAL "a"
                    ZVAL 0
                  ZVAL 1
              RETURN
                DIM
                  VAR
                    ZVAL "numbers"
                  ZVAL 1
        "#}
    );
}

/// ```php
/// public function run(mixed $value, mixed $maybe): mixed { $held = $maybe; return $value; }
/// ```
///
/// `Any` and `Any?` are both PHP's `mixed`, which already holds null and refuses `?`, so neither carries
/// `ZEND_TYPE_NULLABLE`. `[1]` is `ZEND_NAME_NOT_FQ`.
#[test]
fn any_and_nullable_any_are_mixed() {
    let lowered = Lowered::new(
        "class Report\n{\n    public Any run(Any value, Any? maybe)\n    {\n        Any? held = maybe;\n        return value;\n    }\n}\n",
    );
    let method = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_METHOD).expect("a method");

    assert_eq!(
        lowered.render(lowered.child(method as u32, 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                ZVAL [1] "mixed"
                ZVAL "value"
                null
                null
                null
                null
              PARAM
                ZVAL [1] "mixed"
                ZVAL "maybe"
                null
                null
                null
                null
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(method as u32, 3)),
        indoc! {r#"
            ZVAL [1] "mixed"
        "#}
    );
    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "held"
                VAR
                  ZVAL "maybe"
              RETURN
                VAR
                  ZVAL "value"
        "#}
    );
}

/// ```php
/// public function run(): void
/// {
///     return;
/// }
/// ```
#[test]
fn a_void_method_returns_nothing() {
    let lowered = Lowered::new("class Report\n{\n    public void run()\n    {\n        return;\n    }\n}\n");

    assert_eq!(
        lowered.body(),
        indoc! {"
            STMT_LIST
              RETURN
                null
        "}
    );
}

/// ```php
/// $total = $extra * 2;
/// $label = 'one';
/// $total = 3;
/// ```
///
/// `[3]` is `ZEND_MUL`.
#[test]
fn let_and_const_locals_assign_their_variables() {
    assert_eq!(
        body(
            "        let total = extra * 2;\n        const label = 'one';\n        total = 3;\n        return total;\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "total"
                BINARY_OP [3]
                  VAR
                    ZVAL "extra"
                  ZVAL 2
              ASSIGN
                VAR
                  ZVAL "label"
                ZVAL "one"
              ASSIGN
                VAR
                  ZVAL "total"
                ZVAL 3
              RETURN
                VAR
                  ZVAL "total"
        "#}
    );
}

/// A written type only tells the checker the local's type, so a typed local lowers as `let` and `const` do.
///
/// ```php
/// $found = null;
/// $base = 2;
/// ```
#[test]
fn typed_locals_assign_their_variables_and_drop_the_type() {
    assert_eq!(
        body("        Calc? found = null;\n        const int base = 2;\n        return base;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "found"
                ZVAL null
              ASSIGN
                VAR
                  ZVAL "base"
                ZVAL 2
              RETURN
                VAR
                  ZVAL "base"
        "#}
    );
}

/// ```php
/// $this->count += 1;
/// ```
///
/// `[1]` is `ZEND_ADD`.
#[test]
fn this_count_plus_equals_one_is_a_compound_assignment_to_a_property_of_this() {
    assert_eq!(
        child_body(
            RUN,
            "        this.count += 1;\n        return 1;\n",
            "<?php namespace App\\Tenant; class Base { public int $count = 0; }",
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN_OP [1]
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "count"
                ZVAL 1
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// return \Lib\Calc::make(2);
/// ```
///
/// The class is its full name with `ZEND_NAME_FQ`, which is 0.
#[test]
fn calc_make_is_a_static_call_on_the_imported_class() {
    let calc = "<?php namespace Lib; final class Calc { public static function make(int $n): int { return $n; } }";

    assert_eq!(
        body_in(RUN, "        return Calc.make(2);\n", &[("src/Lib/Calc.php", calc)]),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "Lib\\Calc"
                  ZVAL "make"
                  ARG_LIST
                    ZVAL 2
        "#}
    );
}

/// ```php
/// private int|string $key = 1;
/// public function find(int|float|\Lib\Calc $id): \Lib\Calc|string { return "none"; }
/// ```
///
/// A union is one `TYPE_UNION` list of its types in the order they are written, as php-src's `union_type` rule
/// builds `int|float|\Lib\Calc`, on the line of its first type.
#[test]
fn a_union_type_is_one_type_union_list_of_its_types_in_written_order() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int|string key = 1;\n\n    public Calc|string find(\n        int|float|Calc id,\n    ) { return \"none\"; }\n}\n",
        &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")],
    );
    let members = lowered.child(lowered.child(lowered.root(), 2), 2);
    let field = lowered.child(lowered.child(members, 0), 0);
    let method = lowered.child(members, 1);
    let parameter = lowered.child(lowered.child(lowered.child(method, 0), 0), 0);
    let return_type = lowered.child(method, 3);

    assert_eq!(
        lowered.render(field),
        indoc! {r#"
            TYPE_UNION
              ZVAL [1] "int"
              ZVAL [1] "string"
        "#}
    );
    assert_eq!(
        lowered.render(parameter),
        indoc! {r#"
            TYPE_UNION
              ZVAL [1] "int"
              ZVAL [1] "float"
              ZVAL "Lib\\Calc"
        "#}
    );
    assert_eq!(
        lowered.render(return_type),
        indoc! {r#"
            TYPE_UNION
              ZVAL "Lib\\Calc"
              ZVAL [1] "string"
        "#}
    );
    assert_eq!([field, parameter, return_type].map(|union| lowered.nodes()[union as usize].line), [7, 10, 9]);
}

/// ```php
/// public function find(int|string|null $id): \Lib\Calc|string|null { return null; }
/// ```
///
/// A union in parentheses with `?` after it is the `TYPE_UNION` list of its types in the order they are written,
/// then the name `null` with `ZEND_NAME_NOT_FQ`, as php-src's `union_type` rule builds `int|string|null`. The list
/// carries no `ZEND_TYPE_NULLABLE`, and `null` is on the line of the `?`.
#[test]
fn a_nullable_union_is_its_type_union_list_with_null_last() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public (Calc|string)? find(\n        (int|string)? id,\n    ) { return null; }\n}\n",
        &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")],
    );
    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
    let method = lowered.child(lowered.child(lowered.child(lowered.root(), 2), 2), 0);
    let parameters = lowered.child(method, 0);
    let return_type = lowered.child(method, 3);

    assert_eq!(
        lowered.render(parameters),
        indoc! {r#"
            PARAM_LIST
              PARAM
                TYPE_UNION
                  ZVAL [1] "int"
                  ZVAL [1] "string"
                  ZVAL [1] "null"
                ZVAL "id"
                null
                null
                null
                null
        "#}
    );
    assert_eq!(
        lowered.render(return_type),
        indoc! {r#"
            TYPE_UNION
              ZVAL "Lib\\Calc"
              ZVAL [1] "string"
              ZVAL [1] "null"
        "#}
    );
    let parameter = lowered.child(lowered.child(parameters, 0), 0);
    let null_lines = [parameter, return_type].map(|union| lowered.nodes()[lowered.child(union, 2) as usize].line);
    assert_eq!(null_lines, [8, 7]);
}

/// ```php
/// public static function sum(string $label, int|float ...$values): int { return 0; }
/// ```
///
/// `[16]` is `ZEND_PARAM_VARIADIC`, which php-src's grammar adds to the parameter's attr.
#[test]
fn a_variadic_parameter_carries_the_variadic_flag() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public static int sum(string label, int|float ...values) { return 0; }\n}\n",
    );
    let method = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_METHOD).expect("a method");

    assert_eq!(
        lowered.render(lowered.child(method as u32, 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                ZVAL [1] "string"
                ZVAL "label"
                null
                null
                null
                null
              PARAM [16]
                TYPE_UNION
                  ZVAL [1] "int"
                  ZVAL [1] "float"
                ZVAL "values"
                null
                null
                null
                null
        "#}
    );
}

/// ```php
/// $pick = fn(int|string $key, int|\Lib\Calc|null $fallback, int ...$rest) => count($rest);
/// return $pick(1, null, ...$extra);
/// ```
///
/// A lambda's parameters lower as a method's do: a union is a `TYPE_UNION`, a nullable union ends with `null`, and a
/// variadic parameter carries `ZEND_PARAM_VARIADIC`, which is 16. A spread into a local's lambda is an `UNPACK`.
#[test]
fn a_lambda_takes_union_and_variadic_parameters_and_a_spread_call() {
    assert_eq!(
        body_in(
            "int run(List<int> extra)",
            "        const pick = (int|string key, (int|Calc)? fallback, int ...rest) => count(rest);\n        return pick(1, null, ...extra);\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "pick"
                ARROW_FUNC "" @9-9
                  PARAM_LIST
                    PARAM
                      TYPE_UNION
                        ZVAL [1] "int"
                        ZVAL [1] "string"
                      ZVAL "key"
                      null
                      null
                      null
                      null
                    PARAM
                      TYPE_UNION
                        ZVAL [1] "int"
                        ZVAL "Lib\\Calc"
                        ZVAL [1] "null"
                      ZVAL "fallback"
                      null
                      null
                      null
                      null
                    PARAM [16]
                      ZVAL [1] "int"
                      ZVAL "rest"
                      null
                      null
                      null
                      null
                  null
                  CALL
                    ZVAL "count"
                    ARG_LIST
                      VAR
                        ZVAL "rest"
                  null
                  null
              RETURN
                CALL
                  VAR
                    ZVAL "pick"
                  ARG_LIST
                    ZVAL 1
                    ZVAL null
                    UNPACK
                      VAR
                        ZVAL "extra"
        "#}
    );
}

/// ```php
/// $all = [...$extra, 1, ...\Lib\Calc::make()];
/// ```
///
/// A spread in a literal is an `UNPACK` of its value among the literal's elements, as php-src's grammar builds
/// `[...$extra]`.
#[test]
fn a_spread_in_a_literal_is_an_unpack_among_its_elements() {
    assert_eq!(
        body_in(
            "int run(List<int> extra)",
            "        const all = [...extra, 1, ...Calc.make()];\n        return 1;\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { /** @return list<int> */ public static function make(): array { return []; } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "all"
                ARRAY [3]
                  UNPACK
                    VAR
                      ZVAL "extra"
                  ARRAY_ELEM
                    ZVAL 1
                    null
                  UNPACK
                    STATIC_CALL
                      ZVAL "Lib\\Calc"
                      ZVAL "make"
                      ARG_LIST
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// \Lib\Calc::sum(...$extra);
/// $this->run(1, ...$extra);
/// $made = new \Lib\Calc(...$extra);
/// return max(...$extra);
/// ```
///
/// A spread argument is an `UNPACK` of its value, as php-src's grammar builds `...$extra`.
#[test]
fn a_spread_argument_is_an_unpack_of_its_value() {
    assert_eq!(
        body_in(
            "int run(int first, int ...extra)",
            "        Calc.sum(...extra);\n        this.run(1, ...extra);\n        const made = new Calc(...extra);\n        return max(first, ...extra);\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { public function __construct(int ...$n) {} public static function sum(int ...$n): int { return 0; } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              STATIC_CALL
                ZVAL "Lib\\Calc"
                ZVAL "sum"
                ARG_LIST
                  UNPACK
                    VAR
                      ZVAL "extra"
              METHOD_CALL
                VAR
                  ZVAL "this"
                ZVAL "run"
                ARG_LIST
                  ZVAL 1
                  UNPACK
                    VAR
                      ZVAL "extra"
              ASSIGN
                VAR
                  ZVAL "made"
                NEW
                  ZVAL "Lib\\Calc"
                  ARG_LIST
                    UNPACK
                      VAR
                        ZVAL "extra"
              RETURN
                CALL
                  ZVAL "max"
                  ARG_LIST
                    VAR
                      ZVAL "first"
                    UNPACK
                      VAR
                        ZVAL "extra"
        "#}
    );
}

/// ```php
/// return new \Lib\Calc($extra, rate: 2);
/// ```
///
/// The class is its full name with `ZEND_NAME_FQ`, which is 0.
#[test]
fn new_creates_the_imported_class_by_its_full_name() {
    assert_eq!(
        body_in(
            "Calc run(int extra)",
            "        return new Calc(extra, rate: 2);\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { public function __construct(int $start, int $rate) {} }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                NEW
                  ZVAL "Lib\\Calc"
                  ARG_LIST
                    VAR
                      ZVAL "extra"
                    NAMED_ARG
                      ZVAL "rate"
                      ZVAL 2
        "#}
    );
}

/// PHP has no class visibility, so a `public` class is the same class.
#[test]
fn a_public_class_is_a_class() {
    let public = Lowered::new("namespace App.Tenant;\n\npublic class Report\n{\n}\n");
    let internal = Lowered::new("namespace App.Tenant;\n\nclass Report\n{\n}\n");

    assert_eq!(public.tree(), internal.tree());
}

/// PHP has no enum visibility, so a `public` enum is the same enum.
#[test]
fn a_public_enum_is_an_enum() {
    let public = Lowered::new("namespace App.Tenant;\n\npublic enum Suit\n{\n    case Hearts;\n}\n");
    let internal = Lowered::new("namespace App.Tenant;\n\nenum Suit\n{\n    case Hearts;\n}\n");

    assert_eq!(public.tree(), internal.tree());
}

/// ```php
/// abstract class Shape { abstract public function area(): float; }
/// final class Unit { }
/// interface Measured { public function label(int $digits): string; }
/// ```
///
/// `[64]` is `ZEND_ACC_EXPLICIT_ABSTRACT_CLASS` on the class and `ZEND_ACC_ABSTRACT` on the method, `[32]`
/// `ZEND_ACC_FINAL`, `[1]` `ZEND_ACC_INTERFACE` on the interface, and an interface method is `ZEND_ACC_PUBLIC`, `[1]`,
/// as php-src's grammar writes it. An abstract method has no statement list.
#[test]
fn abstract_and_final_classes_and_interfaces_are_class_declarations_with_their_flags() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nabstract class Shape\n{\n    public abstract float area();\n}\n\nfinal class Unit\n{\n}\n\ninterface Measured\n{\n    string label(int digits);\n}\n",
    );

    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              NAMESPACE
                ZVAL "App\\Tenant"
                null
              CLASS [64] "Shape" @3-6
                null
                null
                STMT_LIST
                  METHOD [65] "area" @5-5
                    PARAM_LIST
                    null
                    null
                    ZVAL [1] "float"
                    null
                null
                null
              CLASS [32] "Unit" @8-10
                null
                null
                STMT_LIST
                null
                null
              CLASS [1] "Measured" @12-15
                null
                null
                STMT_LIST
                  METHOD [1] "label" @14-14
                    PARAM_LIST
                      PARAM
                        ZVAL [1] "int"
                        ZVAL "digits"
                        null
                        null
                        null
                        null
                    null
                    null
                    ZVAL [1] "string"
                    null
                null
                null
        "#}
    );
}

/// ```php
/// final class Text
/// {
///     public static function slug(string $title): string { return \Sharp\Internal\Text\Text\slug($title); }
/// }
/// ```
///
/// A static class is a final class, `[32]` `ZEND_ACC_FINAL`. An `extern` method is its `public static` method, `[17]`,
/// whose body returns the call of its native function: `Sharp\Internal`, then the class's full name after `Sharp\`,
/// then the method's name, with `ZEND_NAME_FQ`, which is 0.
#[test]
fn a_static_class_is_a_final_class_whose_extern_method_calls_its_native_function() {
    let lowered = Lowered::named(
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text/Text.sharp",
        "namespace Sharp.Text;\n\npublic static class Text\n{\n    public static extern string slug(string title);\n}\n",
    );

    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              NAMESPACE
                ZVAL "Sharp\\Text"
                null
              CLASS [32] "Text" @3-6
                null
                null
                STMT_LIST
                  METHOD [17] "slug" @5-5
                    PARAM_LIST
                      PARAM
                        ZVAL [1] "string"
                        ZVAL "title"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                      RETURN
                        CALL
                          ZVAL "Sharp\\Internal\\Text\\Text\\slug"
                          ARG_LIST
                            VAR
                              ZVAL "title"
                    ZVAL [1] "string"
                    null
                null
                null
        "#}
    );
}

/// ```php
/// final class Slug
/// {
///     public const SEPARATOR = '-';
///     public static function reset(): void { \Sharp\Internal\Slug\reset(); }
///     public static function of(string $title, string ...$words): string { return \Sharp\Internal\Slug\of($title, ...$words); }
/// }
/// ```
///
/// A `void` method calls its native function without `return`, and a variadic parameter forwards as a spread.
#[test]
fn an_extern_void_method_calls_without_return_and_forwards_a_variadic_parameter_as_a_spread() {
    let lowered = Lowered::named(
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Slug.sharp",
        "namespace Sharp;\n\npublic static class Slug\n{\n    public const string SEPARATOR = \"-\";\n\n    public static extern void reset();\n\n    public static extern string of(string title, string ...words);\n}\n",
    );
    let bodies: Vec<String> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_METHOD)
        .map(|(index, _)| lowered.render(lowered.child(index as u32, 2)))
        .collect();

    assert_eq!(
        bodies,
        [
            indoc! {r#"
                STMT_LIST
                  CALL
                    ZVAL "Sharp\\Internal\\Slug\\reset"
                    ARG_LIST
            "#},
            indoc! {r#"
                STMT_LIST
                  RETURN
                    CALL
                      ZVAL "Sharp\\Internal\\Slug\\of"
                      ARG_LIST
                        VAR
                          ZVAL "title"
                        UNPACK
                          VAR
                            ZVAL "words"
            "#},
        ]
    );
}

/// The checker refuses an `extern` method in a project file, so `check` refuses the file and nothing lowers it. `Text\Text`
/// would otherwise reach `Sharp\Text\Text`'s native body.
#[test]
fn an_extern_method_outside_the_library_is_refused_before_lowering() {
    for namespace in ["App", "Text"] {
        let lowered = Lowered::named(
            "src/Text.sharp",
            &format!(
                "namespace {namespace};\n\npublic static class Text\n{{\n    public static extern string slug(string title);\n}}\n"
            ),
        );

        assert_eq!(
            lowered.diagnostics(),
            ["5:33 compile error: Only the standard library declares native bodies: give `slug` a body."],
            "{namespace}"
        );
        assert!(lowered.nodes().is_empty(), "{namespace}");
    }
}

/// ```php
/// interface Linkable extends \Lib\Named { }
/// class Page extends \Lib\Entity implements \App\Tenant\Linkable { }
/// ```
///
/// A class's header name that the checker found to be a class is its `extends` name, and the rest are its
/// `implements` name list, each with `ZEND_NAME_FQ`, which is 0. An interface's header holds only interfaces, as PHP's
/// `extends` list does.
#[test]
fn a_header_is_the_parent_class_and_the_interface_name_list() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Entity;\nimport Lib.Named;\n\ninterface Linkable : Named\n{\n}\n\nclass Page : Entity, Linkable\n{\n}\n",
        &[("src/Lib/Entity.php", "<?php namespace Lib; interface Named {} class Entity {}")],
    );

    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              NAMESPACE
                ZVAL "App\\Tenant"
                null
              CLASS [1] "Linkable" @6-8
                null
                NAME_LIST
                  ZVAL "Lib\\Named"
                STMT_LIST
                null
                null
              CLASS "Page" @10-12
                ZVAL "Lib\\Entity"
                NAME_LIST
                  ZVAL "App\\Tenant\\Linkable"
                STMT_LIST
                null
                null
        "#}
    );
}

/// ```php
/// public function size(): int { return 1; }
/// #[\Override] public function name(): string { return 'thumbnail'; }
/// ```
///
/// `virtual` lowers to nothing, because PHP methods are open to overriding, and `override` lowers to `#[\Override]`,
/// so PHP checks at link time that a parent method exists. Its attribute group follows the method's own.
#[test]
fn virtual_lowers_to_nothing_and_override_to_the_override_attribute() {
    let lowered = Lowered::with(
        "class Thumbnail : Image\n{\n    public virtual int size()\n    {\n        return 1;\n    }\n\n    [Deprecated]\n    public override string name()\n    {\n        return \"thumbnail\";\n    }\n}\n",
        &[("src/Image.php", "<?php class Image { public function name(): string { return 'image'; } }")],
    );

    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              CLASS "Thumbnail" @1-13
                ZVAL "Image"
                null
                STMT_LIST
                  METHOD [1] "size" @3-6
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        ZVAL 1
                    ZVAL [1] "int"
                    null
                  METHOD [1] "name" @9-12
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        ZVAL "thumbnail"
                    ZVAL [1] "string"
                    ATTRIBUTE_LIST
                      ATTRIBUTE_GROUP
                        ATTRIBUTE
                          ZVAL "Deprecated"
                          null
                      ATTRIBUTE_GROUP
                        ATTRIBUTE
                          ZVAL "Override"
                          null
                null
                null
        "#}
    );
}

/// ```php
/// #[\Override] protected $table = 'orders';
/// #[\Override] public bool $timestamps = false;
/// ```
///
/// An override is a field with `#[\Override]`, so PHP checks at link time that the parent has the property. PHP
/// refuses a type on a property whose parent's has none, so an override of a property with no type has none either,
/// as decision 028 writes. An override of a typed property keeps its written type.
#[test]
fn an_override_of_a_property_with_no_type_has_no_type() {
    let lowered = Lowered::with(
        "class Order : Model\n{\n    protected override string table = \"orders\";\n    public override bool timestamps = false;\n}\n",
        &[("src/Model.php", "<?php class Model { protected $table = ''; public bool $timestamps = true; }")],
    );
    let class = lowered.child(lowered.root(), 1);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [2]
                null
                PROP_DECL
                  PROP_ELEM
                    ZVAL "table"
                    ZVAL "orders"
                    null
                    null
                ATTRIBUTE_LIST
                  ATTRIBUTE_GROUP
                    ATTRIBUTE
                      ZVAL "Override"
                      null
              PROP_GROUP [1]
                ZVAL [1] "bool"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "timestamps"
                    ZVAL false
                    null
                    null
                ATTRIBUTE_LIST
                  ATTRIBUTE_GROUP
                    ATTRIBUTE
                      ZVAL "Override"
                      null
        "#}
    );
}

/// The members of `RushOrder : Order : Model`, where the PHP# class `Order` overrides `table` with `string?` and
/// `model` is the plain PHP class `Model`.
fn rush_order_members(model: &str) -> String {
    let lowered = Lowered::with(
        "class RushOrder : Order\n{\n    protected override string? table = \"rush_orders\";\n}\n",
        &[
            ("src/Model.php", model),
            (
                "src/Order.sharp",
                "public class Order : Model\n{\n    protected override string? table = \"orders\";\n}\n",
            ),
        ],
    );

    lowered.render(lowered.child(lowered.child(lowered.root(), 1), 2))
}

/// ```php
/// #[\Override] protected $table = 'rush_orders';
/// ```
///
/// `Order::$table` runs untyped, because `Model::$table` has no type, so PHP refuses a type on an override of it too.
/// The root declaration of a property, at the bottom of the chain, decides the type of every override above it
/// (decision 028).
#[test]
fn an_override_of_an_override_of_a_property_with_no_type_has_no_type() {
    assert_eq!(
        rush_order_members("<?php class Model { protected $table = ''; }"),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [2]
                null
                PROP_DECL
                  PROP_ELEM
                    ZVAL "table"
                    ZVAL "rush_orders"
                    null
                    null
                ATTRIBUTE_LIST
                  ATTRIBUTE_GROUP
                    ATTRIBUTE
                      ZVAL "Override"
                      null
        "#}
    );
}

/// ```php
/// #[\Override] protected ?string $table = 'rush_orders';
/// ```
///
/// An override of an override keeps its written type when the root declaration has a type.
#[test]
fn an_override_of_an_override_of_a_typed_property_keeps_its_type() {
    assert_eq!(
        rush_order_members("<?php class Model { protected ?string $table = ''; }"),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [2]
                ZVAL [257] "string"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "table"
                    ZVAL "rush_orders"
                    null
                    null
                ATTRIBUTE_LIST
                  ATTRIBUTE_GROUP
                    ATTRIBUTE
                      ZVAL "Override"
                      null
        "#}
    );
}

/// ```php
/// class Account extends \Lib\Ledger
/// {
///     #[\Override]
///     public function total(string $label, int|float ...$amounts): int|float
///     {
///         return parent::total($label, ...$amounts);
///     }
/// }
/// ```
///
/// The classes slice and the signatures slice lower together: the header marks the class to find its parent in the
/// name list, the variadic union parameter carries `ZEND_PARAM_VARIADIC`, and the spread into `super` is an `UNPACK`
/// argument of the static call on `parent`.
#[test]
fn a_subclass_method_with_a_union_variadic_spreads_it_into_the_parent_method() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Ledger;\n\nclass Account : Ledger\n{\n    public override int|float total(string label, int|float ...amounts)\n    {\n        return super.total(label, ...amounts);\n    }\n}\n",
        &[(
            "src/Lib/Ledger.php",
            "<?php namespace Lib; class Ledger { public function total(string $label, int|float ...$amounts): int|float { return 0; } }",
        )],
    );

    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              NAMESPACE
                ZVAL "App\\Tenant"
                null
              CLASS "Account" @5-11
                ZVAL "Lib\\Ledger"
                null
                STMT_LIST
                  METHOD [1] "total" @7-10
                    PARAM_LIST
                      PARAM
                        ZVAL [1] "string"
                        ZVAL "label"
                        null
                        null
                        null
                        null
                      PARAM [16]
                        TYPE_UNION
                          ZVAL [1] "int"
                          ZVAL [1] "float"
                        ZVAL "amounts"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                      RETURN
                        STATIC_CALL
                          ZVAL [1] "parent"
                          ZVAL "total"
                          ARG_LIST
                            VAR
                              ZVAL "label"
                            UNPACK
                              VAR
                                ZVAL "amounts"
                    TYPE_UNION
                      ZVAL [1] "int"
                      ZVAL [1] "float"
                    ATTRIBUTE_LIST
                      ATTRIBUTE_GROUP
                        ATTRIBUTE
                          ZVAL "Override"
                          null
                null
                null
        "#}
    );
}

/// ```php
/// return parent::size(2);
/// ```
///
/// `super` is `parent`, a name php-src writes with `ZEND_NAME_NOT_FQ`, which is 1, so the call is a static call that
/// runs on the parent class.
#[test]
fn super_calls_are_static_calls_on_parent() {
    assert_eq!(
        child_body(
            RUN,
            "        return super.size(2);\n",
            "<?php namespace App\\Tenant; class Base { public function size(int $n): int { return $n; } }",
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL [1] "parent"
                  ZVAL "size"
                  ARG_LIST
                    ZVAL 2
        "#}
    );
}

/// ```php
/// public function __construct(\Lib\Row $row) {}
/// public static function fromSchema(\Lib\Row $row): static { return new static($row); }
/// public static function find(\Lib\Row $row): ?static { return $row->saved ? static::fromSchema($row) : null; }
/// public static function counted(\Lib\Row $row): static|int { return static::fromSchema($row); }
/// ```
///
/// `Self` is `static`. As a return type it is a `TYPE` node with `IS_STATIC`, which is 15, and `Self?` adds
/// `ZEND_TYPE_NULLABLE`, which is 256. As a class it is the name `static` with `ZEND_NAME_NOT_FQ`, which is 1, as
/// php-src's grammar builds `new static` and `static::`. `required` adds no flag, as PHP has no `required`.
#[test]
fn self_is_static_and_required_adds_no_flag() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Row;\n\npublic abstract class DatabaseEntity\n{\n    public required DatabaseEntity(Row row)\n    {\n    }\n\n    public static Self fromSchema(Row row)\n    {\n        return new Self(row);\n    }\n\n    public static Self? find(Row row) => row.saved ? Self.fromSchema(row) : null;\n\n    public static Self|int counted(Row row) => Self.fromSchema(row);\n}\n",
        &[("src/Lib/Row.php", "<?php namespace Lib; final class Row { public bool $saved = true; }")],
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.child(lowered.root(), 2), 2)),
        indoc! {r#"
            STMT_LIST
              METHOD [1] "__construct" @7-9
                PARAM_LIST
                  PARAM
                    ZVAL "Lib\\Row"
                    ZVAL "row"
                    null
                    null
                    null
                    null
                null
                STMT_LIST
                null
                null
              METHOD [17] "fromSchema" @11-14
                PARAM_LIST
                  PARAM
                    ZVAL "Lib\\Row"
                    ZVAL "row"
                    null
                    null
                    null
                    null
                null
                STMT_LIST
                  RETURN
                    NEW
                      ZVAL [1] "static"
                      ARG_LIST
                        VAR
                          ZVAL "row"
                TYPE [15]
                null
              METHOD [17] "find" @16-16
                PARAM_LIST
                  PARAM
                    ZVAL "Lib\\Row"
                    ZVAL "row"
                    null
                    null
                    null
                    null
                null
                STMT_LIST
                  RETURN
                    CONDITIONAL
                      PROP
                        VAR
                          ZVAL "row"
                        ZVAL "saved"
                      STATIC_CALL
                        ZVAL [1] "static"
                        ZVAL "fromSchema"
                        ARG_LIST
                          VAR
                            ZVAL "row"
                      ZVAL null
                TYPE [271]
                null
              METHOD [17] "counted" @18-18
                PARAM_LIST
                  PARAM
                    ZVAL "Lib\\Row"
                    ZVAL "row"
                    null
                    null
                    null
                    null
                null
                STMT_LIST
                  RETURN
                    STATIC_CALL
                      ZVAL [1] "static"
                      ZVAL "fromSchema"
                      ARG_LIST
                        VAR
                          ZVAL "row"
                TYPE_UNION
                  TYPE [15]
                  ZVAL [1] "int"
                null
        "#}
    );
}

/// ```php
/// return static::make(2);
/// ```
#[test]
fn self_calls_are_static_calls_on_static() {
    assert_eq!(
        child_body(
            RUN,
            "        return Self.make(2);\n",
            "<?php namespace App\\Tenant; class Base { public static function make(int $n): int { return $n; } }",
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL [1] "static"
                  ZVAL "make"
                  ARG_LIST
                    ZVAL 2
        "#}
    );
}

/// ```php
/// return \Lib\Calc::class;
/// ```
///
/// The class is its full name with `ZEND_NAME_FQ`, which is 0.
#[test]
fn typeof_is_the_class_name_of_the_imported_class() {
    assert_eq!(
        body_in(
            "string run(int extra)",
            "        return typeof(Calc);\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                CLASS_NAME
                  ZVAL "Lib\\Calc"
        "#}
    );
}

/// ```php
/// #[\Lib\Access(\Lib\Calc::class, role: \App\Tenant\Report::class)]
/// class Report
/// ```
///
/// `typeof(X)` in an attribute argument is the class name, which PHP takes as a constant expression.
#[test]
fn typeof_in_an_attribute_argument_is_the_class_name() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Access;\nimport Lib.Calc;\n\n[Access(typeof(Calc), role: typeof(Report))]\nclass Report\n{\n}\n",
        &[
            (
                "src/Lib/Access.php",
                "<?php namespace Lib; #[\\Attribute] final class Access { public function __construct(string $class, string $role) {} }",
            ),
            ("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}"),
        ],
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.child(lowered.root(), 2), 3)),
        indoc! {r#"
            ATTRIBUTE_LIST
              ATTRIBUTE_GROUP
                ATTRIBUTE
                  ZVAL "Lib\\Access"
                  ARG_LIST
                    CLASS_NAME
                      ZVAL "Lib\\Calc"
                    NAMED_ARG
                      ZVAL "role"
                      CLASS_NAME
                        ZVAL "App\\Tenant\\Report"
        "#}
    );
}

/// ```php
/// public const int MAX = 3;
/// protected const LIMIT = PHP_INT_MAX - 1;
/// ```
///
/// A constant is a class constant group of one constant, as php-src's grammar builds it: the constant list, no
/// attributes, then the type. `[1]` and `[2]` on the groups are `ZEND_ACC_PUBLIC` and `ZEND_ACC_PROTECTED`.
#[test]
fn a_class_constant_is_a_class_constant_group_of_one_constant() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public const int MAX = 3;\n    protected const LIMIT = PHP_INT_MAX - 1;\n}\n",
    );
    let class = lowered.child(lowered.root(), 2);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              CLASS_CONST_GROUP [1]
                CLASS_CONST_DECL
                  CONST_ELEM
                    ZVAL "MAX"
                    ZVAL 3
                    null
                null
                ZVAL [1] "int"
              CLASS_CONST_GROUP [2]
                CLASS_CONST_DECL
                  CONST_ELEM
                    ZVAL "LIMIT"
                    BINARY_OP [2]
                      CONST
                        ZVAL [1] "PHP_INT_MAX"
                      ZVAL 1
                    null
                null
                null
        "#}
    );
}

/// ```php
/// public const int|string KEY = 1;
/// public const int|string|null CODE = null;
/// ```
///
/// A constant's union type is the type union a parameter's is.
#[test]
fn a_union_typed_class_constant_has_its_type_union() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public const int|string KEY = 1;\n    public const (int|string)? CODE = null;\n}\n",
    );
    let class = lowered.child(lowered.root(), 2);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              CLASS_CONST_GROUP [1]
                CLASS_CONST_DECL
                  CONST_ELEM
                    ZVAL "KEY"
                    ZVAL 1
                    null
                null
                TYPE_UNION
                  ZVAL [1] "int"
                  ZVAL [1] "string"
              CLASS_CONST_GROUP [1]
                CLASS_CONST_DECL
                  CONST_ELEM
                    ZVAL "CODE"
                    ZVAL null
                    null
                null
                TYPE_UNION
                  ZVAL [1] "int"
                  ZVAL [1] "string"
                  ZVAL [1] "null"
        "#}
    );
}

/// ```php
/// private static int $made = 0;
/// public private(set) static string $last = "none";
/// ```
///
/// A static member's initial value is constant, so it is the default. `[20]` is `ZEND_ACC_PRIVATE | ZEND_ACC_STATIC`,
/// and `[4113]` is `ZEND_ACC_PUBLIC | ZEND_ACC_STATIC | ZEND_ACC_PRIVATE_SET`.
#[test]
fn a_static_field_or_property_is_a_static_property_group() {
    let lowered = Lowered::new(
        "class Report\n{\n    private static int made = 0;\n    public static string last { get; private set; } = \"none\";\n}\n",
    );
    let groups: Vec<(u32, String)> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_PROP_GROUP)
        .map(|(index, node)| (node.attr, lowered.render(lowered.child(index as u32, 1))))
        .collect();

    assert_eq!(
        groups,
        [
            (20, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"made\"\n    ZVAL 0\n    null\n    null\n".to_owned()),
            (4113, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"last\"\n    ZVAL \"none\"\n    null\n    null\n".to_owned()),
        ]
    );
}

/// ```php
/// \Lib\Calc::$rate = 2;
/// \Lib\Calc::$count++;
/// \Lib\Calc::$limit ??= 1;
/// ```
///
/// A static member written through its class is a static property, as php-src's grammar builds `Class::$name`. The
/// class is its full name with `ZEND_NAME_FQ`, which is 0.
#[test]
fn a_static_member_written_through_its_class_is_a_static_property() {
    assert_eq!(
        body_in(
            RUN,
            "        Calc.rate = 2;\n        Calc.count++;\n        Calc.limit ??= 1;\n        return 1;\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { public static int $rate = 0; public static int $count = 0; public static ?int $limit = null; }",
            )],
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                STATIC_PROP
                  ZVAL "Lib\\Calc"
                  ZVAL "rate"
                ZVAL 2
              POST_INC
                STATIC_PROP
                  ZVAL "Lib\\Calc"
                  ZVAL "count"
              ASSIGN_COALESCE
                STATIC_PROP
                  ZVAL "Lib\\Calc"
                  ZVAL "limit"
                ZVAL 1
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// #[\Lib\Field(\Lib\Mode::Write)] public const int MAX = \Lib\Calc::MAX;
/// private \Lib\Mode $mode = \Lib\Mode::Read;
/// public function run(\Lib\Mode $extra = \Lib\Mode::Read)
/// ```
///
/// A constant expression reads only constants and enum cases, as PHP's does, so a class member read in a constant's
/// value, a constant initial value, a parameter default and an attribute argument is a class constant fetch.
#[test]
fn a_class_member_read_in_a_constant_expression_is_a_class_constant() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\nimport Lib.Field;\nimport Lib.Mode;\n\nclass Report\n{\n    public const int MAX = Calc.MAX;\n    private Mode mode = Mode.Read;\n\n    [Field(Mode.Write)]\n    public int run(Mode extra = Mode.Read)\n    {\n        return 1;\n    }\n}\n",
        &[
            ("src/Lib/Calc.php", "<?php namespace Lib; final class Calc { public const int MAX = 3; }"),
            ("src/Lib/Mode.php", "<?php namespace Lib; enum Mode { case Read; case Write; }"),
            common::FIELD,
        ],
    );

    let class_constants = lowered.nodes().iter().filter(|node| node.kind == sharp_kind::SHARP_AST_CLASS_CONST).count();

    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
    assert_eq!(class_constants, 4);
}

/// ```php
/// $limit = \Lib\Calc::MAX;
/// $status = \App\Tenant\Status::Active;
/// $cents = \Lib\Calc::$rate->cents;
/// $twice = \Lib\Calc::twice(...);
/// return $limit + $cents;
/// ```
///
/// A class member read is the fetch of the member the checker found: a class constant fetch for a constant or an enum
/// case, a static property fetch for a static property, and a first-class callable of the static method for a method.
#[test]
fn a_class_member_read_is_the_fetch_of_the_member_kind_the_checker_found() {
    assert_eq!(
        body_in(
            RUN,
            "        const limit = Calc.MAX;\n        const status = Status.Active;\n        const cents = Calc.rate.cents;\n        const twice = Calc.twice;\n        return limit + cents;\n",
            &[
                ("src/Lib/Money.php", "<?php namespace Lib; final class Money { public int $cents = 0; }"),
                (
                    "src/Lib/Calc.php",
                    "<?php namespace Lib; final class Calc { public const int MAX = 3; public static Money $rate; public static function twice(int $value): int { return $value * 2; } }",
                ),
                (
                    "src/App/Tenant/Status.php",
                    "<?php namespace App\\Tenant; enum Status: string { case Active = 'a'; }"
                ),
            ]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "limit"
                CLASS_CONST
                  ZVAL "Lib\\Calc"
                  ZVAL "MAX"
              ASSIGN
                VAR
                  ZVAL "status"
                CLASS_CONST
                  ZVAL "App\\Tenant\\Status"
                  ZVAL "Active"
              ASSIGN
                VAR
                  ZVAL "cents"
                PROP
                  STATIC_PROP
                    ZVAL "Lib\\Calc"
                    ZVAL "rate"
                  ZVAL "cents"
              ASSIGN
                VAR
                  ZVAL "twice"
                STATIC_CALL
                  ZVAL "Lib\\Calc"
                  ZVAL "twice"
                  CALLABLE_CONVERT
              RETURN
                BINARY_OP [1]
                  VAR
                    ZVAL "limit"
                  VAR
                    ZVAL "cents"
        "#}
    );
}

/// ```php
/// $add = $calc->add(...);
/// $again = $this->count(...);
/// return ($this->scale)($extra) + $add(1, 2) + $again() + $calc->add(1, 2) + $calc->base;
/// ```
///
/// A member named without a call is the property the checker found, or else the method as a first-class callable. A
/// call is the method the checker found, or else a call of the function its property holds.
#[test]
fn a_member_is_the_property_or_method_the_checker_found_on_the_receiver() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private Function<int(int)> scale;\n\n    public Report(int factor)\n    {\n        this.scale = n => n * factor;\n    }\n\n    public int run(Calc calc, int extra)\n    {\n        const Function<int(int, int)> add = calc.add;\n        const Function<int()> again = this.count;\n        return this.scale(extra) + add(1, 2) + again() + calc.add(1, 2) + calc.base;\n    }\n\n    private int count() => 1;\n}\n",
        &[(
            "src/Lib/Calc.php",
            "<?php namespace Lib; final class Calc { public int $base = 0; public function add(int $a, int $b): int { return $a + $b; } }",
        )],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "add"
                METHOD_CALL
                  VAR
                    ZVAL "calc"
                  ZVAL "add"
                  CALLABLE_CONVERT
              ASSIGN
                VAR
                  ZVAL "again"
                METHOD_CALL
                  VAR
                    ZVAL "this"
                  ZVAL "count"
                  CALLABLE_CONVERT
              RETURN
                BINARY_OP [1]
                  BINARY_OP [1]
                    BINARY_OP [1]
                      BINARY_OP [1]
                        CALL
                          PROP
                            VAR
                              ZVAL "this"
                            ZVAL "scale"
                          ARG_LIST
                            VAR
                              ZVAL "extra"
                        CALL
                          VAR
                            ZVAL "add"
                          ARG_LIST
                            ZVAL 1
                            ZVAL 2
                      CALL
                        VAR
                          ZVAL "again"
                        ARG_LIST
                    METHOD_CALL
                      VAR
                        ZVAL "calc"
                      ZVAL "add"
                      ARG_LIST
                        ZVAL 1
                        ZVAL 2
                  PROP
                    VAR
                      ZVAL "calc"
                    ZVAL "base"
        "#}
    );
}

/// ```php
/// return $bag->size + $bag->count() + \strlen(\gettype($bag->other)) + \strlen(\gettype($bag->untagged()));
/// ```
///
/// A member a PHP class's `__get` serves is a property, and one its `__call` serves is a method, whether a
/// `@property` or `@method` tag names it or not.
#[test]
fn a_magic_property_and_a_magic_method_of_a_php_class_are_a_property_and_a_method() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Bag;\n\nclass Report\n{\n    public int run(Bag bag)\n    {\n        return bag.size + bag.count() + strlen(gettype(bag.other)) + strlen(gettype(bag.untagged()));\n    }\n}\n",
        &[(
            "src/Lib/Bag.php",
            "<?php namespace Lib; /** @property int $size\n * @method int count() */ final class Bag { public function __get(string $name): mixed { return 1; } public function __call(string $name, array $arguments): mixed { return 1; } }",
        )],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [1]
                  BINARY_OP [1]
                    BINARY_OP [1]
                      PROP
                        VAR
                          ZVAL "bag"
                        ZVAL "size"
                      METHOD_CALL
                        VAR
                          ZVAL "bag"
                        ZVAL "count"
                        ARG_LIST
                    CALL
                      ZVAL "strlen"
                      ARG_LIST
                        CALL
                          ZVAL "gettype"
                          ARG_LIST
                            PROP
                              VAR
                                ZVAL "bag"
                              ZVAL "other"
                  CALL
                    ZVAL "strlen"
                    ARG_LIST
                      CALL
                        ZVAL "gettype"
                        ARG_LIST
                          METHOD_CALL
                            VAR
                              ZVAL "bag"
                            ZVAL "untagged"
                            ARG_LIST
        "#}
    );
}

/// ```php
/// $again = ($other === null ? null : $other->count(...));
/// return (($nullsafe#1 = $this->next()) === null ? null : ($nullsafe#1->scale)(1)->count());
/// ```
///
/// PHP's `?->` cannot take a method as a first-class callable or call a property's function, so a null-safe method
/// value or property call is a conditional on the receiver. It wraps the rest of the chain, which a null receiver skips
/// as `?->` does. A receiver other than a local goes into a hidden variable, so it runs once.
#[test]
fn a_null_safe_method_value_or_property_call_is_a_conditional_over_the_rest_of_the_chain() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    private Function<Report(int)> scale;\n\n    public Report()\n    {\n        this.scale = n => this;\n    }\n\n    public int? run(Report? other)\n    {\n        const Function<int()>? again = other?.count;\n        return this.next()?.scale(1).count();\n    }\n\n    private Report? next() => null;\n\n    private int count() => 1;\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "again"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    VAR
                      ZVAL "other"
                    ZVAL null
                  ZVAL null
                  METHOD_CALL
                    VAR
                      ZVAL "other"
                    ZVAL "count"
                    CALLABLE_CONVERT
              RETURN
                CONDITIONAL [1]
                  BINARY_OP [16]
                    ASSIGN
                      VAR
                        ZVAL "nullsafe#1"
                      METHOD_CALL
                        VAR
                          ZVAL "this"
                        ZVAL "next"
                        ARG_LIST
                    ZVAL null
                  ZVAL null
                  METHOD_CALL
                    CALL
                      PROP
                        VAR
                          ZVAL "nullsafe#1"
                        ZVAL "scale"
                      ARG_LIST
                        ZVAL 1
                    ZVAL "count"
                    ARG_LIST
        "#}
    );
}

/// ```php
/// return (($nullsafe#1 = (($nullsafe#2 = $this->next()) === null ? null : ($nullsafe#2->scale)(1))) === null
///     ? null : ($nullsafe#1->scale)(2));
/// ```
///
/// A null-safe property call whose receiver is another one tests each receiver in its own hidden variable.
#[test]
fn nested_null_safe_property_calls_each_get_their_own_hidden_variable() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    private Function<Report?(int)> scale;\n\n    public Report()\n    {\n        this.scale = n => null;\n    }\n\n    public Report? run()\n    {\n        return this.next()?.scale(1)?.scale(2);\n    }\n\n    private Report? next() => null;\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                CONDITIONAL [1]
                  BINARY_OP [16]
                    ASSIGN
                      VAR
                        ZVAL "nullsafe#1"
                      CONDITIONAL [1]
                        BINARY_OP [16]
                          ASSIGN
                            VAR
                              ZVAL "nullsafe#2"
                            METHOD_CALL
                              VAR
                                ZVAL "this"
                              ZVAL "next"
                              ARG_LIST
                          ZVAL null
                        ZVAL null
                        CALL
                          PROP
                            VAR
                              ZVAL "nullsafe#2"
                            ZVAL "scale"
                          ARG_LIST
                            ZVAL 1
                    ZVAL null
                  ZVAL null
                  CALL
                    PROP
                      VAR
                        ZVAL "nullsafe#1"
                      ZVAL "scale"
                    ARG_LIST
                      ZVAL 2
        "#}
    );
}

/// ```php
/// $greeting = 'Hi ' . $name;
/// $greeting .= '!';
/// $half = \intdiv($count, 2);
/// $total = $count;
/// $total = \intdiv($total, 2);
/// $ratio = $rate / 2;
/// return $greeting;
/// ```
///
/// `+` on two strings joins them, as PHP's `.` does. `/` on two ints divides toward zero, as `\intdiv` does, and
/// `/=` on an int writes that quotient back. Any other `/` is PHP's.
#[test]
fn string_plus_is_a_concatenation_and_int_division_is_intdiv() {
    assert_eq!(
        body_in(
            "string run(string name, int count, float rate)",
            "        string greeting = \"Hi \" + name;\n        greeting += \"!\";\n        const half = count / 2;\n        int total = count;\n        total /= 2;\n        const ratio = rate / 2;\n        return greeting;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "greeting"
                BINARY_OP [8]
                  ZVAL "Hi "
                  VAR
                    ZVAL "name"
              ASSIGN_OP [8]
                VAR
                  ZVAL "greeting"
                ZVAL "!"
              ASSIGN
                VAR
                  ZVAL "half"
                CALL
                  ZVAL "intdiv"
                  ARG_LIST
                    VAR
                      ZVAL "count"
                    ZVAL 2
              ASSIGN
                VAR
                  ZVAL "total"
                VAR
                  ZVAL "count"
              ASSIGN
                VAR
                  ZVAL "total"
                CALL
                  ZVAL "intdiv"
                  ARG_LIST
                    VAR
                      ZVAL "total"
                    ZVAL 2
              ASSIGN
                VAR
                  ZVAL "ratio"
                BINARY_OP [4]
                  VAR
                    ZVAL "rate"
                  ZVAL 2
              RETURN
                VAR
                  ZVAL "greeting"
        "#}
    );
}

/// ```php
/// return 'Class: ' . \App\Tenant\Order::class;
/// ```
///
/// A class value is a string, so `+` joins it to a string, as the checker accepts it. `[8]` is `ZEND_CONCAT`.
#[test]
fn a_string_plus_a_class_value_is_a_concatenation() {
    assert_eq!(
        body_in(
            "string run()",
            "        return \"Class: \" + typeof(Order);\n",
            &[("src/App/Tenant/Order.php", "<?php namespace App\\Tenant; final class Order {}")]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [8]
                  ZVAL "Class: "
                  CLASS_NAME
                    ZVAL "App\\Tenant\\Order"
        "#}
    );
}

/// ```php
/// $label = 'Class: ';
/// $label .= \App\Tenant\Order::class;
/// return $label;
/// ```
///
/// `+=` of a class value on a string joins them, as `+` does. `[8]` is `ZEND_CONCAT`.
#[test]
fn a_string_plus_equals_a_class_value_is_a_concatenating_assignment() {
    assert_eq!(
        body_in(
            "string run()",
            "        string label = \"Class: \";\n        label += typeof(Order);\n        return label;\n",
            &[("src/App/Tenant/Order.php", "<?php namespace App\\Tenant; final class Order {}")]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "label"
                ZVAL "Class: "
              ASSIGN_OP [8]
                VAR
                  ZVAL "label"
                CLASS_NAME
                  ZVAL "App\\Tenant\\Order"
              RETURN
                VAR
                  ZVAL "label"
        "#}
    );
}

/// ```php
/// $this->total = \intdiv($this->total, 2);
/// $receiver#1->total = \intdiv(($receiver#1 = $this->next())->total, 2);
/// ```
///
/// `/=` on an int property reads and writes the property once each. A receiver that is not a local or `this` goes
/// into a hidden variable, which the read sets and the write reads, so the receiver runs once. PHP refuses an
/// assignment as the object of a written property, and reads the variable it writes through after the value.
#[test]
fn int_division_assignment_to_a_property_runs_its_receiver_once() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public int total { get => field; set => field = value; } = 8;\n\n    public void run()\n    {\n        this.total /= 2;\n        this.next().total /= 2;\n    }\n\n    private Report next() => this;\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "total"
                CALL
                  ZVAL "intdiv"
                  ARG_LIST
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "total"
                    ZVAL 2
              ASSIGN
                PROP
                  VAR
                    ZVAL "receiver#1"
                  ZVAL "total"
                CALL
                  ZVAL "intdiv"
                  ARG_LIST
                    PROP
                      ASSIGN
                        VAR
                          ZVAL "receiver#1"
                        METHOD_CALL
                          VAR
                            ZVAL "this"
                          ZVAL "next"
                          ARG_LIST
                      ZVAL "total"
                    ZVAL 2
        "#}
    );
}

/// ```php
/// $this->next()->total += 2;
/// $this->next()->total %= 3;
/// $this->next()->total |= 4;
/// $receiver#1->total = \intdiv(
///     ($receiver#1 = $this->next())->total,
///     $receiver#2->total = \intdiv(($receiver#2 = $this->next())->total, 2),
/// );
/// ```
///
/// Only `/=` on ints lowers to an assignment of its own. Every other compound operator on an int is PHP's own, which
/// runs a call receiver once. A `/=` inside the value of another keeps its own hidden variable, so the outer write
/// goes through the receiver the outer read set.
#[test]
fn a_compound_assignment_on_a_call_receiver_writes_through_the_receiver_its_read_set() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public int total { get => field; set => field = value; } = 8;\n\n    public void run()\n    {\n        this.next().total += 2;\n        this.next().total %= 3;\n        this.next().total |= 4;\n        this.next().total /= (this.next().total /= 2);\n    }\n\n    private Report next() => this;\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN_OP [1]
                PROP
                  METHOD_CALL
                    VAR
                      ZVAL "this"
                    ZVAL "next"
                    ARG_LIST
                  ZVAL "total"
                ZVAL 2
              ASSIGN_OP [5]
                PROP
                  METHOD_CALL
                    VAR
                      ZVAL "this"
                    ZVAL "next"
                    ARG_LIST
                  ZVAL "total"
                ZVAL 3
              ASSIGN_OP [9]
                PROP
                  METHOD_CALL
                    VAR
                      ZVAL "this"
                    ZVAL "next"
                    ARG_LIST
                  ZVAL "total"
                ZVAL 4
              ASSIGN
                PROP
                  VAR
                    ZVAL "receiver#1"
                  ZVAL "total"
                CALL
                  ZVAL "intdiv"
                  ARG_LIST
                    PROP
                      ASSIGN
                        VAR
                          ZVAL "receiver#1"
                        METHOD_CALL
                          VAR
                            ZVAL "this"
                          ZVAL "next"
                          ARG_LIST
                      ZVAL "total"
                    ASSIGN
                      PROP
                        VAR
                          ZVAL "receiver#2"
                        ZVAL "total"
                      CALL
                        ZVAL "intdiv"
                        ARG_LIST
                          PROP
                            ASSIGN
                              VAR
                                ZVAL "receiver#2"
                              METHOD_CALL
                                VAR
                                  ZVAL "this"
                                ZVAL "next"
                                ARG_LIST
                            ZVAL "total"
                          ZVAL 2
        "#}
    );
}

/// ```php
/// $save = $order->save(...);
/// return ($order->handler)($save());
/// ```
///
/// A member the class declares wins over one its `__get` or `__call` would serve, so a declared method is a method
/// value and a declared property holding a function is called as that function.
#[test]
fn a_declared_member_wins_over_a_magic_one() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Order;\n\nclass Report\n{\n    public int run(Order order)\n    {\n        const Function<int()> save = order.save;\n        return order.handler(save());\n    }\n}\n",
        &[(
            "src/Lib/Order.php",
            "<?php namespace Lib; final class Order { /** @var \\Closure(int): int */ public \\Closure $handler; public function __construct() { $this->handler = fn (int $n): int => $n; } public function save(): int { return 1; } public function __get(string $name): mixed { return null; } public function __call(string $name, array $arguments): mixed { return null; } }",
        )],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "save"
                METHOD_CALL
                  VAR
                    ZVAL "order"
                  ZVAL "save"
                  CALLABLE_CONVERT
              RETURN
                CALL
                  PROP
                    VAR
                      ZVAL "order"
                    ZVAL "handler"
                  ARG_LIST
                    CALL
                      VAR
                        ZVAL "save"
                      ARG_LIST
        "#}
    );
}

/// ```php
/// return \strlen(\gettype($bag->handler())) + \strlen(\gettype($bag->untagged()));
/// ```
///
/// A call `__call` serves is a method call of its name, as the analysis checked it, even beside a private property of
/// that name, which the call cannot read.
#[test]
fn a_call_only_call_serves_is_a_method_call_beside_a_private_property_of_its_name() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Bag;\n\nclass Report\n{\n    public int run(Bag bag)\n    {\n        return strlen(gettype(bag.handler())) + strlen(gettype(bag.untagged()));\n    }\n}\n",
        &[(
            "src/Lib/Bag.php",
            "<?php namespace Lib; final class Bag { private \\Closure $handler; public function __construct() { $this->handler = fn (): int => 1; } public function __call(string $name, array $arguments): mixed { return 1; } }",
        )],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [1]
                  CALL
                    ZVAL "strlen"
                    ARG_LIST
                      CALL
                        ZVAL "gettype"
                        ARG_LIST
                          METHOD_CALL
                            VAR
                              ZVAL "bag"
                            ZVAL "handler"
                            ARG_LIST
                  CALL
                    ZVAL "strlen"
                    ARG_LIST
                      CALL
                        ZVAL "gettype"
                        ARG_LIST
                          METHOD_CALL
                            VAR
                              ZVAL "bag"
                            ZVAL "untagged"
                            ARG_LIST
        "#}
    );
}

/// ```php
/// \Lib\Calc::remember($extra);
/// \Lib\Order::where($extra);
/// ```
///
/// A static call `__callStatic` serves is a static call of its name, as the analysis checked it, whether the class
/// declares `__callStatic` or a `@method static` tag names the method a subclass's `__callStatic` serves.
#[test]
fn a_static_call_call_static_serves_is_a_static_call_of_its_name() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\nimport Lib.Order;\n\nclass Report\n{\n    public void run(int extra)\n    {\n        Calc.remember(extra);\n        Order.where(extra);\n    }\n}\n",
        &[(
            "src/Lib/Calc.php",
            "<?php namespace Lib; final class Calc { public static function __callStatic(string $name, array $arguments): mixed { return null; } } /** @method static int where(int $id) */ class Order {}",
        )],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              STATIC_CALL
                ZVAL "Lib\\Calc"
                ZVAL "remember"
                ARG_LIST
                  VAR
                    ZVAL "extra"
              STATIC_CALL
                ZVAL "Lib\\Order"
                ZVAL "where"
                ARG_LIST
                  VAR
                    ZVAL "extra"
        "#}
    );
}

/// ```php
/// return ($holder->handler)($extra) + ($this->scale)($extra);
/// ```
///
/// A call of a property holding a function calls the function the property holds, as the analysis checked it, whether
/// the property is a PHP `\Closure` an interface declares or a PHP# `Function` the class declares.
#[test]
fn a_call_of_a_property_holding_a_function_calls_the_function() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.HasHandler;\n\nclass Report\n{\n    private Function<int(int)> scale;\n\n    public Report()\n    {\n        this.scale = n => n;\n    }\n\n    public int run(HasHandler holder, int extra)\n    {\n        return holder.handler(extra) + this.scale(extra);\n    }\n}\n",
        &[(
            "src/Lib/HasHandler.php",
            "<?php namespace Lib; interface HasHandler { /** @var \\Closure(int): int */ public \\Closure $handler { get; } }",
        )],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [1]
                  CALL
                    PROP
                      VAR
                        ZVAL "holder"
                      ZVAL "handler"
                    ARG_LIST
                      VAR
                        ZVAL "extra"
                  CALL
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "scale"
                    ARG_LIST
                      VAR
                        ZVAL "extra"
        "#}
    );
}

/// ```php
/// return ($this->format)($amount);
/// ```
///
/// A property holding an object whose class declares `__invoke` is called as that property, though the analysis also
/// records the `__invoke` it runs.
#[test]
fn a_call_of_a_property_holding_an_invokable_object_calls_the_property() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Formatter;\n\nclass Report\n{\n    private Formatter format;\n\n    public Report(Formatter format)\n    {\n        this.format = format;\n    }\n\n    public int run(int amount)\n    {\n        return this.format(amount);\n    }\n}\n",
        &[(
            "src/Lib/Formatter.php",
            "<?php namespace Lib; final class Formatter { public function __invoke(int $amount): int { return $amount; } }",
        )],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "format"
                  ARG_LIST
                    VAR
                      ZVAL "amount"
        "#}
    );
}

/// ```php
/// $total = $doc->total(...);
/// return $total() + $doc->count();
/// ```
///
/// A receiver that can be one of several classes reads the member each of them declares, when it is the same kind of
/// member on every one.
#[test]
fn a_member_of_a_union_receiver_is_the_kind_every_class_declares() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Invoice;\nimport Lib.Order;\n\nclass Report\n{\n    public int run(Order|Invoice doc)\n    {\n        const Function<int()> total = doc.total;\n        return total() + doc.count();\n    }\n}\n",
        &[
            (
                "src/Lib/Order.php",
                "<?php namespace Lib; final class Order { public function total(): int { return 1; } public function count(): int { return 1; } }",
            ),
            (
                "src/Lib/Invoice.php",
                "<?php namespace Lib; final class Invoice { public function total(): int { return 2; } public function count(): int { return 2; } }",
            ),
        ],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "total"
                METHOD_CALL
                  VAR
                    ZVAL "doc"
                  ZVAL "total"
                  CALLABLE_CONVERT
              RETURN
                BINARY_OP [1]
                  CALL
                    VAR
                      ZVAL "total"
                    ARG_LIST
                  METHOD_CALL
                    VAR
                      ZVAL "doc"
                    ZVAL "count"
                    ARG_LIST
        "#}
    );
}

/// ```php
/// $type = \Lib\Calc::class;
/// return \Lib\Calc::MAX + $type::MAX + $type::$count + \strlen($type::defaultTag());
/// ```
///
/// A member read through a class value is the fetch of the member kind on the class the value holds, as `Class.y` is:
/// `typeof(X).y` names the class, and a local holding a class value is the class of the fetch.
#[test]
fn a_member_read_through_a_class_value_is_the_fetch_of_its_kind_on_that_class() {
    assert_eq!(
        body_in(
            RUN,
            "        const type = typeof(Calc);\n        return typeof(Calc).MAX + type.MAX + type.count + strlen(type.defaultTag());\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; abstract class Calc { public const int MAX = 3; public static int $count = 0; public static function defaultTag(): string { return 'div'; } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "type"
                CLASS_NAME
                  ZVAL "Lib\\Calc"
              RETURN
                BINARY_OP [1]
                  BINARY_OP [1]
                    BINARY_OP [1]
                      CLASS_CONST
                        ZVAL "Lib\\Calc"
                        ZVAL "MAX"
                      CLASS_CONST
                        VAR
                          ZVAL "type"
                        ZVAL "MAX"
                    STATIC_PROP
                      VAR
                        ZVAL "type"
                      ZVAL "count"
                  CALL
                    ZVAL "strlen"
                    ARG_LIST
                      STATIC_CALL
                        VAR
                          ZVAL "type"
                        ZVAL "defaultTag"
                        ARG_LIST
        "#}
    );
}

/// ```php
/// $type = $extra > 0 ? \Lib\Calc::class : null;
/// $max = ($type === null ? null : $type::MAX);
/// $count = ($type === null ? null : $type::$count);
/// $tag = ($type === null ? null : $type::defaultTag());
/// return ($max ?? 0) + ($count ?? 0) + \strlen($tag ?? '');
/// ```
///
/// A null-safe read or call through a class value is the conditional a null-safe method value is, which reads the
/// local itself and runs the static fetch or call when it holds a class.
#[test]
fn a_null_safe_member_read_through_a_class_value_is_a_conditional_over_the_static_fetch() {
    assert_eq!(
        body_in(
            RUN,
            "        const type = extra > 0 ? typeof(Calc) : null;\n        const max = type?.MAX;\n        const count = type?.count;\n        const tag = type?.defaultTag();\n        return (max ?? 0) + (count ?? 0) + strlen(tag ?? \"\");\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; abstract class Calc { public const int MAX = 3; public static int $count = 0; public static function defaultTag(): string { return 'div'; } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "type"
                CONDITIONAL
                  GREATER
                    VAR
                      ZVAL "extra"
                    ZVAL 0
                  CLASS_NAME
                    ZVAL "Lib\\Calc"
                  ZVAL null
              ASSIGN
                VAR
                  ZVAL "max"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    VAR
                      ZVAL "type"
                    ZVAL null
                  ZVAL null
                  CLASS_CONST
                    VAR
                      ZVAL "type"
                    ZVAL "MAX"
              ASSIGN
                VAR
                  ZVAL "count"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    VAR
                      ZVAL "type"
                    ZVAL null
                  ZVAL null
                  STATIC_PROP
                    VAR
                      ZVAL "type"
                    ZVAL "count"
              ASSIGN
                VAR
                  ZVAL "tag"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    VAR
                      ZVAL "type"
                    ZVAL null
                  ZVAL null
                  STATIC_CALL
                    VAR
                      ZVAL "type"
                    ZVAL "defaultTag"
                    ARG_LIST
              RETURN
                BINARY_OP [1]
                  BINARY_OP [1]
                    COALESCE
                      VAR
                        ZVAL "max"
                      ZVAL 0
                    COALESCE
                      VAR
                        ZVAL "count"
                      ZVAL 0
                  CALL
                    ZVAL "strlen"
                    ARG_LIST
                      COALESCE
                        VAR
                          ZVAL "tag"
                        ZVAL ""
        "#}
    );
}

/// ```php
/// return \array_replace($defaults, ['root' => 0], $overrides);
/// ```
///
/// A `Map` literal with a spread is `\array_replace` of its parts in order, each spread `Map` and each run of entries
/// as a literal, so every key stays and a later one wins, where PHP's `...` renumbers int keys.
#[test]
fn a_map_spread_is_array_replace_of_the_literal_parts_in_order() {
    assert_eq!(
        body_in(
            "Map<string, int> run(Map<string, int> defaults, Map<string, int> overrides)",
            "        return [...defaults, \"root\": 0, ...overrides];\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "array_replace"
                  ARG_LIST
                    VAR
                      ZVAL "defaults"
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL 0
                        ZVAL "root"
                    VAR
                      ZVAL "overrides"
        "#}
    );
}

/// ```php
/// $type = \Lib\Calc::class;
/// $tag = $type::defaultTag(...);
/// return \strlen($tag());
/// ```
///
/// A static method read through a class value without a call is its first-class callable, as `Class.m` is.
#[test]
fn a_static_method_read_through_a_class_value_is_its_first_class_callable() {
    assert_eq!(
        body_in(
            RUN,
            "        const type = typeof(Calc);\n        const Function<string()> tag = type.defaultTag;\n        return strlen(tag());\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; abstract class Calc { public static function defaultTag(): string { return 'div'; } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "type"
                CLASS_NAME
                  ZVAL "Lib\\Calc"
              ASSIGN
                VAR
                  ZVAL "tag"
                STATIC_CALL
                  VAR
                    ZVAL "type"
                  ZVAL "defaultTag"
                  CALLABLE_CONVERT
              RETURN
                CALL
                  ZVAL "strlen"
                  ARG_LIST
                    CALL
                      VAR
                        ZVAL "tag"
                      ARG_LIST
        "#}
    );
}

/// A PHP class whose `__get` and `__callStatic` serve undeclared members, with a static property and a static method.
const MAGIC_ORDER: (&str, &str) = (
    "src/Lib/Order.php",
    "<?php namespace Lib; final class Order { public static int $max = 3; public static function twice(int $n): int { return $n * 2; } public function __get(string $name): mixed { return null; } public static function __callStatic(string $name, array $arguments): mixed { return null; } }",
);

/// ```php
/// \Lib\Calc::remember($extra);
/// ```
///
/// A static call only `__callStatic` serves is a static call of its name, as PHP calls a facade's methods.
#[test]
fn a_static_call_only_call_static_serves_is_a_static_call() {
    assert_eq!(
        body_in(
            "void run(int extra)",
            "        Calc.remember(extra);\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { public static function __callStatic(string $name, array $arguments): mixed { return null; } }",
            )],
        ),
        indoc! {r#"
            STMT_LIST
              STATIC_CALL
                ZVAL "Lib\\Calc"
                ZVAL "remember"
                ARG_LIST
                  VAR
                    ZVAL "extra"
        "#}
    );
}

/// ```php
/// \Lib\Order::where($extra);
/// ```
///
/// `Order.where(x)` calls the static method `__callStatic` serves, though `__get` would serve a property read of the
/// same name, as PHP runs `Order::where($x)`. So the call lowers to a static call of its name, and a standard library
/// method whose body is that call gives an inline form.
#[test]
fn a_static_call_beside_a_get_is_the_static_method_call_static_serves() {
    let library = "namespace Sharp;\n\nimport Lib.Order;\n\npublic class Text\n{\n    public static Any? found(int id) => Order.where(id);\n}\n";
    let forms: Vec<String> = common::checked(TEXT.0, library, &[MAGIC_ORDER], inline_forms)
        .expect("the library file is checked")
        .into_iter()
        .map(|(name, _)| String::from_utf8_lossy(&name).into_owned())
        .collect();

    assert_eq!(forms, ["sharp\\text::found"]);
    assert_eq!(
        Lowered::with(
            "namespace App.Tenant;\n\nimport Lib.Order;\n\nclass Report\n{\n    public void run(int extra)\n    {\n        Order.where(extra);\n    }\n}\n",
            &[MAGIC_ORDER],
        )
        .body(),
        indoc! {r#"
            STMT_LIST
              STATIC_CALL
                ZVAL "Lib\\Order"
                ZVAL "where"
                ARG_LIST
                  VAR
                    ZVAL "extra"
        "#}
    );
}

/// `Class.y` reads only a member the class declares. One its `__callStatic` would serve is refused, so the lowering of
/// `Class.y` never meets a magic member.
#[test]
fn a_static_member_only_a_magic_method_serves_is_refused() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Order;\n\nclass Report\n{\n    public void run()\n    {\n        const where = Order.where;\n    }\n}\n",
        &[MAGIC_ORDER],
    );

    assert_eq!(lowered.diagnostics(), ["9:29 compile error: `Order.where` does not exist."]);
}

/// A constant expression reads only constants and enum cases, as PHP's does, so a parameter default of a static
/// property or a static method is refused, and the lowering never emits a fetch PHP refuses there.
#[test]
fn a_static_property_or_method_in_a_constant_expression_is_refused() {
    let property = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Order;\n\nclass Report\n{\n    public int run(int limit = Order.max)\n    {\n        return limit;\n    }\n}\n",
        &[MAGIC_ORDER],
    );
    let method = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Order;\n\nclass Report\n{\n    public int run(Function<int(int)> twice = Order.twice)\n    {\n        return twice(1);\n    }\n}\n",
        &[MAGIC_ORDER],
    );

    assert_eq!(property.diagnostics(), ["7:38 compile error: Class-like constant `max` does not exist."]);
    assert_eq!(method.diagnostics(), ["7:53 compile error: Class-like constant `twice` does not exist."]);
}

/// ```php
/// return \App\Tenant\Report::make();
/// ```
#[test]
fn a_class_of_the_same_namespace_is_called_by_its_full_name() {
    assert_eq!(
        child_body(
            RUN,
            "        return Report.make();\n",
            "<?php namespace App\\Tenant; class Base { public static function make(): int { return 0; } }",
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "App\\Tenant\\Report"
                  ZVAL "make"
                  ARG_LIST
        "#}
    );
}

/// ```php
/// return \Sharp\Int::parse($extra) + \Sharp\Float::tryParse($extra);
/// ```
#[test]
fn int_and_float_are_the_classes_of_the_sharp_namespace() {
    assert_eq!(
        body_in(
            "float run(string extra)",
            "        return Int.parse(extra) + Float.tryParse(extra);\n",
            &[common::INT, common::FLOAT]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [1]
                  STATIC_CALL
                    ZVAL "Sharp\\Int"
                    ZVAL "parse"
                    ARG_LIST
                      VAR
                        ZVAL "extra"
                  STATIC_CALL
                    ZVAL "Sharp\\Float"
                    ZVAL "tryParse"
                    ARG_LIST
                      VAR
                        ZVAL "extra"
        "#}
    );
}

/// ```php
/// $here = new \Sharp\Position(__FILE__, 9, 22, 'App.Tenant.Report.run');
/// $later = fn () => new \Sharp\Position(__FILE__, 10, 29, 'App.Tenant.Report.run');
/// return 1;
/// ```
///
/// Spec section 27: `Position.current()` in a body gives the position where it is written: the file, the line and the
/// byte column of `Position`, and the enclosing method's full dotted name, which a lambda shares. The method's name
/// ignores case, as PHP's does.
#[test]
fn position_current_is_a_new_position_of_its_file_line_column_and_enclosing_method() {
    let source = "        const here = Position.current();\n        const later = () => Position.current();\n        return 1;\n";

    assert_eq!(
        body(source),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "here"
                NEW
                  ZVAL "Sharp\\Position"
                  ARG_LIST
                    MAGIC_CONST [347]
                    ZVAL 9
                    ZVAL 22
                    ZVAL "App.Tenant.Report.run"
              ASSIGN
                VAR
                  ZVAL "later"
                ARROW_FUNC "" @10-10
                  PARAM_LIST
                  null
                  NEW
                    ZVAL "Sharp\\Position"
                    ARG_LIST
                      MAGIC_CONST [347]
                      ZVAL 10
                      ZVAL 29
                      ZVAL "App.Tenant.Report.run"
                  null
                  null
              RETURN
                ZVAL 1
        "#}
    );
    assert_eq!(body(&source.replace("current", "CURRENT")), body(source));
}

/// ```php
/// public function __construct()
/// {
///     $this->created = new \Sharp\Position(__FILE__, 3, 32, 'Report.Report');
/// }
/// ```
///
/// An initial value runs at the start of the constructor, so `Position.current()` there names the constructor, whether
/// or not the class writes one. A file without a namespace names the class alone.
#[test]
fn position_current_in_an_initial_value_names_the_constructor_the_class_gets() {
    let lowered = Lowered::new("class Report\n{\n    private Position created = Position.current();\n}\n");
    let class = lowered.child(lowered.root(), 1);
    let constructor = lowered.child(lowered.child(class, 2), 1);

    assert_eq!(
        lowered.render(lowered.child(constructor, 2)),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "created"
                NEW
                  ZVAL "Sharp\\Position"
                  ARG_LIST
                    MAGIC_CONST [347]
                    ZVAL 3
                    ZVAL 32
                    ZVAL "Report.Report"
        "#}
    );
}

/// ```php
/// public function __construct()
/// {
///     $this->created = new \Sharp\Position(__FILE__, 5, 32, 'App.Tenant.Report.Report');
///     $here = new \Sharp\Position(__FILE__, 9, 22, 'App.Tenant.Report.Report');
/// }
/// public function run(): int
/// {
///     return (new \Sharp\Position(__FILE__, 14, 16, 'App.Tenant.Report.run'))->line;
/// }
/// ```
///
/// A written constructor and its initial values name the constructor, and the method after it names itself.
#[test]
fn position_current_in_a_written_constructor_and_its_initial_values_names_the_constructor() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    private Position created = Position.current();\n\n    public Report()\n    {\n        const here = Position.current();\n    }\n\n    public int run()\n    {\n        return Position.current().line;\n    }\n}\n",
    );
    let members = lowered.child(lowered.child(lowered.root(), 2), 2);

    assert_eq!(
        lowered.render(lowered.child(lowered.child(members, 1), 2)),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "created"
                NEW
                  ZVAL "Sharp\\Position"
                  ARG_LIST
                    MAGIC_CONST [347]
                    ZVAL 5
                    ZVAL 32
                    ZVAL "App.Tenant.Report.Report"
              ASSIGN
                VAR
                  ZVAL "here"
                NEW
                  ZVAL "Sharp\\Position"
                  ARG_LIST
                    MAGIC_CONST [347]
                    ZVAL 9
                    ZVAL 22
                    ZVAL "App.Tenant.Report.Report"
        "#}
    );
    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                PROP
                  NEW
                    ZVAL "Sharp\\Position"
                    ARG_LIST
                      MAGIC_CONST [347]
                      ZVAL 14
                      ZVAL 16
                      ZVAL "App.Tenant.Report.run"
                  ZVAL "line"
        "#}
    );
}

/// ```php
/// public string $folder { get => (new \Sharp\Position(__FILE__, 5, 29, 'App.Tenant.Report.folder'))->directory; }
/// public function line(): int { return (new \Sharp\Position(__FILE__, 12, 26, 'App.Tenant.Status.line'))->line; }
/// ```
///
/// A computed property's body names the property, and an enum's method names the enum and the method.
#[test]
fn position_current_in_a_computed_property_or_an_enum_method_names_its_member() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public string folder => Position.current().directory;\n}\n\nenum Status\n{\n    case Active;\n\n    public int line() => Position.current().line;\n}\n",
    );

    assert_eq!(positions(&lowered), ["5:29 App.Tenant.Report.folder", "12:26 App.Tenant.Status.line"]);
}

/// ```php
/// public string $slug { get { return (new \Sharp\Position(__FILE__, 5, 39, 'App.Tenant.Report.slug'))->function; } }
/// ```
///
/// A `get` body names its property, as C#'s `[CallerMemberName]` names a property for code in its accessors.
#[test]
fn position_current_in_a_get_body_names_its_property() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public string slug { get { return Position.current().function; } }\n}\n",
    );

    assert_eq!(positions(&lowered), ["5:39 App.Tenant.Report.slug"]);
}

/// ```php
/// public string $folder { get => (new \Sharp\Position(__FILE__, 5, 29, 'App.Tenant.Report.folder'))->directory; }
/// public string $title { set { $this->title = (new \Sharp\Position(__FILE__, 6, 46, 'App.Tenant.Report.title'))->function; } }
/// ```
///
/// A `set` body names its property, and the computed property before it does not leak its name into it.
#[test]
fn position_current_in_a_set_body_names_its_property() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public string folder => Position.current().directory;\n    public string title { get; set { field = Position.current().function; } }\n}\n",
    );

    assert_eq!(positions(&lowered), ["5:29 App.Tenant.Report.folder", "6:46 App.Tenant.Report.title"]);
}

/// ```php
/// public function __construct(
///     public string $code { get { return (new \Sharp\Position(__FILE__, 5, 53, 'App.Tenant.Report.code'))->function; } set { $this->code = $value; } },
/// ) {
///     $here = new \Sharp\Position(__FILE__, 7, 22, 'App.Tenant.Report.Report');
/// }
/// ```
///
/// A property declared on a constructor parameter names itself in its bodies, and the constructor's body after it
/// names the constructor again.
#[test]
fn position_current_in_a_constructor_parameters_get_body_names_its_property_and_the_body_after_names_the_constructor() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public Report(public string code { get { return Position.current().function; } set => field = value; })\n    {\n        const here = Position.current();\n    }\n}\n",
    );

    assert_eq!(positions(&lowered), ["5:53 App.Tenant.Report.code", "7:22 App.Tenant.Report.Report"]);
}

/// ```php
/// public function __construct()
/// {
///     $this->created = new \Sharp\Position(__FILE__, 6, 32, 'App.Tenant.Report.Report');
/// }
/// ```
///
/// An initial value declared after a computed property still runs in the constructor, so it names the constructor.
#[test]
fn position_current_in_an_initial_value_after_a_computed_property_names_the_constructor() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public string folder => Position.current().directory;\n    private Position created = Position.current();\n}\n",
    );

    assert_eq!(positions(&lowered), ["6:32 App.Tenant.Report.Report", "5:29 App.Tenant.Report.folder"]);
}

/// Each `Position.current()` as `line:column function`, in the order the bridge lowers them.
fn positions(lowered: &Lowered) -> Vec<String> {
    assert_eq!(lowered.diagnostics(), Vec::<String>::new(), "the source lowers");

    lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_NEW)
        .map(|(index, node)| {
            let arguments = lowered.child(index as u32, 1);
            let argument = |index| &lowered.nodes()[lowered.child(arguments, index) as usize];

            format!("{}:{} {}", node.line, argument(2).long_value, lowered.text(argument(3).text))
        })
        .collect()
}

/// ```php
/// $folder = (new \Sharp\Position(__FILE__, 9, 24, 'App.Tenant.Report.run'))->directory;
/// return 1;
/// ```
#[test]
fn a_member_of_position_current_is_a_property_read_on_the_new_position() {
    assert_eq!(
        body("        const folder = Position.current().directory;\n        return 1;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "folder"
                PROP
                  NEW
                    ZVAL "Sharp\\Position"
                    ARG_LIST
                      MAGIC_CONST [347]
                      ZVAL 9
                      ZVAL 24
                      ZVAL "App.Tenant.Report.run"
                  ZVAL "directory"
              RETURN
                ZVAL 1
        "#}
    );
}

/// A compiled file runs on other machines than the one that compiled it, so one workspace file lowers to the same bytes
/// from any workspace root, and no text of it names the file: `Position.current()` reads the file where it runs.
#[test]
fn a_file_lowers_to_the_same_bytes_from_any_workspace_root_and_names_no_path() {
    let code = method("        const here = Position.current();\n        return 1;\n");
    let encoded = ["/home/ci/build", "/srv/app/releases/42"].map(|root| {
        let file = File::new(
            Cow::Borrowed(b"src/Report.sharp"),
            FileType::Host,
            Some(Path::new(root).join("src/Report.sharp")),
            Cow::Owned(code.clone().into_bytes()),
        );
        let unit = common::checked_file(&file, &[], &InlineForms::default(), lower).expect("the source lowers");
        assert!(
            !unit.texts().windows(b"Report.sharp".len()).any(|text| text == b"Report.sharp"),
            "no text names the file"
        );

        encode(&unit, &file.contents, [0; 16], &[], &[])
    });

    assert_eq!(encoded[0], encoded[1]);
}

/// ```php
/// $settings = new \Sharp\Environment();
/// $lines = \Sharp\List::wrap($extra);
/// return 1;
/// ```
#[test]
fn environment_and_list_are_the_classes_of_the_sharp_namespace() {
    assert_eq!(
        body(
            "        const settings = new Environment();\n        const lines = List.wrap(extra);\n        return 1;\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "settings"
                NEW
                  ZVAL "Sharp\\Environment"
                  ARG_LIST
              ASSIGN
                VAR
                  ZVAL "lines"
                STATIC_CALL
                  ZVAL "Sharp\\List"
                  ZVAL "wrap"
                  ARG_LIST
                    VAR
                      ZVAL "extra"
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// \App\Position::current();
/// ```
///
/// Only the standard library's `Position` gives a position. An imported or declared `Position` is the file's own class.
#[test]
fn an_imported_or_declared_position_is_an_ordinary_static_call() {
    let imported = Lowered::with(
        "namespace App.Tenant;\n\nimport App.Position;\n\nclass Report\n{\n    public int run()\n    {\n        Position.current();\n        return 1;\n    }\n}\n",
        &[(
            "src/App/Position.php",
            "<?php namespace App; final class Position { public static function current(): int { return 1; } }",
        )],
    );
    let declared = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public int run()\n    {\n        Position.current();\n        return 1;\n    }\n}\n\nclass Position\n{\n    public static int current() => 1;\n}\n",
    );

    for (lowered, class) in [(imported, r#""App\\Position""#), (declared, r#""App\\Tenant\\Position""#)] {
        assert_eq!(
            lowered.body(),
            format!(
                "STMT_LIST\n  STATIC_CALL\n    ZVAL {class}\n    ZVAL \"current\"\n    ARG_LIST\n  RETURN\n    ZVAL 1\n"
            )
        );
    }
}

/// ```php
/// return $this->total($extra, rate: 2)->value;
/// ```
#[test]
fn member_calls_and_reads_on_values_are_instance_access() {
    assert_eq!(
        child_body(
            RUN,
            "        return this.total(extra, rate: 2).value;\n",
            "<?php namespace App\\Tenant; final class Total { public int $value = 0; } class Base { public function total(int $a, int $rate): Total { return new Total(); } }",
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                PROP
                  METHOD_CALL
                    VAR
                      ZVAL "this"
                    ZVAL "total"
                    ARG_LIST
                      VAR
                        ZVAL "extra"
                      NAMED_ARG
                        ZVAL "rate"
                        ZVAL 2
                  ZVAL "value"
        "#}
    );
}

/// ```php
/// return \Lib\Calc::make()->add($extra);
/// ```
#[test]
fn a_call_on_a_static_call_result_is_an_instance_call() {
    assert_eq!(
        body_in(
            RUN,
            "        return Calc.make().add(extra);\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { public static function make(): self { return new self(); } public function add(int $a): int { return $a; } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                METHOD_CALL
                  STATIC_CALL
                    ZVAL "Lib\\Calc"
                    ZVAL "make"
                    ARG_LIST
                  ZVAL "add"
                  ARG_LIST
                    VAR
                      ZVAL "extra"
        "#}
    );
}

/// ```php
/// $a = "line\n"; $b = 'raw\n'; $c = 1.5; $d = 0x10; $e = 9223372036854775808; $f = true; $g = false; $h = null;
/// ```
///
/// `9223372036854775808` overflows `int`, so PHP reads it as a float.
#[test]
fn literals_are_zvals_of_their_php_value() {
    assert_eq!(
        body(
            "        let a = \"line\\n\";\n        let b = 'raw\\n';\n        let c = 1.5;\n        let d = 0x10;\n        let e = 9223372036854775808;\n        let f = true;\n        let g = false;\n        string? h = null;\n        return 1;\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                ZVAL "line\n"
              ASSIGN
                VAR
                  ZVAL "b"
                ZVAL "raw\\n"
              ASSIGN
                VAR
                  ZVAL "c"
                ZVAL 1.5
              ASSIGN
                VAR
                  ZVAL "d"
                ZVAL 16
              ASSIGN
                VAR
                  ZVAL "e"
                ZVAL 9.223372036854776e18
              ASSIGN
                VAR
                  ZVAL "f"
                ZVAL true
              ASSIGN
                VAR
                  ZVAL "g"
                ZVAL false
              ASSIGN
                VAR
                  ZVAL "h"
                ZVAL null
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// return $a + $a - \intdiv($a * $a, $a) % $a;
/// ```
///
/// `[1]`, `[2]`, `[3]` and `[5]` are `ZEND_ADD`, `ZEND_SUB`, `ZEND_MUL` and `ZEND_MOD`. `/` on two ints is `\intdiv`.
#[test]
fn arithmetic_operators_are_binary_ops() {
    assert_eq!(
        body("        let a = 1;\n        return a + a - a * a / a % a;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                ZVAL 1
              RETURN
                BINARY_OP [2]
                  BINARY_OP [1]
                    VAR
                      ZVAL "a"
                    VAR
                      ZVAL "a"
                  BINARY_OP [5]
                    CALL
                      ZVAL "intdiv"
                      ARG_LIST
                        BINARY_OP [3]
                          VAR
                            ZVAL "a"
                          VAR
                            ZVAL "a"
                        VAR
                          ZVAL "a"
                    VAR
                      ZVAL "a"
        "#}
    );
}

/// ```php
/// $b = $a === $a; $b = $a !== $a; $b = $calc === $calc; $b = $calc !== $calc; $b = $a < $a; $b = $a <= $a;
/// $b = $a > $a; $b = $a >= $a;
/// ```
///
/// `==` and `!=` compare values strictly, so they are `ZEND_IS_IDENTICAL` and `ZEND_IS_NOT_IDENTICAL`, `[16]` and
/// `[17]`, as `===` and `!==` on a class instance are. `[20]` and `[21]` are `ZEND_IS_SMALLER` and
/// `ZEND_IS_SMALLER_OR_EQUAL`. `>` and `>=` have kinds of their own.
#[test]
fn comparison_operators_are_the_kinds_php_gives_them() {
    let tree = body_in(
        "int run(Calc calc)",
        "        let a = 1;\n        let b = a == a;\n        b = a != a;\n        b = calc === calc;\n        b = calc !== calc;\n        b = a < a;\n        b = a <= a;\n        b = a > a;\n        b = a >= a;\n        return a;\n",
        &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")],
    );

    assert_eq!(
        assigned_values(&tree),
        [
            "ZVAL 1",
            "BINARY_OP [16]",
            "BINARY_OP [17]",
            "BINARY_OP [16]",
            "BINARY_OP [17]",
            "BINARY_OP [20]",
            "BINARY_OP [21]",
            "GREATER",
            "GREATER_EQUAL",
        ]
    );
}

/// The kind of the value each top-level assignment of a body's tree assigns, in order.
fn assigned_values(tree: &str) -> Vec<&str> {
    let lines: Vec<&str> = tree.lines().collect();

    lines.windows(4).filter(|window| window[0] == "  ASSIGN").map(|window| window[3].trim_start()).collect()
}

/// ```php
/// $a = $extra; $b = $a === null; $b = null !== $a; $b = $a === $a;
/// ```
///
/// `==` and `!=` on a nullable value are lifted: null equals only null, so `0 == null` is false. `[16]` and `[17]` are
/// `ZEND_IS_IDENTICAL` and `ZEND_IS_NOT_IDENTICAL`, which compare `null` that way.
#[test]
fn equality_with_null_is_identity() {
    let tree = body_in(
        "int run(int? extra)",
        "        let a = extra;\n        let b = a == null;\n        b = null != a;\n        b = a == a;\n        return 1;\n",
        &[],
    );

    assert_eq!(assigned_values(&tree), ["VAR", "BINARY_OP [16]", "BINARY_OP [17]", "BINARY_OP [16]"]);
}

/// ```php
/// $b = $text === "01"; $b = $text !== "01"; $b = $total === 1; $b = $status === Status::Open;
/// ```
///
/// Spec section 19: `==` compares values strictly, so `"1" == "01"` and `"1e3" == "1000"` are false, as PHP's `===`
/// makes them. `[16]` and `[17]` are `ZEND_IS_IDENTICAL` and `ZEND_IS_NOT_IDENTICAL`.
#[test]
fn equality_of_strings_ints_and_enums_is_identity() {
    let tree = body_in(
        "int run(string text, int total, Status status)",
        "        let b = text == \"01\";\n        b = text != \"01\";\n        b = total == 1;\n        b = status == Status.Open;\n        return 1;\n",
        &[("src/App/Tenant/Status.php", "<?php namespace App\\Tenant; enum Status { case Open; case Closed; }")],
    );

    assert_eq!(assigned_values(&tree), ["BINARY_OP [16]", "BINARY_OP [17]", "BINARY_OP [16]", "BINARY_OP [16]"]);
}

/// ```php
/// $b = (float) $count === $ratio; $b = $ratio !== (float) $count;
/// ```
///
/// Spec section 19: numbers compare by value, so `1 == 1.0` is true. A side that may hold an int is cast to float, and
/// the floats compare with `===`. `[5]` is `IS_DOUBLE`.
#[test]
fn equality_of_an_int_and_a_float_compares_them_as_floats() {
    assert_eq!(
        body_in(
            "int run(int count, float ratio)",
            "        let b = count == ratio;\n        b = ratio != count;\n        return 1;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "b"
                BINARY_OP [16]
                  CAST [5]
                    VAR
                      ZVAL "count"
                  VAR
                    ZVAL "ratio"
              ASSIGN
                VAR
                  ZVAL "b"
                BINARY_OP [17]
                  VAR
                    ZVAL "ratio"
                  CAST [5]
                    VAR
                      ZVAL "count"
              RETURN
                ZVAL 1
        "#}
    );
}

/// A value pattern compares as `==` does, so an int subject matches the float `1.0` as a float.
#[test]
fn an_int_subject_matches_a_float_pattern_as_a_float() {
    assert_eq!(
        body_in(
            "int run(int count)",
            "        return match (count) {\n            == 1.0 => 1,\n            default => 2,\n        };\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                MATCH
                  ZVAL true
                  MATCH_ARM_LIST
                    MATCH_ARM
                      EXPR_LIST
                        BINARY_OP [16]
                          CAST [5]
                            VAR
                              ZVAL "count"
                          ZVAL 1.0
                      ZVAL 1
                    MATCH_ARM
                      null
                      ZVAL 2
        "#}
    );
}

/// ```php
/// $b = (($operand#1 = $count) === null) === (($operand#2 = $ratio) === null)
///     && ($operand#1 === null || (float) $operand#1 === $operand#2);
/// $b = !((($operand#1 = $count) === null) === (($operand#2 = 1.5) === null)
///     && ($operand#1 === null || (float) $operand#1 === $operand#2));
/// ```
///
/// A nullable side is lifted: null equals only null, and `(float) null` never compares. Each side runs once into a
/// hidden `$operand#N`, so `int? == float` with a null int is false and two nulls are equal.
#[test]
fn equality_of_a_nullable_int_and_a_float_compares_null_before_the_floats() {
    assert_eq!(
        body_in(
            "int run(int? count, float? ratio)",
            "        let b = count == ratio;\n        b = count != 1.5;\n        return 1;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "b"
                AND
                  BINARY_OP [16]
                    BINARY_OP [16]
                      ASSIGN
                        VAR
                          ZVAL "operand#1"
                        VAR
                          ZVAL "count"
                      ZVAL null
                    BINARY_OP [16]
                      ASSIGN
                        VAR
                          ZVAL "operand#2"
                        VAR
                          ZVAL "ratio"
                      ZVAL null
                  OR
                    BINARY_OP [16]
                      VAR
                        ZVAL "operand#1"
                      ZVAL null
                    BINARY_OP [16]
                      CAST [5]
                        VAR
                          ZVAL "operand#1"
                      VAR
                        ZVAL "operand#2"
              ASSIGN
                VAR
                  ZVAL "b"
                UNARY_OP [14]
                  AND
                    BINARY_OP [16]
                      BINARY_OP [16]
                        ASSIGN
                          VAR
                            ZVAL "operand#1"
                          VAR
                            ZVAL "count"
                        ZVAL null
                      BINARY_OP [16]
                        ASSIGN
                          VAR
                            ZVAL "operand#2"
                          ZVAL 1.5
                        ZVAL null
                    OR
                      BINARY_OP [16]
                        VAR
                          ZVAL "operand#1"
                        ZVAL null
                      BINARY_OP [16]
                        CAST [5]
                          VAR
                            ZVAL "operand#1"
                        VAR
                          ZVAL "operand#2"
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// return (($operand#1 = $a) === null) === (($operand#2 = ((($operand#3 = $b) === null) === (($operand#4 = $c) === null)
///     && ($operand#3 === null || (float) $operand#3 === $operand#4)) ? 1 : 2.5) === null)
///     && ($operand#1 === null || (float) $operand#1 === (float) $operand#2);
/// ```
///
/// An int/float `==` inside another one's operand takes its own `$operand#N`s, so it never overwrites the outer `a`.
#[test]
fn a_nested_nullable_int_and_float_equality_takes_its_own_hidden_variables() {
    assert_eq!(
        body_in("bool run(int? a, int? b, float c)", "        return a == (b == c ? 1 : 2.5);\n", &[]),
        indoc! {r#"
            STMT_LIST
              RETURN
                AND
                  BINARY_OP [16]
                    BINARY_OP [16]
                      ASSIGN
                        VAR
                          ZVAL "operand#1"
                        VAR
                          ZVAL "a"
                      ZVAL null
                    BINARY_OP [16]
                      ASSIGN
                        VAR
                          ZVAL "operand#2"
                        CONDITIONAL [1]
                          AND
                            BINARY_OP [16]
                              BINARY_OP [16]
                                ASSIGN
                                  VAR
                                    ZVAL "operand#3"
                                  VAR
                                    ZVAL "b"
                                ZVAL null
                              BINARY_OP [16]
                                ASSIGN
                                  VAR
                                    ZVAL "operand#4"
                                  VAR
                                    ZVAL "c"
                                ZVAL null
                            OR
                              BINARY_OP [16]
                                VAR
                                  ZVAL "operand#3"
                                ZVAL null
                              BINARY_OP [16]
                                CAST [5]
                                  VAR
                                    ZVAL "operand#3"
                                VAR
                                  ZVAL "operand#4"
                          ZVAL 1
                          ZVAL 2.5
                      ZVAL null
                  OR
                    BINARY_OP [16]
                      VAR
                        ZVAL "operand#1"
                      ZVAL null
                    BINARY_OP [16]
                      CAST [5]
                        VAR
                          ZVAL "operand#1"
                      CAST [5]
                        VAR
                          ZVAL "operand#2"
        "#}
    );
}

/// ```php
/// $b = \strcmp($text, "9") < 0; $b = \strcmp($text, $other) >= 0; $b = $total < 9;
/// ```
///
/// PHP's `<` compares two numeric strings as numbers, so `"10" < "9"` is false. PHP# orders two strings by their
/// bytes, which is the sign of `\strcmp`. Numbers keep PHP's `<`.
#[test]
fn string_ordering_is_strcmp_compared_with_zero() {
    assert_eq!(
        body_in(
            "int run(string text, string other, int total)",
            "        let b = text < \"9\";\n        b = text >= other;\n        b = total < 9;\n        return 1;\n",
            &[],
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "b"
                BINARY_OP [20]
                  CALL
                    ZVAL "strcmp"
                    ARG_LIST
                      VAR
                        ZVAL "text"
                      ZVAL "9"
                  ZVAL 0
              ASSIGN
                VAR
                  ZVAL "b"
                GREATER_EQUAL
                  CALL
                    ZVAL "strcmp"
                    ARG_LIST
                      VAR
                        ZVAL "text"
                      VAR
                        ZVAL "other"
                  ZVAL 0
              ASSIGN
                VAR
                  ZVAL "b"
                BINARY_OP [20]
                  VAR
                    ZVAL "total"
                  ZVAL 9
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// return match (true) {
///     (${'match#1'} = $this->label()) === "01" => 1,
///     \strcmp(${'match#1'}, "m") < 0 => 2,
///     default => 3,
/// };
/// ```
///
/// A comparison pattern on a string compares as `==` and `<` do outside a pattern. The second arm reads the hidden
/// variable, a node no source writes.
#[test]
fn a_string_comparison_pattern_compares_strictly_and_orders_by_strcmp() {
    assert_eq!(
        child_body(
            RUN,
            "        return match (this.label()) {\n            == \"01\" => 1,\n            < \"m\" => 2,\n            default => 3,\n        };\n",
            "<?php namespace App\\Tenant; class Base { public function label(): string { return ''; } }",
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                MATCH
                  ZVAL true
                  MATCH_ARM_LIST
                    MATCH_ARM
                      EXPR_LIST
                        BINARY_OP [16]
                          ASSIGN
                            VAR
                              ZVAL "match#1"
                            METHOD_CALL
                              VAR
                                ZVAL "this"
                              ZVAL "label"
                              ARG_LIST
                          ZVAL "01"
                      ZVAL 1
                    MATCH_ARM
                      EXPR_LIST
                        BINARY_OP [20]
                          CALL
                            ZVAL "strcmp"
                            ARG_LIST
                              VAR
                                ZVAL "match#1"
                              ZVAL "m"
                          ZVAL 0
                      ZVAL 2
                    MATCH_ARM
                      null
                      ZVAL 3
        "#}
    );
}

/// ```php
/// return $a && $a || !$a;
/// ```
///
/// `[14]` is `ZEND_BOOL_NOT`.
#[test]
fn logical_operators_are_and_or_and_bool_not() {
    assert_eq!(
        body_in("bool run(int extra)", "        let a = true;\n        return a && a || !a;\n", &[]),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                ZVAL true
              RETURN
                OR
                  AND
                    VAR
                      ZVAL "a"
                    VAR
                      ZVAL "a"
                  UNARY_OP [14]
                    VAR
                      ZVAL "a"
        "#}
    );
}

/// ```php
/// -$a; +$a; ++$a; --$a; $a++; $a--;
/// ```
#[test]
fn unary_operators_are_the_kinds_php_gives_them() {
    let tree = body(
        "        let a = 1;\n        -a;\n        +a;\n        ++a;\n        --a;\n        a++;\n        a--;\n        return a;\n",
    );
    let operators: Vec<&str> = tree.lines().filter(|line| line.starts_with("  ") && !line.starts_with("   ")).collect();

    assert_eq!(
        operators,
        ["  ASSIGN", "  UNARY_MINUS", "  UNARY_PLUS", "  PRE_INC", "  PRE_DEC", "  POST_INC", "  POST_DEC", "  RETURN"]
    );
}

/// ```php
/// $a -= 1; $a *= 2; $a = \intdiv($a, 3); $a **= 4;
/// ```
///
/// `[2]`, `[3]` and `[12]` are `ZEND_SUB`, `ZEND_MUL` and `ZEND_POW`. `/=` on an int assigns the `\intdiv` quotient.
#[test]
fn compound_assignments_are_assign_ops() {
    let tree = body(
        "        let a = 1;\n        a -= 1;\n        a *= 2;\n        a /= 3;\n        a **= 4;\n        return a;\n",
    );
    let operators: Vec<&str> = tree.lines().filter(|line| line.starts_with("  ") && !line.starts_with("   ")).collect();

    assert_eq!(
        operators,
        ["  ASSIGN", "  ASSIGN_OP [2]", "  ASSIGN_OP [3]", "  ASSIGN", "  ASSIGN_OP [12]", "  RETURN"]
    );
}

/// ```php
/// $b = $extra | $extra; $b = $extra & $extra; $b = $extra ^ $extra; $b = $extra << $extra; $b = $extra >> $extra;
/// $b = ~$extra;
/// ```
///
/// `[9]`, `[10]`, `[11]`, `[6]` and `[7]` are `ZEND_BW_OR`, `ZEND_BW_AND`, `ZEND_BW_XOR`, `ZEND_SL` and `ZEND_SR`, and
/// `[13]` is `ZEND_BW_NOT`, in php-src's `Zend/zend_vm_opcodes.h`. PHP throws `ArithmeticError` for a negative shift
/// count, and `<<` drops the bits it shifts out, as spec section 19 has them.
#[test]
fn bitwise_operators_are_the_binary_and_unary_ops_php_gives_them() {
    let tree = body(
        "        let b = extra | extra;\n        b = extra & extra;\n        b = extra ^ extra;\n        b = extra << extra;\n        b = extra >> extra;\n        b = ~extra;\n        return b;\n",
    );

    assert_eq!(
        assigned_values(&tree),
        ["BINARY_OP [9]", "BINARY_OP [10]", "BINARY_OP [11]", "BINARY_OP [6]", "BINARY_OP [7]", "UNARY_OP [13]"]
    );
}

/// ```php
/// $a |= 1; $a &= 2; $a ^= 4; $a <<= 1; $a >>= 1; $a %= 3;
/// ```
///
/// Each compound form applies the opcode of its operator: `[9]`, `[10]`, `[11]`, `[6]`, `[7]` and `[5]` are
/// `ZEND_BW_OR`, `ZEND_BW_AND`, `ZEND_BW_XOR`, `ZEND_SL`, `ZEND_SR` and `ZEND_MOD`.
#[test]
fn bitwise_and_modulo_compound_assignments_are_assign_ops() {
    let tree = body(
        "        let a = extra;\n        a |= 1;\n        a &= 2;\n        a ^= 4;\n        a <<= 1;\n        a >>= 1;\n        a %= 3;\n        return a;\n",
    );
    let operators: Vec<&str> = tree.lines().filter(|line| line.starts_with("  ") && !line.starts_with("   ")).collect();

    assert_eq!(
        operators,
        [
            "  ASSIGN",
            "  ASSIGN_OP [9]",
            "  ASSIGN_OP [10]",
            "  ASSIGN_OP [11]",
            "  ASSIGN_OP [6]",
            "  ASSIGN_OP [7]",
            "  ASSIGN_OP [5]",
            "  RETURN",
        ]
    );
}

/// ```php
/// return ($extra & 2) !== 0 ? 1 : 0;
/// ```
///
/// `&` binds tighter than `!=` in PHP#, so the flag test compares the `[10]` `ZEND_BW_AND` with 0, where PHP's own
/// grammar would read `$extra & (2 != 0)`. `[17]` is `ZEND_IS_NOT_IDENTICAL`.
#[test]
fn a_flag_test_compares_the_bitwise_and_with_zero() {
    assert_eq!(
        body("        return extra & 2 != 0 ? 1 : 0;\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                CONDITIONAL
                  BINARY_OP [17]
                    BINARY_OP [10]
                      VAR
                        ZVAL "extra"
                      ZVAL 2
                    ZVAL 0
                  ZVAL 1
                  ZVAL 0
        "#}
    );
}

/// ```php
/// return -$a ** $a ** 2;
/// ```
///
/// `[12]` is `ZEND_POW`. `**` groups to the right and binds tighter than unary `-`, as php-src's grammar has it.
#[test]
fn exponentiation_is_a_right_grouped_pow_under_unary_minus() {
    assert_eq!(
        body("        return -extra ** extra ** 2;\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                UNARY_MINUS
                  BINARY_OP [12]
                    VAR
                      ZVAL "extra"
                    BINARY_OP [12]
                      VAR
                        ZVAL "extra"
                      ZVAL 2
        "#}
    );
}

/// ```php
/// return $extra ?? $this->total() ?? 0;
/// ```
///
/// `??` is right-associative, as php-src's grammar declares it.
#[test]
fn null_coalescing_is_coalesce() {
    assert_eq!(
        child_body(
            "int run(int? extra)",
            "        return extra ?? this.total() ?? 0;\n",
            "<?php namespace App\\Tenant; class Base { public function total(): ?int { return null; } }",
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                COALESCE
                  VAR
                    ZVAL "extra"
                  COALESCE
                    METHOD_CALL
                      VAR
                        ZVAL "this"
                      ZVAL "total"
                      ARG_LIST
                    ZVAL 0
        "#}
    );
}

/// ```php
/// return ($extra > 1 ? true : false) ? $extra : ($extra < 0 ? 0 : 1);
/// ```
///
/// A ternary in parentheses carries `ZEND_PARENTHESIZED_CONDITIONAL`, as php-src's grammar marks `'(' expr ')'`, so
/// the engine accepts it as the condition of another ternary.
#[test]
fn the_ternary_is_a_conditional_marked_when_parenthesized() {
    assert_eq!(
        body("        return (extra > 1 ? true : false) ? extra : (extra < 0 ? 0 : 1);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                CONDITIONAL
                  CONDITIONAL [1]
                    GREATER
                      VAR
                        ZVAL "extra"
                      ZVAL 1
                    ZVAL true
                    ZVAL false
                  VAR
                    ZVAL "extra"
                  CONDITIONAL [1]
                    BINARY_OP [20]
                      VAR
                        ZVAL "extra"
                      ZVAL 0
                    ZVAL 0
                    ZVAL 1
        "#}
    );
}

/// ```php
/// return @\trim($x);
/// ```
///
/// php-src's grammar builds `@expr` as a `SILENCE` of one child, the expression.
#[test]
fn error_control_is_a_silence_of_its_expression() {
    let lowered = Lowered::named(
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text/Text.sharp",
        "namespace Sharp.Text;\n\npublic static class Text\n{\n    public static string run(string x)\n    {\n        return @trim(x);\n    }\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                SILENCE
                  CALL
                    ZVAL "trim"
                    ARG_LIST
                      VAR
                        ZVAL "x"
        "#}
    );
}

/// ```php
/// $cents = (int)($extra * 1.5); return (string)(float)$cents;
/// ```
///
/// A cast's attr is the type it converts to, `IS_LONG`, `IS_DOUBLE` or `IS_STRING`, as php-src's grammar builds it.
#[test]
fn casts_between_numbers_are_casts_to_their_types() {
    assert_eq!(
        body_in(
            "string run(int extra)",
            "        const cents = (int)(extra * 1.5);\n        return (string)(float)cents;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "cents"
                CAST [4]
                  BINARY_OP [3]
                    VAR
                      ZVAL "extra"
                    ZVAL 1.5
              RETURN
                CAST [6]
                  CAST [5]
                    VAR
                      ZVAL "cents"
        "#}
    );
}

/// ```php
/// $extra ??= 1; $this->count ??= $extra;
/// ```
#[test]
fn null_coalescing_assignment_is_assign_coalesce() {
    assert_eq!(
        child_body(
            "int run(int? extra)",
            "        extra ??= 1;\n        this.count ??= extra;\n        return extra;\n",
            "<?php namespace App\\Tenant; class Base { public ?int $count = null; }",
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN_COALESCE
                VAR
                  ZVAL "extra"
                ZVAL 1
              ASSIGN_COALESCE
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "count"
                VAR
                  ZVAL "extra"
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// return $base?->total($extra)?->value->cents;
/// ```
#[test]
fn null_safe_calls_and_reads_are_nullsafe_kinds() {
    assert_eq!(
        child_body(
            "int? run(Base? base, int extra)",
            "        return base?.total(extra)?.value.cents;\n",
            "<?php namespace App\\Tenant; final class Money { public int $cents = 0; } final class Total { public Money $value; } class Base { public function total(int $a): ?Total { return null; } }",
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                PROP
                  NULLSAFE_PROP
                    NULLSAFE_METHOD_CALL
                      VAR
                        ZVAL "base"
                      ZVAL "total"
                      ARG_LIST
                        VAR
                          ZVAL "extra"
                    ZVAL "value"
                  ZVAL "cents"
        "#}
    );
}

/// ```php
/// return ($extra[0] ?? null)?->value ?? ($extra[1] ?? null)?->total();
/// ```
///
/// `?.` reads a missing key as null, as `??` does, so the index it reads from is the left side of a `COALESCE`.
#[test]
fn null_safe_access_on_an_index_coalesces_a_missing_key_to_null() {
    assert_eq!(
        body_in(
            "int? run(List<Calc?> extra)",
            "        return extra[0]?.value ?? (extra[1])?.total();\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { public int $value = 0; public function total(): int { return 0; } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                COALESCE
                  NULLSAFE_PROP
                    COALESCE
                      DIM
                        VAR
                          ZVAL "extra"
                        ZVAL 0
                      ZVAL null
                    ZVAL "value"
                  NULLSAFE_METHOD_CALL
                    COALESCE
                      DIM
                        VAR
                          ZVAL "extra"
                        ZVAL 1
                      ZVAL null
                    ZVAL "total"
                    ARG_LIST
        "#}
    );
}

/// ```php
/// return ($extra + 1) * 2;
/// ```
#[test]
fn parentheses_only_group() {
    assert_eq!(
        body("        return (extra + 1) * 2;\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [3]
                  BINARY_OP [1]
                    VAR
                      ZVAL "extra"
                    ZVAL 1
                  ZVAL 2
        "#}
    );
}

/// ```php
/// {
///     $inner = PHP_INT_MAX;
/// }
/// ```
///
/// `[1]` on the constant's name is `ZEND_NAME_NOT_FQ`: like PHP, the engine looks the constant up in the namespace,
/// then globally.
#[test]
fn a_block_is_a_statement_list_and_a_constant_is_looked_up_by_its_short_name() {
    assert_eq!(
        body("        {\n            let inner = PHP_INT_MAX;\n        }\n        return 1;\n"),
        indoc! {r#"
            STMT_LIST
              STMT_LIST
                ASSIGN
                  VAR
                    ZVAL "inner"
                  CONST
                    ZVAL [1] "PHP_INT_MAX"
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// if ($extra > 1) {
///     return 1;
/// } else if ($extra < 0) {
///     return 2;
/// } else {
///     return 3;
/// }
/// ```
///
/// php-src reads `else if` as an `else` whose statement is the next `if`. An `else` has a null condition.
#[test]
fn if_else_if_and_else_are_if_lists_of_if_elems() {
    assert_eq!(
        body(
            "        if (extra > 1) {\n            return 1;\n        } else if (extra < 0) {\n            return 2;\n        } else {\n            return 3;\n        }\n"
        ),
        indoc! {r#"
            STMT_LIST
              IF
                IF_ELEM
                  GREATER
                    VAR
                      ZVAL "extra"
                    ZVAL 1
                  STMT_LIST
                    RETURN
                      ZVAL 1
                IF_ELEM
                  null
                  IF
                    IF_ELEM
                      BINARY_OP [20]
                        VAR
                          ZVAL "extra"
                        ZVAL 0
                      STMT_LIST
                        RETURN
                          ZVAL 2
                    IF_ELEM
                      null
                      STMT_LIST
                        RETURN
                          ZVAL 3
        "#}
    );
}

/// ```php
/// while ($extra > 0) {
///     $extra -= 1;
/// }
/// do {
///     $extra += 1;
/// } while ($extra < 3);
/// ```
///
/// `WHILE` takes its condition first, and `DO_WHILE` its body first.
#[test]
fn while_and_do_while_are_their_php_kinds() {
    assert_eq!(
        body(
            "        while (extra > 0) {\n            extra -= 1;\n        }\n        do {\n            extra += 1;\n        } while (extra < 3);\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              WHILE
                GREATER
                  VAR
                    ZVAL "extra"
                  ZVAL 0
                STMT_LIST
                  ASSIGN_OP [2]
                    VAR
                      ZVAL "extra"
                    ZVAL 1
              DO_WHILE
                STMT_LIST
                  ASSIGN_OP [1]
                    VAR
                      ZVAL "extra"
                    ZVAL 1
                BINARY_OP [20]
                  VAR
                    ZVAL "extra"
                  ZVAL 3
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// while (true) {
///     continue;
///     break;
/// }
/// ```
///
/// A `break` or `continue` without a level has a null depth.
#[test]
fn break_and_continue_have_a_null_depth() {
    assert_eq!(
        body("        while (true) {\n            continue;\n            break;\n        }\n        return 1;\n"),
        indoc! {"
            STMT_LIST
              WHILE
                ZVAL true
                STMT_LIST
                  CONTINUE
                    null
                  BREAK
                    null
              RETURN
                ZVAL 1
        "}
    );
}

/// ```php
/// for ($step = 0; $step < $extra; $step++, $extra--) {
/// }
/// for (;;) {
///     break;
/// }
/// ```
///
/// Each part of the header is an `EXPR_LIST`, or null when it is empty. A `let` counter is the assignment of its
/// value.
#[test]
fn for_loops_are_for_nodes_with_an_expression_list_per_part() {
    assert_eq!(
        body(
            "        for (let step = 0; step < extra; step++, extra--) {\n        }\n        for (;;) {\n            break;\n        }\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              FOR
                EXPR_LIST
                  ASSIGN
                    VAR
                      ZVAL "step"
                    ZVAL 0
                EXPR_LIST
                  BINARY_OP [20]
                    VAR
                      ZVAL "step"
                    VAR
                      ZVAL "extra"
                EXPR_LIST
                  POST_INC
                    VAR
                      ZVAL "step"
                  POST_DEC
                    VAR
                      ZVAL "extra"
                STMT_LIST
              FOR
                null
                null
                null
                STMT_LIST
                  BREAK
                    null
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// A typed counter lowers as a `let` counter does: its type only tells the checker the counter's type.
///
/// ```php
/// for ($step = 0; $step < $extra; ) {
/// }
/// ```
#[test]
fn a_typed_for_counter_is_the_assignment_of_its_value() {
    assert_eq!(
        body("        for (int step = 0; step < extra; ) {\n        }\n        return extra;\n"),
        indoc! {r#"
            STMT_LIST
              FOR
                EXPR_LIST
                  ASSIGN
                    VAR
                      ZVAL "step"
                    ZVAL 0
                EXPR_LIST
                  BINARY_OP [20]
                    VAR
                      ZVAL "step"
                    VAR
                      ZVAL "extra"
                null
                STMT_LIST
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// foreach (\Lib\Calc::make(2) as $value) {
///     $extra += $value;
/// }
/// foreach (\Lib\Calc::make(3) as $key => $value) {
/// }
/// ```
///
/// `FOREACH` takes the collection, the value variable, the key variable or null, and the body.
#[test]
fn for_of_loops_are_foreach_nodes_with_the_value_before_the_key() {
    assert_eq!(
        body_in(
            RUN,
            "        for (const value of Calc.make(2)) {\n            extra += value;\n        }\n        for (let [key, value] of Calc.make(3)) {\n        }\n        return extra;\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { /** @return array<int, int> */ public static function make(int $n): array { return [$n => $n]; } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              FOREACH
                STATIC_CALL
                  ZVAL "Lib\\Calc"
                  ZVAL "make"
                  ARG_LIST
                    ZVAL 2
                VAR
                  ZVAL "value"
                null
                STMT_LIST
                  ASSIGN_OP [1]
                    VAR
                      ZVAL "extra"
                    VAR
                      ZVAL "value"
              FOREACH
                STATIC_CALL
                  ZVAL "Lib\\Calc"
                  ZVAL "make"
                  ARG_LIST
                    ZVAL 3
                VAR
                  ZVAL "value"
                VAR
                  ZVAL "key"
                STMT_LIST
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// foreach ($statuses as $status => $n) {
///     $status = \Lib\Calc::from($status);
///     {
///         $extra += $n;
///     }
/// }
/// foreach ($skus as $sku => $n) {
///     $sku = (string) $sku;
///     {
///     }
/// }
/// foreach ($keys as $key => $n) {
/// }
/// ```
///
/// A `Map` keyed by a backed enum holds each key as its backing value, so a loop reads each key back as its case,
/// through `from` on the enum. PHP stores an all-digit `string` key as an `int`, so a loop over a `Map<string, V>`
/// reads each key back through `(string)`. Both follow the `Map`'s key type, written on the key or not. An `int` key
/// type changes nothing.
#[test]
fn a_loop_key_reads_back_through_from_or_a_cast_by_the_map_key_type() {
    assert_eq!(
        body_in(
            "int run(int extra, Map<Calc, int> statuses, Map<string, int> skus, Map<int, int> keys)",
            "        for (const [status, int n] of statuses) {\n            extra += n;\n        }\n        for (const [string sku, n] of skus) {\n        }\n        for (const [key, n] of keys) {\n        }\n        return 1;\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              FOREACH
                VAR
                  ZVAL "statuses"
                VAR
                  ZVAL "n"
                VAR
                  ZVAL "status"
                STMT_LIST
                  ASSIGN
                    VAR
                      ZVAL "status"
                    STATIC_CALL
                      ZVAL "Lib\\Calc"
                      ZVAL "from"
                      ARG_LIST
                        VAR
                          ZVAL "status"
                  STMT_LIST
                    ASSIGN_OP [1]
                      VAR
                        ZVAL "extra"
                      VAR
                        ZVAL "n"
              FOREACH
                VAR
                  ZVAL "skus"
                VAR
                  ZVAL "n"
                VAR
                  ZVAL "sku"
                STMT_LIST
                  ASSIGN
                    VAR
                      ZVAL "sku"
                    CAST [6]
                      VAR
                        ZVAL "sku"
                  STMT_LIST
              FOREACH
                VAR
                  ZVAL "keys"
                VAR
                  ZVAL "n"
                VAR
                  ZVAL "key"
                STMT_LIST
              RETURN
                ZVAL 1
        "#}
    );
}

/// ```php
/// throw new \App\Tenant\Failure($extra);
/// return $extra ?? throw new \App\Tenant\Failure(0);
/// ```
///
/// `throw` is an expression, and a `throw` statement is that expression alone.
#[test]
fn throw_is_a_throw_expression() {
    assert_eq!(
        body_in(
            RUN,
            "        throw new Failure(extra);\n        return extra ?? throw new Failure(0);\n",
            &[(
                "src/App/Tenant/Failure.php",
                "<?php namespace App\\Tenant; final class Failure extends \\Exception { public function __construct(int $code) { parent::__construct('', $code); } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              THROW
                NEW
                  ZVAL "App\\Tenant\\Failure"
                  ARG_LIST
                    VAR
                      ZVAL "extra"
              RETURN
                COALESCE
                  VAR
                    ZVAL "extra"
                  THROW
                    NEW
                      ZVAL "App\\Tenant\\Failure"
                      ARG_LIST
                        ZVAL 0
        "#}
    );
}

/// ```php
/// try {
///     $extra += 1;
/// } catch (\Lib\Calc | \App\Tenant\Missing $failure) {
///     throw $failure;
/// } catch (\App\Tenant\Broken) {
/// } finally {
///     $extra -= 1;
/// }
/// ```
///
/// `TRY` takes the try block, a `CATCH_LIST` and the finally block or null. Each `CATCH` takes a `NAME_LIST` of
/// classes, the variable's name or null, and its block.
#[test]
fn try_is_a_try_node_with_a_catch_list_and_a_finally_block() {
    assert_eq!(
        body_in(
            RUN,
            "        try {\n            extra += 1;\n        } catch (Calc | Missing failure) {\n            throw failure;\n        } catch (Broken) {\n        } finally {\n            extra -= 1;\n        }\n        return extra;\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc extends \\Exception {}"), common::FAILURES]
        ),
        indoc! {r#"
            STMT_LIST
              TRY
                STMT_LIST
                  ASSIGN_OP [1]
                    VAR
                      ZVAL "extra"
                    ZVAL 1
                CATCH_LIST
                  CATCH
                    NAME_LIST
                      ZVAL "Lib\\Calc"
                      ZVAL "App\\Tenant\\Missing"
                    ZVAL "failure"
                    STMT_LIST
                      THROW
                        VAR
                          ZVAL "failure"
                  CATCH
                    NAME_LIST
                      ZVAL "App\\Tenant\\Broken"
                    null
                    STMT_LIST
                STMT_LIST
                  ASSIGN_OP [2]
                    VAR
                      ZVAL "extra"
                    ZVAL 1
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// try {
/// } finally {
/// }
/// ```
#[test]
fn try_without_a_catch_has_an_empty_catch_list() {
    assert_eq!(
        body("        try {\n        } finally {\n        }\n        return extra;\n"),
        indoc! {r#"
            STMT_LIST
              TRY
                STMT_LIST
                CATCH_LIST
                STMT_LIST
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// return \strlen(\sprintf("%d", \count($extra, mode: 0)));
/// ```
///
/// PHP# calls only global functions, PHP's own and those a library or the app declares, so a call names the global
/// function with `ZEND_NAME_FQ`, which is 0.
#[test]
fn a_function_call_is_a_call_of_the_global_function() {
    assert_eq!(
        body_in("int run(List<int> extra)", "        return strlen(sprintf(\"%d\", count(extra, mode: 0)));\n", &[]),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "strlen"
                  ARG_LIST
                    CALL
                      ZVAL "sprintf"
                      ARG_LIST
                        ZVAL "%d"
                        CALL
                          ZVAL "count"
                          ARG_LIST
                            VAR
                              ZVAL "extra"
                            NAMED_ARG
                              ZVAL "mode"
                              ZVAL 0
        "#}
    );
}

/// ```php
/// return \run($extra);
/// ```
///
/// Spec section 4 writes every member as `this.m()` or `Class.m()`, so a bare call inside `run` calls the global
/// function `run`, not the method.
#[test]
fn a_bare_call_named_like_a_method_of_its_class_is_a_call_of_the_global_function() {
    assert_eq!(
        body_in(
            RUN,
            "        return run(extra);\n",
            &[("src/run.php", "<?php function run(int $extra): int { return $extra; }")]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "run"
                  ARG_LIST
                    VAR
                      ZVAL "extra"
        "#}
    );
}

/// ```php
/// exit(1);
/// exit($extra);
/// exit();
/// ```
///
/// PHP 8.4 makes `exit` a function, so php-src's grammar builds `exit(…)` as a call of the global function `exit`,
/// named with `ZEND_NAME_FQ`, which is 0.
#[test]
fn exit_is_a_call_of_the_global_function_exit() {
    assert_eq!(
        body("        exit(1);\n        exit(extra);\n        exit();\n"),
        indoc! {r#"
            STMT_LIST
              CALL
                ZVAL "exit"
                ARG_LIST
                  ZVAL 1
              CALL
                ZVAL "exit"
                ARG_LIST
                  VAR
                    ZVAL "extra"
              CALL
                ZVAL "exit"
                ARG_LIST
        "#}
    );
}

/// ```php
/// $label = "Order {$extra}: " . \strlen("x") . "!"; $alone = "{$extra}"; $plain = "plain A"; return $extra;
/// ```
///
/// php-src's grammar builds an interpolated string as an `ENCAPS_LIST` of its text and its expressions, and a
/// string without one as a `ZVAL`. A template's `${…}` takes any expression, which `ENCAPS_LIST` compiles as PHP's
/// `{$…}` does.
#[test]
fn a_template_is_an_encaps_list_of_its_text_and_interpolations() {
    assert_eq!(
        body(
            "        const label = `Order ${extra}: ${strlen(\"x\")}!`;\n        const alone = `${extra}`;\n        const plain = `plain \\x41`;\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "label"
                ENCAPS_LIST
                  ZVAL "Order "
                  VAR
                    ZVAL "extra"
                  ZVAL ": "
                  CALL
                    ZVAL "strlen"
                    ARG_LIST
                      ZVAL "x"
                  ZVAL "!"
              ASSIGN
                VAR
                  ZVAL "alone"
                ENCAPS_LIST
                  VAR
                    ZVAL "extra"
              ASSIGN
                VAR
                  ZVAL "plain"
                ZVAL "plain A"
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// return "Paid: {["false", "true"][$paid]} of {$extra} for {$name} {["false", "true"][true]}";
/// ```
///
/// Spec section 18 shows a `bool` in a template as `true` or `false`, where PHP's string conversion gives `1` or
/// nothing, so a `bool` part reads its text from `["false", "true"]`, where PHP reads `false` as the index 0 and
/// `true` as the index 1. The optimizer keeps the read as one `FETCH_DIM_R` of a constant array, where
/// `$paid ? "true" : "false"` takes four instructions. An `int` and a `string` part lower as before.
#[test]
fn a_bool_in_a_template_is_its_text_read_by_the_bool() {
    assert_eq!(
        body_in(
            "string run(bool paid, int extra, string name)",
            "        return `Paid: ${paid} of ${extra} for ${name} ${true}`;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                ENCAPS_LIST
                  ZVAL "Paid: "
                  DIM
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "false"
                        null
                      ARRAY_ELEM
                        ZVAL "true"
                        null
                    VAR
                      ZVAL "paid"
                  ZVAL " of "
                  VAR
                    ZVAL "extra"
                  ZVAL " for "
                  VAR
                    ZVAL "name"
                  ZVAL " "
                  DIM
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "false"
                        null
                      ARRAY_ELEM
                        ZVAL "true"
                        null
                    ZVAL true
        "#}
    );
}

/// ```php
/// $twice = fn (int $a) => $a * $extra; $half = fn ($b) => \intdiv($b, 2); return $twice($half(4));
/// ```
///
/// A lambda that writes none of the locals it captures is PHP's `fn`, which captures by value what its body reads.
/// php-src's grammar gives an arrow function no name, no `use` list and its expression as its body. A call of a local
/// calls the closure the local holds.
#[test]
fn a_lambda_that_writes_no_capture_is_an_arrow_function() {
    assert_eq!(
        body_in(
            RUN,
            "        const twice = (int a) => a * extra;\n        Function<int(int)> half = b => b / 2;\n        return twice(half(4));\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "twice"
                ARROW_FUNC "" @9-9
                  PARAM_LIST
                    PARAM
                      ZVAL [1] "int"
                      ZVAL "a"
                      null
                      null
                      null
                      null
                  null
                  BINARY_OP [3]
                    VAR
                      ZVAL "a"
                    VAR
                      ZVAL "extra"
                  null
                  null
              ASSIGN
                VAR
                  ZVAL "half"
                ARROW_FUNC "" @10-10
                  PARAM_LIST
                    PARAM
                      null
                      ZVAL "b"
                      null
                      null
                      null
                      null
                  null
                  CALL
                    ZVAL "intdiv"
                    ARG_LIST
                      VAR
                        ZVAL "b"
                      ZVAL 2
                  null
                  null
              RETURN
                CALL
                  VAR
                    ZVAL "twice"
                  ARG_LIST
                    CALL
                      VAR
                        ZVAL "half"
                      ARG_LIST
                        ZVAL 4
        "#}
    );
}

/// ```php
/// $count = 0;
/// $add = function (int $step) use (&$count, $extra) {
///     $count += $step + $extra;
/// };
/// $bump = function () use (&$count) { return $count += 1; };
/// $add(1);
/// return $count;
/// ```
///
/// Spec section 3: a lambda captures the variable itself. A lambda with a block body, or one that writes a local it
/// captures, is PHP's `function` with a `use` list in first-use order, which captures a local that code writes by
/// reference, `ZEND_BIND_REF`, which is 1, and any other by value. An expression body is the `return` of it.
#[test]
fn a_lambda_with_a_block_or_a_written_capture_is_a_closure_with_a_use_list() {
    assert_eq!(
        body(
            "        let count = 0;\n        const add = (int step) => {\n            count += step + extra;\n        };\n        const bump = () => count += 1;\n        add(1);\n        return count;\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "count"
                ZVAL 0
              ASSIGN
                VAR
                  ZVAL "add"
                CLOSURE "" @10-12
                  PARAM_LIST
                    PARAM
                      ZVAL [1] "int"
                      ZVAL "step"
                      null
                      null
                      null
                      null
                  CLOSURE_USES
                    ZVAL [1] "count"
                    ZVAL "extra"
                  STMT_LIST
                    ASSIGN_OP [1]
                      VAR
                        ZVAL "count"
                      BINARY_OP [1]
                        VAR
                          ZVAL "step"
                        VAR
                          ZVAL "extra"
                  null
                  null
              ASSIGN
                VAR
                  ZVAL "bump"
                CLOSURE "" @13-13
                  PARAM_LIST
                  CLOSURE_USES
                    ZVAL [1] "count"
                  STMT_LIST
                    RETURN
                      ASSIGN_OP [1]
                        VAR
                          ZVAL "count"
                        ZVAL 1
                  null
                  null
              CALL
                VAR
                  ZVAL "add"
                ARG_LIST
                  ZVAL 1
              RETURN
                VAR
                  ZVAL "count"
        "#}
    );
}

/// ```php
/// while ($extra > 0) {
///     { unset($seen); $seen = $extra; }
///     $mark = function () use (&$seen) { $seen += 1; };
///     $mark();
///     $extra -= 1;
/// }
/// return $extra;
/// ```
///
/// Spec section 3 gives each loop pass its own `let`. PHP reuses one variable across passes, so a `let` declared in a
/// loop body that a lambda captures by reference is unset before its assignment, and the lambda of each pass keeps
/// its own. php-src's grammar builds `unset($seen);` as a statement list of one `UNSET`.
#[test]
fn a_let_in_a_loop_captured_by_reference_is_unset_before_its_assignment() {
    assert_eq!(
        body(
            "        while (extra > 0) {\n            let seen = extra;\n            const mark = () => {\n                seen += 1;\n            };\n            mark();\n            extra -= 1;\n        }\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              WHILE
                GREATER
                  VAR
                    ZVAL "extra"
                  ZVAL 0
                STMT_LIST
                  STMT_LIST
                    STMT_LIST
                      UNSET
                        VAR
                          ZVAL "seen"
                    ASSIGN
                      VAR
                        ZVAL "seen"
                      VAR
                        ZVAL "extra"
                  ASSIGN
                    VAR
                      ZVAL "mark"
                    CLOSURE "" @11-13
                      PARAM_LIST
                      CLOSURE_USES
                        ZVAL [1] "seen"
                      STMT_LIST
                        ASSIGN_OP [1]
                          VAR
                            ZVAL "seen"
                          ZVAL 1
                      null
                      null
                  CALL
                    VAR
                      ZVAL "mark"
                    ARG_LIST
                  ASSIGN_OP [2]
                    VAR
                      ZVAL "extra"
                    ZVAL 1
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// A `let` the lambda declares in its own body lives in each call's own frame, so it is never unset, even when the
/// lambda sits in a loop.
#[test]
fn a_let_declared_inside_a_lambda_in_a_loop_is_not_unset() {
    let tree = body(
        "        while (extra > 0) {\n            const step = () => {\n                let inner = 1;\n                const again = () => {\n                    inner += 1;\n                };\n                again();\n            };\n            step();\n            extra -= 1;\n        }\n        return extra;\n",
    );

    assert!(!tree.contains("UNSET"), "{tree}");
}

/// php-src takes a list's line from its first child, and each piece of text's from where it starts.
#[test]
fn a_template_over_several_lines_keeps_the_line_of_each_part() {
    let lowered = Lowered::new(&method_with("string run(int extra)", "        return `total:\n${extra} more`;\n"));
    let list = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_ENCAPS_LIST).expect("a list");
    let lines: Vec<u32> =
        (0..3).map(|index| lowered.nodes()[lowered.child(list as u32, index) as usize].line).collect();

    assert_eq!(lowered.nodes()[list].line, 9);
    assert_eq!(lines, [9, 10, 10]);
}

/// ```php
/// #[\Lib\Entity(label: "Orders", order: 2 * 3), \App\Tenant\Searchable]
/// #[\Lib\Entity(null)]
/// class Report
/// {
///     #[\Lib\Field] private int $count = 0;
///
///     public function __construct(#[\Lib\Field(-1.5)] public readonly int $total)
///     {
///     }
///
///     #[\Lib\Entity(true, PHP_INT_MAX)]
///     public function run(#[\Lib\Field] int $extra): void
///     {
///     }
/// }
/// ```
///
/// Each `#[...]` is an `ATTRIBUTE_GROUP`, and a declaration's groups are one `ATTRIBUTE_LIST` in the child
/// `zend_ast_with_attributes` gives it: the 4th of a class, the 5th of a method, the 3rd of a property group and the
/// 4th of a parameter.
#[test]
fn attributes_are_attribute_lists_of_attribute_groups_on_their_declarations() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Field;\nimport Lib.Entity;\n\n[Entity(label: \"Orders\", order: 2 * 3), Searchable]\n[Entity(null)]\nclass Report\n{\n    [Field] private int count = 0;\n\n    public Report([Field(-1.5)] public int total { get; })\n    {\n    }\n\n    [Entity(true, PHP_INT_MAX)]\n    public void run([Field] int extra)\n    {\n    }\n}\n",
        &[common::FIELD, common::ENTITY, common::SEARCHABLE],
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.root(), 2)),
        indoc! {r#"
            CLASS "Report" @8-20
              null
              null
              STMT_LIST
                PROP_GROUP [4]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "count"
                      ZVAL 0
                      null
                      null
                  ATTRIBUTE_LIST
                    ATTRIBUTE_GROUP
                      ATTRIBUTE
                        ZVAL "Lib\\Field"
                        null
                METHOD [1] "__construct" @12-14
                  PARAM_LIST
                    PARAM [129]
                      ZVAL [1] "int"
                      ZVAL "total"
                      null
                      ATTRIBUTE_LIST
                        ATTRIBUTE_GROUP
                          ATTRIBUTE
                            ZVAL "Lib\\Field"
                            ARG_LIST
                              UNARY_MINUS
                                ZVAL 1.5
                      null
                      null
                  null
                  STMT_LIST
                  null
                  null
                METHOD [1] "run" @17-19
                  PARAM_LIST
                    PARAM
                      ZVAL [1] "int"
                      ZVAL "extra"
                      null
                      ATTRIBUTE_LIST
                        ATTRIBUTE_GROUP
                          ATTRIBUTE
                            ZVAL "Lib\\Field"
                            null
                      null
                      null
                  null
                  STMT_LIST
                  ZVAL [1] "void"
                  ATTRIBUTE_LIST
                    ATTRIBUTE_GROUP
                      ATTRIBUTE
                        ZVAL "Lib\\Entity"
                        ARG_LIST
                          ZVAL true
                          CONST
                            ZVAL [1] "PHP_INT_MAX"
              ATTRIBUTE_LIST
                ATTRIBUTE_GROUP
                  ATTRIBUTE
                    ZVAL "Lib\\Entity"
                    ARG_LIST
                      NAMED_ARG
                        ZVAL "label"
                        ZVAL "Orders"
                      NAMED_ARG
                        ZVAL "order"
                        BINARY_OP [3]
                          ZVAL 2
                          ZVAL 3
                  ATTRIBUTE
                    ZVAL "App\\Tenant\\Searchable"
                    null
                ATTRIBUTE_GROUP
                  ATTRIBUTE
                    ZVAL "Lib\\Entity"
                    ARG_LIST
                      ZVAL null
              null
        "#}
    );
}

/// ```php
/// $a = $extra instanceof \Lib\Calc; $b = \is_int($extra); $c = \is_string($extra);
/// $d = (${'match#1'} = $this->total()) instanceof \Lib\Calc;
/// ```
///
/// A class is tested with `instanceof` on its full name, and a built-in type with the global `is_` function the
/// engine compiles to `TYPE_CHECK`. A value that is not a local or a parameter is tested through a variable PHP#
/// cannot name, so the analyzer never narrows a property.
#[test]
fn is_without_a_name_is_instanceof_or_a_type_check() {
    assert_eq!(
        child_body(
            "Calc|int|string run(Calc|int|string extra)",
            "        const a = extra is Calc;\n        const b = extra is int;\n        const c = extra is string;\n        const d = this.total() is Calc;\n        return extra;\n",
            "<?php namespace App\\Tenant { class Base { public function total(): ?object { return null; } } } namespace Lib { final class Calc {} }",
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                INSTANCEOF
                  VAR
                    ZVAL "extra"
                  ZVAL "Lib\\Calc"
              ASSIGN
                VAR
                  ZVAL "b"
                CALL
                  ZVAL "is_int"
                  ARG_LIST
                    VAR
                      ZVAL "extra"
              ASSIGN
                VAR
                  ZVAL "c"
                CALL
                  ZVAL "is_string"
                  ARG_LIST
                    VAR
                      ZVAL "extra"
              ASSIGN
                VAR
                  ZVAL "d"
                INSTANCEOF
                  ASSIGN
                    VAR
                      ZVAL "match#1"
                    METHOD_CALL
                      VAR
                        ZVAL "this"
                      ZVAL "total"
                      ARG_LIST
                  ZVAL "Lib\\Calc"
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// if (($calc = $extra) instanceof \Lib\Calc) { return 1; }
/// if (!\is_int($count = $extra)) { return 0; }
/// return $count;
/// ```
///
/// A pattern's variable is assigned the tested value before the test. The binder lets code read it only where the
/// test holds, so the assignment is never seen where it fails.
#[test]
fn is_with_a_name_assigns_the_name_before_the_test() {
    assert_eq!(
        body_in(
            "int run(Calc|int extra)",
            "        if (extra is Calc calc) {\n            return 1;\n        }\n        if (extra is not int count) {\n            return 0;\n        }\n        return count;\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")]
        ),
        indoc! {r#"
            STMT_LIST
              IF
                IF_ELEM
                  INSTANCEOF
                    ASSIGN
                      VAR
                        ZVAL "calc"
                      VAR
                        ZVAL "extra"
                    ZVAL "Lib\\Calc"
                  STMT_LIST
                    RETURN
                      ZVAL 1
              IF
                IF_ELEM
                  UNARY_OP [14]
                    CALL
                      ZVAL "is_int"
                      ARG_LIST
                        ASSIGN
                          VAR
                            ZVAL "count"
                          VAR
                            ZVAL "extra"
                  STMT_LIST
                    RETURN
                      ZVAL 0
              RETURN
                VAR
                  ZVAL "count"
        "#}
    );
}

/// ```php
/// return \is_string($extra) ? 1 : 0;
/// ```
///
/// A `?` after the type that `is` tests starts a `? :`, as in C#, so it lowers as the same test in parentheses does.
#[test]
fn is_before_a_question_mark_is_the_condition_of_a_conditional() {
    let tree = body_in("int run(int|string extra)", "        return extra is string ? 1 : 0;\n", &[]);

    assert_eq!(tree, body_in("int run(int|string extra)", "        return (extra is string) ? 1 : 0;\n", &[]));
    assert_eq!(
        tree,
        indoc! {r#"
            STMT_LIST
              RETURN
                CONDITIONAL
                  CALL
                    ZVAL "is_string"
                    ARG_LIST
                      VAR
                        ZVAL "extra"
                  ZVAL 1
                  ZVAL 0
        "#}
    );
}

/// ```php
/// $calc = $extra instanceof \Lib\Calc ? $extra : null;
/// $made = (${'as#1'} = \Lib\Calc::make()) instanceof \Lib\Calc ? ${'as#1'} : null;
/// ```
///
/// `as` reads a local or a parameter twice. Any other value is assigned to a variable PHP# cannot name, so it runs
/// once.
#[test]
fn as_is_a_conditional_that_gives_the_value_or_null() {
    assert_eq!(
        body_in(
            "Calc|int run(Calc|int extra)",
            "        const calc = extra as Calc;\n        const made = Calc.make() as Calc;\n        return extra;\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { public static function make(): object { return new self(); } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "calc"
                CONDITIONAL
                  INSTANCEOF
                    VAR
                      ZVAL "extra"
                    ZVAL "Lib\\Calc"
                  VAR
                    ZVAL "extra"
                  ZVAL null
              ASSIGN
                VAR
                  ZVAL "made"
                CONDITIONAL
                  INSTANCEOF
                    ASSIGN
                      VAR
                        ZVAL "as#1"
                      STATIC_CALL
                        ZVAL "Lib\\Calc"
                        ZVAL "make"
                        ARG_LIST
                    ZVAL "Lib\\Calc"
                  VAR
                    ZVAL "as#1"
                  ZVAL null
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// if (\is_int($count = $counts[$key] ?? null)) { return $count; }
/// $price = \is_int(${'as#1'} = $counts[$key] ?? null) ? ${'as#1'} : null;
/// ```
///
/// An index a pattern tests reads a missing key as null, as `??` does, so the index is the left side of a
/// `COALESCE`, and a missing key reaches the test as null instead of a warning.
#[test]
fn an_index_a_pattern_tests_coalesces_a_missing_key_to_null() {
    assert_eq!(
        body_in(
            "int? run(Map<string, int> counts, string key)",
            "        if (counts[key] is int count) {\n            return count;\n        }\n        const price = counts[key] as int;\n        return price;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              IF
                IF_ELEM
                  CALL
                    ZVAL "is_int"
                    ARG_LIST
                      ASSIGN
                        VAR
                          ZVAL "count"
                        COALESCE
                          DIM
                            VAR
                              ZVAL "counts"
                            VAR
                              ZVAL "key"
                          ZVAL null
                  STMT_LIST
                    RETURN
                      VAR
                        ZVAL "count"
              ASSIGN
                VAR
                  ZVAL "price"
                CONDITIONAL
                  CALL
                    ZVAL "is_int"
                    ARG_LIST
                      ASSIGN
                        VAR
                          ZVAL "as#1"
                        COALESCE
                          DIM
                            VAR
                              ZVAL "counts"
                            VAR
                              ZVAL "key"
                          ZVAL null
                  VAR
                    ZVAL "as#1"
                  ZVAL null
              RETURN
                VAR
                  ZVAL "price"
        "#}
    );
}

/// ```php
/// $limit = 10;
/// $a = $extra === 200;
/// $b = \is_int($extra) && $extra >= 1 && $extra < $limit || !($extra === -1);
/// $c = $extra === $limit;
/// $d = \is_object($extra) && (${'match#1'} = $extra->count) > 0 && \is_string($label = $extra->name);
/// ```
///
/// A value is compared with `===`, a comparison keeps its operator, and `and`, `or` and `not` are `&&`, `||` and
/// `!`. A bare name is the local's value when a local of that name is in scope. A properties pattern tests that the
/// value is an object, then reads each property once. A comparison orders a `Calc`, which declares no `operator <=>`,
/// only once `int` has ruled it out.
#[test]
fn values_comparisons_and_properties_are_the_php_comparisons_they_name() {
    assert_eq!(
        body_in(
            "Calc|int run(Calc|int extra)",
            "        let limit = 10;\n        const a = extra is 200;\n        const b = extra is int and >= 1 and < limit or (not -1);\n        const c = extra is limit;\n        const d = extra is { count: > 0, name: string label };\n        return extra;\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { public int $count = 0; public ?string $name = null; }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "limit"
                ZVAL 10
              ASSIGN
                VAR
                  ZVAL "a"
                BINARY_OP [16]
                  VAR
                    ZVAL "extra"
                  ZVAL 200
              ASSIGN
                VAR
                  ZVAL "b"
                OR
                  AND
                    AND
                      CALL
                        ZVAL "is_int"
                        ARG_LIST
                          VAR
                            ZVAL "extra"
                      GREATER_EQUAL
                        VAR
                          ZVAL "extra"
                        ZVAL 1
                    BINARY_OP [20]
                      VAR
                        ZVAL "extra"
                      VAR
                        ZVAL "limit"
                  UNARY_OP [14]
                    BINARY_OP [16]
                      VAR
                        ZVAL "extra"
                      UNARY_MINUS
                        ZVAL 1
              ASSIGN
                VAR
                  ZVAL "c"
                BINARY_OP [16]
                  VAR
                    ZVAL "extra"
                  VAR
                    ZVAL "limit"
              ASSIGN
                VAR
                  ZVAL "d"
                AND
                  AND
                    CALL
                      ZVAL "is_object"
                      ARG_LIST
                        VAR
                          ZVAL "extra"
                    GREATER
                      ASSIGN
                        VAR
                          ZVAL "match#1"
                        PROP
                          VAR
                            ZVAL "extra"
                          ZVAL "count"
                      ZVAL 0
                  CALL
                    ZVAL "is_string"
                    ARG_LIST
                      ASSIGN
                        VAR
                          ZVAL "label"
                        PROP
                          VAR
                            ZVAL "extra"
                          ZVAL "name"
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// ```php
/// return match (true) {
///     (${'match#1'} = $this->total()) === 0 => 1,
///     \is_int($n = ${'match#1'}) && $n > 9 => $n,
///     default => 2,
/// };
/// ```
///
/// A `match` that gives a value is PHP's `match (true)` with one condition per arm: its pattern's test, `&&` its
/// `when` condition. The first test assigns the hidden variable.
#[test]
fn a_match_that_gives_a_value_is_a_match_of_true() {
    assert_eq!(
        child_body(
            RUN,
            "        return match (this.total()) {\n            0 => 1,\n            int n when n > 9 => n,\n            default => 2,\n        };\n",
            "<?php namespace App\\Tenant; class Base { public function total(): int { return 0; } }",
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                MATCH
                  ZVAL true
                  MATCH_ARM_LIST
                    MATCH_ARM
                      EXPR_LIST
                        BINARY_OP [16]
                          ASSIGN
                            VAR
                              ZVAL "match#1"
                            METHOD_CALL
                              VAR
                                ZVAL "this"
                              ZVAL "total"
                              ARG_LIST
                          ZVAL 0
                      ZVAL 1
                    MATCH_ARM
                      EXPR_LIST
                        AND
                          CALL
                            ZVAL "is_int"
                            ARG_LIST
                              ASSIGN
                                VAR
                                  ZVAL "n"
                                VAR
                                  ZVAL "match#1"
                          GREATER
                            VAR
                              ZVAL "n"
                            ZVAL 9
                      VAR
                        ZVAL "n"
                    MATCH_ARM
                      null
                      ZVAL 2
        "#}
    );
}

/// ```php
/// if ($extra === 0) {
///     return 1;
/// } else if (\is_int($n = $extra) && $n > 9) $this->run($n); else {
/// }
/// ```
///
/// A `match` that starts a statement is `if`, `else if` per arm, and `else` for `default`. An arm that is an
/// expression is its expression statement.
#[test]
fn a_match_that_starts_a_statement_is_an_if_list() {
    assert_eq!(
        body(
            "        match (extra) {\n            0 => {\n                return 1;\n            },\n            int n when n > 9 => this.run(n),\n            default => {},\n        }\n        return extra;\n"
        ),
        indoc! {r#"
            STMT_LIST
              IF
                IF_ELEM
                  BINARY_OP [16]
                    VAR
                      ZVAL "extra"
                    ZVAL 0
                  STMT_LIST
                    RETURN
                      ZVAL 1
                IF_ELEM
                  null
                  IF
                    IF_ELEM
                      AND
                        CALL
                          ZVAL "is_int"
                          ARG_LIST
                            ASSIGN
                              VAR
                                ZVAL "n"
                              VAR
                                ZVAL "extra"
                        GREATER
                          VAR
                            ZVAL "n"
                          ZVAL 9
                      METHOD_CALL
                        VAR
                          ZVAL "this"
                        ZVAL "run"
                        ARG_LIST
                          VAR
                            ZVAL "n"
                    IF_ELEM
                      null
                      STMT_LIST
              RETURN
                VAR
                  ZVAL "extra"
        "#}
    );
}

/// php-src gives the jump after a branch the line of its `IF_ELEM`, so each arm's `if` is on its pattern's line, as
/// its PHP twin's is, and coverage never counts the line of the arm before it.
#[test]
fn each_arm_of_a_match_that_starts_a_statement_is_on_its_pattern_line() {
    let lowered = Lowered::new(&method(
        "        match (extra) {\n            0 => {},\n            1 => {},\n            default => {},\n        }\n        return extra;\n",
    ));
    let mut lines: Vec<u32> = lowered
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_IF_ELEM)
        .map(|node| node.line)
        .collect();
    lines.sort_unstable();

    assert_eq!(lines, [10, 11, 11, 12]);
}

/// ```php
/// $forced = $extra > 9;
/// return match (true) {
///     $extra === 0 && $forced => 1,
///     default => 2,
/// };
/// ```
///
/// A `when` condition that ends in a bare name is the arm's `&&` operand, as `forced == true` is.
#[test]
fn a_when_condition_that_is_a_bare_name_is_the_arms_and_operand() {
    let guarded = |condition: &str| {
        body(&format!(
            "        const forced = extra > 9;\n        return match (extra) {{\n            0 when {condition} => 1,\n            default => 2,\n        }};\n"
        ))
    };
    let bare = guarded("forced");

    assert_eq!(
        bare,
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "forced"
                GREATER
                  VAR
                    ZVAL "extra"
                  ZVAL 9
              RETURN
                MATCH
                  ZVAL true
                  MATCH_ARM_LIST
                    MATCH_ARM
                      EXPR_LIST
                        AND
                          BINARY_OP [16]
                            VAR
                              ZVAL "extra"
                            ZVAL 0
                          VAR
                            ZVAL "forced"
                      ZVAL 1
                    MATCH_ARM
                      null
                      ZVAL 2
        "#}
    );
    assert_eq!(
        guarded("forced == true"),
        bare.replace(
            "              VAR\n                ZVAL \"forced\"\n          ZVAL 1\n",
            "              BINARY_OP [16]\n                VAR\n                  ZVAL \"forced\"\n                ZVAL true\n          ZVAL 1\n",
        )
    );
}

/// ```php
/// <?php declare(strict_types=1); namespace App\Tenant;
///
///
///
/// #[\Lib\Label("Order status")]
/// enum Status: string
/// {
///     case Active = "active";
///     #[\Lib\Label("Gone")]
///     case Archived = "archived";
///
///     public function title(): string
///     {
///         return $this->value;
///     }
///
///     public static function parse(string $value): \App\Tenant\Status
///     {
///         $all = \App\Tenant\Status::cases();
///         return \App\Tenant\Status::tryFrom($value) ?? \App\Tenant\Status::from("active");
///     }
/// }
/// ```
///
/// An enum is a `CLASS` with `[268435488]`, `ZEND_ACC_ENUM | ZEND_ACC_FINAL`, and its backing type as its 5th child. A
/// case is an `ENUM_CASE` of its name, its value, a null doc comment and its attributes, on the line of its name.
#[test]
fn a_backed_enum_is_a_final_enum_class_with_its_backing_type_cases_and_methods() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Label;\n\n[Label(\"Order status\")]\nenum Status : string\n{\n    case Active = \"active\";\n    [Label(\"Gone\")]\n    case Archived = \"archived\";\n\n    public string title()\n    {\n        return this.value;\n    }\n\n    public static Status parse(string value)\n    {\n        const all = Status.cases();\n        return Status.tryFrom(value) ?? Status.from(\"active\");\n    }\n}\n",
        &[(
            "src/Lib/Label.php",
            "<?php namespace Lib; #[\\Attribute(\\Attribute::TARGET_ALL)] final class Label { public function __construct(string $text) {} }",
        )],
    );
    let case_lines: Vec<u32> = lowered
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_ENUM_CASE)
        .map(|node| node.line)
        .collect();

    assert_eq!(case_lines, [8, 10]);
    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              NAMESPACE
                ZVAL "App\\Tenant"
                null
              CLASS [268435488] "Status" @6-22
                null
                null
                STMT_LIST
                  ENUM_CASE
                    ZVAL "Active"
                    ZVAL "active"
                    null
                    null
                  ENUM_CASE
                    ZVAL "Archived"
                    ZVAL "archived"
                    null
                    ATTRIBUTE_LIST
                      ATTRIBUTE_GROUP
                        ATTRIBUTE
                          ZVAL "Lib\\Label"
                          ARG_LIST
                            ZVAL "Gone"
                  METHOD [1] "title" @12-15
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        PROP
                          VAR
                            ZVAL "this"
                          ZVAL "value"
                    ZVAL [1] "string"
                    null
                  METHOD [17] "parse" @17-21
                    PARAM_LIST
                      PARAM
                        ZVAL [1] "string"
                        ZVAL "value"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                      ASSIGN
                        VAR
                          ZVAL "all"
                        STATIC_CALL
                          ZVAL "App\\Tenant\\Status"
                          ZVAL "cases"
                          ARG_LIST
                      RETURN
                        COALESCE
                          STATIC_CALL
                            ZVAL "App\\Tenant\\Status"
                            ZVAL "tryFrom"
                            ARG_LIST
                              VAR
                                ZVAL "value"
                          STATIC_CALL
                            ZVAL "App\\Tenant\\Status"
                            ZVAL "from"
                            ARG_LIST
                              ZVAL "active"
                    ZVAL "App\\Tenant\\Status"
                    null
                ATTRIBUTE_LIST
                  ATTRIBUTE_GROUP
                    ATTRIBUTE
                      ZVAL "Lib\\Label"
                      ARG_LIST
                        ZVAL "Order status"
                ZVAL [1] "string"
        "#}
    );
}

/// ```php
/// <?php declare(strict_types=1); enum Suit
/// {
///     case Hearts;
///     case Spades;
///
///     public function label(): string { return $this->name; }
///
///     public static function size(): int { return \count(\Suit::cases()); }
/// }
/// ```
///
/// A pure enum has no backing type, and its cases have no value.
#[test]
fn a_pure_enum_is_a_final_enum_class_of_unit_cases_without_a_backing_type() {
    let lowered = Lowered::new(
        "enum Suit\n{\n    case Hearts;\n    case Spades;\n\n    public string label() => this.name;\n\n    public static int size() => count(Suit.cases());\n}\n",
    );

    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              CLASS [268435488] "Suit" @1-9
                null
                null
                STMT_LIST
                  ENUM_CASE
                    ZVAL "Hearts"
                    null
                    null
                    null
                  ENUM_CASE
                    ZVAL "Spades"
                    null
                    null
                    null
                  METHOD [1] "label" @6-6
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        PROP
                          VAR
                            ZVAL "this"
                          ZVAL "name"
                    ZVAL [1] "string"
                    null
                  METHOD [17] "size" @8-8
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        CALL
                          ZVAL "count"
                          ARG_LIST
                            STATIC_CALL
                              ZVAL "Suit"
                              ZVAL "cases"
                              ARG_LIST
                    ZVAL [1] "int"
                    null
                null
                null
        "#}
    );
}

/// ```php
/// <?php declare(strict_types=1); enum Rank: int
/// {
///     case Low = -1;
///     case High = 2;
/// }
/// ```
///
/// An int-backed enum has `int` as its backing type, and a negative case value is the `UNARY_MINUS` of its literal, as
/// php-src's grammar builds `-1`.
#[test]
fn an_int_backed_enum_is_a_final_enum_class_of_int_cases() {
    let lowered = Lowered::new("enum Rank : int\n{\n    case Low = -1;\n    case High = 2;\n}\n");
    let case_lines: Vec<u32> = lowered
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_ENUM_CASE)
        .map(|node| node.line)
        .collect();

    assert_eq!(case_lines, [3, 4]);
    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              CLASS [268435488] "Rank" @1-5
                null
                null
                STMT_LIST
                  ENUM_CASE
                    ZVAL "Low"
                    UNARY_MINUS
                      ZVAL 1
                    null
                    null
                  ENUM_CASE
                    ZVAL "High"
                    ZVAL 2
                    null
                    null
                null
                ZVAL [1] "int"
        "#}
    );
}

/// ```php
/// <?php declare(strict_types=1); namespace App\Tenant;
///
///
///
/// enum Status: string implements \Lib\HasLabel, \App\Tenant\Sorted
/// {
///     case Active = "a";
/// }
///
/// enum Suit implements \Lib\HasLabel
/// {
///     case Hearts;
/// }
/// ```
///
/// An enum's header is the interface name list, its 2nd child, with `ZEND_NAME_FQ`, which is 0. An enum has no
/// parent, so every name there is an interface.
#[test]
fn an_enum_header_is_the_interface_name_list() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.HasLabel;\n\nenum Status : string, HasLabel, Sorted\n{\n    case Active = \"a\";\n}\n\nenum Suit : HasLabel\n{\n    case Hearts;\n}\n",
        &[common::HAS_LABEL, ("src/App/Tenant/Sorted.php", "<?php namespace App\\Tenant; interface Sorted {}")],
    );

    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              NAMESPACE
                ZVAL "App\\Tenant"
                null
              CLASS [268435488] "Status" @5-8
                null
                NAME_LIST
                  ZVAL "Lib\\HasLabel"
                  ZVAL "App\\Tenant\\Sorted"
                STMT_LIST
                  ENUM_CASE
                    ZVAL "Active"
                    ZVAL "a"
                    null
                    null
                null
                ZVAL [1] "string"
              CLASS [268435488] "Suit" @10-13
                null
                NAME_LIST
                  ZVAL "Lib\\HasLabel"
                STMT_LIST
                  ENUM_CASE
                    ZVAL "Hearts"
                    null
                    null
                    null
                null
                null
        "#}
    );
}

/// ```php
/// <?php declare(strict_types=1); namespace App\Tenant;
///
/// enum Status: string
/// {
///     case Active = "a";
///
///     public const \App\Tenant\Status Default = \App\Tenant\Status::Active;
/// }
/// ```
///
/// An enum's constant is a class constant group of one constant, as a class's is, and its read of a case is a class
/// constant fetch.
#[test]
fn an_enum_constant_is_a_class_constant_group_that_reads_a_case() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nenum Status : string\n{\n    case Active = \"a\";\n\n    public const Status Default = Status.Active;\n}\n",
    );
    let r#enum = lowered.child(lowered.root(), 2);

    assert_eq!(
        lowered.render(lowered.child(r#enum, 2)),
        indoc! {r#"
            STMT_LIST
              ENUM_CASE
                ZVAL "Active"
                ZVAL "a"
                null
                null
              CLASS_CONST_GROUP [1]
                CLASS_CONST_DECL
                  CONST_ELEM
                    ZVAL "Default"
                    CLASS_CONST
                      ZVAL "App\\Tenant\\Status"
                      ZVAL "Active"
                    null
                null
                ZVAL "App\\Tenant\\Status"
        "#}
    );
}

/// ```php
/// case Paused = \Lib\Registry::PAUSED;
/// public const \App\Tenant\Status Default = \App\Tenant\Status::Active;
/// return $this === \App\Tenant\Status::Active;
/// public function run(\App\Tenant\Status $status = \App\Tenant\Status::Active): string
/// if ($status === \App\Tenant\Status::Active) { return \App\Tenant\Status::Active->label(); }
/// return \App\Tenant\Status::Default->label();
/// ```
///
/// A case read is a class constant fetch in every place: a case's value, a constant and a parameter default, which are
/// constant expressions, and a method body. A method called on a case is an instance call on that fetch.
#[test]
fn a_case_read_is_a_class_constant_in_every_place() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Registry;\n\npublic enum Status : string\n{\n    case Active = \"a\";\n    case Paused = Registry.PAUSED;\n\n    public const Status Default = Status.Active;\n\n    public bool active() => this == Status.Active;\n\n    public string label() => this.name;\n}\n\nclass Report\n{\n    public string run(Status status = Status.Active)\n    {\n        if (status == Status.Active) {\n            return Status.Active.label();\n        }\n        return Status.Default.label();\n    }\n}\n",
        &[("src/Lib/Registry.php", "<?php namespace Lib; final class Registry { public const string PAUSED = 'p'; }")],
    );
    let reads: Vec<String> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_CLASS_CONST)
        .map(|(index, _)| lowered.render(lowered.child(index as u32, 1)).trim_end().to_owned())
        .collect();

    assert_eq!(
        reads,
        [
            "ZVAL \"PAUSED\"",
            "ZVAL \"Active\"",
            "ZVAL \"Active\"",
            "ZVAL \"Active\"",
            "ZVAL \"Active\"",
            "ZVAL \"Active\"",
            "ZVAL \"Default\"",
        ]
    );
    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              IF
                IF_ELEM
                  BINARY_OP [16]
                    VAR
                      ZVAL "status"
                    CLASS_CONST
                      ZVAL "App\\Tenant\\Status"
                      ZVAL "Active"
                  STMT_LIST
                    RETURN
                      METHOD_CALL
                        CLASS_CONST
                          ZVAL "App\\Tenant\\Status"
                          ZVAL "Active"
                        ZVAL "label"
                        ARG_LIST
              RETURN
                METHOD_CALL
                  CLASS_CONST
                    ZVAL "App\\Tenant\\Status"
                    ZVAL "Default"
                  ZVAL "label"
                  ARG_LIST
        "#}
    );
}

/// ```php
/// <?php declare(strict_types=1); enum Suit
/// {
///     case Hearts;
///     case Spades;
///
///     public static function all(): array { return [\Suit::Hearts, \Suit::Spades]; }
///
///     public function matches(): \Closure
///     {
///         $first = \Suit::Hearts;
///         return fn ($other) => $other === $this || $other === $first;
///     }
/// }
/// ```
///
/// An enum's method lowers a list literal, a function type and a lambda as a class's method does, so a case read in
/// either is the class constant fetch of any method body.
#[test]
fn an_enum_method_lowers_a_list_literal_a_function_type_and_a_lambda_as_a_class_method_does() {
    let lowered = Lowered::new(
        "enum Suit\n{\n    case Hearts;\n    case Spades;\n\n    public static List<Suit> all() => [Suit.Hearts, Suit.Spades];\n\n    public Function<bool(Suit)> matches()\n    {\n        const first = Suit.Hearts;\n        return other => other == this || other == first;\n    }\n}\n",
    );

    assert_eq!(
        lowered.tree(),
        indoc! {r#"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              CLASS [268435488] "Suit" @1-13
                null
                null
                STMT_LIST
                  ENUM_CASE
                    ZVAL "Hearts"
                    null
                    null
                    null
                  ENUM_CASE
                    ZVAL "Spades"
                    null
                    null
                    null
                  METHOD [17] "all" @6-6
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        ARRAY [3]
                          ARRAY_ELEM
                            CLASS_CONST
                              ZVAL "Suit"
                              ZVAL "Hearts"
                            null
                          ARRAY_ELEM
                            CLASS_CONST
                              ZVAL "Suit"
                              ZVAL "Spades"
                            null
                    TYPE [7]
                    null
                  METHOD [1] "matches" @8-12
                    PARAM_LIST
                    null
                    STMT_LIST
                      ASSIGN
                        VAR
                          ZVAL "first"
                        CLASS_CONST
                          ZVAL "Suit"
                          ZVAL "Hearts"
                      RETURN
                        ARROW_FUNC "" @11-11
                          PARAM_LIST
                            PARAM
                              null
                              ZVAL "other"
                              null
                              null
                              null
                              null
                          null
                          OR
                            BINARY_OP [16]
                              VAR
                                ZVAL "other"
                              VAR
                                ZVAL "this"
                            BINARY_OP [16]
                              VAR
                                ZVAL "other"
                              VAR
                                ZVAL "first"
                          null
                          null
                    ZVAL "Closure"
                    null
                null
                null
        "#}
    );
}

/// ```php
/// enum Status: string
/// {
///     public const int|string Key = 1;
///     case Active = "a";
///     public static function first(): static { return Status::Active; }
///     public static function all(): array { return static::cases(); }
/// }
/// ```
///
/// An enum's method returns `Self` as a class's does, a `TYPE` of `IS_STATIC`, which is 15, and a `List<Self>` as a
/// `TYPE` of `IS_ARRAY`, which is 7. `Self.cases()` is a static call on `static`. Its constant's union type is a
/// `TYPE_UNION`.
#[test]
fn an_enum_returns_self_and_holds_a_union_typed_constant() {
    let lowered = Lowered::new(
        "namespace App;\n\nenum Status : string\n{\n    public const int|string Key = 1;\n\n    case Active = \"a\";\n\n    public static Self first() => Status.Active;\n\n    public static List<Self> all() => Self.cases();\n}\n",
    );
    let class = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_CLASS).expect("an enum");

    assert_eq!(
        lowered.render(class as u32),
        indoc! {r#"
            CLASS [268435488] "Status" @3-12
              null
              null
              STMT_LIST
                CLASS_CONST_GROUP [1]
                  CLASS_CONST_DECL
                    CONST_ELEM
                      ZVAL "Key"
                      ZVAL 1
                      null
                  null
                  TYPE_UNION
                    ZVAL [1] "int"
                    ZVAL [1] "string"
                ENUM_CASE
                  ZVAL "Active"
                  ZVAL "a"
                  null
                  null
                METHOD [17] "first" @9-9
                  PARAM_LIST
                  null
                  STMT_LIST
                    RETURN
                      CLASS_CONST
                        ZVAL "App\\Status"
                        ZVAL "Active"
                  TYPE [15]
                  null
                METHOD [17] "all" @11-11
                  PARAM_LIST
                  null
                  STMT_LIST
                    RETURN
                      STATIC_CALL
                        ZVAL [1] "static"
                        ZVAL "cases"
                        ARG_LIST
                  TYPE [7]
                  null
              null
              ZVAL [1] "string"
        "#}
    );
}

/// The child count `zend_ast_get_num_children` gives a fixed-size kind, or 5 for a declaration. `None` for a list.
fn fixed_child_count(kind: sharp_kind) -> Option<u32> {
    const IS_LIST: u32 = 1 << 7;
    const NUM_CHILDREN_SHIFT: u32 = 8;

    let value = kind as u32;
    if value & IS_LIST != 0 {
        None
    } else if kind == sharp_kind::SHARP_AST_ZVAL {
        Some(0)
    } else if (sharp_kind::SHARP_AST_FUNC_DECL as u32..=sharp_kind::SHARP_AST_PROPERTY_HOOK as u32).contains(&value) {
        Some(5)
    } else {
        Some(value >> NUM_CHILDREN_SHIFT)
    }
}

/// The fixtures hold every construct the checker accepts, the standard library's own under the namespace `Sharp`, so
/// the bridge lowers all of them, and each fixed-size node has the child count of its kind.
#[test]
fn every_construct_of_the_slice_lowers_into_nodes_of_their_kinds_child_count() {
    let slice = Lowered::with(
        include_str!("../../semantics/tests/fixtures/slice.sharp"),
        &[
            (
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Money { public int $cents = 0; } final class Calc extends \\Exception { public const int MAX = 3; public static Money $standard; public ?float $rate = null; public Money $price; public function __construct(int|float|Calc $start = 0, int $rate = 1, int ...$more) {} public static function make(): Calc { return new Calc(); } public function add(int ...$amounts): Calc { return $this; } /** @return array<int, int> */ public static function values(): array { return []; } }",
            ),
            (
                "src/App/Tenant/Access.php",
                "<?php namespace App\\Tenant; #[\\Attribute] final class Access { public function __construct(string $class) {} }",
            ),
            common::ENTITY,
            common::FIELD,
            common::SEARCHABLE,
            common::HAS_LABEL,
            common::FAILURES,
            common::INT,
            common::FLOAT,
        ],
    );
    let library = Lowered::named(
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text/Text.sharp",
        include_str!("../../semantics/tests/fixtures/library.sharp"),
    );

    for lowered in [slice, library] {
        assert_eq!(lowered.diagnostics(), Vec::<String>::new());

        let wrong: Vec<String> = lowered
            .nodes()
            .iter()
            .filter(|node| fixed_child_count(node.kind).is_some_and(|count| count != node.child_count))
            .map(|node| format!("{:?} has {} children", node.kind, node.child_count))
            .collect();

        assert_eq!(wrong, Vec::<String>::new());
    }
}

#[test]
fn each_node_carries_the_line_of_its_first_token() {
    let lowered = Lowered::new(&method("        let total =\n            extra;\n        return total;\n"));
    let lines: Vec<(String, u32)> = lowered
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_ZVAL && node.value == sharp_value::SHARP_STRING)
        .map(|node| (lowered.text(node.text), node.line))
        .filter(|(name, _)| name == "total" || name == "extra")
        .collect();

    assert_eq!(
        lines,
        [("extra".to_owned(), 7), ("total".to_owned(), 9), ("extra".to_owned(), 10), ("total".to_owned(), 11)]
    );
}

/// The keys of the inline forms the standard library file `code` gives, in the order its methods are declared.
fn form_names(code: &str) -> Vec<String> {
    common::checked(TEXT.0, code, &[], inline_forms)
        .expect("the library file is checked")
        .into_iter()
        .map(|(name, _)| String::from_utf8_lossy(&name).into_owned())
        .collect()
}

/// A public method gives an inline form when it returns the value of one expression built only from calls of public
/// members, literals and reads of its receiver and parameters, each read exactly once. `twice` reads `text` twice,
/// `trimmed` has two statements, `make` creates an object, and `label` never reads its receiver. `secret` calls a
/// private method and `quiet` a protected one, which the caller's class cannot call. `echoed` can be overridden, and
/// `again` calls `Self`, which names the caller's class once copied there. `hidden` and `muted` are not public.
#[test]
fn a_method_whose_body_is_one_call_reading_each_slot_once_gives_an_inline_form() {
    assert_eq!(form_names(TEXT.1), ["sharp\\text::shout", "sharp\\text::padded", "sharp\\text::wrapped"]);
}

/// `Position.current()` in a standard library method is the position in the library's file, inside that method. Copied
/// into a caller, it would give the caller's file with the library's line, so a method that runs it gives no form.
#[test]
fn a_method_that_runs_position_current_gives_no_inline_form() {
    assert_eq!(
        form_names(
            "namespace Sharp;\n\npublic class Here\n{\n    public static Position now() => Position.current();\n\n    public static string shout(string text) => strtoupper(text);\n}\n"
        ),
        ["sharp\\here::shout"]
    );
}

/// ```php
/// return \strtoupper($text->label());
/// ```
///
/// A form that reads every slot in order before its first call runs its arguments in the order the call would, so
/// it inlines whatever its arguments are.
#[test]
fn a_form_that_reads_its_slots_in_order_first_inlines_with_any_arguments() {
    assert_eq!(
        inlined_body("string run(Text text)", "        return Text.shout(text.label());\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "strtoupper"
                  ARG_LIST
                    METHOD_CALL
                      VAR
                        ZVAL "text"
                      ZVAL "label"
                      ARG_LIST
        "#}
    );
}

/// `Sharp\Padding` at the standard library's path, with a method whose body reads the global constant `STR_PAD_LEFT`.
const PADDING: (&str, &str) = (
    "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Padding.sharp",
    "namespace Sharp;\n\npublic class Padding\n{\n    public static string left(string text, int width) => str_pad(text, width, \" \", STR_PAD_LEFT);\n\n    public static string after(string text, int start) => substr(text, start, null);\n}\n",
);

/// The statements of `run`, declared with `signature` and holding `statements` in a class of `namespace` that imports
/// `Sharp.Padding`, lowered with the inline forms of `PADDING`.
fn padded_body(namespace: &str, signature: &str, statements: &str) -> String {
    let code = format!(
        "namespace {namespace};\n\nimport Sharp.Padding;\n\nclass Report\n{{\n    public {signature}\n    {{\n{statements}    }}\n}}\n"
    );

    Lowered::inlining(&code, &[], &[PADDING]).body()
}

/// ```php
/// return \str_pad($name, 4, ' ', \STR_PAD_LEFT);
/// ```
///
/// A form may read a global constant, as it reads a literal. The form is copied out of the standard library file's
/// namespace, so it names the constant by its full name, `ZEND_NAME_FQ`, which is 0.
#[test]
fn a_form_that_reads_a_global_constant_names_it_by_its_full_name() {
    assert_eq!(
        padded_body("App.Tenant", "string run(string name)", "        return Padding.left(name, 4);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "str_pad"
                  ARG_LIST
                    VAR
                      ZVAL "name"
                    ZVAL 4
                    ZVAL " "
                    CONST
                      ZVAL "STR_PAD_LEFT"
        "#}
    );
}

/// ```php
/// return \substr($name, 2, null);
/// ```
///
/// `null`, `true` and `false` are literals, never constant reads, so a form that passes one inlines it as its value.
#[test]
fn a_form_that_passes_null_inlines_it_as_a_literal() {
    assert_eq!(
        padded_body("App.Tenant", "string run(string name)", "        return Padding.after(name, 2);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "substr"
                  ARG_LIST
                    VAR
                      ZVAL "name"
                    ZVAL 2
                    ZVAL null
        "#}
    );
}

/// PHP reads `\null`, `\true` and `\false` as constants, which the codebase declares no metadata for. PHP# refuses a
/// `\` name, so a standard library form never holds one, and only the literals `null`, `true` and `false` reach it.
#[test]
fn a_fully_qualified_null_true_or_false_in_a_library_body_is_refused() {
    for value in ["\\null", "\\true", "\\false"] {
        let library = format!(
            "namespace Sharp;\n\npublic class Padding\n{{\n    public static Any? tail(Any? value) => {value};\n}}\n"
        );
        let refusal = common::checked(PADDING.0, &library, &[], inline_forms)
            .map(|_| ())
            .expect_err("PHP# refuses a `\\` name in a library body");

        assert_eq!(refusal.len(), 1, "{refusal:?}");
        assert!(refusal[0].contains("parse error: A `\\` name is PHP syntax"), "{refusal:?}");
    }
}

/// A form's constant names the same constant in every namespace that calls it, so its tree is the same in each.
#[test]
fn a_form_that_reads_a_global_constant_inlines_the_same_tree_in_any_namespace() {
    let [library, tenant] = ["Lib.Reports", "App.Tenant"]
        .map(|namespace| padded_body(namespace, "string run(string name)", "        return Padding.left(name, 4);\n"));

    assert!(tenant.contains("CONST\n          ZVAL \"STR_PAD_LEFT\"\n"), "{tenant}");
    assert_eq!(library, tenant);
}

/// ```php
/// return \str_pad($name, PHP_INT_SIZE, ' ', \STR_PAD_LEFT);
/// ```
///
/// A constant the caller passes keeps the caller's short name, `[1]` `ZEND_NAME_NOT_FQ`, which the engine looks up
/// in the caller's namespace first, as the caller wrote it. Only the form's own constant takes its full name.
#[test]
fn a_constant_the_caller_passes_keeps_its_short_name_beside_the_forms_full_name() {
    assert_eq!(
        padded_body("App.Tenant", "string run(string name)", "        return Padding.left(name, PHP_INT_SIZE);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "str_pad"
                  ARG_LIST
                    VAR
                      ZVAL "name"
                    CONST
                      ZVAL [1] "PHP_INT_SIZE"
                    ZVAL " "
                    CONST
                      ZVAL "STR_PAD_LEFT"
        "#}
    );
}

/// ```php
/// return \str_pad(\trim($name), 4);
/// ```
///
/// `padded` calls `trim` before it reads `width`, which the call would have read first. With arguments that do
/// nothing when read, the order cannot show, so it inlines.
#[test]
fn a_form_that_calls_before_its_last_slot_read_inlines_with_pure_arguments() {
    assert_eq!(
        inlined_body("string run(string name)", "        return Text.padded(name, 4);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "str_pad"
                  ARG_LIST
                    CALL
                      ZVAL "trim"
                      ARG_LIST
                        VAR
                          ZVAL "name"
                    ZVAL 4
        "#}
    );
}

/// ```php
/// return \Sharp\Text::padded($text->label(), 4);
/// ```
///
/// Inlined, `trim` would run before `label()`. The call runs `label()` first, so it stays a call.
#[test]
fn a_form_that_calls_before_its_last_slot_read_keeps_the_call_with_an_impure_argument() {
    assert_eq!(
        inlined_body("string run(Text text)", "        return Text.padded(text.label(), 4);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "Sharp\\Text"
                  ZVAL "padded"
                  ARG_LIST
                    METHOD_CALL
                      VAR
                        ZVAL "text"
                      ZVAL "label"
                      ARG_LIST
                    ZVAL 4
        "#}
    );
}

/// ```php
/// return \sprintf($name, $text->label()) . \Sharp\Text::make()->wrapped($name);
/// ```
///
/// `wrapped` reads `format` before its receiver. With a receiver that does nothing when read it inlines, and with
/// `Text.make()` as its receiver it stays a call, because inlined, `make()` would run after `name` is read.
#[test]
fn a_form_that_reads_its_slots_out_of_order_keeps_the_call_with_an_impure_receiver() {
    assert_eq!(
        inlined_body(
            "string run(Text text, string name)",
            "        return text.wrapped(name) + Text.make().wrapped(name);\n"
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [8]
                  CALL
                    ZVAL "sprintf"
                    ARG_LIST
                      VAR
                        ZVAL "name"
                      METHOD_CALL
                        VAR
                          ZVAL "text"
                        ZVAL "label"
                        ARG_LIST
                  METHOD_CALL
                    STATIC_CALL
                      ZVAL "Sharp\\Text"
                      ZVAL "make"
                      ARG_LIST
                    ZVAL "wrapped"
                    ARG_LIST
                      VAR
                        ZVAL "name"
        "#}
    );
}

/// A named argument, an omitted default, a method without a form and a method of a project file each keep the call:
/// a body that calls a private or a protected method, calls `Self`, or can be overridden has no form.
#[test]
fn a_call_the_inlining_rule_does_not_take_keeps_its_call() {
    let static_call = |class: &str, method: &str, arguments: &str| {
        format!(
            "STMT_LIST\n  RETURN\n    STATIC_CALL\n      ZVAL \"{class}\"\n      ZVAL \"{method}\"\n      ARG_LIST\n{arguments}"
        )
    };
    let method_call = |method: &str| {
        format!(
            "STMT_LIST\n  RETURN\n    METHOD_CALL\n      VAR\n        ZVAL \"text\"\n      ZVAL \"{method}\"\n      ARG_LIST\n        VAR\n          ZVAL \"name\"\n"
        )
    };
    let name = "        VAR\n          ZVAL \"name\"\n";
    let named = "        NAMED_ARG\n          ZVAL \"text\"\n          VAR\n            ZVAL \"name\"\n";
    let cases = [
        ("Text.shout(text: name)", static_call("Sharp\\\\Text", "shout", named)),
        ("Text.padded(name)", static_call("Sharp\\\\Text", "padded", name)),
        ("Text.twice(name)", static_call("Sharp\\\\Text", "twice", name)),
        ("Text.trimmed(name)", static_call("Sharp\\\\Text", "trimmed", name)),
        ("Loud.shout(name)", static_call("Lib\\\\Loud", "shout", name)),
        ("Text.quiet(name)", static_call("Sharp\\\\Text", "quiet", name)),
        ("Text.again(name)", static_call("Sharp\\\\Text", "again", name)),
        ("text.secret(name)", method_call("secret")),
        ("text.echoed(name)", method_call("echoed")),
    ];

    for (call, tree) in cases {
        assert_eq!(
            inlined_body("string run(Text text, string name)", &format!("        return {call};\n")),
            tree,
            "{call}"
        );
    }
}

/// `Sharp\Slug` at the standard library's path, with two native bodies: one with plain parameters and one variadic.
const SLUG: (&str, &str) = (
    "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Slug.sharp",
    "namespace Sharp;\n\npublic static class Slug\n{\n    public static extern string title(string text);\n\n    public static extern string join(string text, string ...words);\n}\n",
);

/// The statements of `run`, which takes a `string name` and holds `statements` in a class that imports `Sharp.Slug`,
/// lowered with the inline forms of `SLUG`.
fn native_body(statements: &str) -> String {
    let code = format!(
        "namespace App.Tenant;\n\nimport Sharp.Slug;\n\nclass Report\n{{\n    public string run(string name)\n    {{\n{statements}    }}\n}}\n"
    );

    Lowered::inlining(&code, &[], &[SLUG]).body()
}

/// ```php
/// return \Sharp\Internal\Slug\title($name);
/// ```
///
/// An `extern` method's body is the call of its native function, so a call of it inlines as that call, as any one-call
/// library method does, and the caller calls the native function directly.
#[test]
fn a_call_of_a_native_body_inlines_as_the_call_of_its_native_function() {
    assert_eq!(
        native_body("        return Slug.title(name);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "Sharp\\Internal\\Slug\\title"
                  ARG_LIST
                    VAR
                      ZVAL "name"
        "#}
    );
}

/// ```php
/// return \Sharp\Slug::join($name, $name);
/// ```
///
/// A variadic parameter gives no form, for a native body as for any other, so the call keeps its static call.
#[test]
fn a_call_of_a_variadic_native_body_keeps_its_static_call() {
    assert_eq!(
        native_body("        return Slug.join(name, name);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "Sharp\\Slug"
                  ZVAL "join"
                  ARG_LIST
                    VAR
                      ZVAL "name"
                    VAR
                      ZVAL "name"
        "#}
    );
}

/// The unit names each form it inlines once, with the form's fingerprint, which `Reads::inlined` takes.
#[test]
fn the_unit_names_each_form_it_inlines_with_its_fingerprint() {
    let forms = common::checked(TEXT.0, TEXT.1, &[], inline_forms).expect("the library file is checked");
    let (_, shout) = forms.iter().find(|(name, _)| name == b"sharp\\text::shout").expect("shout has a form");
    let code = "namespace App.Tenant;\n\nimport Sharp.Text;\n\nclass Report\n{\n    public string run(string name) => Text.shout(name) + Text.shout(name);\n}\n";
    let lowered = Lowered::inlining(code, &[], &[TEXT]);
    let inlined: Vec<(String, u64)> = lowered
        .unit()
        .expect("the source lowers")
        .inlined()
        .iter()
        .map(|read| (String::from_utf8_lossy(&read.name).into_owned(), read.fingerprint))
        .collect();

    assert_eq!(inlined, [("sharp\\text::shout".to_owned(), shout.fingerprint())]);
}

/// Each node an inlined form copies takes the line of its call, as the call's own nodes would, and each value keeps
/// its own line.
#[test]
fn an_inlined_form_runs_on_the_line_of_its_call() {
    let code = "namespace App.Tenant;\n\nimport Sharp.Text;\n\nclass Report\n{\n    public string run(string name)\n    {\n        return Text.padded(\n            name,\n            4\n        );\n    }\n}\n";
    let lowered = Lowered::inlining(code, &[], &[TEXT]);
    let unit = lowered.unit().expect("the source lowers");
    let lines: Vec<(String, u32)> = unit
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_ZVAL)
        .map(|node| match node.value {
            sharp_value::SHARP_STRING => (String::from_utf8_lossy(unit.text(node.text)).into_owned(), node.line),
            _ => (node.long_value.to_string(), node.line),
        })
        .filter(|(text, _)| ["str_pad", "trim", "name", "4"].contains(&text.as_str()))
        .collect();

    assert_eq!(
        lines,
        [
            ("name".to_owned(), 7),
            ("name".to_owned(), 10),
            ("4".to_owned(), 11),
            ("str_pad".to_owned(), 9),
            ("trim".to_owned(), 9)
        ]
    );
}

/// A form's fingerprint changes with what it runs, and not with the line the library writes it on, because each
/// inlined node takes the line of its call.
#[test]
fn a_form_fingerprint_follows_its_body_and_not_its_lines() {
    let fingerprint = |code: &str| {
        common::checked(TEXT.0, code, &[], inline_forms)
            .expect("the library file is checked")
            .into_iter()
            .find(|(name, _)| name == b"sharp\\text::shout")
            .map(|(_, form)| form.fingerprint())
            .expect("shout has a form")
    };

    assert_eq!(fingerprint(&TEXT.1.replacen("{\n", "{\n\n\n", 1)), fingerprint(TEXT.1));
    assert_ne!(fingerprint(&TEXT.1.replace("strtoupper", "strtolower")), fingerprint(TEXT.1));
}

/// `Money`, which declares `==`, `+`, unary `-` and `<=>` on lines 7, 9, 11 and 13.
const MONEY: &str = "namespace App;\n\npublic class Money\n{\n    public int hash() => 1;\n\n    public static bool operator ==(Money a, Money b) => true;\n\n    public static Money operator +(Money a, Money b) => a;\n\n    public static Money operator -(Money a) => a;\n\n    public static int operator <=>(Money a, Money b) => 0;\n}\n";

/// ```php
/// public static function op_Equality(?\App\Money $a, ?\App\Money $b): bool
/// {
///     if ($a === null || $b === null) { return $a === $b; }
///     return true;
/// }
/// ```
///
/// `==` is lifted over null, as C# lifts it: its parameters are nullable, with `ZEND_TYPE_NULLABLE`, which is 256, and
/// null equals only null before the declared body runs. `public static` is 17, `===` is 16, and `bool` is a name with
/// `ZEND_NAME_NOT_FQ`, which is 1.
#[test]
fn equality_is_op_equality_with_nullable_parameters_and_the_null_prologue() {
    assert_eq!(
        Lowered::new(MONEY).declared("op_Equality"),
        indoc! {r#"
            METHOD [17] "op_Equality" @7-7
              PARAM_LIST
                PARAM
                  ZVAL [256] "App\\Money"
                  ZVAL "a"
                  null
                  null
                  null
                  null
                PARAM
                  ZVAL [256] "App\\Money"
                  ZVAL "b"
                  null
                  null
                  null
                  null
              null
              STMT_LIST
                IF
                  IF_ELEM
                    OR
                      BINARY_OP [16]
                        VAR
                          ZVAL "a"
                        ZVAL null
                      BINARY_OP [16]
                        VAR
                          ZVAL "b"
                        ZVAL null
                    STMT_LIST
                      RETURN
                        BINARY_OP [16]
                          VAR
                            ZVAL "a"
                          VAR
                            ZVAL "b"
                RETURN
                  ZVAL true
              ZVAL [1] "bool"
              null
        "#}
    );
}

/// ```php
/// public static function op_Addition(\App\Money $a, \App\Money $b): \App\Money { return $a; }
/// public static function op_UnaryNegation(\App\Money $a): \App\Money { return $a; }
/// ```
///
/// Every other operator is the static method it runs as, with its parameters as declared and no prologue. `-` with one
/// parameter is unary `-`.
#[test]
fn addition_and_negation_are_their_static_methods_without_a_prologue() {
    let lowered = Lowered::new(MONEY);

    assert_eq!(
        lowered.declared("op_Addition"),
        indoc! {r#"
            METHOD [17] "op_Addition" @9-9
              PARAM_LIST
                PARAM
                  ZVAL "App\\Money"
                  ZVAL "a"
                  null
                  null
                  null
                  null
                PARAM
                  ZVAL "App\\Money"
                  ZVAL "b"
                  null
                  null
                  null
                  null
              null
              STMT_LIST
                RETURN
                  VAR
                    ZVAL "a"
              ZVAL "App\\Money"
              null
        "#}
    );
    assert_eq!(
        lowered.declared("op_UnaryNegation"),
        indoc! {r#"
            METHOD [17] "op_UnaryNegation" @11-11
              PARAM_LIST
                PARAM
                  ZVAL "App\\Money"
                  ZVAL "a"
                  null
                  null
                  null
                  null
              null
              STMT_LIST
                RETURN
                  VAR
                    ZVAL "a"
              ZVAL "App\\Money"
              null
        "#}
    );
}

/// `App\Order`, which extends `Money` and declares no operator.
const ORDER_OF_MONEY: (&str, &str) = ("src/App/Order.sharp", "namespace App;\n\npublic class Order : Money\n{\n}\n");

/// The statements of `run`, declared with `signature` and holding `statements`, in `App\Ledger`, whose `total` holds a
/// `Money` and whose `current()` returns it, lowered beside `Money` and `Order`.
fn ledger_body(signature: &str, statements: &str) -> String {
    let code = format!(
        "namespace App;\n\nclass Ledger\n{{\n    public Money total {{ get; set; }}\n\n    public Ledger(Money total)\n    {{\n        this.total = total;\n    }}\n\n    public Ledger current() => this;\n\n    public {signature}\n    {{\n{statements}    }}\n}}\n"
    );

    Lowered::with(&code, &[("src/App/Money.sharp", MONEY), ORDER_OF_MONEY]).body()
}

/// ```php
/// if ($c === null) { return \App\Money::op_Equality($a, $b); }
/// return !\App\Money::op_Equality($a, $c);
/// ```
///
/// `==` on instances calls the `operator ==` their class declares, and `!=` is its `!`, which is `ZEND_BOOL_NOT`, 14.
/// The method lifts a nullable side itself. `== null` tests for null with `===`, which is 16, and calls nothing.
#[test]
fn equality_of_instances_calls_op_equality_and_inequality_negates_it() {
    assert_eq!(
        ledger_body(
            "bool run(Money a, Money b, Money? c)",
            "        if (c == null) {\n            return a == b;\n        }\n        return a != c;\n"
        ),
        indoc! {r#"
            STMT_LIST
              IF
                IF_ELEM
                  BINARY_OP [16]
                    VAR
                      ZVAL "c"
                    ZVAL null
                  STMT_LIST
                    RETURN
                      STATIC_CALL
                        ZVAL "App\\Money"
                        ZVAL "op_Equality"
                        ARG_LIST
                          VAR
                            ZVAL "a"
                          VAR
                            ZVAL "b"
              RETURN
                UNARY_OP [14]
                  STATIC_CALL
                    ZVAL "App\\Money"
                    ZVAL "op_Equality"
                    ARG_LIST
                      VAR
                        ZVAL "a"
                      VAR
                        ZVAL "c"
        "#}
    );
}

/// ```php
/// if (\App\Money::op_Comparison($a, $b) < 0) { return \App\Money::op_Comparison($a, $b); }
/// return 0;
/// ```
///
/// An ordering compares what `operator <=>` returns with 0, and `<=>` is the call itself. `<` is `ZEND_IS_SMALLER`, 20.
#[test]
fn ordering_instances_compares_op_comparison_with_zero() {
    assert_eq!(
        ledger_body(
            "int run(Money a, Money b)",
            "        if (a < b) {\n            return a <=> b;\n        }\n        return 0;\n"
        ),
        indoc! {r#"
            STMT_LIST
              IF
                IF_ELEM
                  BINARY_OP [20]
                    STATIC_CALL
                      ZVAL "App\\Money"
                      ZVAL "op_Comparison"
                      ARG_LIST
                        VAR
                          ZVAL "a"
                        VAR
                          ZVAL "b"
                    ZVAL 0
                  STMT_LIST
                    RETURN
                      STATIC_CALL
                        ZVAL "App\\Money"
                        ZVAL "op_Comparison"
                        ARG_LIST
                          VAR
                            ZVAL "a"
                          VAR
                            ZVAL "b"
              RETURN
                ZVAL 0
        "#}
    );
}

/// ```php
/// return \App\Money::op_UnaryNegation(\App\Money::op_Addition($a, $b));
/// ```
#[test]
fn arithmetic_on_instances_calls_the_declared_operators() {
    assert_eq!(
        ledger_body("Money run(Money a, Money b)", "        return -(a + b);\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "App\\Money"
                  ZVAL "op_UnaryNegation"
                  ARG_LIST
                    STATIC_CALL
                      ZVAL "App\\Money"
                      ZVAL "op_Addition"
                      ARG_LIST
                        VAR
                          ZVAL "a"
                        VAR
                          ZVAL "b"
        "#}
    );
}

/// ```php
/// $this->total = \App\Money::op_Addition($this->total, $price);
/// $receiver#1->total = \App\Money::op_Addition(($receiver#1 = $this->current())->total, $price);
/// ```
///
/// `+=` on an instance assigns what `operator +` returns. A receiver that is not a local or `this` goes into a hidden
/// variable, which the read sets and the write reads, so the receiver runs once.
#[test]
fn a_compound_assignment_to_an_instance_assigns_the_operator_result_and_runs_its_receiver_once() {
    assert_eq!(
        ledger_body("void run(Money price)", "        this.total += price;\n        this.current().total += price;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "total"
                STATIC_CALL
                  ZVAL "App\\Money"
                  ZVAL "op_Addition"
                  ARG_LIST
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "total"
                    VAR
                      ZVAL "price"
              ASSIGN
                PROP
                  VAR
                    ZVAL "receiver#1"
                  ZVAL "total"
                STATIC_CALL
                  ZVAL "App\\Money"
                  ZVAL "op_Addition"
                  ARG_LIST
                    PROP
                      ASSIGN
                        VAR
                          ZVAL "receiver#1"
                        METHOD_CALL
                          VAR
                            ZVAL "this"
                          ZVAL "current"
                          ARG_LIST
                      ZVAL "total"
                    VAR
                      ZVAL "price"
        "#}
    );
}

/// ```php
/// return (\strcmp($x, $y) <=> 0) + ($m <=> $n);
/// ```
///
/// `<=>` orders two strings by their bytes, as the other orderings do, and two numbers as PHP's `<=>`, which is
/// `ZEND_SPACESHIP`, 170.
#[test]
fn spaceship_orders_strings_by_their_bytes_and_numbers_as_php() {
    assert_eq!(
        body_in("int run(string x, string y, int m, int n)", "        return (x <=> y) + (m <=> n);\n", &[]),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [1]
                  BINARY_OP [170]
                    CALL
                      ZVAL "strcmp"
                      ARG_LIST
                        VAR
                          ZVAL "x"
                        VAR
                          ZVAL "y"
                    ZVAL 0
                  BINARY_OP [170]
                    VAR
                      ZVAL "m"
                    VAR
                      ZVAL "n"
        "#}
    );
}

/// ```php
/// return \App\Money::op_Equality($order, $other);
/// ```
///
/// `Order` inherits `operator ==` from `Money`, so the call names the class that declares it.
#[test]
fn equality_of_a_subclass_calls_the_operator_its_parent_declares() {
    assert_eq!(
        ledger_body("bool run(Order order, Order other)", "        return order == other;\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "App\\Money"
                  ZVAL "op_Equality"
                  ARG_LIST
                    VAR
                      ZVAL "order"
                    VAR
                      ZVAL "other"
        "#}
    );
}

/// ```php
/// return \App\Money::op_Comparison(\App\Money::op_Addition(\App\Money::op_UnaryNegation($order), $order), $money) < 0
///     && \App\Money::op_Equality($maybe, $money)
///     && !\App\Money::op_Equality($maybe, $other)
///     && \App\Money::op_Comparison($money, $order) <= 0;
/// ```
///
/// The bridge calls the class the checker chose for every operand shape it accepts: an operator `Order` inherits, unary
/// and binary, a `Money` against an `Order`, a nullable left side of `==`, and two nullable sides of `!=`.
#[test]
fn every_operand_shape_the_checker_accepts_calls_the_class_that_declares_the_operator() {
    assert_eq!(
        ledger_body(
            "bool run(Order order, Money money, Order? maybe, Money? other)",
            "        return (-order + order) < money && maybe == money && maybe != other && money <= order;\n"
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                AND
                  AND
                    AND
                      BINARY_OP [20]
                        STATIC_CALL
                          ZVAL "App\\Money"
                          ZVAL "op_Comparison"
                          ARG_LIST
                            STATIC_CALL
                              ZVAL "App\\Money"
                              ZVAL "op_Addition"
                              ARG_LIST
                                STATIC_CALL
                                  ZVAL "App\\Money"
                                  ZVAL "op_UnaryNegation"
                                  ARG_LIST
                                    VAR
                                      ZVAL "order"
                                VAR
                                  ZVAL "order"
                            VAR
                              ZVAL "money"
                        ZVAL 0
                      STATIC_CALL
                        ZVAL "App\\Money"
                        ZVAL "op_Equality"
                        ARG_LIST
                          VAR
                            ZVAL "maybe"
                          VAR
                            ZVAL "money"
                    UNARY_OP [14]
                      STATIC_CALL
                        ZVAL "App\\Money"
                        ZVAL "op_Equality"
                        ARG_LIST
                          VAR
                            ZVAL "maybe"
                          VAR
                            ZVAL "other"
                  BINARY_OP [21]
                    STATIC_CALL
                      ZVAL "App\\Money"
                      ZVAL "op_Comparison"
                      ARG_LIST
                        VAR
                          ZVAL "money"
                        VAR
                          ZVAL "order"
                    ZVAL 0
        "#}
    );
}
