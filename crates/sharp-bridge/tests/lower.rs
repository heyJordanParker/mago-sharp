//! Each test lowers a PHP# snippet and compares the node tree with the `zend_ast` that php-src's
//! `zend_language_parser.y` builds for the equivalent PHP, quoted above each test.
//!
//! A tree prints one node per line: its kind without `SHARP_AST_`, its attr in brackets when it is not 0, a ZVAL's
//! value, and a declaration's name and lines. `null` is a null child.

#![allow(clippy::panic, clippy::expect_used, clippy::use_debug)]

mod common;

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;
use std::thread;

use indoc::indoc;

use mago_database::file::File;
use mago_database::file::FileType;
use mago_names::INLINED_COLLECTION_METHODS;
use mago_sharp_bridge::InlineForms;
use mago_sharp_bridge::Unit;
use mago_sharp_bridge::inline_forms;
use mago_sharp_bridge::lower;
use mago_sharp_bridge::sharp_kind;
use mago_sharp_bridge::sharp_node;
use mago_sharp_bridge::sharp_str;
use mago_sharp_bridge::sharp_value;
use mago_sharp_bridge::unit::encode;
use xxhash_rust::xxh3::xxh3_64;

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
        self.tree_without(&[])
    }

    /// The tree, leaving out the methods PHP declares as one of `methods`, and the `parse` and `tryParse` the bridge
    /// gives a struct or a class, which tests of their own show.
    fn tree_without(&self, methods: &[&str]) -> String {
        assert_eq!(self.diagnostics(), Vec::<String>::new(), "the source lowers");
        let mut tree = String::new();
        self.render_into(self.root(), 0, methods, &self.generated(), &mut tree);

        tree
    }

    /// The `parse` and `tryParse` methods the bridge gives a struct or a class. Each spans the type's name, so it
    /// starts and ends on the line of its class.
    fn generated(&self) -> HashSet<u32> {
        let mut generated = HashSet::new();
        for (index, class) in self.nodes().iter().enumerate() {
            if class.kind != sharp_kind::SHARP_AST_CLASS {
                continue;
            }
            let members = self.child(index as u32, 2);
            if self.nodes().get(members as usize).is_none_or(|members| members.kind != sharp_kind::SHARP_AST_STMT_LIST)
            {
                continue;
            }

            for member in 0..self.nodes()[members as usize].child_count {
                let member = self.child(members, member);
                let method = &self.nodes()[member as usize];
                if method.kind == sharp_kind::SHARP_AST_METHOD
                    && ["parse", "tryParse"].contains(&self.text(method.text).as_str())
                    && (method.line, method.end_line) == (class.line, class.line)
                {
                    generated.insert(member);
                }
            }
        }

        generated
    }

    /// Each node the source writes, with its index, in the order the bridge lowers them: every node but those of the
    /// `parse` and `tryParse` the bridge gives a struct or a class.
    fn written(&self) -> Vec<(usize, &sharp_node)> {
        let mut left_out = HashSet::new();
        let mut pending: Vec<u32> = self.generated().into_iter().collect();
        while let Some(index) = pending.pop() {
            if let Some(node) = self.nodes().get(index as usize)
                && left_out.insert(index as usize)
            {
                pending.extend((0..node.child_count).map(|child| self.child(index, child)));
            }
        }

        self.nodes().iter().enumerate().filter(|(index, _)| !left_out.contains(index)).collect()
    }

    /// The statements of the method `run`.
    fn body(&self) -> String {
        self.body_of("run")
    }

    /// The statements of the first method the source declares as `name`.
    fn body_of(&self, name: &str) -> String {
        self.render(self.child(self.method_named(name), 2))
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

    /// The parameter list of the first method the source declares as `name`.
    fn parameters_of(&self, name: &str) -> String {
        assert_eq!(self.diagnostics(), Vec::<String>::new(), "the source lowers");
        let method = self
            .nodes()
            .iter()
            .position(|node| node.kind == sharp_kind::SHARP_AST_METHOD && self.text(node.text) == name)
            .unwrap_or_else(|| panic!("the source declares `{name}`"));

        self.render(self.child(method as u32, 0))
    }

    fn child(&self, node: u32, index: u32) -> u32 {
        self.children()[(self.nodes()[node as usize].first_child + index) as usize]
    }

    /// `node` and what it holds, leaving out the `parse` and `tryParse` the bridge gives a struct or a class below it.
    fn render(&self, node: u32) -> String {
        let mut tree = String::new();
        self.render_into(node, 0, &[], &self.generated(), &mut tree);

        tree
    }

    fn render_into(&self, index: u32, depth: usize, skipped: &[&str], generated: &HashSet<u32>, tree: &mut String) {
        let node = self.nodes().get(index as usize);
        if depth > 0 && generated.contains(&index)
            || node.is_some_and(|node| {
                node.kind == sharp_kind::SHARP_AST_METHOD && skipped.contains(&self.text(node.text).as_str())
            })
        {
            return;
        }
        tree.push_str(&"  ".repeat(depth));
        let Some(node) = node else {
            tree.push_str("null\n");

            return;
        };

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
            self.render_into(self.child(index, child), depth + 1, skipped, generated, tree);
        }
    }

    /// The statement of `parse` that reads each field of the struct, in order, each after a blank line.
    fn parse_reads(&self) -> String {
        let parse = self.method_named("parse");
        let body = self.child(parse, 2);
        // `parse` starts with the input and three empty arrays, and ends with the throw, the object, its properties
        // and the return.
        let reads = 4..self.nodes()[body as usize].child_count - 4;

        reads.map(|read| self.render(self.child(body, read))).collect::<Vec<String>>().join("\n")
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
    "namespace Sharp;\n\npublic class Text\n{\n    public static string shout(string text) => global.strtoupper(text);\n\n    public static string padded(string text, int width = 8) => global.str_pad(global.trim(text), width);\n\n    public static string twice(string text) => global.str_repeat(text, global.strlen(text));\n\n    public static string trimmed(string text)\n    {\n        const trimmed = global.trim(text);\n        return trimmed;\n    }\n\n    public static Text make() => new Text();\n\n    public string label() => \"text\";\n\n    public string wrapped(string format) => global.sprintf(format, this.label());\n\n    public string secret(string text) => this.hidden(text);\n\n    private string hidden(string text) => global.strtolower(text);\n\n    public static string quiet(string text) => Text.muted(text);\n\n    protected static string muted(string text) => global.strtolower(text);\n\n    public virtual string echoed(string text) => this.wrapped(text);\n\n    public static string again(string text) => Self.shout(text);\n}\n",
);

/// `Lib\Loud`, a project file with a method the inlining rule would take in the standard library.
const LOUD: (&str, &str) = (
    "src/Lib/Loud.sharp",
    "namespace Lib;\n\npublic class Loud\n{\n    public static string shout(string text) => global.strtoupper(text);\n}\n",
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
        "        while (extra > 0) {\n            while (extra > 1) {\n                break 2;\n            }\n        }\n        return 1;\n",
    ));

    assert_eq!(lowered.diagnostics(), ["11:17 compile error: This statement is not supported yet in PHP#."]);
}

/// Spec section 2: each of the 43 PHP keywords PHP# doesn't use is a plain name, and so is a word that means something
/// only inside its own PHP# construct, as C#'s contextual keywords are. Each names a property, a local and a
/// parameter, and each reads as the variable or property of its name.
///
/// ```php
/// $list = $extra + $this->list; return $list;
/// ```
#[test]
fn a_word_php_sharp_does_not_reserve_names_a_property_a_local_and_a_parameter() {
    let words = [
        "fn",
        "or",
        "and",
        "die",
        "use",
        "var",
        "xor",
        "echo",
        "eval",
        "from",
        "goto",
        "list",
        "array",
        "clone",
        "empty",
        "endif",
        "isset",
        "print",
        "trait",
        "unset",
        "elseif",
        "endfor",
        "global",
        "parent",
        "switch",
        "declare",
        "default",
        "extends",
        "foreach",
        "include",
        "require",
        "callable",
        "endwhile",
        "function",
        "endswitch",
        "insteadof",
        "enddeclare",
        "endforeach",
        "implements",
        "instanceof",
        "include_once",
        "require_once",
        "__halt_compiler",
        "get",
        "set",
        "init",
        "field",
        "value",
        "of",
        "via",
        "uses",
        "not",
    ];

    for word in words {
        let lowered = Lowered::new(&format!(
            "namespace App.Tenant;\n\nclass Report\n{{\n    private int {word} = 1;\n\n    public int run(int extra)\n    {{\n        const {word} = extra + this.{word};\n        return {word};\n    }}\n\n    public int read(int {word}) => {word};\n}}\n"
        ));

        assert_eq!(
            lowered.body(),
            format!(
                "STMT_LIST\n  ASSIGN\n    VAR\n      ZVAL \"{word}\"\n    BINARY_OP [1]\n      VAR\n        ZVAL \"extra\"\n      PROP\n        VAR\n          ZVAL \"this\"\n        ZVAL \"{word}\"\n  RETURN\n    VAR\n      ZVAL \"{word}\"\n"
            ),
            "{word}"
        );
        assert_eq!(
            lowered.declared("read"),
            format!(
                "METHOD [1] \"read\" @13-13\n  PARAM_LIST\n    PARAM\n      ZVAL [1] \"int\"\n      ZVAL \"{word}\"\n      null\n      null\n      null\n      null\n  null\n  STMT_LIST\n    RETURN\n      VAR\n        ZVAL \"{word}\"\n  SHARP_TYPE_ARGS\n    ZVAL [1] \"int\"\n    ZVAL \"Function<int(int)>\"\n  null\n"
            ),
            "{word}"
        );
    }
}

/// Any word names a method, a property or a field, a keyword PHP# uses included, as in TypeScript and PHP: a member's
/// name is written only where it is declared and after `.`, so it never reads as the keyword. A PHP# class can then
/// override a plain PHP method named `is` or `lazy`, as Eloquent's `Model::is` and `Collection::lazy` are.
///
/// ```php
/// return $this->is($this) ? $this->lazy()->match() : $this->default;
/// ```
#[test]
fn any_word_names_a_member() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    private int return = 2;\n\n    public int default { get; } = 3;\n\n    public bool is(Report other) => other.default == this.default;\n\n    public int match() => this.return;\n\n    public Report lazy() => this;\n\n    public int run(int extra)\n    {\n        return this.is(this) ? this.lazy().match() : this.default;\n    }\n}\n",
    );

    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
    assert_eq!(
        lowered.body(),
        "STMT_LIST\n  RETURN\n    CONDITIONAL\n      METHOD_CALL\n        VAR\n          ZVAL \"this\"\n        ZVAL \"is\"\n        ARG_LIST\n          VAR\n            ZVAL \"this\"\n      METHOD_CALL\n        METHOD_CALL\n          VAR\n            ZVAL \"this\"\n          ZVAL \"lazy\"\n          ARG_LIST\n        ZVAL \"match\"\n        ARG_LIST\n      PROP\n        VAR\n          ZVAL \"this\"\n        ZVAL \"default\"\n"
    );
}

/// A PHP keyword PHP# doesn't use names a class, and so does a capitalized PHP# keyword, since PHP#'s keywords are
/// lowercase. Code reads each one as a type and creates it. PHP reaches the class by its full name, as the engine
/// takes any class name but the reserved type names.
///
/// ```php
/// $print = new \App\Shop\Print(); $refund = new \App\Shop\Return(); return $print->n() + $refund->n();
/// ```
#[test]
fn a_php_keyword_or_a_capitalized_php_sharp_keyword_names_a_class() {
    let print = ("src/Shop/Print.sharp", "namespace App.Shop;\n\npublic class Print\n{\n    public int n() => 1;\n}\n");
    let r#return =
        ("src/Shop/Return.sharp", "namespace App.Shop;\n\npublic class Return\n{\n    public int n() => 2;\n}\n");
    let lowered = Lowered::with(
        "namespace App.Shop;\n\npublic class Match\n{\n    public int run(int extra)\n    {\n        Print print = new Print();\n        Return refund = new Return();\n        return print.n() + refund.n();\n    }\n}\n",
        &[print, r#return],
    );

    assert_eq!(
        lowered.body(),
        "STMT_LIST\n  ASSIGN\n    VAR\n      ZVAL \"print\"\n    NEW\n      ZVAL \"App\\\\Shop\\\\Print\"\n      ARG_LIST\n  ASSIGN\n    VAR\n      ZVAL \"refund\"\n    NEW\n      ZVAL \"App\\\\Shop\\\\Return\"\n      ARG_LIST\n  RETURN\n    BINARY_OP [1]\n      METHOD_CALL\n        VAR\n          ZVAL \"print\"\n        ZVAL \"n\"\n        ARG_LIST\n      METHOD_CALL\n        VAR\n          ZVAL \"refund\"\n        ZVAL \"n\"\n        ARG_LIST\n"
    );
}

/// An event is a class, so a PHP-only word or a capitalized PHP# keyword names one, as `Print` does. It lowers to the
/// final class `[32]` the engine declares, whose constructor promotes `id`.
#[test]
fn a_php_keyword_or_a_capitalized_php_sharp_keyword_names_an_event() {
    let lowered = Lowered::new("namespace App.Shop;\n\npublic event Print(int id);\n");

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
                ZVAL "App\\Shop"
                null
              CLASS [32] "Print" @3-3
                null
                null
                STMT_LIST
                  METHOD [1] "__construct" @3-3
                    PARAM_LIST
                      PARAM [129]
                        ZVAL [1] "int"
                        ZVAL "id"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                    null
                    null
                null
                null
        "#}
    );
}

/// Only a file declares a type, so in a class body a class named after a type's keyword or a modifier is a member's
/// type, as `Struct`, `Class` and `Lazy` are here, and an operator's return type keeps its name.
#[test]
fn a_class_named_after_a_type_keyword_or_a_modifier_types_a_member() {
    let r#struct =
        ("src/Shop/Struct.sharp", "namespace App.Shop;\n\npublic class Struct\n{\n    public Struct() { }\n}\n");
    let class = ("src/Shop/Class.sharp", "namespace App.Shop;\n\npublic class Class\n{\n    public Class() { }\n}\n");
    let lazy = (
        "src/Shop/Lazy.sharp",
        "namespace App.Shop;\n\npublic class Lazy\n{\n    public Lazy() { }\n\n    public static Lazy operator +(Lazy a, Lazy b) { return a; }\n}\n",
    );
    let lowered = Lowered::with(
        "namespace App.Shop;\n\npublic class Report\n{\n    public Struct shape { get; set; }\n    public Class kind { get; set; }\n\n    public Report()\n    {\n        this.shape = new Struct();\n        this.kind = new Class();\n    }\n\n    public Lazy sum(Lazy a, Lazy b) => a + b;\n}\n",
        &[r#struct, class, lazy],
    );

    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
    assert!(
        lowered.tree().contains("ZVAL \"shape\"") && lowered.tree().contains("ZVAL \"kind\""),
        "{}",
        lowered.tree()
    );
}

/// Each keyword an error on a keyword, `true`, `false` or `null` written in another case names compiles once written
/// in lowercase, in every place the error fires: a statement on two lines and two keywords on one line included.
#[test]
fn the_lowercase_keyword_a_keyword_in_another_case_names_compiles() {
    let lowered = Lowered::new(
        "class Report\n{\n    public static int count() { return 1; }\n\n    public int run(bool flag, List<int> items)\n    {\n        const base = 2;\n        let total = 1;\n        if (flag) { }\n        const c = new\n            Report();\n        new Report();\n        let on = true;\n        let off = false;\n        int? none = null;\n        if (flag) { return 5; }\n        try { total = 3; } catch (Exception e) { total = 4; } finally { }\n        if (flag) { total = 5; } else if (items.count() == 0) { total = 6; } else { total = 7; }\n        do { total = 8; } while (off);\n        for (const item of items) { if (flag) { break; } continue; }\n        return flag ? base : total;\n    }\n}\n",
    );

    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
}

/// The PHP# form each refusal of `isset`, `empty`, `unset`, `array` and `list` quotes compiles.
#[test]
fn the_php_sharp_form_a_refused_php_construct_names_compiles() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public bool run(int? value, List<int> items, List<int> pair, Map<string, int> map, string key)\n    {\n        const listed = [1, 2];\n        const first = pair[0];\n        map.remove(key);\n        return value != null && items.count() == 0 && listed.count() == first;\n    }\n}\n",
    );

    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
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
        .written()
        .into_iter()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_METHOD || node.kind == sharp_kind::SHARP_AST_RETURN)
        .map(|(_, node)| (format!("{:?}", node.kind), node.line, node.end_line))
        .collect();
    assert_eq!(lines, [("SHARP_AST_RETURN".to_owned(), 5, 0), ("SHARP_AST_METHOD".to_owned(), 3, 6)]);

    let refused = Lowered::new("class Report\r{\r    public void run() { echo 1; }\r}\r");
    assert_eq!(
        refused.diagnostics(),
        ["3:25 compile error: PHP# has no `echo`: write `global.printf` or `global.fwrite`."]
    );
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
    let library =
        [("vendor/acme/Mailer.php", "<?php\n\nnamespace Acme;\n\nclass Mailer\n{\n}\n\nclass Failed\n{\n}\n")];
    let with = Lowered::with(
        "namespace App.Tenant;\n\nimport Acme.Failed;\nimport Acme.Mailer;\n\nextern Mailer uses Mail, Http;\nextern global.trim;\nextern event Failed;\n\nclass Report\n{\n}\n",
        &library,
    );
    let without = Lowered::with(
        "namespace App.Tenant;\n\nimport Acme.Failed;\nimport Acme.Mailer;\n\n\n\n\n\nclass Report\n{\n}\n",
        &library,
    );

    assert_eq!(with.tree(), without.tree());
}

/// `foreign` tells the checker which effect a class's objects carry, spec section 29, so the engine gets the plain
/// class, which a test fake may extend. Only the function type of each method lists the effect, as `is` tests a
/// closure of it.
#[test]
fn a_foreign_class_lowers_to_the_nodes_of_the_plain_class() {
    let foreign = Lowered::new(
        "namespace App.Billing;\n\npublic foreign class Payments\n{\n    public int charge(int cents) => cents;\n}\n",
    );
    let plain = Lowered::new(
        "namespace App.Billing;\n\npublic class Payments\n{\n    public int charge(int cents) => cents;\n}\n",
    );

    assert_eq!(
        foreign.tree(),
        plain.tree().replace("\"Function<int(int)>\"", "\"Function<int(int) uses App.Billing.Payments>\"")
    );
}

/// A law, spec section 28, is checked and never runs, so the engine gets the same tree without it. A module of laws
/// alone binds no service, so its `Module.sharp` lowers to its namespace alone.
#[test]
fn a_class_an_enum_or_a_module_with_a_law_lowers_to_the_nodes_it_lowers_to_without_it() {
    for (path, with, without) in [
        (
            "src/Report.sharp",
            "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }) { }\n\n    law amountIsItself(Money a) => a.amount == a.amount;\n}\n\npublic enum Status\n{\n    case Open;\n\n    law openIsOpen(Status s) => Status.Open == Status.Open;\n}\n",
            "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }) { }\n\n\n}\n\npublic enum Status\n{\n    case Open;\n\n\n}\n",
        ),
        (
            "app/Shared/Module.sharp",
            "namespace App.Shared;\n\nmodule\n{\n    law sameAmount(int a) => a == a;\n}\n",
            "namespace App.Shared;\n",
        ),
    ] {
        let with = Lowered::named(path, with);
        let without = Lowered::named(path, without);

        assert!(with.0.is_ok(), "{path}: {:?}", with.0.err());
        assert_eq!(with.tree(), without.tree(), "{path}");
    }
}

/// Spec section 32: a module file returns its bindings, after its classes, as the table `Sharp\Container` reads. Each
/// row is a module's bindings and the binding the table holds for them, as php-src builds the PHP each comment shows.
#[test]
fn a_module_returns_each_binding_as_its_namespace_service_class_lifetime_and_value() {
    let library = [
        ("app/Mail/Mailer.sharp", "namespace App.Mail;\n\npublic interface Mailer\n{\n}\n"),
        ("app/Mail/SmtpMailer.sharp", "namespace App.Mail;\n\npublic class SmtpMailer : Mailer\n{\n}\n"),
        ("app/Pricing/PriceRule.sharp", "namespace App.Pricing;\n\npublic interface PriceRule\n{\n}\n"),
        ("app/Pricing/TaxRule.sharp", "namespace App.Pricing;\n\npublic class TaxRule : PriceRule\n{\n}\n"),
        (
            "app/Shop/SendReceipt.sharp",
            "namespace App.Shop;\n\nimport App.Mail.Mailer;\n\npublic class SendReceipt\n{\n    public SendReceipt(private Mailer mailer) { }\n}\n",
        ),
    ];
    let module = |bindings: &str| {
        format!(
            "namespace App.Shop;\n\nimport App.Mail.Mailer;\nimport App.Mail.SmtpMailer;\nimport App.Pricing.PriceRule;\nimport App.Pricing.TaxRule;\n\nmodule\n{{\n{bindings}\n}}\n"
        )
    };

    let mailer = r#"ZVAL "app\\mail\\mailer""#;
    let smtp = r#"ZVAL "App\\Mail\\SmtpMailer""#;
    for (bindings, expected) in [
        // ['app\shop', 'app\mail\mailer', null, 'singleton', 'App\Mail\SmtpMailer']
        ("    singleton Mailer = SmtpMailer;", [mailer, "ZVAL null", r#"ZVAL "singleton""#, smtp]),
        // ['app\shop', 'app\mail\mailer', 'App\Shop\SendReceipt', 'scoped', 'App\Mail\SmtpMailer']
        (
            "    scoped Mailer for SendReceipt = SmtpMailer;",
            [mailer, r#"ZVAL "App\\Shop\\SendReceipt""#, r#"ZVAL "scoped""#, smtp],
        ),
        // ['app\shop', 'list<app\pricing\pricerule>', null, 'transient', ['App\Pricing\TaxRule']]
        (
            "    transient List<PriceRule> = [TaxRule];",
            [
                r#"ZVAL "list<app\\pricing\\pricerule>""#,
                "ZVAL null",
                r#"ZVAL "transient""#,
                indoc! {r#"
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "App\\Pricing\\TaxRule"
                        null"#},
            ],
        ),
        // ['app\shop', 'app\mail\mailer', null, 'singleton', static fn (\App\Mail\SmtpMailer $smtp) => $smtp]
        (
            "    singleton Mailer(SmtpMailer smtp) = smtp;\n    transient SmtpMailer = SmtpMailer;",
            [
                mailer,
                "ZVAL null",
                r#"ZVAL "singleton""#,
                indoc! {r#"
                    ARROW_FUNC [16] "" @10-10
                      PARAM_LIST
                        PARAM
                          ZVAL "App\\Mail\\SmtpMailer"
                          ZVAL "smtp"
                          null
                          null
                          null
                          null
                      null
                      VAR
                        ZVAL "smtp"
                      null
                      null"#},
            ],
        ),
        // ['app\shop', 'app\mail\mailer', null, 'singleton',
        //     static fn (#[\Sharp\Internal\Service('list<app\pricing\pricerule>')] array $rules) => new \App\Mail\SmtpMailer()]
        (
            "    singleton Mailer(List<PriceRule> rules) = new SmtpMailer();\n    transient List<PriceRule> = [TaxRule];",
            [
                mailer,
                "ZVAL null",
                r#"ZVAL "singleton""#,
                indoc! {r#"
                    ARROW_FUNC [16] "" @10-10
                      PARAM_LIST
                        PARAM
                          TYPE [7]
                          ZVAL "rules"
                          null
                          ATTRIBUTE_LIST
                            ATTRIBUTE_GROUP
                              ATTRIBUTE
                                ZVAL "Sharp\\Internal\\Service"
                                ARG_LIST
                                  ZVAL "list<app\\pricing\\pricerule>"
                          null
                          null
                      null
                      NEW
                        ZVAL "App\\Mail\\SmtpMailer"
                        ARG_LIST
                      null
                      null"#},
            ],
        ),
    ] {
        let lowered = Lowered(common::checked("app/Shop/Module.sharp", &module(bindings), &library, lower));
        // `return [[namespace, service, class, lifetime, value]];`, the file's third statement.
        let table = lowered.child(lowered.root(), 2);
        let entry = lowered.child(lowered.child(lowered.child(table, 0), 0), 0);
        let fields: Vec<String> =
            (0..5).map(|index| lowered.render(lowered.child(lowered.child(entry, index), 0))).collect();

        let expected: Vec<String> =
            std::iter::once(r#"ZVAL "app\\shop""#).chain(expected).map(|field| format!("{field}\n")).collect();
        assert_eq!(fields, expected, "{bindings}");
    }
}

/// Spec section 32: the standard library's module binds the services the library gives, such as `Environment`, for
/// every namespace, so its table names the root namespace, which every class's lookup reaches last. An app's module
/// names its own namespace. Each row is a module file, its code and the namespace its table names.
#[test]
fn the_standard_library_module_binds_its_services_for_the_root_namespace() {
    let binding = "module\n{\n    singleton Environment = Environment;\n}\n";
    for (path, namespace, expected) in [
        ("vendor/heyjordanparker/php-sharp-composer/library/Sharp/Module.sharp", "Sharp", "ZVAL \"\"\n"),
        ("app/Module.sharp", "App", "ZVAL \"app\"\n"),
    ] {
        let code = format!("namespace {namespace};\n\n{binding}");
        let lowered = Lowered(common::checked(path, &code, &[common::ENVIRONMENT], lower));
        // `return [[namespace, service, class, lifetime, value]];`, the file's third statement.
        let table = lowered.child(lowered.root(), 2);
        let entry = lowered.child(lowered.child(lowered.child(table, 0), 0), 0);

        assert_eq!(lowered.render(lowered.child(lowered.child(entry, 0), 0)), expected, "{path}");
    }
}

/// Spec section 32: a test file's module applies while a test of a class the file declares runs, so the file hands
/// its bindings to the container for each of its classes, as `\Sharp\Internal\testModule([...], [...])`, and returns
/// nothing. Each row is the types the file declares beside its module and the classes the call names.
#[test]
fn a_test_module_hands_its_bindings_to_the_container_for_each_class_its_file_declares() {
    let library = [
        ("app/Mail/Mailer.sharp", "namespace App.Mail;\n\npublic interface Mailer\n{\n}\n"),
        ("app/Mail/FakeMailer.sharp", "namespace App.Mail;\n\npublic class FakeMailer : Mailer\n{\n}\n"),
        ("app/Mail/SmtpMailer.sharp", "namespace App.Mail;\n\npublic class SmtpMailer : Mailer\n{\n}\n"),
        (
            "app/Shop/Module.sharp",
            "namespace App.Shop;\n\nimport App.Mail.Mailer;\nimport App.Mail.SmtpMailer;\n\nmodule\n{\n    singleton Mailer = SmtpMailer;\n}\n",
        ),
    ];
    let file = |types: &str| {
        format!(
            "namespace App.Shop;\n\nimport App.Mail.Mailer;\nimport App.Mail.FakeMailer;\n\nmodule : App.Shop\n{{\n    singleton Mailer = FakeMailer;\n}}\n\n{types}"
        )
    };

    for (types, classes) in [
        ("public class CheckoutTest\n{\n}\n", vec!["App\\Shop\\CheckoutTest"]),
        (
            "public class CheckoutTest\n{\n}\n\npublic class RefundTest\n{\n}\n",
            vec!["App\\Shop\\CheckoutTest", "App\\Shop\\RefundTest"],
        ),
        ("public class CheckoutTest\n{\n}\n\npublic interface Step\n{\n}\n", vec!["App\\Shop\\CheckoutTest"]),
    ] {
        let lowered = Lowered(common::checked("tests/Shop/CheckoutTest.sharp", &file(types), &library, lower));
        let calls: Vec<u32> = (0..lowered.nodes().len() as u32)
            .filter(|&node| {
                lowered.nodes()[node as usize].kind == sharp_kind::SHARP_AST_CALL
                    && lowered.render(lowered.child(node, 0)) == "ZVAL \"Sharp\\\\Internal\\\\testModule\"\n"
            })
            .collect();
        let elements = classes
            .iter()
            .map(|class| format!("  ARRAY_ELEM\n    ZVAL {class:?}\n    null\n"))
            .collect::<Vec<_>>()
            .concat();

        assert_eq!(calls.len(), 1, "{types}");
        assert_eq!(
            lowered.render(lowered.child(lowered.child(calls[0], 1), 0)),
            format!("ARRAY [3]\n{elements}"),
            "{types}"
        );
        let root = lowered.root();
        let statements = 0..lowered.nodes()[root as usize].child_count;
        assert!(
            statements
                .map(|index| lowered.child(root, index))
                .all(|statement| lowered.nodes()[statement as usize].kind != sharp_kind::SHARP_AST_RETURN),
            "{types}: the file returns no binding table"
        );
    }
}

/// Spec section 32: PHPUnit runs the methods `Before` and `After` mark in its own process and in a child one, so
/// each class of a test file starts its tests with the file's module before every other `Before` method and `setUp`,
/// and ends them after every other `After` method and `tearDown`, by priority. A test module with no bindings still
/// applies, so its tests run without the environment modules. A class of any other file gets neither method.
#[test]
fn a_test_files_class_starts_each_test_with_the_test_module_and_ends_it_after() {
    let library = [
        ("app/Mail/Mailer.sharp", "namespace App.Mail;\n\npublic interface Mailer\n{\n}\n"),
        ("app/Mail/FakeMailer.sharp", "namespace App.Mail;\n\npublic class FakeMailer : Mailer\n{\n}\n"),
    ];
    let file = |members: &str| {
        format!(
            "namespace App.Shop;\n\nimport App.Mail.Mailer;\nimport App.Mail.FakeMailer;\n\nmodule : App.Shop\n{{\n{members}}}\n\npublic class CheckoutTest\n{{\n}}\n"
        )
    };
    let hook = |name: &str, function: &str, argument: &str, attribute: &str, priority: i64| {
        format!(
            "METHOD [1] {name:?} @{{line}}-{{end}}\n  PARAM_LIST\n  null\n  STMT_LIST\n    CALL\n      ZVAL {function:?}\n      ARG_LIST\n{argument}  ZVAL [1] \"void\"\n  ATTRIBUTE_LIST\n    ATTRIBUTE_GROUP\n      ATTRIBUTE\n        ZVAL {attribute:?}\n        ARG_LIST\n          ZVAL {priority}\n"
        )
    };
    let start = hook(
        "__sharpStartTest",
        "Sharp\\Internal\\startTest",
        "        CLASS_NAME\n          ZVAL [1] \"static\"\n",
        "PHPUnit\\Framework\\Attributes\\Before",
        i64::MAX,
    );
    let end = hook("__sharpEndTest", "Sharp\\Internal\\endTest", "", "PHPUnit\\Framework\\Attributes\\After", i64::MIN);

    for (members, line, table) in
        [("    singleton Mailer = FakeMailer;\n", 11, "ARRAY [3]\n  ARRAY_ELEM\n"), ("", 10, "ARRAY [3]\n")]
    {
        let lowered = Lowered(common::checked("tests/Shop/CheckoutTest.sharp", &file(members), &library, lower));
        let at = |hook: &str| hook.replace("{line}", &line.to_string()).replace("{end}", &(line + 2).to_string());

        assert_eq!(lowered.declared("__sharpStartTest"), at(&start), "{members:?}");
        assert_eq!(lowered.declared("__sharpEndTest"), at(&end), "{members:?}");
        let call = (0..lowered.nodes().len() as u32)
            .find(|&node| {
                lowered.nodes()[node as usize].kind == sharp_kind::SHARP_AST_CALL
                    && lowered.render(lowered.child(node, 0)) == "ZVAL \"Sharp\\\\Internal\\\\testModule\"\n"
            })
            .unwrap_or_else(|| panic!("{members:?}: the file calls testModule"));
        let bindings = lowered.render(lowered.child(lowered.child(call, 1), 1));
        assert!(bindings.starts_with(table), "{members:?}: {bindings}");
    }

    let base = Lowered(common::checked(
        "app/Shop/Checkout.sharp",
        "namespace App.Shop;\n\npublic class Checkout\n{\n}\n",
        &library,
        lower,
    ));
    let methods: Vec<String> = base
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_METHOD)
        .map(|node| base.text(node.text))
        .collect();
    assert!(!methods.iter().any(|method| method == "__sharpStartTest" || method == "__sharpEndTest"), "{methods:?}");
}

/// Spec section 32: the container fills a class's main constructor, and reads each parameter's service from its PHP
/// type. A `List` runs as `array`, so a constructor's collection of objects names its service in an attribute, and no
/// other parameter does: a collection of values is no service. Each row is a parameter and the attributes the PHP of
/// the constructor gives it. A method's parameter never has one.
#[test]
fn a_constructor_parameter_whose_php_type_drops_its_service_names_the_service() {
    let library = [("app/Pricing/TaxRule.sharp", "namespace App.Pricing;\n\npublic class TaxRule\n{\n}\n")];
    let service = |id: &str| {
        format!(
            "ATTRIBUTE_LIST\n  ATTRIBUTE_GROUP\n    ATTRIBUTE\n      ZVAL \"Sharp\\\\Internal\\\\Service\"\n      ARG_LIST\n        ZVAL {id:?}\n"
        )
    };

    for (parameter, constructor) in [
        ("List<TaxRule> rules", service("list<app\\pricing\\taxrule>")),
        ("Map<string, TaxRule> rules", service("array<string, app\\pricing\\taxrule>")),
        ("List<string> names", "null\n".to_owned()),
        ("TaxRule rule", "null\n".to_owned()),
        ("int count", "null\n".to_owned()),
    ] {
        let code = format!(
            "namespace App.Shop;\n\nimport App.Pricing.TaxRule;\n\npublic class Cart\n{{\n    public Cart({parameter}) {{ }}\n\n    public void price({parameter}) {{ }}\n}}\n"
        );
        let lowered = Lowered::with(&code, &library);
        let attributes = |method: u32| lowered.render(lowered.child(lowered.child(lowered.child(method, 0), 0), 3));

        assert_eq!(attributes(lowered.method_named("__construct")), constructor, "{parameter}");
        assert_eq!(attributes(lowered.method_named("price")), "null\n", "{parameter}");
    }
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
/// `[1]` on the method is `ZEND_ACC_PUBLIC`, and `[1]` on `int` is `ZEND_NAME_NOT_FQ`. The return type sits in a
/// `SHARP_TYPE_ARGS` with the method's whole function type, which a closure of the method keeps for `is`, spec section
/// 21.
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
                  SHARP_TYPE_ARGS
                    ZVAL [1] "int"
                    ZVAL "Function<int(int)>"
                  null
              null
              null
        "#}
    );
}

/// ```php
/// private function reserve(): int { return 1; }
/// private static function make(): int { return 2; }
/// ```
///
/// Spec section 5: a method with no access modifier is `private`, `[4]`, and a static one `[20]`, while PHP makes it
/// `public`.
#[test]
fn a_method_without_an_access_modifier_is_a_private_function() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\npublic class Report\n{\n    int reserve() => 1;\n    static int make() => 2;\n}\n",
    );
    let members = lowered.child(lowered.child(lowered.root(), 2), 2);

    assert_eq!(
        lowered.render(members).lines().filter(|line| line.trim_start().starts_with("METHOD")).collect::<Vec<_>>(),
        ["  METHOD [4] \"reserve\" @5-5", "  METHOD [20] \"make\" @6-6"]
    );
}

/// ```php
/// private int $count = 0;
/// protected \Lib\Calc $calc;
/// public function __construct(\Lib\Calc $calc) { $this->calc = $calc; }
/// ```
///
/// `[32772]` and `[32770]` are `ZEND_ACC_PRIVATE` and `ZEND_ACC_PROTECTED` with `ZEND_ACC_FIELD`, `1 << 15`, which
/// `ReflectionProperty::isField()` reads.
#[test]
fn a_field_is_a_property_group_of_one_property() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int count = 0;\n    protected Calc calc;\n\n    public Report(Calc calc)\n    {\n        this.calc = calc;\n    }\n}\n",
        &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")],
    );
    let class = lowered.child(lowered.root(), 2);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [32772]
                ZVAL [1] "int"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "count"
                    ZVAL 0
                    null
                    null
                null
              PROP_GROUP [32770]
                ZVAL "Lib\\Calc"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "calc"
                    null
                    null
                    null
                null
              METHOD [1] "__construct" @10-13
                PARAM_LIST
                  PARAM
                    ZVAL "Lib\\Calc"
                    ZVAL "calc"
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
                    VAR
                      ZVAL "calc"
                null
                null
        "#}
    );
}

/// ```php
/// #[\Sharp\Crossing(['map', 'string', null])]
/// private \Sharp\Map $cache = new \Sharp\Map();
/// private \Closure $priceOf;
/// private \Closure $charge;
/// public function __construct(\Closure $priceOf, \Closure $charge) { … }
/// ```
///
/// Spec sections 6 and 29 write these fields with no access modifier. A field with no modifier is `private`, `[32772]`
/// with `ZEND_ACC_FIELD`, while PHP makes a member with no modifier `public`.
#[test]
fn a_field_without_an_access_modifier_is_a_private_property() {
    let lowered = Lowered::with(
        "namespace App.Shop;\n\nclass Checkout\n{\n    Map<string, Plan> cache = [:];\n    Function<Money(Offer)> priceOf;\n    Function<Charge(Cart) uses Http> charge;\n\n    public Checkout(Function<Money(Offer)> priceOf, Function<Charge(Cart) uses Http> charge)\n    {\n        this.priceOf = priceOf;\n        this.charge = charge;\n    }\n}\n",
        &[
            SHOP,
            (
                "src/App/Shop/Offers.php",
                "<?php namespace App\\Shop; final class Plan {} final class Money {} final class Offer {}",
            ),
        ],
    );
    let class = lowered.child(lowered.root(), 2);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [32772]
                SHARP_TYPE_ARGS
                  ZVAL "Sharp\\Map"
                  ZVAL "Map<string, App.Shop.Plan>"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "cache"
                    NEW
                      ZVAL "Sharp\\Map"
                      ARG_LIST
                    null
                    null
                ATTRIBUTE_LIST
                  ATTRIBUTE_GROUP
                    ATTRIBUTE
                      ZVAL "Sharp\\Crossing"
                      ARG_LIST
                        ARRAY [3]
                          ARRAY_ELEM
                            ZVAL "map"
                            null
                          ARRAY_ELEM
                            ZVAL "string"
                            null
                          ARRAY_ELEM
                            ZVAL null
                            null
              PROP_GROUP [32772]
                ZVAL "Closure"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "priceOf"
                    null
                    null
                    null
                null
              PROP_GROUP [32772]
                ZVAL "Closure"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "charge"
                    null
                    null
                    null
                null
              METHOD [1] "__construct" @9-13
                PARAM_LIST
                  PARAM
                    ZVAL "Closure"
                    ZVAL "priceOf"
                    null
                    null
                    null
                    null
                  PARAM
                    ZVAL "Closure"
                    ZVAL "charge"
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
                      ZVAL "priceOf"
                    VAR
                      ZVAL "priceOf"
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "charge"
                    VAR
                      ZVAL "charge"
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
/// public protected(set) string $title;   // written `public string title { get; init; }`
/// public private(set) string $tag;       // written `public string tag { get; private init; }`
/// ```
///
/// An auto-property is a property with asymmetric visibility. `[4097]` is `ZEND_ACC_PUBLIC | ZEND_ACC_PRIVATE_SET`,
/// and `[2049]` is `ZEND_ACC_PUBLIC | ZEND_ACC_PROTECTED_SET`. A get-only property is `readonly`, so `[129]` is
/// `ZEND_ACC_PUBLIC | ZEND_ACC_READONLY` and `[132]` is `ZEND_ACC_PRIVATE | ZEND_ACC_READONLY`. An `init` property is
/// `protected(set)` without `readonly`, so a named constructor sets it again, and the analyzer refuses each write after
/// the object is created. PHP has no set visibility for that, so a plain PHP subclass can still write it.
#[test]
fn an_auto_property_is_a_property_with_its_set_visibility() {
    let lowered = Lowered::new(
        "class Report\n{\n    public int views { get; private set; } = 0;\n    public string name { get; set; }\n    public int id { get; }\n    public int total { get; protected set; }\n    private int hidden { get; }\n    public string title { get; init; }\n    public string tag { get; private init; }\n\n    public Report()\n    {\n        this.name = \"\";\n        this.id = 1;\n        this.total = 0;\n        this.hidden = 2;\n    }\n}\n",
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
            (2049, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"title\"\n    null\n    null\n    null\n".to_owned()),
            (4097, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"tag\"\n    null\n    null\n    null\n".to_owned()),
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
        .written()
        .into_iter()
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
        "class Report\n{\n    private string name = \"\";\n\n    public string slug => global.strtolower(this.name);\n}\n",
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
/// public protected(set) int $slug { set { $this->slug = $value * 2; } }
/// ```
///
/// Each accessor body is a hook: `get => expr;` keeps PHP's short body, and a block or `set => expr;` is a statement
/// list, because PHP stores the result of a short `set` body. An `init` body is the `set` hook, under
/// `protected(set)`. `field` is `$this->name` and `value` is `$value`. An auto
/// accessor beside a body that uses `field` is PHP's backing store, so it lowers to nothing, and otherwise it is the
/// hook over the storage, so the property stays backed. A public get-only property whose body uses `field` takes
/// `protected(set)`, the set visibility of `readonly`, which PHP refuses on a hooked property. A protected one takes
/// none, because PHP drops a set visibility equal to the property's own: `[4097]` is
/// `ZEND_ACC_PUBLIC | ZEND_ACC_PRIVATE_SET`, `[2049]` is `ZEND_ACC_PUBLIC | ZEND_ACC_PROTECTED_SET` and `[2]` is
/// `ZEND_ACC_PROTECTED`.
#[test]
fn accessor_bodies_are_property_hooks() {
    let lowered = Lowered::new(
        "class Report\n{\n    public string name { get => field; set => field = value; }\n    public int count { get; private set => field = value + 1; }\n    public int total { get { return this.count; } }\n    public int size { get; set => this.resize(value); }\n    public int kept { get => field * 2; } = 3;\n    protected int guarded { get => field; }\n    public int slug { get; init => field = value * 2; }\n\n    public Report()\n    {\n        this.name = \"\";\n        this.count = 0;\n        this.size = 0;\n        this.guarded = 1;\n    }\n\n    public void resize(int to)\n    {\n    }\n}\n",
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
            indoc! {r#"
                PROP_GROUP [2049]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "slug"
                      null
                      null
                      STMT_LIST
                        PROPERTY_HOOK "set" @9-9
                          null
                          null
                          STMT_LIST
                            ASSIGN
                              PROP
                                VAR
                                  ZVAL "this"
                                ZVAL "slug"
                              BINARY_OP [3]
                                VAR
                                  ZVAL "value"
                                ZVAL 2
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
        "class Report\n{\n    public int a { get { if (this.ready()) { return field; } return 0; } }\n\n    public Report()\n    {\n        this.a = 1;\n    }\n\n    public bool ready()\n    {\n        return true;\n    }\n}\n",
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
/// class Tally
/// {
///     public static function {'doubled#get'}(): int { return Tally::{'total#get'}() * 2; }
///     private static int $total = 0;
///     public static function {'total#get'}(): int { return self::$total; }
///     private static function {'total#set'}(int $value) { self::$total = $value + 1; }
///
///     public static function run(): int
///     {
///         Tally::{'total#set'}($value#1 = 4) ?? $value#1;
///         Tally::{'total#set'}($value#1 = Tally::{'total#get'}() + 2) ?? $value#1;
///         return (Tally::{'total#set'}(($old#1 = Tally::{'total#get'}()) + 1) ?? $old#1) + Tally::{'doubled#get'}();
///     }
/// }
/// ```
///
/// Decision 3: PHP has no hooks on a static, so a static computed property or one with an accessor body runs its
/// accessors as its class's hidden static methods, `name#get` at the property's level and `name#set` at its `set`'s,
/// `[17]` being `ZEND_ACC_PUBLIC | ZEND_ACC_STATIC` and `[20]` `ZEND_ACC_PRIVATE | ZEND_ACC_STATIC`. Its storage is a
/// private static only they reach, `[20]`. Every read calls `name#get`, and every write `name#set`, with the
/// value written held in a hidden variable, so the write's value is the value written, and a postfix `++` the value
/// read.
/// ```php
/// public static function {'twice#get'}(): int { return 2; }
/// private static int $hits = 0;
/// public static function {'hits#set'}(int $value) { Meter::log("hit"); self::$hits = $value; }
/// protected static function {'label#set'}(string $value) { self::$label = $value; Meter::log(self::$label); }
///
/// Meter::{'hits#set'}($value#1 = Meter::{'hits#get'}() + 1) ?? $value#1;
/// Meter::{'hits#set'}(($old#1 = Meter::{'hits#get'}()) - 1) ?? $old#1;
/// Meter::{'note#get'}() ?? (Meter::{'note#set'}($value#1 = "n") ?? $value#1);
/// Meter::{'label#set'}($value#1 = Meter::{'label#get'}() . "l") ?? $value#1;
///
/// private static function #statics(): void { self::$label = strval(1); }
/// ```
///
/// Decision 3, for each form a static property's accessors take: a `get` body of either form, `set;` and a narrower
/// `protected set`, and `willSet` before and `didSet` after the write. Prefix `++` and `--` give the value written and
/// postfix `--` the value read, `??=` writes only when the read is null, and `+=` on a string appends. A static's
/// initial value fills its storage, spec section 6.2: a constant one is its default, and any other a write of the
/// storage in `#statics`, which runs when a request first builds the class's statics, so it runs no observer.
#[test]
fn a_static_property_runs_each_accessor_form_and_each_write_through_its_hidden_methods() {
    let lowered = Lowered::new(
        "class Meter\n{\n    public static int twice { get => 2; }\n    public static int thrice { get { return 3; } }\n    public static int hits { get; set; willSet => Meter.log(\"hit\"); } = 0;\n    public static string label { get; protected set; didSet => Meter.log(field); } = global.strval(1);\n    public static string? note { get; set; didSet => Meter.log(field); } = null;\n\n    public static void log(string? text)\n    {\n    }\n\n    public static void run()\n    {\n        ++Meter.hits;\n        --Meter.hits;\n        Meter.hits--;\n        Meter.note ??= \"n\";\n        Meter.label += \"l\";\n    }\n}\n",
    );

    assert_eq!(
        lowered.tree(),
        indoc! {r##"
            STMT_LIST
              DECLARE
                CONST_DECL
                  CONST_ELEM
                    ZVAL "strict_types"
                    ZVAL 1
                    null
                null
              CLASS "Meter" @1-21
                null
                null
                STMT_LIST
                  METHOD [17] "twice#get" @3-3
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        ZVAL 2
                    ZVAL [1] "int"
                    null
                  METHOD [17] "thrice#get" @4-4
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        ZVAL 3
                    ZVAL [1] "int"
                    null
                  PROP_GROUP [20]
                    ZVAL [1] "int"
                    PROP_DECL
                      PROP_ELEM
                        ZVAL "hits"
                        ZVAL 0
                        null
                        null
                    null
                  METHOD [17] "hits#get" @5-5
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        STATIC_PROP
                          ZVAL [1] "self"
                          ZVAL "hits"
                    ZVAL [1] "int"
                    null
                  METHOD [17] "hits#set" @5-5
                    PARAM_LIST
                      PARAM
                        ZVAL [1] "int"
                        ZVAL "value"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                      STMT_LIST
                        STATIC_CALL
                          ZVAL "Meter"
                          ZVAL "log"
                          ARG_LIST
                            ZVAL "hit"
                      STMT_LIST
                        ASSIGN
                          STATIC_PROP
                            ZVAL [1] "self"
                            ZVAL "hits"
                          VAR
                            ZVAL "value"
                    null
                    null
                  PROP_GROUP [20]
                    ZVAL [1] "string"
                    PROP_DECL
                      PROP_ELEM
                        ZVAL "label"
                        null
                        null
                        null
                    null
                  METHOD [17] "label#get" @6-6
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        STATIC_PROP
                          ZVAL [1] "self"
                          ZVAL "label"
                    ZVAL [1] "string"
                    null
                  METHOD [18] "label#set" @6-6
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
                      STMT_LIST
                        ASSIGN
                          STATIC_PROP
                            ZVAL [1] "self"
                            ZVAL "label"
                          VAR
                            ZVAL "value"
                      STMT_LIST
                        STATIC_CALL
                          ZVAL "Meter"
                          ZVAL "log"
                          ARG_LIST
                            STATIC_PROP
                              ZVAL [1] "self"
                              ZVAL "label"
                    null
                    null
                  PROP_GROUP [20]
                    ZVAL [257] "string"
                    PROP_DECL
                      PROP_ELEM
                        ZVAL "note"
                        ZVAL null
                        null
                        null
                    null
                  METHOD [17] "note#get" @7-7
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        STATIC_PROP
                          ZVAL [1] "self"
                          ZVAL "note"
                    ZVAL [257] "string"
                    null
                  METHOD [17] "note#set" @7-7
                    PARAM_LIST
                      PARAM
                        ZVAL [257] "string"
                        ZVAL "value"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                      STMT_LIST
                        ASSIGN
                          STATIC_PROP
                            ZVAL [1] "self"
                            ZVAL "note"
                          VAR
                            ZVAL "value"
                      STMT_LIST
                        STATIC_CALL
                          ZVAL "Meter"
                          ZVAL "log"
                          ARG_LIST
                            STATIC_PROP
                              ZVAL [1] "self"
                              ZVAL "note"
                    null
                    null
                  METHOD [17] "log" @9-11
                    PARAM_LIST
                      PARAM
                        ZVAL [257] "string"
                        ZVAL "text"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                    SHARP_TYPE_ARGS
                      ZVAL [1] "void"
                      ZVAL "Function<void(string?)>"
                    null
                  METHOD [17] "run" @13-20
                    PARAM_LIST
                    null
                    STMT_LIST
                      COALESCE
                        STATIC_CALL
                          ZVAL "Meter"
                          ZVAL "hits#set"
                          ARG_LIST
                            ASSIGN
                              VAR
                                ZVAL "value#1"
                              BINARY_OP [1]
                                STATIC_CALL
                                  ZVAL "Meter"
                                  ZVAL "hits#get"
                                  ARG_LIST
                                ZVAL 1
                        VAR
                          ZVAL "value#1"
                      COALESCE
                        STATIC_CALL
                          ZVAL "Meter"
                          ZVAL "hits#set"
                          ARG_LIST
                            ASSIGN
                              VAR
                                ZVAL "value#1"
                              BINARY_OP [2]
                                STATIC_CALL
                                  ZVAL "Meter"
                                  ZVAL "hits#get"
                                  ARG_LIST
                                ZVAL 1
                        VAR
                          ZVAL "value#1"
                      COALESCE
                        STATIC_CALL
                          ZVAL "Meter"
                          ZVAL "hits#set"
                          ARG_LIST
                            BINARY_OP [2]
                              ASSIGN
                                VAR
                                  ZVAL "old#1"
                                STATIC_CALL
                                  ZVAL "Meter"
                                  ZVAL "hits#get"
                                  ARG_LIST
                              ZVAL 1
                        VAR
                          ZVAL "old#1"
                      COALESCE
                        STATIC_CALL
                          ZVAL "Meter"
                          ZVAL "note#get"
                          ARG_LIST
                        COALESCE
                          STATIC_CALL
                            ZVAL "Meter"
                            ZVAL "note#set"
                            ARG_LIST
                              ASSIGN
                                VAR
                                  ZVAL "value#1"
                                ZVAL "n"
                          VAR
                            ZVAL "value#1"
                      COALESCE
                        STATIC_CALL
                          ZVAL "Meter"
                          ZVAL "label#set"
                          ARG_LIST
                            ASSIGN
                              VAR
                                ZVAL "value#1"
                              BINARY_OP [8]
                                STATIC_CALL
                                  ZVAL "Meter"
                                  ZVAL "label#get"
                                  ARG_LIST
                                ZVAL "l"
                        VAR
                          ZVAL "value#1"
                    SHARP_TYPE_ARGS
                      ZVAL [1] "void"
                      ZVAL "Function<void()>"
                    null
                  METHOD [20] "#statics" @6-6
                    PARAM_LIST
                    null
                    STMT_LIST
                      ASSIGN
                        STATIC_PROP
                          ZVAL [1] "self"
                          ZVAL "label"
                        CALL
                          ZVAL "strval"
                          ARG_LIST
                            ZVAL 1
                    ZVAL [1] "void"
                    null
                null
                null
        "##}
    );
}

#[test]
fn a_static_property_with_accessors_runs_them_as_hidden_static_methods() {
    let lowered = Lowered::new(
        "class Tally\n{\n    public static int doubled => Tally.total * 2;\n    public static int total { get; private set => field = value + 1; } = 0;\n\n    public static int run()\n    {\n        Tally.total = 4;\n        Tally.total += 2;\n        return Tally.total++ + Tally.doubled;\n    }\n}\n",
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.root(), 1)),
        indoc! {r#"
            CLASS "Tally" @1-12
              null
              null
              STMT_LIST
                METHOD [17] "doubled#get" @3-3
                  PARAM_LIST
                  null
                  STMT_LIST
                    RETURN
                      BINARY_OP [3]
                        STATIC_CALL
                          ZVAL "Tally"
                          ZVAL "total#get"
                          ARG_LIST
                        ZVAL 2
                  ZVAL [1] "int"
                  null
                PROP_GROUP [20]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "total"
                      ZVAL 0
                      null
                      null
                  null
                METHOD [17] "total#get" @4-4
                  PARAM_LIST
                  null
                  STMT_LIST
                    RETURN
                      STATIC_PROP
                        ZVAL [1] "self"
                        ZVAL "total"
                  ZVAL [1] "int"
                  null
                METHOD [20] "total#set" @4-4
                  PARAM_LIST
                    PARAM
                      ZVAL [1] "int"
                      ZVAL "value"
                      null
                      null
                      null
                      null
                  null
                  STMT_LIST
                    ASSIGN
                      STATIC_PROP
                        ZVAL [1] "self"
                        ZVAL "total"
                      BINARY_OP [1]
                        VAR
                          ZVAL "value"
                        ZVAL 1
                  null
                  null
                METHOD [17] "run" @6-11
                  PARAM_LIST
                  null
                  STMT_LIST
                    COALESCE
                      STATIC_CALL
                        ZVAL "Tally"
                        ZVAL "total#set"
                        ARG_LIST
                          ASSIGN
                            VAR
                              ZVAL "value#1"
                            ZVAL 4
                      VAR
                        ZVAL "value#1"
                    COALESCE
                      STATIC_CALL
                        ZVAL "Tally"
                        ZVAL "total#set"
                        ARG_LIST
                          ASSIGN
                            VAR
                              ZVAL "value#1"
                            BINARY_OP [1]
                              STATIC_CALL
                                ZVAL "Tally"
                                ZVAL "total#get"
                                ARG_LIST
                              ZVAL 2
                      VAR
                        ZVAL "value#1"
                    RETURN
                      BINARY_OP [1]
                        COALESCE
                          STATIC_CALL
                            ZVAL "Tally"
                            ZVAL "total#set"
                            ARG_LIST
                              BINARY_OP [1]
                                ASSIGN
                                  VAR
                                    ZVAL "old#1"
                                  STATIC_CALL
                                    ZVAL "Tally"
                                    ZVAL "total#get"
                                    ARG_LIST
                                ZVAL 1
                          VAR
                            ZVAL "old#1"
                        STATIC_CALL
                          ZVAL "Tally"
                          ZVAL "doubled#get"
                          ARG_LIST
                  SHARP_TYPE_ARGS
                    ZVAL [1] "int"
                    ZVAL "Function<int()>"
                  null
              null
              null
        "#}
    );
}

/// ```php
/// public static function {'plan#get'}(): int
/// {
///     return \Sharp\Internal\keep('App\Tenant\Report::plan', static fn () => \trim(' a '));
/// }
/// ```
///
/// Spec section 6.5: a `static lazy` property's `name#get` runs its body on the first read and keeps the value for a
/// singleton's lifetime, under the key of its class and name.
#[test]
fn a_static_lazy_property_keeps_its_value_for_a_singleton_s_lifetime() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public static lazy string plan => global.trim(\" a \");\n}\n",
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.method_named("plan#get"), 2)),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "Sharp\\Internal\\keep"
                  ARG_LIST
                    ZVAL "App\\Tenant\\Report::plan"
                    ARROW_FUNC [16] "" @5-5
                      PARAM_LIST
                      null
                      CALL
                        ZVAL "trim"
                        ARG_LIST
                          ZVAL " a "
                      null
                      null
        "#}
    );
}

/// ```php
/// class Report
/// {
///     public private(set) static int $size = 3;
///     public private(set) static \Lib\Calc $calc;
///     private static string $name;
///
///     private static function #statics(): void { self::$calc = new \Lib\Calc(1); self::$name = trim(' a '); }
/// }
/// ```
///
/// Spec section 6: PHP has no `readonly` static, so a get-only static is `private(set)`, `[4113]` being
/// `ZEND_ACC_PRIVATE_SET | ZEND_ACC_STATIC | ZEND_ACC_PUBLIC`, and the analyzer refuses every write of it. PHP gives a
/// static only a constant default, so an initial value that isn't constant runs in the class's hidden method
/// `#statics`, `[20]` being `ZEND_ACC_PRIVATE | ZEND_ACC_STATIC`, which the engine runs once each request, when the
/// request first builds the class's statics.
#[test]
fn a_static_initial_value_that_is_not_constant_runs_once_when_its_class_s_statics_are_built() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int size { get; } = 3;\n    public static Calc calc { get; } = new Calc(1);\n    private static string name = global.trim(\" a \");\n}\n",
        &[CALC_CLASS],
    );

    assert_eq!(
        lowered.render(lowered.root()),
        indoc! {r##"
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
              CLASS "Report" @5-10
                null
                null
                STMT_LIST
                  PROP_GROUP [4113]
                    ZVAL [1] "int"
                    PROP_DECL
                      PROP_ELEM
                        ZVAL "size"
                        ZVAL 3
                        null
                        null
                    null
                  PROP_GROUP [4113]
                    ZVAL "Lib\\Calc"
                    PROP_DECL
                      PROP_ELEM
                        ZVAL "calc"
                        null
                        null
                        null
                    null
                  PROP_GROUP [32788]
                    ZVAL [1] "string"
                    PROP_DECL
                      PROP_ELEM
                        ZVAL "name"
                        null
                        null
                        null
                    null
                  METHOD [20] "#statics" @8-8
                    PARAM_LIST
                    null
                    STMT_LIST
                      ASSIGN
                        STATIC_PROP
                          ZVAL [1] "self"
                          ZVAL "calc"
                        NEW
                          ZVAL "Lib\\Calc"
                          ARG_LIST
                            ZVAL 1
                      ASSIGN
                        STATIC_PROP
                          ZVAL [1] "self"
                          ZVAL "name"
                        CALL
                          ZVAL "trim"
                          ARG_LIST
                            ZVAL " a "
                    ZVAL [1] "void"
                    null
                null
                null
        "##}
    );
}

/// ```php
/// public string $name { set { $this->log($value); $this->name = $value; $this->log($this->name); } }
/// public int $count { set { $this->count = $value * 2; $this->log('count'); } }
///
/// public function __construct()
/// {
///     $this->name = '';
///     $this->count = 0;
/// }
/// ```
///
/// ```php
/// private \App\Trimmed $name#via0;
/// private \App\Upper $name#via1;
/// public string $name {
///     get => $this->{'name#via0'}->get($this->{'name#via1'}->get($this->name));
///     set { $this->name = $this->{'name#via1'}->set($this->{'name#via0'}->set($value)); }
/// }
/// public function __construct() {
///     $this->{'name#via0'} = new \App\Trimmed();
///     $this->{'name#via1'} = new \App\Upper();
///     $this->name = "";
/// }
/// ```
///
/// Spec section 6.4: each behavior is a hidden private property the constructor fills with `new` before the initial
/// value, which it writes through the hooks. `set` runs the behaviors in the order they are written and `get` the other
/// way. A behavior's `set(string value)` takes its type argument where `Behavior.set` erases its parameter to any value,
/// so it keeps its PHP type `string` and carries `ZEND_PARAM_WIDENED`, `[64]`, which skips PHP's variance check.
#[test]
fn behaviors_are_hidden_slots_the_hooks_call_in_order() {
    let behavior = (
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Behavior.sharp",
        "namespace Sharp;\n\npublic interface Behavior<T>\n{\n    T get(T value);\n\n    T set(T value);\n}\n",
    );
    let upper = (
        "src/Upper.sharp",
        "namespace App;\n\nimport Sharp.Behavior;\n\npublic class Upper : Behavior<string>\n{\n    public string get(string value) => value;\n\n    public string set(string value) => global.strtoupper(value);\n}\n",
    );
    let lowered = Lowered::with(
        "namespace App;\n\nimport Sharp.Behavior;\n\npublic class Trimmed : Behavior<string>\n{\n    public string get(string value) => value;\n\n    public string set(string value) => global.trim(value);\n}\n\npublic class Report\n{\n    public string name { get; set; } via Trimmed, Upper = \"\";\n}\n",
        &[behavior, upper],
    );

    assert_eq!(
        property_groups(&lowered),
        [
            indoc! {r#"
                PROP_GROUP [4]
                  ZVAL "App\\Trimmed"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "name#via0"
                      null
                      null
                      null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [4]
                  ZVAL "App\\Upper"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "name#via1"
                      null
                      null
                      null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [1]
                  ZVAL [1] "string"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "name"
                      null
                      null
                      STMT_LIST
                        PROPERTY_HOOK "get" @14-14
                          null
                          null
                          PROPERTY_HOOK_SHORT_BODY
                            METHOD_CALL
                              PROP
                                VAR
                                  ZVAL "this"
                                ZVAL "name#via0"
                              ZVAL "get"
                              ARG_LIST
                                METHOD_CALL
                                  PROP
                                    VAR
                                      ZVAL "this"
                                    ZVAL "name#via1"
                                  ZVAL "get"
                                  ARG_LIST
                                    PROP
                                      VAR
                                        ZVAL "this"
                                      ZVAL "name"
                          null
                          null
                        PROPERTY_HOOK "set" @14-14
                          null
                          null
                          STMT_LIST
                            ASSIGN
                              PROP
                                VAR
                                  ZVAL "this"
                                ZVAL "name"
                              METHOD_CALL
                                PROP
                                  VAR
                                    ZVAL "this"
                                  ZVAL "name#via1"
                                ZVAL "set"
                                ARG_LIST
                                  METHOD_CALL
                                    PROP
                                      VAR
                                        ZVAL "this"
                                      ZVAL "name#via0"
                                    ZVAL "set"
                                    ARG_LIST
                                      VAR
                                        ZVAL "value"
                          null
                          null
                  null
            "#},
        ]
    );
    assert_eq!(
        lowered.body_of("__construct"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "name#via0"
                NEW
                  ZVAL "App\\Trimmed"
                  ARG_LIST
              ASSIGN
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "name#via1"
                NEW
                  ZVAL "App\\Upper"
                  ARG_LIST
              ASSIGN
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "name"
                ZVAL ""
        "#}
    );
    assert_eq!(
        lowered.parameters_of("set"),
        indoc! {r#"
            PARAM_LIST
              PARAM [64]
                ZVAL [1] "string"
                ZVAL "value"
                null
                null
                null
                null
        "#}
    );
}

/// ```php
/// public function __clone(): void
/// {
///     if (\method_exists(parent::class, '__clone')) { parent::__clone(); }
///     $this->{'name#via0'} = new \App\Trimmed();
/// }
/// ```
///
/// Spec sections 6.4 and 16: a copy made with `with` or with plain PHP's `clone` gets a new instance of each behavior,
/// so the two never share a behavior's state. A parent's own `__clone` runs first, when it has one.
#[test]
fn a_copy_gets_new_behaviors_after_its_parent_s_copy_runs() {
    let behavior = (
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Behavior.sharp",
        "namespace Sharp;\n\npublic interface Behavior<T>\n{\n    T get(T value);\n\n    T set(T value);\n}\n",
    );
    let named = ("src/Named.sharp", "namespace App;\n\npublic abstract class Named\n{\n}\n");
    let lowered = Lowered::with(
        "namespace App;\n\nimport Sharp.Behavior;\n\npublic class Trimmed : Behavior<string>\n{\n    public string get(string value) => value;\n\n    public string set(string value) => global.trim(value);\n}\n\npublic class Report : Named\n{\n    public string name { get; set; } via Trimmed = \"\";\n}\n",
        &[behavior, named],
    );

    assert_eq!(
        lowered.declared("__clone"),
        indoc! {r#"
            METHOD [1] "__clone" @12-15
              PARAM_LIST
              null
              STMT_LIST
                IF
                  IF_ELEM
                    CALL
                      ZVAL "method_exists"
                      ARG_LIST
                        CLASS_NAME
                          ZVAL [1] "parent"
                        ZVAL "__clone"
                    STMT_LIST
                      STATIC_CALL
                        ZVAL [1] "parent"
                        ZVAL "__clone"
                        ARG_LIST
                ASSIGN
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "name#via0"
                  NEW
                    ZVAL "App\\Trimmed"
                    ARG_LIST
              ZVAL [1] "void"
              null
        "#}
    );
}

/// Spec sections 6.4 and 11: a generic behavior's `new` carries its type arguments under a `SHARP_TYPE_ARGS`, as a
/// generic `new` does: `$this->{'level#via0'} = new \App\Kept<int>();`.
#[test]
fn a_generic_behavior_is_created_with_its_type_arguments() {
    let behavior = (
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Behavior.sharp",
        "namespace Sharp;\n\npublic interface Behavior<T>\n{\n    T get(T value);\n\n    T set(T value);\n}\n",
    );
    let lowered = Lowered::with(
        "namespace App;\n\nimport Sharp.Behavior;\n\npublic class Kept<T> : Behavior<T>\n{\n    public T get(T value) => value;\n\n    public T set(T value) => value;\n}\n\npublic class Report\n{\n    public int level { get; set; } via Kept<int> = 1;\n}\n",
        &[behavior],
    );

    assert_eq!(
        lowered.declared("__construct"),
        indoc! {r#"
            METHOD [1] "__construct" @12-15
              PARAM_LIST
              null
              STMT_LIST
                ASSIGN
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "level#via0"
                  SHARP_TYPE_ARGS
                    NEW
                      ZVAL "App\\Kept"
                      ARG_LIST
                    ZVAL "int"
                ASSIGN
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "level"
                  ZVAL 1
              null
              null
        "#}
    );
}

/// Spec section 6.2: the observers are no hooks of their own. The `set` hook runs `willSet`, then the `set` body or the
/// storage write an auto `set` is, then `didSet`, and both run only once the object is built, so each waits on
/// `\Sharp\Internal\building($this)`, asked once per write. An initial value fills the storage, spec section 6: a
/// constant one is PHP's default, and any other, on a property with a `set` hook, is PHP's `setRawValue` at the start
/// of the constructor, which runs no hook.
#[test]
fn observers_run_inside_the_set_hook_once_the_object_is_built() {
    let lowered = Lowered::new(
        "class Report\n{\n    public string name { get; set; willSet => this.log(field); didSet => this.log(field); } = \"\";\n    public int count { get; set { field = value * 2; } didSet { this.log(\"count\"); } } = 0;\n    public string title { get; set => field = global.trim(value); } = global.strval(1);\n\n    public void log(Any? text)\n    {\n    }\n}\n",
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
                      ZVAL ""
                      null
                      STMT_LIST
                        PROPERTY_HOOK "set" @3-3
                          null
                          null
                          STMT_LIST
                            ASSIGN
                              VAR
                                ZVAL "built#1"
                              UNARY_OP [14]
                                CALL
                                  ZVAL "Sharp\\Internal\\building"
                                  ARG_LIST
                                    VAR
                                      ZVAL "this"
                            IF
                              IF_ELEM
                                VAR
                                  ZVAL "built#1"
                                STMT_LIST
                                  STMT_LIST
                                    METHOD_CALL
                                      VAR
                                        ZVAL "this"
                                      ZVAL "log"
                                      ARG_LIST
                                        PROP
                                          VAR
                                            ZVAL "this"
                                          ZVAL "name"
                            STMT_LIST
                              ASSIGN
                                PROP
                                  VAR
                                    ZVAL "this"
                                  ZVAL "name"
                                VAR
                                  ZVAL "value"
                            IF
                              IF_ELEM
                                VAR
                                  ZVAL "built#1"
                                STMT_LIST
                                  STMT_LIST
                                    METHOD_CALL
                                      VAR
                                        ZVAL "this"
                                      ZVAL "log"
                                      ARG_LIST
                                        PROP
                                          VAR
                                            ZVAL "this"
                                          ZVAL "name"
                          null
                          null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [1]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "count"
                      ZVAL 0
                      null
                      STMT_LIST
                        PROPERTY_HOOK "set" @4-4
                          null
                          null
                          STMT_LIST
                            ASSIGN
                              VAR
                                ZVAL "built#1"
                              UNARY_OP [14]
                                CALL
                                  ZVAL "Sharp\\Internal\\building"
                                  ARG_LIST
                                    VAR
                                      ZVAL "this"
                            STMT_LIST
                              ASSIGN
                                PROP
                                  VAR
                                    ZVAL "this"
                                  ZVAL "count"
                                BINARY_OP [3]
                                  VAR
                                    ZVAL "value"
                                  ZVAL 2
                            IF
                              IF_ELEM
                                VAR
                                  ZVAL "built#1"
                                STMT_LIST
                                  STMT_LIST
                                    METHOD_CALL
                                      VAR
                                        ZVAL "this"
                                      ZVAL "log"
                                      ARG_LIST
                                        ZVAL "count"
                          null
                          null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [1]
                  ZVAL [1] "string"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "title"
                      null
                      null
                      STMT_LIST
                        PROPERTY_HOOK "set" @5-5
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
                  null
            "#},
        ]
    );
    assert_eq!(
        lowered.render(lowered.child(lowered.method_named("__construct"), 2)),
        indoc! {r#"
            STMT_LIST
              METHOD_CALL
                NEW
                  ZVAL "ReflectionProperty"
                  ARG_LIST
                    CLASS_NAME
                      ZVAL [1] "self"
                    ZVAL "title"
                ZVAL "setRawValue"
                ARG_LIST
                  VAR
                    ZVAL "this"
                  CALL
                    ZVAL "strval"
                    ARG_LIST
                      ZVAL 1
        "#}
    );
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
        "class Report\n{\n    public int size { get => this.read(); set; }\n\n    public Report()\n    {\n        this.size = 1;\n    }\n\n    public int read()\n    {\n        return 1;\n    }\n}\n",
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
/// has storage, and its `= null` is its default, as any constant initial value is. One without storage is virtual,
/// and PHP refuses a default on a virtual property, so it takes none, get-only or not. `[256]` is `ZEND_TYPE_NULLABLE`
/// on a class name, and `[257]` is `ZEND_NAME_NOT_FQ | ZEND_TYPE_NULLABLE`.
#[test]
fn nullable_properties_with_accessor_bodies_are_their_php_hooks() {
    let lowered = Lowered::with(
        "import Lib.Model;\n\nclass Order : Model\n{\n    public Address? shipping { get => this.getAttribute(\"shipping\"); set => this.setAttribute(\"shipping\", value); }\n    public string? note { get => field; set => field = value; } = null;\n    public string? summary { get => this.note; }\n}\n\nclass Address\n{\n}\n",
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
/// A parameter that declares a member carries the member's flags: `(1 << 15) | 4` is `ZEND_ACC_FIELD |
/// ZEND_ACC_PRIVATE`, because a parameter without accessors is a field, `129` is `ZEND_ACC_PUBLIC | ZEND_ACC_READONLY`,
/// and `2` is `ZEND_ACC_PROTECTED`.
#[test]
fn a_constructor_parameter_with_an_access_modifier_is_a_promoted_parameter() {
    let lowered = Lowered::new(
        "class Report\n{\n    public Report(private int count, public int id { get; }, protected string name { get; set; }, int extra) {}\n}\n",
    );
    let parameters: Vec<(String, u32)> = lowered
        .written()
        .into_iter()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_PARAM)
        .map(|(index, node)| {
            assert_eq!(lowered.child(index as u32, 5), u32::MAX, "a promoted parameter has no hooks");

            (lowered.text(lowered.nodes()[lowered.child(index as u32, 1) as usize].text), node.attr)
        })
        .collect();

    assert_eq!(
        parameters,
        [("count".to_owned(), (1 << 15) | 4), ("id".to_owned(), 129), ("name".to_owned(), 2), ("extra".to_owned(), 0)]
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
              PROP_GROUP [32772]
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
              PROP_GROUP [32772]
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

const CALC_CLASS: (&str, &str) =
    ("src/Lib/Calc.php", "<?php namespace Lib; final class Calc { public function __construct(int $start) {} }");

/// ```php
/// public function __construct(int $id)
/// {
///     $this->calc = new \Lib\Calc(1);
///     parent::__construct($id);
///     $this->calc = new \Lib\Calc($id);
/// }
/// ```
///
/// Spec section 9.1: `: super(…)` runs as `parent::__construct(…)` after the initial values and before the body.
#[test]
fn super_runs_as_parent_construct_after_the_initial_values_and_before_the_body() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\nimport Lib.Entity;\n\nclass Report : Entity\n{\n    private Calc calc = new Calc(1);\n\n    public Report(int id) : super(id)\n    {\n        this.calc = new Calc(id);\n    }\n}\n",
        &[
            CALC_CLASS,
            (
                "src/Lib/Entity.php",
                "<?php namespace Lib; class Entity { public function __construct(public readonly int $id) {} }",
            ),
        ],
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.method_named("__construct"), 2)),
        indoc! {r#"
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
              STATIC_CALL
                ZVAL [1] "parent"
                ZVAL "__construct"
                ARG_LIST
                  VAR
                    ZVAL "id"
              ASSIGN
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "calc"
                NEW
                  ZVAL "Lib\\Calc"
                  ARG_LIST
                    VAR
                      ZVAL "id"
        "#}
    );
}

/// ```php
/// public function __construct()
/// {
///     $this->calc = new \Lib\Calc(1);
///     parent::__construct();
/// }
/// ```
///
/// The constructor a class gets for its initial values calls its base class's constructor after them.
#[test]
fn the_constructor_for_initial_values_calls_the_base_class_constructor() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\nimport Lib.Entity;\n\nclass Report : Entity\n{\n    private Calc calc = new Calc(1);\n}\n",
        &[CALC_CLASS, ("src/Lib/Entity.php", "<?php namespace Lib; class Entity { public function __construct() {} }")],
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.method_named("__construct"), 2)),
        indoc! {r#"
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
              STATIC_CALL
                ZVAL [1] "parent"
                ZVAL "__construct"
                ARG_LIST
        "#}
    );
}

/// The base class's constructor `parent::__construct` calls can be one it inherits, for `: super(…)` and for the
/// constructor that runs the initial values.
#[test]
fn the_base_class_constructor_can_be_inherited() {
    let model = "<?php namespace Lib; class Model { public function __construct(int $id) {} } class Entity extends Model {} class Plain { public function __construct() {} } class Record extends Plain {}";
    for (code, body) in [
        (
            "namespace App.Tenant;\n\nimport Lib.Entity;\n\nclass Report : Entity\n{\n    public Report(int id) : super(id) { }\n}\n",
            "STMT_LIST\n  STATIC_CALL\n    ZVAL [1] \"parent\"\n    ZVAL \"__construct\"\n    ARG_LIST\n      VAR\n        ZVAL \"id\"\n",
        ),
        (
            "namespace App.Tenant;\n\nimport Lib.Record;\n\nclass Report : Record\n{\n    private string label = global.strtoupper(\"x\");\n}\n",
            "STMT_LIST\n  ASSIGN\n    PROP\n      VAR\n        ZVAL \"this\"\n      ZVAL \"label\"\n    CALL\n      ZVAL \"strtoupper\"\n      ARG_LIST\n        ZVAL \"x\"\n  STATIC_CALL\n    ZVAL [1] \"parent\"\n    ZVAL \"__construct\"\n    ARG_LIST\n",
        ),
    ] {
        let lowered = Lowered::with(code, &[("src/Lib/Model.php", model)]);

        assert_eq!(lowered.render(lowered.child(lowered.method_named("__construct"), 2)), body, "{code}");
    }
}

/// ```php
/// public function __construct() { parent::__construct(); }
/// ```
///
/// A PHP# base class without a constructor gets one for its initial values that are not constant, so `: super()` and
/// the constructor that runs a subclass's initial values call it, with the base class in another file or the same one.
#[test]
fn super_calls_the_constructor_a_base_class_gets_for_its_initial_values() {
    let base = (
        "src/Lib/Stamped.sharp",
        "namespace Lib;\n\npublic class Stamped\n{\n    public string stamp { get; set; } = global.strtoupper(\"p\");\n}\n",
    );
    let parent_construct = "  STATIC_CALL\n    ZVAL [1] \"parent\"\n    ZVAL \"__construct\"\n    ARG_LIST\n";
    for (code, body) in [
        (
            "namespace App.Tenant;\n\nimport Lib.Stamped;\n\nclass Report : Stamped\n{\n    public Report() : super() { }\n}\n",
            format!("STMT_LIST\n{parent_construct}"),
        ),
        (
            "namespace App.Tenant;\n\nclass Stamped\n{\n    public string stamp { get; set; } = global.strtoupper(\"q\");\n}\n\nclass Report : Stamped\n{\n    public Report() : super() { }\n}\n",
            format!("STMT_LIST\n{parent_construct}"),
        ),
        (
            "namespace App.Tenant;\n\nimport Lib.Stamped;\n\nclass Report : Stamped\n{\n    private string extra = global.strtoupper(\"e\");\n}\n",
            format!(
                "STMT_LIST\n  ASSIGN\n    PROP\n      VAR\n        ZVAL \"this\"\n      ZVAL \"extra\"\n    CALL\n      ZVAL \"strtoupper\"\n      ARG_LIST\n        ZVAL \"e\"\n{parent_construct}"
            ),
        ),
    ] {
        let lowered = Lowered::with(code, &[base]);
        assert_eq!(lowered.diagnostics(), Vec::<String>::new(), "{code}");
        // `Report` is the file's last class, so its constructor is the last one.
        let report = lowered
            .nodes()
            .iter()
            .rposition(|node| node.kind == sharp_kind::SHARP_AST_METHOD && lowered.text(node.text) == "__construct")
            .expect("Report declares a constructor") as u32;

        assert_eq!(lowered.render(lowered.child(report, 2)), body, "{code}");
    }
}

/// `: super()` with no constructor above the class does nothing, as C#'s `: base()` on `object`.
#[test]
fn super_with_no_constructor_above_lowers_to_nothing() {
    for code in [
        "namespace App.Tenant;\n\nimport Lib.Entity;\n\nclass Report : Entity\n{\n    public Report() : super() { }\n}\n",
        "namespace App.Tenant;\n\nclass Report\n{\n    public Report() : super() { }\n}\n",
    ] {
        let lowered = Lowered::with(code, &[("src/Lib/Entity.php", "<?php namespace Lib; class Entity {}")]);

        assert_eq!(lowered.render(lowered.child(lowered.method_named("__construct"), 2)), "STMT_LIST\n", "{code}");
    }
}

/// ```php
/// public static function sized(int $size, string ...$notes): \App\Report
/// {
///     ${'this#1'} = new \App\Report($size);
///     \Sharp\Internal\build(${'this#1'}, function ($size, ...$notes) {
///         if ($size === 0) { return; }
///         $this->label = parent::describe();
///         $read = function () { return $this->label; };
///         $skip = function () { return; };
///     }, $size, ...$notes);
///     return ${'this#1'};
/// }
/// ```
///
/// Spec section 9.1: a named constructor is the static method it runs as. `: this(…)` creates the object with the
/// main constructor, and the body is a closure bound to it, so it runs as a method body on a real `$this`: `super.m()`
/// is `parent::m()`, `return;` and a lambda's `return;` return nothing, and a lambda reads `$this` as in a method. The
/// closure takes each parameter, a variadic one spread back out. The method returns the object after its body.
/// `\Sharp\Internal\build` binds the closure as `Closure::call` does while the object is still being built, so the
/// body's writes run no observer, spec section 6.2.
#[test]
fn a_named_constructor_is_a_static_method_that_returns_the_object_this_creates() {
    let lowered = Lowered::new(
        "namespace App;\n\nclass Base\n{\n    public Base() { }\n    public string describe() => \"base\";\n}\n\nclass Report : Base\n{\n    public string label { get; set; } = \"\";\n\n    public Report(public int size { get; }) : super() { }\n\n    public Report.sized(int size, string ...notes) : this(size)\n    {\n        if (size == 0)\n        {\n            return;\n        }\n        this.label = super.describe();\n        let read = () => { return this.label; };\n        let skip = () => { return; };\n    }\n}\n",
    );

    assert_eq!(
        lowered.render(lowered.method_named("sized")),
        indoc! {r#"
            METHOD [17] "sized" @15-24
              PARAM_LIST
                PARAM
                  ZVAL [1] "int"
                  ZVAL "size"
                  null
                  null
                  null
                  null
                PARAM [16]
                  ZVAL [1] "string"
                  ZVAL "notes"
                  null
                  null
                  null
                  null
              null
              STMT_LIST
                ASSIGN
                  VAR
                    ZVAL "this#1"
                  NEW
                    ZVAL "App\\Report"
                    ARG_LIST
                      VAR
                        ZVAL "size"
                CALL
                  ZVAL "Sharp\\Internal\\build"
                  ARG_LIST
                    VAR
                      ZVAL "this#1"
                    CLOSURE "" @16-24
                      PARAM_LIST
                        PARAM
                          null
                          ZVAL "size"
                          null
                          null
                          null
                          null
                        PARAM [16]
                          null
                          ZVAL "notes"
                          null
                          null
                          null
                          null
                      null
                      STMT_LIST
                        IF
                          IF_ELEM
                            BINARY_OP [16]
                              VAR
                                ZVAL "size"
                              ZVAL 0
                            STMT_LIST
                              RETURN
                                null
                        ASSIGN
                          PROP
                            VAR
                              ZVAL "this"
                            ZVAL "label"
                          STATIC_CALL
                            ZVAL [1] "parent"
                            ZVAL "describe"
                            ARG_LIST
                        ASSIGN
                          VAR
                            ZVAL "read"
                          CLOSURE "" @22-22
                            PARAM_LIST
                            null
                            STMT_LIST
                              RETURN
                                PROP
                                  VAR
                                    ZVAL "this"
                                  ZVAL "label"
                            SHARP_TYPE_ARGS
                              null
                              ZVAL "Function<string()>"
                            null
                        ASSIGN
                          VAR
                            ZVAL "skip"
                          CLOSURE "" @23-23
                            PARAM_LIST
                            null
                            STMT_LIST
                              RETURN
                                null
                            null
                            null
                      null
                      null
                    VAR
                      ZVAL "size"
                    UNPACK
                      VAR
                        ZVAL "notes"
                RETURN
                  VAR
                    ZVAL "this#1"
              ZVAL "App\\Report"
              null
        "#}
    );
}

/// ```php
/// public static function make(): \App\Report { return \App\Report::sized(3); }
/// ```
///
/// Spec section 9.1: `new Report.sized(…)` calls the static method the named constructor runs as.
#[test]
fn new_with_a_named_constructor_is_a_static_call_of_it() {
    let lowered = Lowered::new(
        "namespace App;\n\nclass Report\n{\n    public Report(public int size { get; }) { }\n\n    public Report.sized(int size) : this(size) { }\n\n    public static Report make() => new Report.sized(3);\n}\n",
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.method_named("make"), 2)),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "App\\Report"
                  ZVAL "sized"
                  ARG_LIST
                    ZVAL 3
        "#}
    );
}

/// ```php
/// public static function of($item, /* type args: bounds "Any?", checks "#0" */): \App\Box {
///     ${'this#1'} = new \App\Box($item) /* type args "#0" */; …
/// }
/// public static function numbers(): \App\Box { return \App\Box::of(1, /* type args "int" */); }
/// ```
///
/// Spec section 9.1: a generic class's named constructor runs as a static method, which has no `this` to read the
/// class's type arguments from, so it takes them as its own: the class's `TItem` is the method's `#0`, the `new` of
/// `: this(…)` gives them to the object, and `new Box<int>.of(1)` passes them after the arguments, as a generic call
/// does. The body runs on the object it created, so it reads `TItem` from `this` as `$0`.
#[test]
fn a_named_constructor_of_a_generic_class_takes_the_class_type_arguments_as_its_own() {
    let lowered = Lowered::new(
        "namespace App;\n\nclass Box<TItem>\n{\n    public Box(public TItem item { get; }) { }\n\n    public Box.of(TItem item) : this(item)\n    {\n        let held = item is TItem;\n    }\n\n    public static Box<int> numbers() => new Box<int>.of(1);\n}\n",
    );
    let of = lowered.method_named("of");

    assert_eq!(
        lowered.render(lowered.child(of, 0)),
        indoc! {r##"
            PARAM_LIST
              PARAM
                ZVAL [1] "mixed"
                ZVAL "item"
                null
                null
                null
                null
              SHARP_TYPE_ARGS
                ZVAL "Any?"
                ZVAL "#0"
        "##}
    );
    assert_eq!(
        lowered.render(lowered.child(of, 2)),
        indoc! {r##"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "this#1"
                SHARP_TYPE_ARGS
                  NEW
                    ZVAL "App\\Box"
                    ARG_LIST
                      VAR
                        ZVAL "item"
                  ZVAL "#0"
              CALL
                ZVAL "Sharp\\Internal\\build"
                ARG_LIST
                  VAR
                    ZVAL "this#1"
                  CLOSURE "" @8-10
                    PARAM_LIST
                      PARAM
                        null
                        ZVAL "item"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                      ASSIGN
                        VAR
                          ZVAL "held"
                        SHARP_IS_TYPE
                          VAR
                            ZVAL "item"
                          ZVAL "$0"
                    null
                    null
                  VAR
                    ZVAL "item"
              RETURN
                VAR
                  ZVAL "this#1"
        "##}
    );
    assert_eq!(
        lowered.render(lowered.child(lowered.method_named("numbers"), 2)),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "App\\Box"
                  ZVAL "of"
                  ARG_LIST
                    ZVAL 1
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "int"
        "#}
    );
}

/// ```php
/// public static function track($item, /* type args: bounds "Any?", checks "#0" */): bool {
///     \Sharp\Internal\statics(\App\Counter::class, /* the text of "#0" */)['count']++;
///     (${'receiver#1'} = &\Sharp\Internal\statics(\App\Counter::class, /* the text of "#0" */)['items'])->add($item);
///     return /* $item is #0 */;
/// }
/// \App\Counter::track($order, /* type args "App.Order" */);
/// \App\Orders::track($order, /* type args "App.Order" */);
/// (${'receiver#1'} = &\Sharp\Internal\statics(\App\Counter::class, 'App.Order')['items'])->add($order);
/// \Sharp\Internal\statics(\App\Counter::class, 'App.Order')['count'] = 5;
/// ```
///
/// Spec section 11: a static method of a generic class has no `this` to read the class's type arguments from, so it
/// takes them as its own, as a named constructor does. `TItem` is the method's `#0`, and `Counter<Order>.track(order)`
/// passes `App.Order` after the arguments, as `Orders.track(order)` does from the header `Orders : Counter<Order>`. Each
/// type argument keeps its own statics, which `Sharp\Internal\statics` returns by reference, keyed by the type argument
/// text, so `Orders.count` is the entry `Counter<Order>.count` is, also where a write before it makes its type known. A
/// method call binds the field to a reference first, so `items.add(item)` changes the list where it lives.
#[test]
fn a_static_method_of_a_generic_class_takes_the_class_type_arguments_as_its_own() {
    let lowered = Lowered::new(
        "namespace App;\n\nclass Order\n{\n}\n\nclass Counter<TItem>\n{\n    public static int count { get; set; } = 0;\n\n    public static List<TItem> items { get; set; } = [];\n\n    public static bool track(TItem item)\n    {\n        Counter<TItem>.count++;\n        Counter<TItem>.items.add(item);\n\n        return item is TItem;\n    }\n}\n\nclass Orders : Counter<Order>\n{\n}\n\nclass Report\n{\n    public static int run(Order order)\n    {\n        Counter<Order>.track(order);\n        Orders.track(order);\n        Orders.items.add(order);\n        Orders.count = 5;\n\n        return Orders.count;\n    }\n}\n",
    );
    let track = lowered.method_named("track");

    assert_eq!(
        lowered.render(lowered.child(track, 0)),
        indoc! {r##"
            PARAM_LIST
              PARAM
                ZVAL [1] "mixed"
                ZVAL "item"
                null
                null
                null
                null
              SHARP_TYPE_ARGS
                ZVAL "Any?"
                ZVAL "#0"
        "##}
    );
    assert_eq!(
        lowered.render(lowered.child(track, 2)),
        indoc! {r##"
            STMT_LIST
              POST_INC
                DIM
                  CALL
                    ZVAL "Sharp\\Internal\\statics"
                    ARG_LIST
                      CLASS_NAME
                        ZVAL "App\\Counter"
                      SHARP_TYPE_OF [1]
                        ZVAL "#0"
                  ZVAL "count"
              METHOD_CALL
                ASSIGN_REF
                  VAR
                    ZVAL "receiver#1"
                  DIM
                    CALL
                      ZVAL "Sharp\\Internal\\statics"
                      ARG_LIST
                        CLASS_NAME
                          ZVAL "App\\Counter"
                        SHARP_TYPE_OF [1]
                          ZVAL "#0"
                    ZVAL "items"
                ZVAL "add"
                ARG_LIST
                  VAR
                    ZVAL "item"
              RETURN
                SHARP_IS_TYPE
                  VAR
                    ZVAL "item"
                  ZVAL "#0"
        "##}
    );
    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              STATIC_CALL
                ZVAL "App\\Counter"
                ZVAL "track"
                ARG_LIST
                  VAR
                    ZVAL "order"
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "App.Order"
              STATIC_CALL
                ZVAL "App\\Orders"
                ZVAL "track"
                ARG_LIST
                  VAR
                    ZVAL "order"
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "App.Order"
              METHOD_CALL
                ASSIGN_REF
                  VAR
                    ZVAL "receiver#1"
                  DIM
                    CALL
                      ZVAL "Sharp\\Internal\\statics"
                      ARG_LIST
                        CLASS_NAME
                          ZVAL "App\\Counter"
                        SHARP_TYPE_OF [1]
                          ZVAL "App.Order"
                    ZVAL "items"
                ZVAL "add"
                ARG_LIST
                  VAR
                    ZVAL "order"
              ASSIGN
                DIM
                  CALL
                    ZVAL "Sharp\\Internal\\statics"
                    ARG_LIST
                      CLASS_NAME
                        ZVAL "App\\Counter"
                      SHARP_TYPE_OF [1]
                        ZVAL "App.Order"
                  ZVAL "count"
                ZVAL 5
              RETURN
                DIM
                  CALL
                    ZVAL "Sharp\\Internal\\statics"
                    ARG_LIST
                      CLASS_NAME
                        ZVAL "App\\Counter"
                      SHARP_TYPE_OF [1]
                        ZVAL "App.Order"
                  ZVAL "count"
        "#}
    );
}

/// ```php
/// public static $"\0<sharp>\0statics" = ["" => ["count" => 0, "last" => null]];
/// ```
///
/// Spec section 11: a generic class keeps each type argument's static fields in one hidden static, whose entry under ""
/// holds each field's initial value. `\Sharp\Internal\statics` copies
/// that entry the first time a type argument uses its statics.
#[test]
fn a_generic_class_keeps_the_initial_values_of_its_static_fields_in_one_hidden_static() {
    let lowered = Lowered::new(
        "namespace App;\n\nclass Counter<TItem>\n{\n    public static int count { get; set; } = 0;\n\n    public static TItem? last { get; set; } = null;\n}\n",
    );
    let store = (0..lowered.nodes().len() as u32)
        .map(|node| lowered.render(node))
        .find(|tree| tree.starts_with("PROP_GROUP") && tree.contains(r#"ZVAL "\0<sharp>\0statics""#))
        .unwrap_or_else(|| panic!("`Counter` declares its statics: {}", lowered.tree()));

    assert_eq!(
        store,
        indoc! {r#"
            PROP_GROUP [8209]
              null
              PROP_DECL
                PROP_ELEM
                  ZVAL "\0<sharp>\0statics"
                  ARRAY [3]
                    ARRAY_ELEM
                      ARRAY [3]
                        ARRAY_ELEM
                          ZVAL 0
                          ZVAL "count"
                        ARRAY_ELEM
                          ZVAL null
                          ZVAL "last"
                      ZVAL ""
                  null
                  null
              null
        "#}
    );
}

/// ```php
/// $type = /* the class value of "App.Box<App.Order>" */;
/// $made = new ($type)();
/// $page = new ($type)(/* type args "App.Order" */);
/// $plain = new (\App\Order::class)();
/// ```
///
/// Spec section 25: `typeof(Box<Order>)` is the class value of `Box<Order>`, which plain PHP reads as the class name.
/// `new (type)()` with no type arguments marks its `new` so the engine creates the class the value names with the type
/// arguments the value carries. `new (type)<Order>()` names its own, as `new Box<Order>()` does, and a class without
/// type arguments stays PHP's `Order::class`.
#[test]
fn new_through_a_class_value_creates_the_class_with_the_type_arguments_it_carries() {
    let lowered = Lowered::new(
        "namespace App;\n\nclass Order\n{\n    public required Order()\n    {\n    }\n}\n\nclass Box<TItem>\n{\n    public required Box()\n    {\n    }\n}\n\nclass Report\n{\n    public static void run()\n    {\n        const type = typeof(Box<Order>);\n        const made = new (type)();\n        const page = new (type)<Order>();\n        const plain = new (typeof(Order))();\n    }\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "type"
                SHARP_TYPE_OF
                  ZVAL "App.Box<App.Order>"
              ASSIGN
                VAR
                  ZVAL "made"
                NEW [1]
                  VAR
                    ZVAL "type"
                  ARG_LIST
              ASSIGN
                VAR
                  ZVAL "page"
                SHARP_TYPE_ARGS
                  NEW
                    VAR
                      ZVAL "type"
                    ARG_LIST
                  ZVAL "App.Order"
              ASSIGN
                VAR
                  ZVAL "plain"
                NEW [1]
                  CLASS_NAME
                    ZVAL "App\\Order"
                  ARG_LIST
        "#}
    );
}

/// ```php
/// $type = /* the class value of "App.Counter<App.Order>" */;
/// \Sharp\Internal\statics(\App\Counter::class, 'App.Order')['count'] = 5;
/// (${'receiver#1'} = &\Sharp\Internal\statics(\App\Counter::class, 'App.Order')['items'])->add($order);
/// ```
///
/// Spec section 25: a write through a class value writes the static field of the class it holds, as
/// `Counter<Order>.count = 5` does, so the field is the entry of the type arguments the value carries. A method call
/// binds the field to a reference first, so `type.items.add(order)` changes the list where it lives.
#[test]
fn a_write_through_a_class_value_writes_the_static_field_of_the_type_arguments_it_carries() {
    let lowered = Lowered::new(
        "namespace App;\n\nclass Order\n{\n}\n\nclass Counter<TItem>\n{\n    public static int count { get; set; } = 0;\n\n    public static List<TItem> items { get; set; } = [];\n}\n\nclass Report\n{\n    public static void run(Order order)\n    {\n        const type = typeof(Counter<Order>);\n        type.count = 5;\n        type.items.add(order);\n    }\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "type"
                SHARP_TYPE_OF
                  ZVAL "App.Counter<App.Order>"
              ASSIGN
                DIM
                  CALL
                    ZVAL "Sharp\\Internal\\statics"
                    ARG_LIST
                      CLASS_NAME
                        ZVAL "App\\Counter"
                      SHARP_TYPE_OF [1]
                        ZVAL "App.Order"
                  ZVAL "count"
                ZVAL 5
              METHOD_CALL
                ASSIGN_REF
                  VAR
                    ZVAL "receiver#1"
                  DIM
                    CALL
                      ZVAL "Sharp\\Internal\\statics"
                      ARG_LIST
                        CLASS_NAME
                          ZVAL "App\\Counter"
                        SHARP_TYPE_OF [1]
                          ZVAL "App.Order"
                    ZVAL "items"
                ZVAL "add"
                ARG_LIST
                  VAR
                    ZVAL "order"
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
/// A nullable member has no implicit starting value, spec section 6, so it writes `= null`, which a field or a settable
/// auto-property takes as its default, as any constant initial value. A get-only property is `readonly`, which takes
/// no default, so its initial value runs in the constructor. `[257]` is `ZEND_NAME_NOT_FQ | ZEND_TYPE_NULLABLE`, and `[256]` is
/// `ZEND_TYPE_NULLABLE` on a class name.
#[test]
fn a_nullable_member_s_initial_null_is_its_default() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int? total = null;\n    public Calc? owner { get; set; } = null;\n    private (int|string)? key = null;\n    private int? start = 1;\n    public int? limit { get; } = null;\n}\n",
        &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc {}")],
    );
    let class = lowered.child(lowered.root(), 2);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [32772]
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
              PROP_GROUP [32772]
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
              PROP_GROUP [32772]
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
        .written()
        .into_iter()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_METHOD)
        .map(|(_, node)| (lowered.text(node.text), node.attr))
        .collect();

    assert_eq!(methods, [("make".to_owned(), 17), ("hide".to_owned(), 4), ("share".to_owned(), 2)]);
}

/// ```php
/// public function make(\Lib\Calc $other, float $rate = 1.5, int $loud = \PHP_DEBUG): \Lib\Calc {}
/// ```
///
/// A class type and PHP's constant are each their full name with `ZEND_NAME_FQ`, which is 0.
#[test]
fn parameters_carry_their_type_name_and_default() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public Calc make(Calc other, float rate = 1.5, int loud = global.PHP_DEBUG) { return other; }\n}\n",
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
                  ZVAL "PHP_DEBUG"
                null
                null
                null
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(method as u32, 3)),
        indoc! {r#"
            SHARP_TYPE_ARGS
              ZVAL "Lib\\Calc"
              ZVAL "Function<Lib.Calc(Lib.Calc, float, int)>"
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
            SHARP_TYPE_ARGS
              ZVAL [256] "Lib\\Calc"
              ZVAL "Function<Lib.Calc?(int?, Lib.Calc?)>"
        "#}
    );
}

/// ```php
/// public function group(array $items, \Sharp\Map|array|null $sizes = new \Sharp\Map(['a', 1])): ?\Sharp\Map { return null; }
/// ```
///
/// A `List` is a PHP array, so its type is `array`: a `TYPE` with `IS_ARRAY`, which is 7, as php-src's grammar builds
/// it. A `Map` is a `\Sharp\Map`, and a parameter takes plain PHP's array too, so its type is the union
/// `\Sharp\Map|array|null`. Neither type checks an element, so the parameter list ends with the type texts a call
/// from plain PHP checks each argument's elements against.
#[test]
fn a_list_type_is_the_array_type_and_a_map_type_its_class() {
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
                TYPE_UNION
                  ZVAL "Sharp\\Map"
                  TYPE [7]
                  ZVAL [1] "null"
                ZVAL "sizes"
                NEW
                  ZVAL "Sharp\\Map"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "a"
                        null
                      ARRAY_ELEM
                        ZVAL 1
                        null
                null
                null
                null
              SHARP_TYPE_ARGS
                null
                ZVAL "List<int>, Map<string, int>?"
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(method as u32, 3)),
        indoc! {r#"
            SHARP_TYPE_ARGS
              ZVAL [256] "Sharp\\Map"
              ZVAL "Function<Map<string, List<int>>?(List<int>, Map<string, int>?)>"
        "#}
    );
}

/// ```php
/// public function group(array $sizes, \Sharp\Map|array $calcs, array $keys): ?\Sharp\Map { return null; }
/// ```
///
/// A nullable type argument lowers as any other, so each `List` is `array`, a `TYPE` with `IS_ARRAY`, which is 7, and
/// each `Map` its class, and `[256]` adds `ZEND_TYPE_NULLABLE` to the collection itself.
#[test]
fn a_collection_with_a_nullable_type_argument_is_the_type_of_the_collection() {
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
                TYPE_UNION
                  ZVAL "Sharp\\Map"
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
              SHARP_TYPE_ARGS
                null
                ZVAL "List<int?>, Map<string, List<Lib.Calc?>>, List<(int|string)?>"
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(method as u32, 3)),
        indoc! {r#"
            SHARP_TYPE_ARGS
              ZVAL [256] "Sharp\\Map"
              ZVAL "Function<Map<string, Any?>?(List<int?>, Map<string, List<Lib.Calc?>>, List<(int|string)?>)>"
        "#}
    );
}

/// ```php
/// $counts = new \Sharp\Map([\Lib\Calc::Active, 1]);
/// $counts[\Lib\Calc::Closed] = 2;
/// return $counts[\Lib\Calc::Active] ?? 0;
/// ```
///
/// A `Map` holds any key itself, so a backed enum case goes in as the case, in a literal or an index.
#[test]
fn a_backed_enum_key_goes_into_a_map_as_its_case() {
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
                NEW
                  ZVAL "Sharp\\Map"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        CLASS_CONST
                          ZVAL "Lib\\Calc"
                          ZVAL "Active"
                        null
                      ARRAY_ELEM
                        ZVAL 1
                        null
              ASSIGN
                DIM
                  VAR
                    ZVAL "counts"
                  CLASS_CONST
                    ZVAL "Lib\\Calc"
                    ZVAL "Closed"
                ZVAL 2
              RETURN
                COALESCE
                  DIM
                    VAR
                      ZVAL "counts"
                    CLASS_CONST
                      ZVAL "Lib\\Calc"
                      ZVAL "Active"
                  ZVAL 0
        "#}
    );
}

/// ```php
/// $defaults = \is_array($defaults) ? new \Sharp\Map(\array_merge([], ...\array_map(fn ($key#1, $value#2) => [\Lib\Calc::from((string) $key#1), $value#2], \array_keys($defaults), $defaults))) : $defaults;
/// return new \Sharp\Map([...\array_merge([], ...\array_map(null, $defaults->keys(), \iterator_to_array($defaults, false))), \Lib\Calc::Active, 1]);
/// ```
///
/// A `Map` spread puts in each key of the spread `Map` followed by its value, and a backed enum key beside it goes in
/// as its case. Plain PHP passes a backed enum key as its backing value, so the parameter takes each one back as its
/// case first.
#[test]
fn a_backed_enum_key_beside_a_map_spread_goes_in_as_its_case() {
    assert_eq!(
        body_in(
            "Map<Calc, int> run(Map<Calc, int> defaults)",
            "        return [...defaults, Calc.Active: 1];\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "defaults"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "defaults"
                  NEW
                    ZVAL "Sharp\\Map"
                    ARG_LIST
                      CALL
                        ZVAL "array_merge"
                        ARG_LIST
                          ARRAY [3]
                          UNPACK
                            CALL
                              ZVAL "array_map"
                              ARG_LIST
                                ARROW_FUNC "" @7-7
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "key#1"
                                      null
                                      null
                                      null
                                      null
                                    PARAM
                                      null
                                      ZVAL "value#2"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  ARRAY [3]
                                    ARRAY_ELEM
                                      STATIC_CALL
                                        ZVAL "Lib\\Calc"
                                        ZVAL "from"
                                        ARG_LIST
                                          CAST [6]
                                            VAR
                                              ZVAL "key#1"
                                      null
                                    ARRAY_ELEM
                                      VAR
                                        ZVAL "value#2"
                                      null
                                  null
                                  null
                                CALL
                                  ZVAL "array_keys"
                                  ARG_LIST
                                    VAR
                                      ZVAL "defaults"
                                VAR
                                  ZVAL "defaults"
                  VAR
                    ZVAL "defaults"
              RETURN
                NEW
                  ZVAL "Sharp\\Map"
                  ARG_LIST
                    ARRAY [3]
                      UNPACK
                        CALL
                          ZVAL "array_merge"
                          ARG_LIST
                            ARRAY [3]
                            UNPACK
                              CALL
                                ZVAL "array_map"
                                ARG_LIST
                                  ZVAL null
                                  METHOD_CALL
                                    VAR
                                      ZVAL "defaults"
                                    ZVAL "keys"
                                    ARG_LIST
                                  CALL
                                    ZVAL "iterator_to_array"
                                    ARG_LIST
                                      VAR
                                        ZVAL "defaults"
                                      ZVAL false
                      ARRAY_ELEM
                        CLASS_CONST
                          ZVAL "Lib\\Calc"
                          ZVAL "Active"
                        null
                      ARRAY_ELEM
                        ZVAL 1
                        null
        "#}
    );
}

/// ```php
/// $counts = new \Sharp\Map();
/// $counts->remove($status);
/// $counts->remove(\Lib\Calc::Closed);
/// $maybe = $flag ? $counts : null;
/// return $counts->has($status) && $maybe?->get(key: $status) === null;
/// ```
///
/// A `Map` method runs on the `Map`, a `Sharp\Collection` method that serves every collection, and a key goes in as
/// itself. `Sharp\Collection` names its one parameter for what every collection takes, so a named argument goes in by
/// position.
#[test]
fn a_map_method_runs_on_the_map_and_takes_a_named_argument_by_position() {
    assert_eq!(
        body_in(
            "bool run(Calc status, bool flag)",
            "        Map<Calc, int> counts = [:];\n        counts.remove(status);\n        counts.remove(key: Calc.Closed);\n        Map<Calc, int>? maybe = flag ? counts : null;\n        return counts.has(status) && maybe?.get(key: status) == null;\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "counts"
                NEW
                  ZVAL "Sharp\\Map"
                  ARG_LIST
              METHOD_CALL
                VAR
                  ZVAL "counts"
                ZVAL "remove"
                ARG_LIST
                  VAR
                    ZVAL "status"
              METHOD_CALL
                VAR
                  ZVAL "counts"
                ZVAL "remove"
                ARG_LIST
                  CLASS_CONST
                    ZVAL "Lib\\Calc"
                    ZVAL "Closed"
              ASSIGN
                VAR
                  ZVAL "maybe"
                CONDITIONAL
                  VAR
                    ZVAL "flag"
                  VAR
                    ZVAL "counts"
                  ZVAL null
              RETURN
                AND
                  METHOD_CALL
                    VAR
                      ZVAL "counts"
                    ZVAL "has"
                    ARG_LIST
                      VAR
                        ZVAL "status"
                  BINARY_OP [16]
                    NULLSAFE_METHOD_CALL
                      VAR
                        ZVAL "maybe"
                      ZVAL "get"
                      ARG_LIST
                        VAR
                          ZVAL "status"
                    ZVAL null
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
/// $tags = new \Sharp\Set(["vip", "new"]);
/// $states = new \Sharp\Set([\Lib\Calc::Active, $status]);
/// $names = new \Sharp\Set([\strtoupper($name)]);
/// ```
///
/// Spec section 12: a `Set` literal is a new `Set` of its elements, each one as itself, a backed enum case too.
#[test]
fn a_set_literal_is_a_new_set_of_its_elements() {
    assert_eq!(
        body_in(
            "void run(Calc status, string name)",
            "        Set<string> tags = [\"vip\", \"new\"];\n        Set<Calc> states = [Calc.Active, status];\n        Set<string> names = [global.strtoupper(name)];\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "tags"
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "vip"
                        null
                      ARRAY_ELEM
                        ZVAL "new"
                        null
              ASSIGN
                VAR
                  ZVAL "states"
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        CLASS_CONST
                          ZVAL "Lib\\Calc"
                          ZVAL "Active"
                        null
                      ARRAY_ELEM
                        VAR
                          ZVAL "status"
                        null
              ASSIGN
                VAR
                  ZVAL "names"
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        CALL
                          ZVAL "strtoupper"
                          ARG_LIST
                            VAR
                              ZVAL "name"
                        null
        "#}
    );
}

/// ```php
/// $named = …;
/// $sets = [new \Sharp\Set([1, 1]), new \Sharp\Set([2])];
/// $named['b'] = new \Sharp\Set(['y']);
/// ```
///
/// A list literal inside a literal whose elements are declared `Set`s, or written into a `Map` entry declared a
/// `Set`, is that `Set`.
#[test]
fn a_list_literal_inside_a_literal_of_sets_is_a_set() {
    assert_eq!(
        body_in(
            "void run(Map<string, Set<string>> named)",
            "        List<Set<int>> sets = [[1, 1], [2]];\n        named[\"b\"] = [\"y\"];\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "named"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "named"
                  NEW
                    ZVAL "Sharp\\Map"
                    ARG_LIST
                      CALL
                        ZVAL "array_merge"
                        ARG_LIST
                          ARRAY [3]
                          UNPACK
                            CALL
                              ZVAL "array_map"
                              ARG_LIST
                                ARROW_FUNC "" @7-7
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "key#2"
                                      null
                                      null
                                      null
                                      null
                                    PARAM
                                      null
                                      ZVAL "value#3"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  ARRAY [3]
                                    ARRAY_ELEM
                                      CAST [6]
                                        VAR
                                          ZVAL "key#2"
                                      null
                                    ARRAY_ELEM
                                      VAR
                                        ZVAL "value#3"
                                      null
                                  null
                                  null
                                CALL
                                  ZVAL "array_keys"
                                  ARG_LIST
                                    VAR
                                      ZVAL "named"
                                CALL
                                  ZVAL "array_map"
                                  ARG_LIST
                                    ARROW_FUNC "" @7-7
                                      PARAM_LIST
                                        PARAM
                                          null
                                          ZVAL "element#1"
                                          null
                                          null
                                          null
                                          null
                                      null
                                      CONDITIONAL
                                        CALL
                                          ZVAL "is_array"
                                          ARG_LIST
                                            VAR
                                              ZVAL "element#1"
                                        NEW
                                          ZVAL "Sharp\\Set"
                                          ARG_LIST
                                            VAR
                                              ZVAL "element#1"
                                        VAR
                                          ZVAL "element#1"
                                      null
                                      null
                                    VAR
                                      ZVAL "named"
                  VAR
                    ZVAL "named"
              ASSIGN
                VAR
                  ZVAL "sets"
                ARRAY [3]
                  ARRAY_ELEM
                    NEW
                      ZVAL "Sharp\\Set"
                      ARG_LIST
                        ARRAY [3]
                          ARRAY_ELEM
                            ZVAL 1
                            null
                          ARRAY_ELEM
                            ZVAL 1
                            null
                    null
                  ARRAY_ELEM
                    NEW
                      ZVAL "Sharp\\Set"
                      ARG_LIST
                        ARRAY [3]
                          ARRAY_ELEM
                            ZVAL 2
                            null
                    null
              ASSIGN
                DIM
                  VAR
                    ZVAL "named"
                  ZVAL "b"
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "y"
                        null
        "#}
    );
}

/// ```php
/// $tags = new \Sharp\Set([]);
/// $tags = new \Sharp\Set(["b"]);
/// ```
///
/// A list literal assigned to a local declared `Set`, as its initial value or later, is that `Set`.
#[test]
fn a_list_literal_assigned_to_a_set_local_is_a_set() {
    assert_eq!(
        body_in("void run(bool flag)", "        Set<string> tags = [];\n        tags = [\"b\"];\n", &[]),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "tags"
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
              ASSIGN
                VAR
                  ZVAL "tags"
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "b"
                        null
        "#}
    );
}

/// ```php
/// $branch = new \Sharp\Set([]);
/// if ($flag) { $branch = new \Sharp\Set(["d"]); } else { $branch = new \Sharp\Set(["e"]); }
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
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
              IF
                IF_ELEM
                  VAR
                    ZVAL "flag"
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "branch"
                      NEW
                        ZVAL "Sharp\\Set"
                        ARG_LIST
                          ARRAY [3]
                            ARRAY_ELEM
                              ZVAL "d"
                              null
                IF_ELEM
                  null
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "branch"
                      NEW
                        ZVAL "Sharp\\Set"
                        ARG_LIST
                          ARRAY [3]
                            ARRAY_ELEM
                              ZVAL "e"
                              null
        "#}
    );
}

/// ```php
/// $maybe = \is_array($maybe) ? new \Sharp\Set($maybe) : $maybe;
/// $maybe ??= new \Sharp\Set(["c"]);
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
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "maybe"
                  NEW
                    ZVAL "Sharp\\Set"
                    ARG_LIST
                      VAR
                        ZVAL "maybe"
                  VAR
                    ZVAL "maybe"
              ASSIGN_COALESCE
                VAR
                  ZVAL "maybe"
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "c"
                        null
        "#}
    );
}

/// ```php
/// #[\Sharp\Crossing(['set', null])]
/// private \Sharp\Set $tags = new \Sharp\Set(["a"]);
/// ```
///
/// A list literal that is the initial value of a property declared `Set` is that `Set`, a `new` the engine runs for
/// each object.
#[test]
fn a_list_literal_initial_value_of_a_set_property_is_a_set() {
    assert_eq!(
        property_groups(&Lowered::new("class Report\n{\n    private Set<string> tags = [\"a\"];\n}\n")),
        [indoc! {r#"
            PROP_GROUP [32772]
              SHARP_TYPE_ARGS
                ZVAL "Sharp\\Set"
                ZVAL "Set<string>"
              PROP_DECL
                PROP_ELEM
                  ZVAL "tags"
                  NEW
                    ZVAL "Sharp\\Set"
                    ARG_LIST
                      ARRAY [3]
                        ARRAY_ELEM
                          ZVAL "a"
                          null
                  null
                  null
              ATTRIBUTE_LIST
                ATTRIBUTE_GROUP
                  ATTRIBUTE
                    ZVAL "Sharp\\Crossing"
                    ARG_LIST
                      ARRAY [3]
                        ARRAY_ELEM
                          ZVAL "set"
                          null
                        ARRAY_ELEM
                          ZVAL null
                          null
        "#}]
    );
}

/// ```php
/// $this->tags = new \Sharp\Set(["c"]);
/// \Report::$shared = new \Sharp\Set(["d"]);
/// $this->hooked = new \Sharp\Set(["e"]);
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
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "c"
                        null
              ASSIGN
                STATIC_PROP
                  ZVAL "Report"
                  ZVAL "shared"
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "d"
                        null
              ASSIGN
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "hooked"
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "e"
                        null
        "#}
    );
}

/// ```php
/// public function run(): \Sharp\Set { return new \Sharp\Set(["a"]); }
/// public function short(): \Sharp\Set { return new \Sharp\Set(["b"]); }
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
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "a"
                        null
        "#}
    );
    assert_eq!(
        body("short"),
        indoc! {r#"
            STMT_LIST
              RETURN
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "b"
                        null
        "#}
    );
}

/// ```php
/// public function run(\Sharp\Set|array $seed = new \Sharp\Set(["a"])): int { … }
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
                TYPE_UNION
                  ZVAL "Sharp\\Set"
                  TYPE [7]
                ZVAL "seed"
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "a"
                        null
                null
                null
                null
              SHARP_TYPE_ARGS
                null
                ZVAL "Set<string>"
        "#}
    );
}

/// ```php
/// $tags = \is_array($tags) ? new \Sharp\Set($tags) : $tags;
/// $states = \is_array($states) ? new \Sharp\Set($states) : $states;
/// $tags->add($name);
/// $tags->remove(\strtoupper($name));
/// $states->add($status);
/// $states->remove(\Lib\Calc::Closed);
/// $tags->clear();
/// return $tags->contains($name);
/// ```
///
/// A `Set` method runs on the `Set`, a `Sharp\Collection` method that serves every collection, which reads and
/// changes the `Set` where it lives. Each element goes in as itself.
#[test]
fn set_methods_run_on_the_set() {
    assert_eq!(
        body_in(
            "bool run(Set<string> tags, Set<Calc> states, Calc status, string name)",
            "        tags.add(name);\n        tags.remove(global.strtoupper(name));\n        states.add(status);\n        states.remove(Calc.Closed);\n        tags.clear();\n        return tags.contains(name);\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; enum Calc: string { case Active = 'a'; case Closed = 'c'; }")]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "tags"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "tags"
                  NEW
                    ZVAL "Sharp\\Set"
                    ARG_LIST
                      VAR
                        ZVAL "tags"
                  VAR
                    ZVAL "tags"
              ASSIGN
                VAR
                  ZVAL "states"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "states"
                  NEW
                    ZVAL "Sharp\\Set"
                    ARG_LIST
                      VAR
                        ZVAL "states"
                  VAR
                    ZVAL "states"
              METHOD_CALL
                VAR
                  ZVAL "tags"
                ZVAL "add"
                ARG_LIST
                  VAR
                    ZVAL "name"
              METHOD_CALL
                VAR
                  ZVAL "tags"
                ZVAL "remove"
                ARG_LIST
                  CALL
                    ZVAL "strtoupper"
                    ARG_LIST
                      VAR
                        ZVAL "name"
              METHOD_CALL
                VAR
                  ZVAL "states"
                ZVAL "add"
                ARG_LIST
                  VAR
                    ZVAL "status"
              METHOD_CALL
                VAR
                  ZVAL "states"
                ZVAL "remove"
                ARG_LIST
                  CLASS_CONST
                    ZVAL "Lib\\Calc"
                    ZVAL "Closed"
              METHOD_CALL
                VAR
                  ZVAL "tags"
                ZVAL "clear"
                ARG_LIST
              RETURN
                METHOD_CALL
                  VAR
                    ZVAL "tags"
                  ZVAL "contains"
                  ARG_LIST
                    VAR
                      ZVAL "name"
        "#}
    );
}

/// ```php
/// $tags = \is_array($tags) ? new \Sharp\Set($tags) : $tags;
/// $prices = …;
/// $count = $tags->count();
/// $kept = $tags->filter($keep);
/// $priced = $prices->filter($positive);
/// $tags->any($keep);
/// return $tags->toList();
/// ```
///
/// Every `Set` and `Map` method is the `Sharp\Collection` method of its name, `count`, `filter` and `toList` too.
#[test]
fn set_and_map_methods_are_the_collection_methods_of_their_names() {
    assert_eq!(
        body_in(
            "List<string> run(Set<string> tags, Function<bool(string)> keep, Map<string, int> prices, Function<bool(int)> positive)",
            "        const count = tags.count();\n        const kept = tags.filter(keep);\n        const priced = prices.filter(positive);\n        tags.any(keep);\n        return tags.toList();\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "tags"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "tags"
                  NEW
                    ZVAL "Sharp\\Set"
                    ARG_LIST
                      VAR
                        ZVAL "tags"
                  VAR
                    ZVAL "tags"
              ASSIGN
                VAR
                  ZVAL "prices"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "prices"
                  NEW
                    ZVAL "Sharp\\Map"
                    ARG_LIST
                      CALL
                        ZVAL "array_merge"
                        ARG_LIST
                          ARRAY [3]
                          UNPACK
                            CALL
                              ZVAL "array_map"
                              ARG_LIST
                                ARROW_FUNC "" @7-7
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "key#1"
                                      null
                                      null
                                      null
                                      null
                                    PARAM
                                      null
                                      ZVAL "value#2"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  ARRAY [3]
                                    ARRAY_ELEM
                                      CAST [6]
                                        VAR
                                          ZVAL "key#1"
                                      null
                                    ARRAY_ELEM
                                      VAR
                                        ZVAL "value#2"
                                      null
                                  null
                                  null
                                CALL
                                  ZVAL "array_keys"
                                  ARG_LIST
                                    VAR
                                      ZVAL "prices"
                                VAR
                                  ZVAL "prices"
                  VAR
                    ZVAL "prices"
              ASSIGN
                VAR
                  ZVAL "count"
                METHOD_CALL
                  VAR
                    ZVAL "tags"
                  ZVAL "count"
                  ARG_LIST
              ASSIGN
                VAR
                  ZVAL "kept"
                METHOD_CALL
                  VAR
                    ZVAL "tags"
                  ZVAL "filter"
                  ARG_LIST
                    VAR
                      ZVAL "keep"
              ASSIGN
                VAR
                  ZVAL "priced"
                METHOD_CALL
                  VAR
                    ZVAL "prices"
                  ZVAL "filter"
                  ARG_LIST
                    VAR
                      ZVAL "positive"
              METHOD_CALL
                VAR
                  ZVAL "tags"
                ZVAL "any"
                ARG_LIST
                  VAR
                    ZVAL "keep"
              RETURN
                METHOD_CALL
                  VAR
                    ZVAL "tags"
                  ZVAL "toList"
                  ARG_LIST
        "#}
    );
}

/// ```php
/// return \count($this->lines) + $counts->count();
/// ```
///
/// A `List`'s `count()` is PHP's `count` on the array, and a `Map`'s is the `Sharp\Collection` method.
#[test]
fn a_list_counts_with_php_count_and_a_map_with_its_method() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public List<int> lines { get; private set; } = [];\n\n    public int run(Map<string, int> counts)\n    {\n        return this.lines.count() + counts.count();\n    }\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "counts"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "counts"
                  NEW
                    ZVAL "Sharp\\Map"
                    ARG_LIST
                      CALL
                        ZVAL "array_merge"
                        ARG_LIST
                          ARRAY [3]
                          UNPACK
                            CALL
                              ZVAL "array_map"
                              ARG_LIST
                                ARROW_FUNC "" @7-7
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "key#1"
                                      null
                                      null
                                      null
                                      null
                                    PARAM
                                      null
                                      ZVAL "value#2"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  ARRAY [3]
                                    ARRAY_ELEM
                                      CAST [6]
                                        VAR
                                          ZVAL "key#1"
                                      null
                                    ARRAY_ELEM
                                      VAR
                                        ZVAL "value#2"
                                      null
                                  null
                                  null
                                CALL
                                  ZVAL "array_keys"
                                  ARG_LIST
                                    VAR
                                      ZVAL "counts"
                                VAR
                                  ZVAL "counts"
                  VAR
                    ZVAL "counts"
              RETURN
                BINARY_OP [1]
                  CALL
                    ZVAL "count"
                    ARG_LIST
                      PROP
                        VAR
                          ZVAL "this"
                        ZVAL "lines"
                  METHOD_CALL
                    VAR
                      ZVAL "counts"
                    ZVAL "count"
                    ARG_LIST
        "#}
    );
}

/// Each method `INLINED_COLLECTION_METHODS` names lowers to PHP on the array, never to a call of the method itself.
#[test]
fn every_inlined_collection_method_lowers_to_php_on_the_array() {
    for (collection, method) in INLINED_COLLECTION_METHODS {
        let argument = match method {
            "remove" | "contains" | "indexOf" | "getOrNull" => "1",
            _ => "",
        };
        let body = Lowered::new(&format!(
            "namespace App.Tenant;\n\nclass Report\n{{\n    public void run({collection}<int> values)\n    {{\n        values.{method}({argument});\n    }}\n}}\n"
        ))
        .body();
        let root = body
            .lines()
            .rfind(|line| line.starts_with("  ") && !line.starts_with("   "))
            .expect("the call lowers to a statement")
            .trim();

        assert_ne!(root, "METHOD_CALL", "{collection}.{method}:\n{body}");
    }
}

/// ```php
/// $found = $sizes[$extra] ?? null;
/// $unique = new \Sharp\Set($sizes);
/// ```
///
/// Spec section 12: `getOrNull(i)` is the safe read, which gives null past either end, and `toSet()` is the `Set` a
/// literal of the elements makes, each element once.
#[test]
fn get_or_null_and_to_set_lower_to_php_on_the_array() {
    assert_eq!(
        body_in(
            "void run(List<int> sizes, int extra)",
            "        let found = sizes.getOrNull(extra);\n        let unique = sizes.toSet();\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "found"
                COALESCE
                  DIM
                    VAR
                      ZVAL "sizes"
                    VAR
                      ZVAL "extra"
                  ZVAL null
              ASSIGN
                VAR
                  ZVAL "unique"
                NEW
                  ZVAL "Sharp\\Set"
                  ARG_LIST
                    VAR
                      ZVAL "sizes"
        "#}
    );
}

/// ```php
/// $held = \in_array($tag, $tags, true);
/// $at = ((${'index#1'} = \array_search('vip', $tags, true)) === false) ? null : ${'index#1'};
/// $next = ((${'index#1'} = \array_search($tag, $tags, true)) === false) ? null : ${'index#1'};
/// $found = (($maybe === null) ? null : \in_array($tag, $maybe, true)) ?? false;
/// return $held && $at !== $next && $found && \in_array($ratio, $ratios, true);
/// ```
///
/// Spec section 12's `contains` and `indexOf` compare by section 19's `==`, which is `===` on a string and on two floats,
/// so where neither the receiver nor the value runs code they are PHP's strict `in_array` and `array_search`. `indexOf`
/// gives null where `array_search` gives false, and its hidden `$index#N` holds the index, since index 0 is falsy. The
/// next search takes the same hidden variable. A null-safe search tests its receiver first.
#[test]
fn a_list_search_is_a_strict_php_search_where_equality_is_identity() {
    assert_eq!(
        body_in(
            "bool run(List<string> tags, string tag, List<float> ratios, float ratio, List<string>? maybe)",
            "        const held = tags.contains(tag);\n        const at = tags.indexOf(\"vip\");\n        const next = tags.indexOf(tag);\n        const found = maybe?.contains(tag) ?? false;\n        return held && at != next && found && ratios.contains(ratio);\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "held"
                CALL
                  ZVAL "in_array"
                  ARG_LIST
                    VAR
                      ZVAL "tag"
                    VAR
                      ZVAL "tags"
                    ZVAL true
              ASSIGN
                VAR
                  ZVAL "at"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    ASSIGN
                      VAR
                        ZVAL "index#1"
                      CALL
                        ZVAL "array_search"
                        ARG_LIST
                          ZVAL "vip"
                          VAR
                            ZVAL "tags"
                          ZVAL true
                    ZVAL false
                  ZVAL null
                  VAR
                    ZVAL "index#1"
              ASSIGN
                VAR
                  ZVAL "next"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    ASSIGN
                      VAR
                        ZVAL "index#1"
                      CALL
                        ZVAL "array_search"
                        ARG_LIST
                          VAR
                            ZVAL "tag"
                          VAR
                            ZVAL "tags"
                          ZVAL true
                    ZVAL false
                  ZVAL null
                  VAR
                    ZVAL "index#1"
              ASSIGN
                VAR
                  ZVAL "found"
                COALESCE
                  CONDITIONAL [1]
                    BINARY_OP [16]
                      VAR
                        ZVAL "maybe"
                      ZVAL null
                    ZVAL null
                    CALL
                      ZVAL "in_array"
                      ARG_LIST
                        VAR
                          ZVAL "tag"
                        VAR
                          ZVAL "maybe"
                        ZVAL true
                  ZVAL false
              RETURN
                AND
                  AND
                    AND
                      VAR
                        ZVAL "held"
                      BINARY_OP [17]
                        VAR
                          ZVAL "at"
                        VAR
                          ZVAL "next"
                    VAR
                      ZVAL "found"
                  CALL
                    ZVAL "in_array"
                    ARG_LIST
                      VAR
                        ZVAL "ratio"
                      VAR
                        ZVAL "ratios"
                      ZVAL true
        "#}
    );
}

/// ```php
/// return \array_find_key($prices, (fn (${'value#1'}) => fn (${'element#2'}) => \App\Money::op_Equality(${'element#2'}, ${'value#1'}))($price)) !== null
///     && \array_find_key($amounts, (fn (${'value#1'}) => fn (${'element#2'}) =>
///         ((${'operand#3'} = ${'element#2'}) === null) === ((${'operand#4'} = ${'value#1'}) === null)
///         && (${'operand#3'} === null || (float) ${'operand#3'} === (float) ${'operand#4'}))(1)) === 0
///     && \array_find_key($rows, (fn (${'value#1'}) => fn (${'element#2'}) => ${'element#2'} == ${'value#1'})($row)) !== null;
/// ```
///
/// Where section 19's `==` is not `===`, a search runs the `==` the bridge lowers for each element: the `op_Equality`
/// the element's class declares, an int compared with a float as two floats, where a null element equals only null,
/// or PHP's `==` on two `Map`s. The outer function takes the value once, as an argument, after the receiver, and the
/// inner one compares an element with it. `contains` is whether `\array_find_key` found a key.
#[test]
fn a_list_search_runs_the_equality_of_its_element_type_for_each_element() {
    assert_eq!(
        ledger_body(
            "bool run(List<Money> prices, Money price, List<(int|float)?> amounts, List<Map<string, int>> rows, Map<string, int> row)",
            "        return prices.contains(price) && amounts.indexOf(1) == 0 && rows.contains(row);\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "rows"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "rows"
                  CALL
                    ZVAL "array_map"
                    ARG_LIST
                      ARROW_FUNC "" @14-14
                        PARAM_LIST
                          PARAM
                            null
                            ZVAL "element#1"
                            null
                            null
                            null
                            null
                        null
                        CONDITIONAL
                          CALL
                            ZVAL "is_array"
                            ARG_LIST
                              VAR
                                ZVAL "element#1"
                          NEW
                            ZVAL "Sharp\\Map"
                            ARG_LIST
                              CALL
                                ZVAL "array_merge"
                                ARG_LIST
                                  ARRAY [3]
                                  UNPACK
                                    CALL
                                      ZVAL "array_map"
                                      ARG_LIST
                                        ARROW_FUNC "" @14-14
                                          PARAM_LIST
                                            PARAM
                                              null
                                              ZVAL "key#2"
                                              null
                                              null
                                              null
                                              null
                                            PARAM
                                              null
                                              ZVAL "value#3"
                                              null
                                              null
                                              null
                                              null
                                          null
                                          ARRAY [3]
                                            ARRAY_ELEM
                                              CAST [6]
                                                VAR
                                                  ZVAL "key#2"
                                              null
                                            ARRAY_ELEM
                                              VAR
                                                ZVAL "value#3"
                                              null
                                          null
                                          null
                                        CALL
                                          ZVAL "array_keys"
                                          ARG_LIST
                                            VAR
                                              ZVAL "element#1"
                                        VAR
                                          ZVAL "element#1"
                          VAR
                            ZVAL "element#1"
                        null
                        null
                      VAR
                        ZVAL "rows"
                  VAR
                    ZVAL "rows"
              ASSIGN
                VAR
                  ZVAL "row"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "row"
                  NEW
                    ZVAL "Sharp\\Map"
                    ARG_LIST
                      CALL
                        ZVAL "array_merge"
                        ARG_LIST
                          ARRAY [3]
                          UNPACK
                            CALL
                              ZVAL "array_map"
                              ARG_LIST
                                ARROW_FUNC "" @14-14
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "key#1"
                                      null
                                      null
                                      null
                                      null
                                    PARAM
                                      null
                                      ZVAL "value#2"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  ARRAY [3]
                                    ARRAY_ELEM
                                      CAST [6]
                                        VAR
                                          ZVAL "key#1"
                                      null
                                    ARRAY_ELEM
                                      VAR
                                        ZVAL "value#2"
                                      null
                                  null
                                  null
                                CALL
                                  ZVAL "array_keys"
                                  ARG_LIST
                                    VAR
                                      ZVAL "row"
                                VAR
                                  ZVAL "row"
                  VAR
                    ZVAL "row"
              RETURN
                AND
                  AND
                    BINARY_OP [17]
                      CALL
                        ZVAL "array_find_key"
                        ARG_LIST
                          VAR
                            ZVAL "prices"
                          CALL
                            ARROW_FUNC "" @16-16
                              PARAM_LIST
                                PARAM
                                  null
                                  ZVAL "value#1"
                                  null
                                  null
                                  null
                                  null
                              null
                              ARROW_FUNC "" @16-16
                                PARAM_LIST
                                  PARAM
                                    null
                                    ZVAL "element#2"
                                    null
                                    null
                                    null
                                    null
                                null
                                STATIC_CALL
                                  ZVAL "App\\Money"
                                  ZVAL "op_Equality"
                                  ARG_LIST
                                    VAR
                                      ZVAL "element#2"
                                    VAR
                                      ZVAL "value#1"
                                null
                                null
                              null
                              null
                            ARG_LIST
                              VAR
                                ZVAL "price"
                      ZVAL null
                    BINARY_OP [16]
                      CALL
                        ZVAL "array_find_key"
                        ARG_LIST
                          VAR
                            ZVAL "amounts"
                          CALL
                            ARROW_FUNC "" @16-16
                              PARAM_LIST
                                PARAM
                                  null
                                  ZVAL "value#1"
                                  null
                                  null
                                  null
                                  null
                              null
                              ARROW_FUNC "" @16-16
                                PARAM_LIST
                                  PARAM
                                    null
                                    ZVAL "element#2"
                                    null
                                    null
                                    null
                                    null
                                null
                                AND
                                  BINARY_OP [16]
                                    BINARY_OP [16]
                                      ASSIGN
                                        VAR
                                          ZVAL "operand#3"
                                        VAR
                                          ZVAL "element#2"
                                      ZVAL null
                                    BINARY_OP [16]
                                      ASSIGN
                                        VAR
                                          ZVAL "operand#4"
                                        VAR
                                          ZVAL "value#1"
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
                                      CAST [5]
                                        VAR
                                          ZVAL "operand#4"
                                null
                                null
                              null
                              null
                            ARG_LIST
                              ZVAL 1
                      ZVAL 0
                  BINARY_OP [17]
                    CALL
                      ZVAL "array_find_key"
                      ARG_LIST
                        VAR
                          ZVAL "rows"
                        CALL
                          ARROW_FUNC "" @16-16
                            PARAM_LIST
                              PARAM
                                null
                                ZVAL "value#1"
                                null
                                null
                                null
                                null
                            null
                            ARROW_FUNC "" @16-16
                              PARAM_LIST
                                PARAM
                                  null
                                  ZVAL "element#2"
                                  null
                                  null
                                  null
                                  null
                              null
                              BINARY_OP [18]
                                VAR
                                  ZVAL "element#2"
                                VAR
                                  ZVAL "value#1"
                              null
                              null
                            null
                            null
                          ARG_LIST
                            VAR
                              ZVAL "row"
                    ZVAL null
        "#}
    );
}

/// ```php
/// $x = 1;
/// return \array_find_key($this->bump($xs), (fn (${'value#1'}) => fn (${'element#2'}) => ${'element#2'} === ${'value#1'})($this->n)) !== null
///     && \array_find_key(\App\Tenant\Report::pick($xs, $x = 2), (fn (${'value#1'}) => fn (${'element#2'}) => ${'element#2'} === ${'value#1'})($x)) === 0
///     && ((${'index#2'} = \array_find_key((${'receiver#1'} = $this->bumped())->items, (fn (${'value#3'}) => fn (${'element#4'}) =>
///         ${'element#4'} === ${'value#3'})($this->n))) !== null) && ${'receiver#1'}->items->removeAt(${'index#2'}) === null;
///
/// return $this->bumped()->counts->has($this->n) && $this->bumped()->counts->remove($this->n);
/// ```
///
/// PHP's `in_array` and `array_search` read their value before their array, so a search whose receiver or value runs
/// code takes the form that runs the receiver first, as the call runs it: `this.bump(xs)` changes `this.n` before
/// `contains` reads it, `Report.pick(xs, x = 2)` assigns `x` before `indexOf` reads it, and `this.bumped()` changes
/// `this.n` before a `List`'s `remove` reads it. A `Map`'s `has` and `remove` are method calls on the `Map`, which PHP
/// runs receiver first.

#[test]
fn a_search_runs_its_receiver_before_its_value() {
    assert_eq!(
        Lowered::new(
            "namespace App.Tenant;\n\nclass Report\n{\n    private int n = 1;\n\n    public List<int> items { get; set; } = [];\n\n    public List<int> bump(List<int> xs)\n    {\n        this.n = this.n + 1;\n        return xs;\n    }\n\n    public Report bumped()\n    {\n        this.n = this.n + 1;\n        return this;\n    }\n\n    public static List<int> pick(List<int> xs, int ignored) => xs;\n\n    public bool run(List<int> xs)\n    {\n        let x = 1;\n        return this.bump(xs).contains(this.n) && Report.pick(xs, x = 2).indexOf(x) == 0 && this.bumped().items.remove(this.n);\n    }\n}\n"
        )
        .body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "x"
                ZVAL 1
              RETURN
                AND
                  AND
                    BINARY_OP [17]
                      CALL
                        ZVAL "array_find_key"
                        ARG_LIST
                          METHOD_CALL
                            VAR
                              ZVAL "this"
                            ZVAL "bump"
                            ARG_LIST
                              VAR
                                ZVAL "xs"
                          CALL
                            ARROW_FUNC "" @26-26
                              PARAM_LIST
                                PARAM
                                  null
                                  ZVAL "value#1"
                                  null
                                  null
                                  null
                                  null
                              null
                              ARROW_FUNC "" @26-26
                                PARAM_LIST
                                  PARAM
                                    null
                                    ZVAL "element#2"
                                    null
                                    null
                                    null
                                    null
                                null
                                BINARY_OP [16]
                                  VAR
                                    ZVAL "element#2"
                                  VAR
                                    ZVAL "value#1"
                                null
                                null
                              null
                              null
                            ARG_LIST
                              PROP
                                VAR
                                  ZVAL "this"
                                ZVAL "n"
                      ZVAL null
                    BINARY_OP [16]
                      CALL
                        ZVAL "array_find_key"
                        ARG_LIST
                          STATIC_CALL
                            ZVAL "App\\Tenant\\Report"
                            ZVAL "pick"
                            ARG_LIST
                              VAR
                                ZVAL "xs"
                              ASSIGN
                                VAR
                                  ZVAL "x"
                                ZVAL 2
                          CALL
                            ARROW_FUNC "" @26-26
                              PARAM_LIST
                                PARAM
                                  null
                                  ZVAL "value#1"
                                  null
                                  null
                                  null
                                  null
                              null
                              ARROW_FUNC "" @26-26
                                PARAM_LIST
                                  PARAM
                                    null
                                    ZVAL "element#2"
                                    null
                                    null
                                    null
                                    null
                                null
                                BINARY_OP [16]
                                  VAR
                                    ZVAL "element#2"
                                  VAR
                                    ZVAL "value#1"
                                null
                                null
                              null
                              null
                            ARG_LIST
                              VAR
                                ZVAL "x"
                      ZVAL 0
                  AND
                    BINARY_OP [17]
                      ASSIGN
                        VAR
                          ZVAL "index#2"
                        CALL
                          ZVAL "array_find_key"
                          ARG_LIST
                            PROP
                              ASSIGN
                                VAR
                                  ZVAL "receiver#1"
                                METHOD_CALL
                                  VAR
                                    ZVAL "this"
                                  ZVAL "bumped"
                                  ARG_LIST
                              ZVAL "items"
                            CALL
                              ARROW_FUNC "" @26-26
                                PARAM_LIST
                                  PARAM
                                    null
                                    ZVAL "value#3"
                                    null
                                    null
                                    null
                                    null
                                null
                                ARROW_FUNC "" @26-26
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "element#4"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  BINARY_OP [16]
                                    VAR
                                      ZVAL "element#4"
                                    VAR
                                      ZVAL "value#3"
                                  null
                                  null
                                null
                                null
                              ARG_LIST
                                PROP
                                  VAR
                                    ZVAL "this"
                                  ZVAL "n"
                      ZVAL null
                    BINARY_OP [16]
                      METHOD_CALL
                        PROP
                          VAR
                            ZVAL "receiver#1"
                          ZVAL "items"
                        ZVAL "removeAt"
                        ARG_LIST
                          VAR
                            ZVAL "index#2"
                      ZVAL null
        "#}
    );
    assert_eq!(
        Lowered::new(
            "namespace App.Tenant;\n\nclass Report\n{\n    private int n = 1;\n\n    public Map<int, int> counts { get; set; } = [:];\n\n    public Report bumped()\n    {\n        this.n = this.n + 1;\n        return this;\n    }\n\n    public bool run()\n    {\n        return this.bumped().counts.has(this.n) && this.bumped().counts.remove(this.n);\n    }\n}\n"
        )
        .body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                AND
                  METHOD_CALL
                    PROP
                      METHOD_CALL
                        VAR
                          ZVAL "this"
                        ZVAL "bumped"
                        ARG_LIST
                      ZVAL "counts"
                    ZVAL "has"
                    ARG_LIST
                      PROP
                        VAR
                          ZVAL "this"
                        ZVAL "n"
                  METHOD_CALL
                    PROP
                      METHOD_CALL
                        VAR
                          ZVAL "this"
                        ZVAL "bumped"
                        ARG_LIST
                      ZVAL "counts"
                    ZVAL "remove"
                    ARG_LIST
                      PROP
                        VAR
                          ZVAL "this"
                        ZVAL "n"
        "#}
    );
}

/// ```php
/// $last = ($sizes === []) ? throw new \OutOfRangeException('The list is empty.') : \array_last($sizes);
/// return ((${'receiver#1'} = $this->make()) === []) ? throw new \OutOfRangeException('The list is empty.') : \array_last(${'receiver#1'});
/// ```
///
/// `last()` is the last element, and throws `OutOfRangeException` on an empty list, as Kotlin's `last()` throws. It
/// tests the list for `[]` rather than `\array_last` for null, which a `List<int?>` may hold last, so it reads the
/// receiver twice, a receiver that runs code through a hidden `$receiver#N`.
#[test]
fn last_is_the_last_element_and_throws_on_an_empty_list() {
    assert_eq!(
        Lowered::new(
            "namespace App.Tenant;\n\nclass Report\n{\n    public List<int> make() => [1];\n\n    public int run(List<int> sizes)\n    {\n        const last = sizes.last();\n        return last + this.make().last();\n    }\n}\n"
        )
        .body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "last"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    VAR
                      ZVAL "sizes"
                    ARRAY [3]
                  THROW
                    NEW
                      ZVAL "OutOfRangeException"
                      ARG_LIST
                        ZVAL "The list is empty."
                  CALL
                    ZVAL "array_last"
                    ARG_LIST
                      VAR
                        ZVAL "sizes"
              RETURN
                BINARY_OP [1]
                  VAR
                    ZVAL "last"
                  CONDITIONAL [1]
                    BINARY_OP [16]
                      ASSIGN
                        VAR
                          ZVAL "receiver#1"
                        METHOD_CALL
                          VAR
                            ZVAL "this"
                          ZVAL "make"
                          ARG_LIST
                      ARRAY [3]
                    THROW
                      NEW
                        ZVAL "OutOfRangeException"
                        ARG_LIST
                          ZVAL "The list is empty."
                    CALL
                      ZVAL "array_last"
                      ARG_LIST
                        VAR
                          ZVAL "receiver#1"
        "#}
    );
}

/// ```php
/// $tags->insert(0, 'a');
/// $removed = ((${'index#1'} = \array_search('a', $tags, true)) !== false) && $tags->removeAt(${'index#1'}) === null;
/// $tags = [];
/// return $removed && ((${'index#1'} = \array_find_key($prices, (fn (${'value#2'}) => fn (${'element#3'}) =>
///     \App\Money::op_Equality(${'element#3'}, ${'value#2'}))($price))) !== null) && $prices->removeAt(${'index#1'}) === null;
/// ```
///
/// Spec section 12's changes to a `List` write it back where it lives. `insert` is the `Sharp\Collection` method of
/// its name. `remove(value)` finds the index of the first element `==` the value, as `indexOf` does, and gives true
/// when `Sharp\Collection::removeAt` removed it, which renumbers the list. `clear` assigns `[]`.
#[test]
fn list_insert_remove_and_clear_change_the_list_where_it_lives() {
    assert_eq!(
        ledger_body(
            "bool run(List<string> tags, List<Money> prices, Money price)",
            "        tags.insert(0, \"a\");\n        const removed = tags.remove(\"a\");\n        tags.clear();\n        return removed && prices.remove(price);\n"
        ),
        indoc! {r#"
            STMT_LIST
              METHOD_CALL
                VAR
                  ZVAL "tags"
                ZVAL "insert"
                ARG_LIST
                  ZVAL 0
                  ZVAL "a"
              ASSIGN
                VAR
                  ZVAL "removed"
                AND
                  BINARY_OP [17]
                    ASSIGN
                      VAR
                        ZVAL "index#1"
                      CALL
                        ZVAL "array_search"
                        ARG_LIST
                          ZVAL "a"
                          VAR
                            ZVAL "tags"
                          ZVAL true
                    ZVAL false
                  BINARY_OP [16]
                    METHOD_CALL
                      VAR
                        ZVAL "tags"
                      ZVAL "removeAt"
                      ARG_LIST
                        VAR
                          ZVAL "index#1"
                    ZVAL null
              ASSIGN
                VAR
                  ZVAL "tags"
                ARRAY [3]
              RETURN
                AND
                  VAR
                    ZVAL "removed"
                  AND
                    BINARY_OP [17]
                      ASSIGN
                        VAR
                          ZVAL "index#1"
                        CALL
                          ZVAL "array_find_key"
                          ARG_LIST
                            VAR
                              ZVAL "prices"
                            CALL
                              ARROW_FUNC "" @19-19
                                PARAM_LIST
                                  PARAM
                                    null
                                    ZVAL "value#2"
                                    null
                                    null
                                    null
                                    null
                                null
                                ARROW_FUNC "" @19-19
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "element#3"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  STATIC_CALL
                                    ZVAL "App\\Money"
                                    ZVAL "op_Equality"
                                    ARG_LIST
                                      VAR
                                        ZVAL "element#3"
                                      VAR
                                        ZVAL "value#2"
                                  null
                                  null
                                null
                                null
                              ARG_LIST
                                VAR
                                  ZVAL "price"
                      ZVAL null
                    BINARY_OP [16]
                      METHOD_CALL
                        VAR
                          ZVAL "prices"
                        ZVAL "removeAt"
                        ARG_LIST
                          VAR
                            ZVAL "index#1"
                      ZVAL null
        "#}
    );
}

/// ```php
/// $sets = \is_array($sets) ? \array_map(fn ($element#1) => \is_array($element#1) ? new \Sharp\Set($element#1) : $element#1, $sets) : $sets;
/// $maps = …;
/// $stock = …;
/// $grid[0]->clear();
/// $sets[0]->add(4);
/// $maps[0]->put('c', 3);
/// $maps[0]['d'] ?? $maps[0]->put('d', 4);
/// ((${'index#2'} = \array_find_key($grid[${'index#1'} = $this->next()], …)) !== null)
///     && $grid[${'index#1'}]->removeAt(${'index#2'}) === null;
/// return (($stock['a'] ?? null) === null) ? null : \count($stock['a']);
/// ```
///
/// Spec section 12 makes a `List` index a place, which only the engine's collection of the element reaches, so a
/// change through it is a `Sharp\Collection` method on the index: `clear`, a `Set`'s `add`, and `put` for a write
/// into a `Map` element, whose `??=` puts only for a missing key. An index that runs something is held in a hidden
/// variable, so it runs once. A null-safe method that runs as PHP reads a missing `Map` key as null. Each parameter
/// first makes each `Set` and `Map` plain PHP gave as an array the object.
#[test]
fn a_change_through_a_list_index_runs_a_collection_method_on_the_index() {
    assert_eq!(
        body_in(
            "int? run(List<List<int>> grid, List<Set<int>> sets, List<Map<string, int>> maps, Map<string, List<int>> stock)",
            "        grid[0].clear();\n        sets[0].add(4);\n        maps[0][\"c\"] = 3;\n        maps[0][\"d\"] ??= 4;\n        grid[this.next()].remove(2);\n        return stock[\"a\"]?.count();\n    }\n\n    private int next()\n    {\n        return 0;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "sets"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "sets"
                  CALL
                    ZVAL "array_map"
                    ARG_LIST
                      ARROW_FUNC "" @7-7
                        PARAM_LIST
                          PARAM
                            null
                            ZVAL "element#1"
                            null
                            null
                            null
                            null
                        null
                        CONDITIONAL
                          CALL
                            ZVAL "is_array"
                            ARG_LIST
                              VAR
                                ZVAL "element#1"
                          NEW
                            ZVAL "Sharp\\Set"
                            ARG_LIST
                              VAR
                                ZVAL "element#1"
                          VAR
                            ZVAL "element#1"
                        null
                        null
                      VAR
                        ZVAL "sets"
                  VAR
                    ZVAL "sets"
              ASSIGN
                VAR
                  ZVAL "maps"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "maps"
                  CALL
                    ZVAL "array_map"
                    ARG_LIST
                      ARROW_FUNC "" @7-7
                        PARAM_LIST
                          PARAM
                            null
                            ZVAL "element#1"
                            null
                            null
                            null
                            null
                        null
                        CONDITIONAL
                          CALL
                            ZVAL "is_array"
                            ARG_LIST
                              VAR
                                ZVAL "element#1"
                          NEW
                            ZVAL "Sharp\\Map"
                            ARG_LIST
                              CALL
                                ZVAL "array_merge"
                                ARG_LIST
                                  ARRAY [3]
                                  UNPACK
                                    CALL
                                      ZVAL "array_map"
                                      ARG_LIST
                                        ARROW_FUNC "" @7-7
                                          PARAM_LIST
                                            PARAM
                                              null
                                              ZVAL "key#2"
                                              null
                                              null
                                              null
                                              null
                                            PARAM
                                              null
                                              ZVAL "value#3"
                                              null
                                              null
                                              null
                                              null
                                          null
                                          ARRAY [3]
                                            ARRAY_ELEM
                                              CAST [6]
                                                VAR
                                                  ZVAL "key#2"
                                              null
                                            ARRAY_ELEM
                                              VAR
                                                ZVAL "value#3"
                                              null
                                          null
                                          null
                                        CALL
                                          ZVAL "array_keys"
                                          ARG_LIST
                                            VAR
                                              ZVAL "element#1"
                                        VAR
                                          ZVAL "element#1"
                          VAR
                            ZVAL "element#1"
                        null
                        null
                      VAR
                        ZVAL "maps"
                  VAR
                    ZVAL "maps"
              ASSIGN
                VAR
                  ZVAL "stock"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "stock"
                  NEW
                    ZVAL "Sharp\\Map"
                    ARG_LIST
                      CALL
                        ZVAL "array_merge"
                        ARG_LIST
                          ARRAY [3]
                          UNPACK
                            CALL
                              ZVAL "array_map"
                              ARG_LIST
                                ARROW_FUNC "" @7-7
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "key#1"
                                      null
                                      null
                                      null
                                      null
                                    PARAM
                                      null
                                      ZVAL "value#2"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  ARRAY [3]
                                    ARRAY_ELEM
                                      CAST [6]
                                        VAR
                                          ZVAL "key#1"
                                      null
                                    ARRAY_ELEM
                                      VAR
                                        ZVAL "value#2"
                                      null
                                  null
                                  null
                                CALL
                                  ZVAL "array_keys"
                                  ARG_LIST
                                    VAR
                                      ZVAL "stock"
                                VAR
                                  ZVAL "stock"
                  VAR
                    ZVAL "stock"
              METHOD_CALL
                DIM
                  VAR
                    ZVAL "grid"
                  ZVAL 0
                ZVAL "clear"
                ARG_LIST
              METHOD_CALL
                DIM
                  VAR
                    ZVAL "sets"
                  ZVAL 0
                ZVAL "add"
                ARG_LIST
                  ZVAL 4
              METHOD_CALL
                DIM
                  VAR
                    ZVAL "maps"
                  ZVAL 0
                ZVAL "put"
                ARG_LIST
                  ZVAL "c"
                  ZVAL 3
              COALESCE
                DIM
                  DIM
                    VAR
                      ZVAL "maps"
                    ZVAL 0
                  ZVAL "d"
                METHOD_CALL
                  DIM
                    VAR
                      ZVAL "maps"
                    ZVAL 0
                  ZVAL "put"
                  ARG_LIST
                    ZVAL "d"
                    ZVAL 4
              AND
                BINARY_OP [17]
                  ASSIGN
                    VAR
                      ZVAL "index#2"
                    CALL
                      ZVAL "array_find_key"
                      ARG_LIST
                        DIM
                          VAR
                            ZVAL "grid"
                          ASSIGN
                            VAR
                              ZVAL "index#1"
                            METHOD_CALL
                              VAR
                                ZVAL "this"
                              ZVAL "next"
                              ARG_LIST
                        CALL
                          ARROW_FUNC "" @13-13
                            PARAM_LIST
                              PARAM
                                null
                                ZVAL "value#3"
                                null
                                null
                                null
                                null
                            null
                            ARROW_FUNC "" @13-13
                              PARAM_LIST
                                PARAM
                                  null
                                  ZVAL "element#4"
                                  null
                                  null
                                  null
                                  null
                              null
                              BINARY_OP [16]
                                VAR
                                  ZVAL "element#4"
                                VAR
                                  ZVAL "value#3"
                              null
                              null
                            null
                            null
                          ARG_LIST
                            ZVAL 2
                  ZVAL null
                BINARY_OP [16]
                  METHOD_CALL
                    DIM
                      VAR
                        ZVAL "grid"
                      VAR
                        ZVAL "index#1"
                    ZVAL "removeAt"
                    ARG_LIST
                      VAR
                        ZVAL "index#2"
                  ZVAL null
              RETURN
                CONDITIONAL [1]
                  BINARY_OP [16]
                    COALESCE
                      DIM
                        VAR
                          ZVAL "stock"
                        ZVAL "a"
                      ZVAL null
                    ZVAL null
                  ZVAL null
                  CALL
                    ZVAL "count"
                    ARG_LIST
                      DIM
                        VAR
                          ZVAL "stock"
                        ZVAL "a"
        "#}
    );
}

/// ```php
/// $rows[0]?->add(5);
/// $maybe[1]?->insert(0, 6);
/// $this->held[0]?->add(7);
/// $maybe[0]?->entries();
/// ($maybe[2] === null) ? null : $maybe[2]->clear();
/// return ($maybe[3] === null) ? null : \in_array(4, $maybe[3], true);
/// ```
///
/// Spec section 12 makes a `List` index a place, so a change after `?.` on one runs on the index itself, as a change
/// without `?.` does, and reaches the element the list stores, here a local's, a parameter's and a hooked property's.
/// The index is a bare read on every path, which throws on a missing index, so `?.` skips only a null element: a
/// change, a method that changes nothing, and the test an inlined method runs first.
#[test]
fn a_null_safe_change_through_a_list_index_runs_on_the_index() {
    assert_eq!(
        child_body(
            "bool? run(List<List<int>?> rows, List<List<int>?> maybe)",
            "        rows[0]?.add(5);\n        maybe[1]?.insert(0, 6);\n        this.held[0]?.add(7);\n        maybe[0]?.entries();\n        maybe[2]?.clear();\n        return maybe[3]?.contains(4);\n",
            "<?php namespace App\\Tenant; class Base { /** @var list<list<int>|null> */ public array $held = [[1]] { set { $this->held = $value; } } }"
        ),
        indoc! {r#"
            STMT_LIST
              NULLSAFE_METHOD_CALL
                DIM
                  VAR
                    ZVAL "rows"
                  ZVAL 0
                ZVAL "add"
                ARG_LIST
                  ZVAL 5
              NULLSAFE_METHOD_CALL
                DIM
                  VAR
                    ZVAL "maybe"
                  ZVAL 1
                ZVAL "insert"
                ARG_LIST
                  ZVAL 0
                  ZVAL 6
              NULLSAFE_METHOD_CALL
                DIM
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "held"
                  ZVAL 0
                ZVAL "add"
                ARG_LIST
                  ZVAL 7
              NULLSAFE_METHOD_CALL
                DIM
                  VAR
                    ZVAL "maybe"
                  ZVAL 0
                ZVAL "entries"
                ARG_LIST
              CONDITIONAL [1]
                BINARY_OP [16]
                  DIM
                    VAR
                      ZVAL "maybe"
                    ZVAL 2
                  ZVAL null
                ZVAL null
                METHOD_CALL
                  DIM
                    VAR
                      ZVAL "maybe"
                    ZVAL 2
                  ZVAL "clear"
                  ARG_LIST
              RETURN
                CONDITIONAL [1]
                  BINARY_OP [16]
                    DIM
                      VAR
                        ZVAL "maybe"
                      ZVAL 3
                    ZVAL null
                  ZVAL null
                  CALL
                    ZVAL "in_array"
                    ARG_LIST
                      ZVAL 4
                      DIM
                        VAR
                          ZVAL "maybe"
                        ZVAL 3
                      ZVAL true
        "#}
    );
}

/// ```php
/// $this->sizes->put('a', $extra);
/// $this->sizes['b'] ?? $this->sizes->put('b', 2);
/// (${'receiver#1'} = $this->me())->sizes['c'] ?? ${'receiver#1'}->sizes->put('c', 3);
/// ```
///
/// PHP gives no reference to a hooked property, so a write into the `Map` one holds reaches it, and its `set` hook,
/// only through the engine's collection of the property, as a change through a `List` index reaches the element. It
/// is a `put` on the property, and a `??=` puts only for a missing key, holding an object that runs something in a
/// hidden variable. A property without hooks keeps PHP's write, as `literals_are_short_arrays_and_an_index_is_a_dim`
/// shows.
#[test]
fn a_write_into_a_map_a_hooked_property_holds_puts_it_through_the_property() {
    assert_eq!(
        child_body(
            "void run(int extra)",
            "        this.sizes[\"a\"] = extra;\n        this.sizes[\"b\"] ??= 2;\n        this.me().sizes[\"c\"] ??= 3;\n    }\n\n    private Report me()\n    {\n        return this;\n",
            "<?php namespace App\\Tenant; class Base { /** @var array<string, int> */ public array $sizes = ['a' => 0] { set { $this->sizes = $value; } } }"
        ),
        indoc! {r#"
            STMT_LIST
              METHOD_CALL
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "sizes"
                ZVAL "put"
                ARG_LIST
                  ZVAL "a"
                  VAR
                    ZVAL "extra"
              COALESCE
                DIM
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "sizes"
                  ZVAL "b"
                METHOD_CALL
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "sizes"
                  ZVAL "put"
                  ARG_LIST
                    ZVAL "b"
                    ZVAL 2
              COALESCE
                DIM
                  PROP
                    ASSIGN
                      VAR
                        ZVAL "receiver#1"
                      METHOD_CALL
                        VAR
                          ZVAL "this"
                        ZVAL "me"
                        ARG_LIST
                    ZVAL "sizes"
                  ZVAL "c"
                METHOD_CALL
                  PROP
                    VAR
                      ZVAL "receiver#1"
                    ZVAL "sizes"
                  ZVAL "put"
                  ARG_LIST
                    ZVAL "c"
                    ZVAL 3
        "#}
    );
}

/// ```php
/// $sizes->set(0, $extra);
/// $grid[$extra]->set(1, 2);
/// ```
///
/// Spec section 12: `x[i] = v` on a `List` replaces the element at `i`, and throws `OutOfRangeException` past the end,
/// where PHP's write would append or leave a gap. So it is the engine's `set` on the list, a local or an element of
/// another `List`.
#[test]
fn a_list_index_write_replaces_the_element_through_set() {
    assert_eq!(
        body_in(
            "void run(List<int> sizes, List<List<int>> grid, int extra)",
            "        sizes[0] = extra;\n        grid[extra][1] = 2;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              METHOD_CALL
                VAR
                  ZVAL "sizes"
                ZVAL "set"
                ARG_LIST
                  ZVAL 0
                  VAR
                    ZVAL "extra"
              METHOD_CALL
                DIM
                  VAR
                    ZVAL "grid"
                  VAR
                    ZVAL "extra"
                ZVAL "set"
                ARG_LIST
                  ZVAL 1
                  ZVAL 2
        "#}
    );
}

/// ```php
/// $sizes->set(${'index#1'} = $this->at(), $sizes[${'index#1'}] + $extra);
/// $sizes->set(0, $sizes[0] + 1);
/// $names->set(0, $names[0] . "s");
/// return ($sizes->set(1, (${'old#1'} = $sizes[1]) - 1)) ? ${'old#1'} : ${'old#1'};
/// ```
///
/// `list[i] op= v`, `++list[i]` and `--list[i]` write what the operator gives on the element, through `set`, which
/// gives the value it stores, and `+=` on a `List<string>` element joins the strings, spec section 24. An index that
/// runs something runs once, in a hidden variable. `list[i]++` and `list[i]--` give the element before the write,
/// which a hidden variable holds, as PHP has no expression that runs one and gives another.
#[test]
fn a_compound_write_or_a_step_on_a_list_index_sets_what_the_operator_gives() {
    assert_eq!(
        body_in(
            "int run(List<int> sizes, int extra, List<string> names)",
            "        sizes[this.at()] += extra;\n        ++sizes[0];\n        names[0] += \"s\";\n        return sizes[1]--;\n    }\n\n    private int at()\n    {\n        return 0;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              METHOD_CALL
                VAR
                  ZVAL "sizes"
                ZVAL "set"
                ARG_LIST
                  ASSIGN
                    VAR
                      ZVAL "index#1"
                    METHOD_CALL
                      VAR
                        ZVAL "this"
                      ZVAL "at"
                      ARG_LIST
                  BINARY_OP [1]
                    DIM
                      VAR
                        ZVAL "sizes"
                      VAR
                        ZVAL "index#1"
                    VAR
                      ZVAL "extra"
              METHOD_CALL
                VAR
                  ZVAL "sizes"
                ZVAL "set"
                ARG_LIST
                  ZVAL 0
                  BINARY_OP [1]
                    DIM
                      VAR
                        ZVAL "sizes"
                      ZVAL 0
                    ZVAL 1
              METHOD_CALL
                VAR
                  ZVAL "names"
                ZVAL "set"
                ARG_LIST
                  ZVAL 0
                  BINARY_OP [8]
                    DIM
                      VAR
                        ZVAL "names"
                      ZVAL 0
                    ZVAL "s"
              RETURN
                CONDITIONAL [1]
                  METHOD_CALL
                    VAR
                      ZVAL "sizes"
                    ZVAL "set"
                    ARG_LIST
                      ZVAL 1
                      BINARY_OP [2]
                        ASSIGN
                          VAR
                            ZVAL "old#1"
                          DIM
                            VAR
                              ZVAL "sizes"
                            ZVAL 1
                        ZVAL 1
                  VAR
                    ZVAL "old#1"
                  VAR
                    ZVAL "old#1"
        "#}
    );
}

/// ```php
/// $this->held->set(0, $extra);
/// (${'receiver#1'} = $this->me())->held->set(0, ${'receiver#1'}->held[0] * 2);
/// ```
///
/// PHP gives no reference to a hooked property, so a write into the `List` one holds reaches it, and its `set` hook,
/// only through the engine's collection of the property, as a write into a `Map` there does. An object that runs
/// something runs once, in a hidden variable.
#[test]
fn a_write_into_a_list_a_hooked_property_holds_sets_it_through_the_property() {
    assert_eq!(
        child_body(
            "void run(int extra)",
            "        this.held[0] = extra;\n        this.me().held[0] *= 2;\n    }\n\n    private Report me()\n    {\n        return this;\n",
            "<?php namespace App\\Tenant; class Base { /** @var list<int> */ public array $held = [0] { set { $this->held = $value; } } }"
        ),
        indoc! {r#"
            STMT_LIST
              METHOD_CALL
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "held"
                ZVAL "set"
                ARG_LIST
                  ZVAL 0
                  VAR
                    ZVAL "extra"
              METHOD_CALL
                PROP
                  ASSIGN
                    VAR
                      ZVAL "receiver#1"
                    METHOD_CALL
                      VAR
                        ZVAL "this"
                      ZVAL "me"
                      ARG_LIST
                  ZVAL "held"
                ZVAL "set"
                ARG_LIST
                  ZVAL 0
                  BINARY_OP [3]
                    DIM
                      PROP
                        VAR
                          ZVAL "receiver#1"
                        ZVAL "held"
                      ZVAL 0
                    ZVAL 2
        "#}
    );
}

/// ```php
/// $lines = ($holder === null) ? null : \count($holder->lines);
/// $next = (($nullsafe#1 = $other->next) === null) ? null : \count($nullsafe#1->lines);
/// $made = ($holder === null) ? null : \count($holder->make());
/// $parenthesized = ($holder === null) ? null : \count($holder->lines);
/// ```
///
/// A collection method that runs as a PHP function after a `?.` link tests the link's receiver around the whole
/// chain, as `?->` cannot skip a function call, and reads the link plainly inside it. PHP's `?->` skips the chain
/// past parentheses too, so a parenthesized link is tested the same way.
#[test]
fn a_collection_method_after_a_null_safe_link_tests_the_link_around_the_function() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public void run(Holder? holder, Holder other)\n    {\n        const lines = holder?.lines.count();\n        const next = other.next?.lines.count();\n        const made = holder?.make().count();\n        const parenthesized = (holder?.lines).count();\n    }\n}\n\nclass Holder\n{\n    public List<int> lines { get; private set; } = [];\n    public Holder? next { get; private set; } = null;\n\n    public List<int> make() => this.lines;\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "lines"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    VAR
                      ZVAL "holder"
                    ZVAL null
                  ZVAL null
                  CALL
                    ZVAL "count"
                    ARG_LIST
                      PROP
                        VAR
                          ZVAL "holder"
                        ZVAL "lines"
              ASSIGN
                VAR
                  ZVAL "next"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    ASSIGN
                      VAR
                        ZVAL "nullsafe#1"
                      PROP
                        VAR
                          ZVAL "other"
                        ZVAL "next"
                    ZVAL null
                  ZVAL null
                  CALL
                    ZVAL "count"
                    ARG_LIST
                      PROP
                        VAR
                          ZVAL "nullsafe#1"
                        ZVAL "lines"
              ASSIGN
                VAR
                  ZVAL "made"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    VAR
                      ZVAL "holder"
                    ZVAL null
                  ZVAL null
                  CALL
                    ZVAL "count"
                    ARG_LIST
                      METHOD_CALL
                        VAR
                          ZVAL "holder"
                        ZVAL "make"
                        ARG_LIST
              ASSIGN
                VAR
                  ZVAL "parenthesized"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    VAR
                      ZVAL "holder"
                    ZVAL null
                  ZVAL null
                  CALL
                    ZVAL "count"
                    ARG_LIST
                      PROP
                        VAR
                          ZVAL "holder"
                        ZVAL "lines"
        "#}
    );
}

/// ```php
/// $this->tags->add("vip");
/// $this->next()->tags->remove("old");
/// $this->next()->tags->clear();
/// return $this->names()->contains("vip");
/// ```
///
/// A `Set` method on a property runs on the property's `Set`, which `Sharp\Collection` changes where it lives, and
/// reads the receiver once.

#[test]
fn a_set_method_changes_a_property_through_the_property() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public Set<string> tags { get => field; set => field = value; } = [];\n\n    public bool run()\n    {\n        this.tags.add(\"vip\");\n        this.next().tags.remove(\"old\");\n        this.next().tags.clear();\n        return this.names().contains(\"vip\");\n    }\n\n    private Report next() => this;\n\n    private Set<string> names() => this.tags;\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              METHOD_CALL
                PROP
                  VAR
                    ZVAL "this"
                  ZVAL "tags"
                ZVAL "add"
                ARG_LIST
                  ZVAL "vip"
              METHOD_CALL
                PROP
                  METHOD_CALL
                    VAR
                      ZVAL "this"
                    ZVAL "next"
                    ARG_LIST
                  ZVAL "tags"
                ZVAL "remove"
                ARG_LIST
                  ZVAL "old"
              METHOD_CALL
                PROP
                  METHOD_CALL
                    VAR
                      ZVAL "this"
                    ZVAL "next"
                    ARG_LIST
                  ZVAL "tags"
                ZVAL "clear"
                ARG_LIST
              RETURN
                METHOD_CALL
                  METHOD_CALL
                    VAR
                      ZVAL "this"
                    ZVAL "names"
                    ARG_LIST
                  ZVAL "contains"
                  ARG_LIST
                    ZVAL "vip"
        "#}
    );
}

/// ```php
/// $names = \is_array($names) ? new \Sharp\Set($names) : $names;
/// $names?->add("vip");
/// $other?->tags?->remove("old");
/// return $names?->contains("vip");
/// ```
///
/// A null-safe `Set` method is PHP's null-safe method call on the `Set`, which gives null for a null receiver.

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
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "names"
                  NEW
                    ZVAL "Sharp\\Set"
                    ARG_LIST
                      VAR
                        ZVAL "names"
                  VAR
                    ZVAL "names"
              NULLSAFE_METHOD_CALL
                VAR
                  ZVAL "names"
                ZVAL "add"
                ARG_LIST
                  ZVAL "vip"
              NULLSAFE_METHOD_CALL
                NULLSAFE_PROP
                  VAR
                    ZVAL "other"
                  ZVAL "tags"
                ZVAL "remove"
                ARG_LIST
                  ZVAL "old"
              RETURN
                NULLSAFE_METHOD_CALL
                  VAR
                    ZVAL "names"
                  ZVAL "contains"
                  ARG_LIST
                    ZVAL "vip"
        "#}
    );
}

/// ```php
/// #[\Sharp\Crossing(['set', null])]
/// public readonly \Sharp\Set $tags;
/// public function __construct(\Sharp\Set|array $tags, \Sharp\Set|array $ids)
/// {
///     $this->tags = \is_array($tags) ? new \Sharp\Set($tags) : $tags;
///     $ids = \is_array($ids) ? new \Sharp\Set($ids) : $ids;
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
              SHARP_TYPE_ARGS
                ZVAL "Sharp\\Set"
                ZVAL "Set<string>"
              PROP_DECL
                PROP_ELEM
                  ZVAL "tags"
                  null
                  null
                  null
              ATTRIBUTE_LIST
                ATTRIBUTE_GROUP
                  ATTRIBUTE
                    ZVAL "Sharp\\Crossing"
                    ARG_LIST
                      ARRAY [3]
                        ARRAY_ELEM
                          ZVAL "set"
                          null
                        ARRAY_ELEM
                          ZVAL null
                          null
        "#}]
    );
    assert_eq!(
        lowered.render(lowered.child(lowered.method_named("__construct"), 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                TYPE_UNION
                  ZVAL "Sharp\\Set"
                  TYPE [7]
                ZVAL "tags"
                null
                null
                null
                null
              PARAM
                TYPE_UNION
                  ZVAL "Sharp\\Set"
                  TYPE [7]
                ZVAL "ids"
                null
                null
                null
                null
              SHARP_TYPE_ARGS
                null
                ZVAL "Set<string>, Set<int>"
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
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "tags"
                  NEW
                    ZVAL "Sharp\\Set"
                    ARG_LIST
                      VAR
                        ZVAL "tags"
                  VAR
                    ZVAL "tags"
              ASSIGN
                VAR
                  ZVAL "ids"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "ids"
                  NEW
                    ZVAL "Sharp\\Set"
                    ARG_LIST
                      VAR
                        ZVAL "ids"
                  VAR
                    ZVAL "ids"
        "#}
    );
}

/// ```php
/// $prefix = "v";
/// $check = function (\Sharp\Set|array $tags) use ($prefix) { $tags = \is_array($tags) ? new \Sharp\Set($tags) : $tags; return $tags->contains($prefix); };
/// $size = function (\Sharp\Set|array $tags) { $tags = \is_array($tags) ? new \Sharp\Set($tags) : $tags; return $tags->count(); };
/// return $check(['a']) ? $size(['b']) : 0;
/// ```
///
/// A call of a lambda that declares a parameter `Set` passes it a list literal too, so a lambda makes each `Set` parameter a
/// `Set` object first, as a method does. PHP's `fn` has no statements before its expression, so a lambda with an
/// expression body and a `Set` parameter is a `CLOSURE` whose `use` list captures what the binder says it captures.
#[test]
fn a_lambda_with_a_set_parameter_is_a_closure_that_makes_it_a_set_first() {
    assert_eq!(
        body_in(
            "int run()",
            "        const prefix = \"v\";\n        const check = (Set<string> tags) => tags.contains(prefix);\n        const size = (Set<string> tags) => {\n            return tags.count();\n        };\n        return check([\"a\"]) ? size([\"b\"]) : 0;\n",
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
                      TYPE_UNION
                        ZVAL "Sharp\\Set"
                        TYPE [7]
                      ZVAL "tags"
                      null
                      null
                      null
                      null
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "Set<string>"
                  CLOSURE_USES
                    ZVAL "prefix"
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "tags"
                      CONDITIONAL
                        CALL
                          ZVAL "is_array"
                          ARG_LIST
                            VAR
                              ZVAL "tags"
                        NEW
                          ZVAL "Sharp\\Set"
                          ARG_LIST
                            VAR
                              ZVAL "tags"
                        VAR
                          ZVAL "tags"
                    RETURN
                      METHOD_CALL
                        VAR
                          ZVAL "tags"
                        ZVAL "contains"
                        ARG_LIST
                          VAR
                            ZVAL "prefix"
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "Function<bool(Set<string>)>"
                  null
              ASSIGN
                VAR
                  ZVAL "size"
                CLOSURE "" @11-13
                  PARAM_LIST
                    PARAM
                      TYPE_UNION
                        ZVAL "Sharp\\Set"
                        TYPE [7]
                      ZVAL "tags"
                      null
                      null
                      null
                      null
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "Set<string>"
                  null
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "tags"
                      CONDITIONAL
                        CALL
                          ZVAL "is_array"
                          ARG_LIST
                            VAR
                              ZVAL "tags"
                        NEW
                          ZVAL "Sharp\\Set"
                          ARG_LIST
                            VAR
                              ZVAL "tags"
                        VAR
                          ZVAL "tags"
                    RETURN
                      METHOD_CALL
                        VAR
                          ZVAL "tags"
                        ZVAL "count"
                        ARG_LIST
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "Function<int(Set<string>)>"
                  null
              RETURN
                CONDITIONAL
                  CALL
                    VAR
                      ZVAL "check"
                    ARG_LIST
                      ARRAY [3]
                        ARRAY_ELEM
                          ZVAL "a"
                          null
                  CALL
                    VAR
                      ZVAL "size"
                    ARG_LIST
                      ARRAY [3]
                        ARRAY_ELEM
                          ZVAL "b"
                          null
                  ZVAL 0
        "#}
    );
}

/// `App\Tenant\Order`, a plain PHP class whose rows a lazy chain reads.
const ORDER: (&str, &str) = (
    "src/App/Tenant/Order.php",
    "<?php\n\nnamespace App\\Tenant;\n\nfinal class Order\n{\n    public function __construct(public bool $paid, public string $number)\n    {\n    }\n}\n",
);

/// ```php
/// return \Sharp\Sequence::from($orders)->filter(fn ($o) => $o->paid)->map(fn ($o) => $o->number)->take(100)->toList();
/// ```
///
/// A call of a lazy operation of `Iterable<T>`, spec section 12, runs the method `Sharp\IterableMethods` declares on
/// the engine's `Sharp\Sequence`. A chain reads its receiver through one `\Sharp\Sequence::from`, and each later
/// operation calls the `Sequence` the one before it gave.
#[test]
fn a_lazy_chain_is_one_sequence_from_its_receiver_and_each_operation_on_it() {
    assert_eq!(
        body_in(
            "List<string> run(Iterable<Order> orders)",
            "        return orders.filter(o => o.paid).map(o => o.number).take(100).toList();\n",
            &[ORDER]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                METHOD_CALL
                  METHOD_CALL
                    METHOD_CALL
                      METHOD_CALL
                        STATIC_CALL
                          ZVAL "Sharp\\Sequence"
                          ZVAL "from"
                          ARG_LIST
                            VAR
                              ZVAL "orders"
                        ZVAL "filter"
                        ARG_LIST
                          ARROW_FUNC "" @9-9
                            PARAM_LIST
                              PARAM
                                null
                                ZVAL "o"
                                null
                                null
                                null
                                null
                            null
                            PROP
                              VAR
                                ZVAL "o"
                              ZVAL "paid"
                            SHARP_TYPE_ARGS
                              null
                              ZVAL "Function<bool(App.Tenant.Order)>"
                            null
                      ZVAL "map"
                      ARG_LIST
                        ARROW_FUNC "" @9-9
                          PARAM_LIST
                            PARAM
                              null
                              ZVAL "o"
                              null
                              null
                              null
                              null
                          null
                          PROP
                            VAR
                              ZVAL "o"
                            ZVAL "number"
                          SHARP_TYPE_ARGS
                            null
                            ZVAL "Function<string(App.Tenant.Order)>"
                          null
                    ZVAL "take"
                    ARG_LIST
                      ZVAL 100
                  ZVAL "toList"
                  ARG_LIST
        "#}
    );
}

/// ```php
/// $held = $sizes;
/// $held = $sizes;
/// $small = \Sharp\Sequence::from($held)->filter(fn ($n) => $n < 10);
/// return \Sharp\Sequence::from($small)->take(2)->toList();
/// ```
///
/// A `List` held as an `Iterable<T>` has the lazy operations, whatever was assigned to its place last, so its call
/// reads it through `from` too. A local that holds a chain's `Sequence` is read through `from` again, which gives that
/// `Sequence` back.
#[test]
fn a_list_held_as_an_iterable_and_a_local_holding_a_chain_are_read_through_from() {
    assert_eq!(
        body_in(
            "List<int> run(List<int> sizes)",
            "        Iterable<int> held = sizes;\n        held = sizes;\n        Iterable<int> small = held.filter(n => n < 10);\n        return small.take(2).toList();\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "held"
                VAR
                  ZVAL "sizes"
              ASSIGN
                VAR
                  ZVAL "held"
                VAR
                  ZVAL "sizes"
              ASSIGN
                VAR
                  ZVAL "small"
                METHOD_CALL
                  STATIC_CALL
                    ZVAL "Sharp\\Sequence"
                    ZVAL "from"
                    ARG_LIST
                      VAR
                        ZVAL "held"
                  ZVAL "filter"
                  ARG_LIST
                    ARROW_FUNC "" @11-11
                      PARAM_LIST
                        PARAM
                          null
                          ZVAL "n"
                          null
                          null
                          null
                          null
                      null
                      BINARY_OP [20]
                        VAR
                          ZVAL "n"
                        ZVAL 10
                      SHARP_TYPE_ARGS
                        null
                        ZVAL "Function<bool(int)>"
                      null
              RETURN
                METHOD_CALL
                  METHOD_CALL
                    STATIC_CALL
                      ZVAL "Sharp\\Sequence"
                      ZVAL "from"
                      ARG_LIST
                        VAR
                          ZVAL "small"
                    ZVAL "take"
                    ARG_LIST
                      ZVAL 2
                  ZVAL "toList"
                  ARG_LIST
        "#}
    );
}

/// ```php
/// return ($maybe === null ? null : \Sharp\Sequence::from($maybe)->filter(fn ($o) => $o->paid));
/// ```
///
/// PHP's `?->` cannot read a receiver through `from`, so a null-safe lazy operation is the conditional a null-safe
/// property call is, over the `Sequence` call. php-src writes each `null` as a `CONST`, which compiles to the same
/// value as the `ZVAL` the bridge writes.
#[test]
fn a_null_safe_lazy_operation_is_a_conditional_over_the_sequence_call() {
    assert_eq!(
        body_in(
            "Iterable<Order>? run(Iterable<Order>? maybe)",
            "        return maybe?.filter(o => o.paid);\n",
            &[ORDER]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                CONDITIONAL [1]
                  BINARY_OP [16]
                    VAR
                      ZVAL "maybe"
                    ZVAL null
                  ZVAL null
                  METHOD_CALL
                    STATIC_CALL
                      ZVAL "Sharp\\Sequence"
                      ZVAL "from"
                      ARG_LIST
                        VAR
                          ZVAL "maybe"
                    ZVAL "filter"
                    ARG_LIST
                      ARROW_FUNC "" @9-9
                        PARAM_LIST
                          PARAM
                            null
                            ZVAL "o"
                            null
                            null
                            null
                            null
                        null
                        PROP
                          VAR
                            ZVAL "o"
                          ZVAL "paid"
                        SHARP_TYPE_ARGS
                          null
                          ZVAL "Function<bool(App.Tenant.Order)>"
                        null
        "#}
    );
}

/// ```php
/// public function many(\Sharp\Set|array ...$groups): int
/// {
///     $groups = \array_map(fn ($collection#1) => \is_array($collection#1) ? new \Sharp\Set($collection#1) : $collection#1, $groups);
///     return \count($groups);
/// }
/// public static function op_Addition(\App\Tenant\Order $order, \Sharp\Set|array $tags): \App\Tenant\Order
/// {
///     $tags = \is_array($tags) ? new \Sharp\Set($tags) : $tags;
///     return $order;
/// }
/// ```
///
/// A variadic `Set` parameter takes a list or a `Set` for each of its elements, so each list becomes a `Set`, under
/// its position or its name, and a null element of a nullable one stays null. An operator is a method, and makes each
/// `Set` parameter a `Set` first too.

#[test]
fn a_variadic_or_operator_set_parameter_becomes_a_set_before_the_body_runs() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Order\n{\n    public int many(Set<string> ...groups) => global.count(groups);\n\n    public static readonly Order operator +(Order order, Set<string> tags) => order;\n\n    public int maybe(Set<string>? ...groups) => global.count(groups);\n}\n",
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
                    ARROW_FUNC "" @5-5
                      PARAM_LIST
                        PARAM
                          null
                          ZVAL "collection#1"
                          null
                          null
                          null
                          null
                      null
                      CONDITIONAL
                        CALL
                          ZVAL "is_array"
                          ARG_LIST
                            VAR
                              ZVAL "collection#1"
                        NEW
                          ZVAL "Sharp\\Set"
                          ARG_LIST
                            VAR
                              ZVAL "collection#1"
                        VAR
                          ZVAL "collection#1"
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
    assert_eq!(
        body("op_Addition"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "tags"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "tags"
                  NEW
                    ZVAL "Sharp\\Set"
                    ARG_LIST
                      VAR
                        ZVAL "tags"
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
                          ZVAL "collection#1"
                          null
                          null
                          null
                          null
                      null
                      CONDITIONAL
                        CALL
                          ZVAL "is_array"
                          ARG_LIST
                            VAR
                              ZVAL "collection#1"
                        NEW
                          ZVAL "Sharp\\Set"
                          ARG_LIST
                            VAR
                              ZVAL "collection#1"
                        VAR
                          ZVAL "collection#1"
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
              SHARP_TYPE_ARGS
                TYPE [7]
                ZVAL "List<string>"
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
            SHARP_TYPE_ARGS
              ZVAL "Closure"
              ZVAL "Function<Function<int(string, int)>(Function<int(string, int)>, Function<void()>?)>"
        "#}
    );
}

/// `App\Shop\Cart` and `App\Shop\Charge`, the parameter and return types of the function types below.
const SHOP: (&str, &str) =
    ("src/App/Shop/Shop.php", "<?php namespace App\\Shop; final class Cart {} final class Charge {}");
/// The type and the declaration of the one property `code` declares, rendered, after the checker accepts `code`.
fn lowered_field(code: &str) -> (String, String) {
    let lowered = Lowered::with(code, &[SHOP]);
    assert_eq!(lowered.diagnostics(), Vec::<String>::new(), "the source lowers");
    let group =
        lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_PROP_GROUP).expect("a property");

    (lowered.render(lowered.child(group as u32, 0)), lowered.render(lowered.child(group as u32, 1)))
}

/// ```php
/// private \Closure $charge;
/// ```
///
/// A function type's effects are the checker's alone, so a function type with effects runs as PHP's `\Closure`.
#[test]
fn a_function_type_with_effects_is_the_closure_class() {
    let (hint, declaration) = lowered_field(
        "namespace App.Shop;\n\nclass Checkout\n{\n    Function<Charge(Cart) uses Http> charge;\n\n    public Checkout(Function<Charge(Cart) uses Http> charge)\n    {\n        this.charge = charge;\n    }\n}\n",
    );

    assert_eq!(hint, "ZVAL \"Closure\"\n");
    assert_eq!(declaration, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"charge\"\n    null\n    null\n    null\n");
}

/// ```php
/// private ?\Closure $fallback = null;
/// ```
///
/// `[256]` is `ZEND_TYPE_NULLABLE`.
#[test]
fn a_nullable_function_type_with_effects_is_the_nullable_closure_class() {
    let (hint, declaration) = lowered_field(
        "namespace App.Shop;\n\nclass Checkout\n{\n    Function<Charge(Cart) uses Http>? fallback = null;\n\n    public Function<Charge(Cart) uses Http>? current() => this.fallback;\n}\n",
    );

    assert_eq!(hint, "ZVAL [256] \"Closure\"\n");
    assert_eq!(declaration, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"fallback\"\n    ZVAL null\n    null\n    null\n");
}

/// ```php
/// private \Sharp\Map $handlers;
/// ```
///
/// A `Map` is its class, whatever its value type, under the type text a write from plain PHP is checked against, whose
/// function type lists its effects by their full names, sorted.
#[test]
fn a_map_of_function_types_with_two_effects_is_the_map_class() {
    let (hint, declaration) = lowered_field(
        "namespace App.Shop;\n\nclass Checkout\n{\n    Map<string, Function<Charge(Cart) uses Mail, Http>> handlers;\n\n    public Checkout(Map<string, Function<Charge(Cart) uses Mail, Http>> handlers)\n    {\n        this.handlers = handlers;\n    }\n}\n",
    );

    assert_eq!(
        hint,
        indoc! {r#"
            SHARP_TYPE_ARGS
              ZVAL "Sharp\\Map"
              ZVAL "Map<string, Function<App.Shop.Charge(App.Shop.Cart) uses Sharp.Http, Sharp.Mail>>"
        "#}
    );
    assert_eq!(declaration, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"handlers\"\n    null\n    null\n    null\n");
}

/// ```php
/// private \Closure $apply;
/// ```
#[test]
fn a_function_type_taking_a_function_type_with_effects_is_the_closure_class() {
    let (hint, declaration) = lowered_field(
        "namespace App.Shop;\n\nclass Checkout\n{\n    Function<int(Function<int(int) uses Http>)> apply;\n\n    public Checkout(Function<int(Function<int(int) uses Http>)> apply)\n    {\n        this.apply = apply;\n    }\n}\n",
    );

    assert_eq!(hint, "ZVAL \"Closure\"\n");
    assert_eq!(declaration, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"apply\"\n    null\n    null\n    null\n");
}

/// ```php
/// /** @template TItem of \Lib\DatabaseEntity */
/// class PaginatedList
/// {
///     /** @var list<TItem> */
///     private array $rows;
///
///     public function __construct(array $rows)
///     {
///         $this->rows = $rows;
///     }
///
///     public function first(): \Lib\DatabaseEntity { return $this->rows[0]; }
///
///     public function find(int $id): ?\Lib\DatabaseEntity { return null; }
/// }
/// ```
///
/// PHP types are erased, so a type parameter is the type of its bound, its list lowers to nothing, and a `List` of it
/// is still `array`, as the PHP twin a developer writes with `@template` declares it. A call or a write from plain PHP
/// checks each element of `rows` against the object's own type argument, `List<$0>`. The class's last member is the
/// `SHARP_TYPE_ARGS` that declares its hidden type-argument slot, whose text is its bounds.
#[test]
fn a_type_parameter_is_its_bound_and_its_list_lowers_to_nothing() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App.Tenant;

        import Lib.DatabaseEntity;

        public class PaginatedList<TItem : DatabaseEntity>
        {
            private List<TItem> rows;

            public PaginatedList(List<TItem> rows)
            {
                this.rows = rows;
            }

            public TItem first() => this.rows[0];

            public TItem? find(int id) { return null; }
        }
    "},
        &[("src/Lib/DatabaseEntity.php", "<?php namespace Lib; abstract class DatabaseEntity {}")],
    );
    let tree = lowered.tree();

    assert_eq!(
        lowered.render(lowered.child(lowered.root(), 2)),
        indoc! {r#"
            CLASS "PaginatedList" @5-17
              null
              null
              STMT_LIST
                PROP_GROUP [32772]
                  SHARP_TYPE_ARGS
                    TYPE [7]
                    ZVAL "List<$0>"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "rows"
                      null
                      null
                      null
                  null
                METHOD [1] "__construct" @9-12
                  PARAM_LIST
                    PARAM
                      TYPE [7]
                      ZVAL "rows"
                      null
                      null
                      null
                      null
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "List<$0>"
                  null
                  STMT_LIST
                    ASSIGN
                      PROP
                        VAR
                          ZVAL "this"
                        ZVAL "rows"
                      VAR
                        ZVAL "rows"
                  null
                  null
                METHOD [1] "first" @14-14
                  PARAM_LIST
                  null
                  STMT_LIST
                    RETURN
                      DIM
                        PROP
                          VAR
                            ZVAL "this"
                          ZVAL "rows"
                        ZVAL 0
                  SHARP_TYPE_ARGS
                    ZVAL "Lib\\DatabaseEntity"
                    ZVAL "Function<$0()>"
                  null
                METHOD [1] "find" @16-16
                  PARAM_LIST
                    PARAM
                      ZVAL [1] "int"
                      ZVAL "id"
                      null
                      null
                      null
                      null
                  null
                  STMT_LIST
                    RETURN
                      ZVAL null
                  SHARP_TYPE_ARGS
                    ZVAL [256] "Lib\\DatabaseEntity"
                    ZVAL "Function<$0?(int)>"
                  null
                SHARP_CLASS_METADATA
                  null
                  ZVAL "Lib.DatabaseEntity"
                  null
              null
              null
        "#}
    );
    assert!(!tree.contains("TItem"), "{tree}");
}

/// ```php
/// /** @template T */
/// public function first(array $items): mixed { return $items[0]; }
/// /** @template T */
/// public function maybe(): mixed { return null; }
/// /** @template T */
/// public function pick(mixed $a): mixed { return $a; }
/// ```
///
/// A type parameter without a bound is `mixed`. PHP refuses `?mixed` and `mixed` in a union, and `mixed` already holds
/// null and every other type, so `T?` and a union that holds `T` are `mixed` too.
#[test]
fn a_type_parameter_without_a_bound_is_mixed_alone_nullable_or_in_a_union() {
    let lowered = Lowered::new(indoc! {"
        class Report
        {
            public T first<T>(List<T> items) => items[0];

            public T? maybe<T>() { return null; }

            public T|int pick<T>(T a) { return a; }
        }
    "});
    let members = lowered.child(lowered.child(lowered.root(), 1), 2);
    // The PHP return type is the first child of the `SHARP_TYPE_ARGS` that holds the method's function type.
    let signature = |index: u32| {
        let method = lowered.child(members, index);

        format!(
            "{}{}",
            lowered.render(lowered.child(method, 0)),
            lowered.render(lowered.child(lowered.child(method, 3), 0))
        )
    };

    assert_eq!(
        signature(0),
        indoc! {r#"
            PARAM_LIST
              PARAM
                TYPE [7]
                ZVAL "items"
                null
                null
                null
                null
              SHARP_TYPE_ARGS
                ZVAL "Any?"
                null
            ZVAL [1] "mixed"
        "#}
    );
    assert_eq!(
        signature(1),
        indoc! {r#"
            PARAM_LIST
              SHARP_TYPE_ARGS
                ZVAL "Any?"
                null
            ZVAL [1] "mixed"
        "#}
    );
    assert_eq!(
        signature(2),
        indoc! {r##"
            PARAM_LIST
              PARAM
                ZVAL [1] "mixed"
                ZVAL "a"
                null
                null
                null
                null
              SHARP_TYPE_ARGS
                ZVAL "Any?"
                null
            ZVAL [1] "mixed"
        "##}
    );
}

/// ```php
/// /** @template T of object */
/// public function keep(object $value): object { return $value; }
/// ```
///
/// A type parameter bounded by `Object` is PHP's `object`, and a call from plain PHP gives it `Object`.
#[test]
fn a_type_parameter_bounded_by_object_is_php_object() {
    let lowered = Lowered::new(indoc! {"
        class Report
        {
            public T keep<T : Object>(T value) => value;
        }
    "});
    let method = lowered.child(lowered.child(lowered.child(lowered.root(), 1), 2), 0);

    assert_eq!(
        format!("{}{}", lowered.render(lowered.child(method, 0)), lowered.render(lowered.child(method, 3))),
        indoc! {r#"
            PARAM_LIST
              PARAM
                ZVAL [1] "Object"
                ZVAL "value"
                null
                null
                null
                null
              SHARP_TYPE_ARGS
                ZVAL "Object"
                null
            SHARP_TYPE_ARGS
              ZVAL [1] "Object"
              ZVAL "Function<#0(#0)>"
        "#}
    );
}

/// ```php
/// /** @template TItem of \Lib\DatabaseEntity&\Lib\Shareable */
/// public function share(\Lib\DatabaseEntity&\Lib\Shareable $item): (\Lib\DatabaseEntity&\Lib\Shareable)|null
/// ```
///
/// A type parameter bound by several classes is their `TYPE_INTERSECTION`, as php-src's `intersection_type` rule
/// builds `A&B`. PHP refuses `?` before an intersection, so a nullable one is the DNF type `(A&B)|null`: a
/// `TYPE_UNION` of the intersection and `null`.
#[test]
fn a_type_parameter_bound_by_several_classes_is_their_intersection() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App.Tenant;

        import Lib.DatabaseEntity;
        import Lib.Shareable;

        class Report<TItem : DatabaseEntity & Shareable>
        {
            public TItem? share(TItem item) { return null; }
        }
    "},
        &[(
            "src/Lib/DatabaseEntity.php",
            "<?php namespace Lib; abstract class DatabaseEntity {} interface Shareable {}",
        )],
    );
    let method = lowered.child(lowered.child(lowered.child(lowered.root(), 2), 2), 0);

    assert_eq!(
        lowered.render(lowered.child(method, 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                TYPE_INTERSECTION
                  ZVAL "Lib\\DatabaseEntity"
                  ZVAL "Lib\\Shareable"
                ZVAL "item"
                null
                null
                null
                null
              SHARP_TYPE_ARGS
                null
                ZVAL "$0"
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(method, 3)),
        indoc! {r#"
            SHARP_TYPE_ARGS
              TYPE_UNION
                TYPE_INTERSECTION
                  ZVAL "Lib\\DatabaseEntity"
                  ZVAL "Lib\\Shareable"
                ZVAL [1] "null"
              ZVAL "Function<$0?($0)>"
        "#}
    );
}

/// ```php
/// /** @template TItem of \Lib\Order */
/// public function keep(\Lib\Order $order, ?\Lib\Order $other): string { return 'kept'; }
/// /** @template TShared of \Lib\Order&\Lib\Shareable */
/// public function share(\Lib\Order $order, string $type): string { return 'shared'; }
/// ```
///
/// PHP refuses a union that names a type twice, or names an intersection beside one of its classes. So members that
/// erase to one type appear once, and a member another member already holds is left out: `TItem|Order` is `Order`,
/// `(TShared|Order)` is `Order`, and `Class<Order>|string` is `string`.
#[test]
fn union_members_that_erase_to_one_type_appear_once() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App.Tenant;

        import Lib.Order;
        import Lib.Shareable;

        class Report<TItem : Order>
        {
            public string keep(TItem|Order order, (Order|TItem)? other) { return \"kept\"; }

            public string share<TShared : Order & Shareable>(TShared|Order order, Class<Order>|string type)
            {
                return \"shared\";
            }
        }
    "},
        &[("src/Lib/Order.php", "<?php namespace Lib; class Order {} interface Shareable {}")],
    );
    let members = lowered.child(lowered.child(lowered.root(), 2), 2);
    let parameter_types = |index: u32| {
        let parameters = lowered.child(lowered.child(members, index), 0);

        format!(
            "{}{}",
            lowered.render(lowered.child(lowered.child(parameters, 0), 0)),
            lowered.render(lowered.child(lowered.child(parameters, 1), 0))
        )
    };

    assert_eq!(
        parameter_types(0),
        indoc! {r#"
            ZVAL "Lib\\Order"
            ZVAL [256] "Lib\\Order"
        "#}
    );
    assert_eq!(
        parameter_types(1),
        indoc! {r#"
            ZVAL "Lib\\Order"
            ZVAL [1] "string"
        "#}
    );
}

/// ```php
/// /** @var array<string, class-string<\Lib\Element>> */
/// private \Sharp\Map $elements = new \Sharp\Map();
///
/// /** @param class-string<\Lib\Model> $type */
/// public function load(string $type, ?string $other = null): ?string { return $type; }
///
/// /** @param \Lib\PaginatedList<\Lib\Order> $rows */
/// public function page(\Lib\PaginatedList $rows): ?\Lib\PaginatedList { return $rows; }
/// ```
///
/// A generic class type is its class by its full name. Plain PHP receives a class value as its class-name string,
/// spec section 25, so `Class<T>` is `string`, and a `Map` of class values is still its class.
#[test]
fn a_generic_class_type_is_its_class_and_a_class_value_type_is_string() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App.Tenant;

        import Lib.Element;
        import Lib.Model;
        import Lib.Order;
        import Lib.PaginatedList;

        class Report
        {
            private Map<string, Class<Element>> elements = [:];

            public Class<Model>? load(Class<Model> type, Class<Model>? other = null) { return other; }

            public PaginatedList<Order>? page(PaginatedList<Order> rows) { return global.count(this.elements) > 0 ? rows : null; }
        }
    "},
        &[(
            "src/Lib/Model.php",
            "<?php namespace Lib; class Element {} class Model {} class Order {} /** @template T */ class PaginatedList {}",
        )],
    );
    let members = lowered.child(lowered.child(lowered.root(), 2), 2);
    let load = lowered.child(members, 1);
    let page = lowered.child(members, 2);

    assert_eq!(
        lowered.render(lowered.child(lowered.child(members, 0), 0)),
        indoc! {r#"
            SHARP_TYPE_ARGS
              ZVAL "Sharp\\Map"
              ZVAL "Map<string, Class<Lib.Element>>"
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(load, 0)),
        indoc! {r#"
            PARAM_LIST
              PARAM
                ZVAL [1] "string"
                ZVAL "type"
                null
                null
                null
                null
              PARAM
                ZVAL [257] "string"
                ZVAL "other"
                ZVAL null
                null
                null
                null
        "#}
    );
    assert_eq!(
        lowered.render(lowered.child(load, 3)),
        indoc! {r#"
            SHARP_TYPE_ARGS
              ZVAL [257] "string"
              ZVAL "Function<Class<Lib.Model>?(Class<Lib.Model>, Class<Lib.Model>?)>"
        "#}
    );
    assert_eq!(
        format!("{}{}", lowered.render(lowered.child(page, 0)), lowered.render(lowered.child(page, 3))),
        indoc! {r#"
            PARAM_LIST
              PARAM
                ZVAL "Lib\\PaginatedList"
                ZVAL "rows"
                null
                null
                null
                null
            SHARP_TYPE_ARGS
              ZVAL [256] "Lib\\PaginatedList"
              ZVAL "Lib.PaginatedList<Lib.Order>?"
        "#}
    );
}

/// ```php
/// $numbers = [1, $extra];
/// $named = new \Sharp\Map(['a', 1, 2, $numbers[0]]);
/// $empty = new \Sharp\Map();
/// $named['b'] = $numbers[0];
/// $this->sizes['a'] = ($this->sizes['a'] ?? 0) + 1;
/// return $numbers[1];
/// ```
///
/// A list literal is an `ARRAY` with `ZEND_ARRAY_SYNTAX_SHORT`, which is 3, of `ARRAY_ELEM`s. A map literal is a new
/// `Map` of each key followed by its value. An index is a `DIM` of the value and the key, read or written as its place
/// in the tree decides.

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
                NEW
                  ZVAL "Sharp\\Map"
                  ARG_LIST
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "a"
                        null
                      ARRAY_ELEM
                        ZVAL 1
                        null
                      ARRAY_ELEM
                        ZVAL 2
                        null
                      ARRAY_ELEM
                        DIM
                          VAR
                            ZVAL "numbers"
                          ZVAL 0
                        null
              ASSIGN
                VAR
                  ZVAL "empty"
                NEW
                  ZVAL "Sharp\\Map"
                  ARG_LIST
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
            SHARP_TYPE_ARGS
              ZVAL [1] "mixed"
              ZVAL "Function<Any(Any, Any?)>"
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
        body("        string? found = null;\n        const int base = 2;\n        return base;\n"),
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
    // The PHP return type, inside the `SHARP_TYPE_ARGS` that holds the method's function type.
    let return_type = lowered.child(lowered.child(method, 3), 0);

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
    // The PHP return type, inside the `SHARP_TYPE_ARGS` that holds the method's function type.
    let return_type = lowered.child(lowered.child(method, 3), 0);

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
            "        const pick = (int|string key, (int|Calc)? fallback, int ...rest) => global.count(rest);\n        return pick(1, null, ...extra);\n",
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
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "int"
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
/// $all = [...$extra, 1, ...<check List<int>>(\Lib\Calc::make())];
/// ```
///
/// A spread in a literal is an `UNPACK` of its value among the literal's elements, as php-src's grammar builds
/// `[...$extra]`. The list a plain PHP docblock claims is checked element by element first.
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
                    VERIFY_TYPE_ARGS
                      STATIC_CALL
                        ZVAL "Lib\\Calc"
                        ZVAL "make"
                        ARG_LIST
                      ZVAL "List<int>"
                      ZVAL "Lib\\Calc::make()"
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
            "        Calc.sum(...extra);\n        this.run(1, ...extra);\n        const made = new Calc(...extra);\n        return global.max(first, ...extra);\n",
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

/// ```php
/// $page = new \Lib\PaginatedList($rows);
/// $payload = \Lib\Json::decode($body);
/// $found = $this->repository?->find($id);
/// $type = \Lib\PaginatedList::class;
/// ```
///
/// A plain PHP class's `@template` type arguments are erased, so those of its `new` and its `typeof` lower to nothing.
/// A call of its generic method, static or null-safe, carries them, since a PHP# method that overrides or implements it
/// reads them.
#[test]
fn type_arguments_of_a_plain_php_new_lower_to_nothing_and_of_a_call_to_its_type_arguments() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App.Tenant;

        import Lib.Json;
        import Lib.Order;
        import Lib.PaginatedList;
        import Lib.Repository;
        import Lib.WebhookPayload;

        class Report
        {
            private Repository? repository = null;

            public void run(List<Order> rows, string body, int id)
            {
                const page = new PaginatedList<Order>(rows);
                const payload = Json.decode<WebhookPayload>(body);
                const found = this.repository?.find<Order>(id);
                const type = typeof(PaginatedList<Order>);
            }
        }
    "},
        &[(
            "src/Lib/Json.php",
            "<?php namespace Lib; class Order {} class WebhookPayload {} /** @template T */ class PaginatedList { /** @param list<T> $rows */ public function __construct(array $rows) {} } final class Json { /** @template T @return T */ public static function decode(string $body): mixed { return null; } } interface Repository { /** @template T @return T|null */ public function find(int $id): mixed; }",
        )],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "page"
                NEW
                  ZVAL "Lib\\PaginatedList"
                  ARG_LIST
                    VAR
                      ZVAL "rows"
              ASSIGN
                VAR
                  ZVAL "payload"
                STATIC_CALL
                  ZVAL "Lib\\Json"
                  ZVAL "decode"
                  ARG_LIST
                    VAR
                      ZVAL "body"
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "Lib.WebhookPayload"
              ASSIGN
                VAR
                  ZVAL "found"
                NULLSAFE_METHOD_CALL
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "repository"
                  ZVAL "find"
                  ARG_LIST
                    VAR
                      ZVAL "id"
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "Lib.Order"
              ASSIGN
                VAR
                  ZVAL "type"
                CLASS_NAME
                  ZVAL "Lib\\PaginatedList"
        "#}
    );
}

/// A class-like's metadata holds, first, the type arguments its header gives each generic parent and interface, as a
/// type text list of those classes sorted by their text, each of its own type parameters written as `$` and its index.
/// Its second text is its bounds, an interface's as a class's, so the engine knows how many type arguments each takes
/// and which class a type argument may name. Its third is the variance of its type parameters, `+` for `out`, `-` for
/// `in` and `=` for neither, when one of them has `out` or `in`, which the engine reads where it checks a value against
/// a generic type.
#[test]
fn a_class_like_metadata_holds_its_header_type_arguments_its_bounds_and_its_variance() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App;

        public interface Query<TItem : DatabaseEntity>
        {
        }

        public class Base<T>
        {
        }

        public class Sub<T> : Base<List<T>>, Query<Order>
        {
        }

        public class OrderPage : Base<Order>
        {
        }

        public interface Feed<out TItem, in TKey, TValue>
        {
        }
    "},
        &[(
            "src/App/Entities.php",
            "<?php namespace App; abstract class DatabaseEntity {} final class Order extends DatabaseEntity {}",
        )],
    );
    let metadata = |class: u32| {
        let members = lowered.child(lowered.child(lowered.root(), class), 2);

        lowered.render(lowered.child(members, lowered.nodes()[members as usize].child_count - 1))
    };

    assert_eq!(
        [metadata(2), metadata(3), metadata(4), metadata(5), metadata(6)],
        [
            indoc! {r#"
                SHARP_CLASS_METADATA
                  null
                  ZVAL "App.DatabaseEntity"
                  null
            "#},
            indoc! {r#"
                SHARP_CLASS_METADATA
                  null
                  ZVAL "Any?"
                  null
            "#},
            indoc! {r#"
                SHARP_CLASS_METADATA
                  ZVAL "App.Base<List<$0>>, App.Query<App.Order>"
                  ZVAL "Any?"
                  null
            "#},
            indoc! {r#"
                SHARP_CLASS_METADATA
                  ZVAL "App.Base<App.Order>"
                  null
                  null
            "#},
            indoc! {r#"
                SHARP_CLASS_METADATA
                  null
                  ZVAL "Any?, Any?, Any?"
                  ZVAL "+-="
            "#},
        ]
    );
}

/// A bound that names a type parameter of its own class writes it as `Any?`: the bounds are the type arguments of an
/// object plain PHP creates, which has no type argument to give it, and a text that names one would be open.
#[test]
fn a_bound_writes_a_type_parameter_of_its_own_class_as_any() {
    let lowered = Lowered::with(
        indoc! {"
        namespace Demo;

        public interface Comparable<T>
        {
        }

        public class Sorted<TItem : Comparable<TItem>>
        {
        }

        public class Ranked<TKey, TItem : Comparable<TKey>>
        {
        }
    "},
        &[],
    );
    let bounds = |class: u32| {
        let members = lowered.child(lowered.child(lowered.root(), class), 2);
        let metadata = lowered.child(members, lowered.nodes()[members as usize].child_count - 1);

        lowered.render(lowered.child(metadata, 1))
    };

    assert_eq!([bounds(3), bounds(4)], ["ZVAL \"Demo.Comparable<Any?>\"\n", "ZVAL \"Any?, Demo.Comparable<Any?>\"\n"]);
}

/// A call of a generic method carries the type arguments the checker found, written or inferred, as the last child of
/// its argument list: a `SHARP_TYPE_ARGS` without a `new`. A type parameter of the class the call is in is `$` and its
/// index, and one of the method the call is in is `#` and its index, which the engine reads from the method's own
/// call. A lambda writes them the same way, and the engine reads them from the method's call the lambda captured them
/// from: a lambda whose body, or a lambda in it, names a `#` is a `CLOSURE` whose `use` list ends with the method's
/// hidden local, by value. A method whose call needs its type arguments, or whose parameter names a PHP# class with type
/// arguments, a class value of one or of a type that is no class, or a type parameter, ends its parameter list with its metadata: a `SHARP_TYPE_ARGS` of the bounds of its
/// own type parameters and a type text list with each parameter's type, `Any?` for one the engine never checks. Plain
/// PHP's generics are erased, so a parameter of a plain PHP class with type arguments is never checked, and a call from
/// plain PHP gives a method's own type parameters none, so a parameter of one is never checked either.
#[test]
fn a_generic_call_carries_its_type_arguments_and_a_generic_method_its_metadata() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App;

        public class Box<TValue>
        {
            public Box() { }
        }

        public class Pair<TFirst, TSecond>
        {
            public Pair() { }
        }

        public class Repository
        {
            public static List<T> repeat<T>(T value, int times) => [value];

            public List<Order> run(Order order) => Repository.repeat<Order>(order, 3);

            public List<Order> inferred(Order order) => Repository.repeat(order, 2);

            public List<T> forward<T : DatabaseEntity>(T item) => Repository.repeat<T>(item, 1);

            public Box<T> box<T : DatabaseEntity>() => new Box<T>();

            public Function<Box<T>()> later<T>() => () => new Box<T>();

            public Function<Function<List<T>()>()> nested<T>(T item) => () => () => Repository.repeat<T>(item, 1);

            public void show(PaginatedList<Order> page, int count) { }

            public void keep(Holder<Order> holder) { }

            public List<Any?> unbound() => Repository.repeat(new Holder().get(), 4);

            public void kinds(Class<PaginatedList<Order>> pages, Class<PaginatedList> any, Class<Order> order, Class<Holder<Order>> holders) { }

            public void types(Class<int|string> either, Class<int> number, Class<Any> anything) { }
        }

        public class PaginatedList<TItem : DatabaseEntity>
        {
            public PaginatedList() { }

            public Pair<TItem, TOther> pairWith<TOther>() => new Pair<TItem, TOther>();

            public Pair<TItem, TOther> again<TOther>() => this.pairWith<TOther>();

            public int checks<TOther>(Box<TOther> box, Box<TItem> own, Pair<Box<TOther>, Order> nested) => 1;
        }
    "},
        &[
            (
                "src/App/Entities.php",
                "<?php namespace App; abstract class DatabaseEntity {} final class Order extends DatabaseEntity {}",
            ),
            (
                "src/App/Holder.php",
                "<?php namespace App; /** @template T */ final class Holder { /** @return T */ public function get(): mixed { return null; } }",
            ),
        ],
    );

    let call = |count: u32, type_arguments: &str| {
        format!(
            indoc! {r#"
                STMT_LIST
                  RETURN
                    STATIC_CALL
                      ZVAL "App\\Repository"
                      ZVAL "repeat"
                      ARG_LIST
                        VAR
                          ZVAL "{}"
                        ZVAL {}
                        SHARP_TYPE_ARGS
                          null
                          ZVAL "{}"
            "#},
            if count == 1 { "item" } else { "order" },
            count,
            type_arguments,
        )
    };
    assert_eq!(lowered.body(), call(3, "App.Order"));
    assert_eq!(lowered.body_of("inferred"), call(2, "App.Order"));
    // `new Holder()` binds no `T`, so `get()` gives a type argument nothing chose, which the engine takes as `Any?`.
    assert_eq!(
        lowered.body_of("unbound"),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "App\\Repository"
                  ZVAL "repeat"
                  ARG_LIST
                    METHOD_CALL
                      NEW
                        ZVAL "App\\Holder"
                        ARG_LIST
                      ZVAL "get"
                      ARG_LIST
                    ZVAL 4
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "Any?"
        "#}
    );
    assert_eq!(lowered.body_of("forward"), call(1, "#0"));
    assert_eq!(
        lowered.body_of("box"),
        indoc! {r##"
            STMT_LIST
              RETURN
                SHARP_TYPE_ARGS
                  NEW
                    ZVAL "App\\Box"
                    ARG_LIST
                  ZVAL "#0"
        "##}
    );
    assert_eq!(
        lowered.body_of("later"),
        indoc! {r##"
            STMT_LIST
              RETURN
                CLOSURE "" @25-25
                  PARAM_LIST
                  CLOSURE_USES
                    ZVAL "\0<sharp>\0types"
                  STMT_LIST
                    RETURN
                      SHARP_TYPE_ARGS
                        NEW
                          ZVAL "App\\Box"
                          ARG_LIST
                        ZVAL "#0"
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "Function<App.Box<#0>()>"
                  null
        "##}
    );
    assert_eq!(
        lowered.body_of("nested"),
        indoc! {r##"
            STMT_LIST
              RETURN
                CLOSURE "" @27-27
                  PARAM_LIST
                  CLOSURE_USES
                    ZVAL "item"
                    ZVAL "\0<sharp>\0types"
                  STMT_LIST
                    RETURN
                      CLOSURE "" @27-27
                        PARAM_LIST
                        CLOSURE_USES
                          ZVAL "item"
                          ZVAL "\0<sharp>\0types"
                        STMT_LIST
                          RETURN
                            STATIC_CALL
                              ZVAL "App\\Repository"
                              ZVAL "repeat"
                              ARG_LIST
                                VAR
                                  ZVAL "item"
                                ZVAL 1
                                SHARP_TYPE_ARGS
                                  null
                                  ZVAL "#0"
                        SHARP_TYPE_ARGS
                          null
                          ZVAL "Function<List<#0>()>"
                        null
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "Function<Function<List<#0>()>()>"
                  null
        "##}
    );
    assert_eq!(
        lowered.body_of("pairWith"),
        indoc! {r##"
            STMT_LIST
              RETURN
                SHARP_TYPE_ARGS
                  NEW
                    ZVAL "App\\Pair"
                    ARG_LIST
                  ZVAL "$0, #0"
        "##}
    );
    assert_eq!(
        lowered.body_of("again"),
        indoc! {r##"
            STMT_LIST
              RETURN
                METHOD_CALL
                  VAR
                    ZVAL "this"
                  ZVAL "pairWith"
                  ARG_LIST
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "#0"
        "##}
    );

    let metadata = |bounds: &str, parameters: &str| format!("  SHARP_TYPE_ARGS\n    {bounds}\n    {parameters}\n");
    // A call from plain PHP gives a method's own type parameter no type argument, so a parameter of one has no check.
    assert!(
        lowered.parameters_of("repeat").ends_with(&metadata(r#"ZVAL "Any?""#, "null")),
        "{}",
        lowered.parameters_of("repeat")
    );
    assert!(lowered.parameters_of("forward").ends_with(&metadata(r#"ZVAL "App.DatabaseEntity""#, "null")));
    // A class all of whose type arguments are the method's own takes any object of it, and one that holds such a class
    // as its type argument checks the class.
    assert!(
        lowered
            .parameters_of("checks")
            .ends_with(&metadata(r#"ZVAL "Any?""#, r##"ZVAL "Any?, App.Box<$0>, App.Pair<App.Box<#0>, App.Order>""##)),
        "{}",
        lowered.parameters_of("checks")
    );
    assert!(
        lowered.parameters_of("show").ends_with(&metadata("null", r#"ZVAL "App.PaginatedList<App.Order>, Any?""#)),
        "{}",
        lowered.parameters_of("show")
    );
    // PHP's `string` type checks no class value, so one of a PHP# class with type arguments is checked as its object is.
    assert!(
        lowered
            .parameters_of("kinds")
            .ends_with(&metadata("null", r#"ZVAL "Class<App.PaginatedList<App.Order>>, Any?, Any?, Any?""#)),
        "{}",
        lowered.parameters_of("kinds")
    );
    // A class value of a type that is no class is only the text `typeof` gives that type, which PHP's `string` type
    // doesn't check.
    assert!(
        lowered.parameters_of("types").ends_with(&metadata("null", r#"ZVAL "Class<int|string>, Class<int>, Any?""#)),
        "{}",
        lowered.parameters_of("types")
    );
    assert_eq!(lowered.parameters_of("pairWith"), format!("PARAM_LIST\n{}", metadata(r#"ZVAL "Any?""#, "null")));
    assert!(!lowered.parameters_of("run").contains("SHARP_TYPE_ARGS"));
    assert!(!lowered.parameters_of("__construct").contains("SHARP_TYPE_ARGS"));
    assert!(!lowered.parameters_of("keep").contains("SHARP_TYPE_ARGS"), "{}", lowered.parameters_of("keep"));
}

/// A value that enters PHP# from plain PHP is checked, as a parameter checks an argument from plain PHP: a call the
/// checker resolved to plain PHP checks its result in a `VERIFY_TYPE_ARGS` that names the call, wherever the value goes,
/// whether a typed local, a `let`, an argument, a return or a property write takes it. A property has its PHP# type
/// around its PHP type, which PHP's typed property check reads on each write plain PHP makes, and a write from PHP#
/// skips. A PHP# call's return, a call of a function a property holds and a type with nothing to check need none.
#[test]
fn a_value_from_plain_php_is_checked_where_it_enters_php_sharp() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App;

        import Lib.Pager;

        public class Box<TValue>
        {
            public Box() { }
        }

        public class Shelf<TItem>
        {
            public Box<Order>? auto { get; set; } = null;

            public Box<TItem>? own { get; set; } = null;

            public Box<Order>? logged
            {
                get => field;
                set { field = value; }
            } = null;

            protected Box<Order>? inherited = null;

            private Box<Order>? hidden = null;

            public Box<Order>? fixed { get; private set; } = null;

            public Box<Order>? created
            {
                get => field;
                init { field = value; }
            }

            public int count { get; set; } = 0;

            private Function<Box<Order>()> maker = () => new Box<Order>();

            public Shelf(public Box<Order>? promoted { get; set; }, public Box<Order>? frozen { get; }) { }

            public void run()
            {
                Box<Order> box = Pager.orders();
                box = Pager.orders();
                let untyped = Pager.orders();
                this.keep(Pager.orders());
                this.auto = Pager.orders();
                Box<Order> made = Order.make();
                Box<Order> held = this.maker();
                int count = Pager.count();
            }

            public Box<Order> give()
            {
                return Pager.orders();
            }

            public Box<Order> listed()
            {
                return global.orders();
            }

            public void keep(Box<Order> box) { }
        }

        public class Order
        {
            private static Box<Order>? latest = null;

            public static Box<Order> make() => new Box<Order>();
        }
        "},
        &[
            (
                "src/Lib/Pager.php",
                "<?php\n\nnamespace Lib;\n\nuse App\\Box;\n\nfinal class Pager\n{\n    /** @return Box<\\App\\Order> */\n    public static function orders(): Box\n    {\n        return new Box();\n    }\n\n    public static function count(): int\n    {\n        return 1;\n    }\n}\n",
            ),
            (
                "src/orders.php",
                "<?php\n\n/** @return \\App\\Box<\\App\\Order> */\nfunction orders(): \\App\\Box\n{\n    return new \\App\\Box();\n}\n",
            ),
        ],
    );

    // `Pager.orders()`, checked, at the depth `depth`.
    let orders = |depth: usize| {
        let lines = [
            "VERIFY_TYPE_ARGS",
            "  STATIC_CALL",
            r#"    ZVAL "Lib\\Pager""#,
            r#"    ZVAL "orders""#,
            "    ARG_LIST",
            r#"  ZVAL "App.Box<App.Order>""#,
            r#"  ZVAL "Lib\\Pager::orders()""#,
        ];

        lines.map(|line| format!("{}{line}\n", "  ".repeat(depth))).concat()
    };
    let assign = |variable: &str| format!("  ASSIGN\n    VAR\n      ZVAL \"{variable}\"\n{}", orders(2));
    let unchecked = indoc! {r#"
          ASSIGN
            VAR
              ZVAL "made"
            STATIC_CALL
              ZVAL "App\\Order"
              ZVAL "make"
              ARG_LIST
          ASSIGN
            VAR
              ZVAL "held"
            CALL
              PROP
                VAR
                  ZVAL "this"
                ZVAL "maker"
              ARG_LIST
          ASSIGN
            VAR
              ZVAL "count"
            STATIC_CALL
              ZVAL "Lib\\Pager"
              ZVAL "count"
              ARG_LIST
    "#};
    assert_eq!(
        lowered.body(),
        [
            "STMT_LIST\n".to_owned(),
            assign("box"),
            assign("box"),
            assign("untyped"),
            format!("  METHOD_CALL\n    VAR\n      ZVAL \"this\"\n    ZVAL \"keep\"\n    ARG_LIST\n{}", orders(3)),
            format!("  ASSIGN\n    PROP\n      VAR\n        ZVAL \"this\"\n      ZVAL \"auto\"\n{}", orders(2)),
            unchecked.lines().flat_map(|line| ["  ", line, "\n"]).collect(),
        ]
        .concat()
    );
    assert_eq!(lowered.body_of("give"), format!("STMT_LIST\n  RETURN\n{}", orders(2)));
    // A plain PHP function's result names the function.
    assert_eq!(
        lowered.body_of("listed"),
        indoc! {r#"
            STMT_LIST
              RETURN
                VERIFY_TYPE_ARGS
                  CALL
                    ZVAL "orders"
                    ARG_LIST
                  ZVAL "App.Box<App.Order>"
                  ZVAL "orders()"
        "#}
    );

    let tree = lowered.tree();
    // A property's PHP# type stands around its PHP type, whatever the property's visibility and accessors, with a type
    // parameter of its class as `$0`, and a promoted one's around its parameter's.
    let typed = |property: &str, text: &str| {
        format!(
            "SHARP_TYPE_ARGS\n          ZVAL [256] \"App\\\\Box\"\n          ZVAL \"{text}\"\n        PROP_DECL\n          PROP_ELEM\n            ZVAL \"{property}\"\n"
        )
    };
    for (property, text) in [
        ("auto", "App.Box<App.Order>?"),
        ("own", "App.Box<$0>?"),
        ("logged", "App.Box<App.Order>?"),
        ("inherited", "App.Box<App.Order>?"),
        ("hidden", "App.Box<App.Order>?"),
        ("fixed", "App.Box<App.Order>?"),
        ("created", "App.Box<App.Order>?"),
        ("latest", "App.Box<App.Order>?"),
    ] {
        assert!(tree.contains(&typed(property, text)), "{property}: {tree}");
    }
    for parameter in ["promoted", "frozen"] {
        let typed = format!(
            "SHARP_TYPE_ARGS\n              ZVAL [256] \"App\\\\Box\"\n              ZVAL \"App.Box<App.Order>?\"\n            ZVAL \"{parameter}\"\n"
        );
        assert!(tree.contains(&typed), "{parameter}: {tree}");
    }
    // Each of the eight properties above, and no other: a method's result type holds its function type instead.
    assert_eq!(
        tree.matches("SHARP_TYPE_ARGS\n          ZVAL [256] \"App\\\\Box\"\n          ZVAL \"App.").count(),
        8,
        "{tree}"
    );
    // PHP's typed property check runs the check, so a property takes no `set` hook for it: only a written `set` or
    // `init` body is a hook.
    assert!(!tree.contains("VERIFY_TYPE_ARGS ["), "{tree}");
    assert_eq!(tree.matches("PROPERTY_HOOK \"set\"").count(), 2, "{tree}");
    assert_eq!(tree.matches("PROPERTY_HOOK \"get\"").count(), 2, "{tree}");
}

/// A collection the project's plain PHP returns is checked element by element where it enters PHP#, since its type is
/// a docblock nothing checks. A collection PHP's internal functions return is not: the engine guarantees their types.
/// Neither is one whose elements take any value, nor one PHP# can't write, such as `array<array-key, int>`.
#[test]
fn a_collection_the_project_returns_from_plain_php_is_checked_element_by_element() {
    let body = body_in(
        "int run()",
        "        let ints = Calc.ints();\n        let keyed = Calc.keyed();\n        let untyped = Calc.untyped();\n        let anything = Calc.anything();\n        let letters = global.str_split(\"ab\");\n        return 1;\n",
        &[(
            "src/Lib/Calc.php",
            "<?php namespace Lib; final class Calc { /** @return list<int> */ public static function ints(): array { return []; } /** @return array<array-key, int> */ public static function keyed(): array { return []; } public static function untyped(): array { return []; } /** @return list<mixed> */ public static function anything(): array { return []; } }",
        )],
    );

    assert_eq!(body.matches("VERIFY_TYPE_ARGS").count(), 1, "{body}");
    assert!(body.contains("ZVAL \"List<int>\"\n      ZVAL \"Lib\\\\Calc::ints()\""), "{body}");
}

/// Every function a call from plain PHP reaches checks a parameter that names a PHP# class with type arguments, as a
/// method does: an event's constructor, a named constructor, an operator and a lambda, with a block body or an
/// expression body. Each one's parameter list ends with the metadata that holds the parameter's type text.
#[test]
fn every_function_plain_php_calls_checks_its_parameters_type_arguments() {
    let lowered = Lowered::new(indoc! {"
        namespace App;

        public event Paged(PaginatedList<Order> page);

        public class Order
        {
        }

        public class PaginatedList<TItem>
        {
            public PaginatedList() { }
        }

        public class Shelf
        {
            public Shelf(public int count { get; }) { }

            public Shelf.of(PaginatedList<Order> page) : this(1) { }

            public static Shelf operator +(Shelf shelf, PaginatedList<Order> page) => new Shelf(shelf.count);

            public Function<int(PaginatedList<Order>)> counted() => (PaginatedList<Order> page) => 1;

            public Function<void(PaginatedList<Order>)> kept()
            {
                let seen = 0;
                return (PaginatedList<Order> page) => {
                    seen += 1;
                };
            }
        }
    "});
    let metadata = |checks: &str| format!("  SHARP_TYPE_ARGS\n    null\n    ZVAL \"{checks}\"\n");

    assert!(lowered.parameters_of("__construct").ends_with(&metadata("App.PaginatedList<App.Order>")));
    assert!(lowered.parameters_of("of").ends_with(&metadata("App.PaginatedList<App.Order>")));
    assert!(lowered.parameters_of("op_Addition").ends_with(&metadata("Any?, App.PaginatedList<App.Order>")));
    assert_eq!(
        lowered.body_of("counted"),
        indoc! {r#"
            STMT_LIST
              RETURN
                ARROW_FUNC "" @22-22
                  PARAM_LIST
                    PARAM
                      ZVAL "App\\PaginatedList"
                      ZVAL "page"
                      null
                      null
                      null
                      null
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "App.PaginatedList<App.Order>"
                  null
                  ZVAL 1
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "Function<int(App.PaginatedList<App.Order>)>"
                  null
        "#}
    );
    assert_eq!(
        lowered.body_of("kept"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "seen"
                ZVAL 0
              RETURN
                CLOSURE "" @27-29
                  PARAM_LIST
                    PARAM
                      ZVAL "App\\PaginatedList"
                      ZVAL "page"
                      null
                      null
                      null
                      null
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "App.PaginatedList<App.Order>"
                  CLOSURE_USES
                    ZVAL [1] "seen"
                  STMT_LIST
                    ASSIGN_OP [1]
                      VAR
                        ZVAL "seen"
                      ZVAL 1
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "Function<void(App.PaginatedList<App.Order>)>"
                  null
        "#}
    );
}

/// A type argument a plain PHP value fixes as a refinement PHP# writes as a wider type carries that type's text:
/// `list<int>` is `List<int>`, `array{a: int}` is `Map<string, int>`, `non-empty-string` is `string`, `positive-int` is
/// `int` and `class-string` is `Class<Object>`. A list or map literal carries its collection's text, as `[1, 2]` does
/// `List<int>`, and keeps a `Set` in it a `Set`. A written type argument carries its own text.
#[test]
fn a_type_argument_a_plain_php_refinement_fixes_carries_its_php_sharp_text() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App;

        import Lib.Pager;

        public class Registry
        {
            public static T keep<T>(T value) => value;

            public void run(Set<string> tags)
            {
                Registry.keep(Pager.list());
                Registry.keep(Pager.text());
                Registry.keep(Pager.count());
                Registry.keep(Pager.class());
                Registry.keep<Map<string, int>>(Pager.shape());
                Registry.keep<int|string>(Pager.key());
                Registry.keep(Pager.shape());
                Registry.keep([1, 2]);
                Registry.keep([\"k\": [1]]);
                Registry.keep([tags]);
            }
        }
        "},
        &[(
            "src/Lib/Pager.php",
            "<?php\n\nnamespace Lib;\n\nfinal class Pager\n{\n    /** @return list<int> */\n    public static function list(): array\n    {\n        return [1];\n    }\n\n    /** @return non-empty-string */\n    public static function text(): string\n    {\n        return 'a';\n    }\n\n    /** @return positive-int */\n    public static function count(): int\n    {\n        return 1;\n    }\n\n    /** @return class-string */\n    public static function class(): string\n    {\n        return self::class;\n    }\n\n    /** @return array{a: int} */\n    public static function shape(): array\n    {\n        return ['a' => 1];\n    }\n\n    /** @return array-key */\n    public static function key(): int|string\n    {\n        return 1;\n    }\n}\n",
        )],
    );

    let body = lowered.body();
    assert_eq!(body.matches("SHARP_TYPE_ARGS").count(), 10, "{body}");
    for (text, calls) in [
        // Two type arguments, and the check of the list `Pager.list()` returns.
        ("List<int>", 3),
        ("string", 1),
        ("int", 1),
        ("Class<Object>", 1),
        ("Map<string, int>", 2),
        ("int|string", 1),
        ("Map<string, List<int>>", 1),
        ("List<Set<string>>", 1),
    ] {
        assert_eq!(body.matches(&format!("ZVAL \"{text}\"\n")).count(), calls, "{text}: {body}");
    }
}

/// A value of a type parameter is a value of its bound, so a generic method called on it carries its type arguments as
/// a call on the bound does.
#[test]
fn a_generic_call_on_a_value_of_a_type_parameter_carries_its_type_arguments() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App;

        public class Box<TValue>
        {
            public Box() { }
        }

        public class Shelf
        {
            public Shelf() { }

            public Box<T> make<T>() => new Box<T>();
        }

        public class Store
        {
            public Box<Order> stock<TShelf : Shelf>(TShelf shelf) => shelf.make<Order>();
        }
    "},
        &[(
            "src/App/Entities.php",
            "<?php namespace App; abstract class DatabaseEntity {} final class Order extends DatabaseEntity {}",
        )],
    );

    assert_eq!(
        lowered.body_of("stock"),
        indoc! {r#"
            STMT_LIST
              RETURN
                METHOD_CALL
                  VAR
                    ZVAL "shelf"
                  ZVAL "make"
                  ARG_LIST
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "App.Order"
        "#}
    );
}

/// A test of a class with type arguments or of a type parameter, in `is`, `as` and a `match` arm, and `typeof` of a type
/// parameter, read the type arguments while the code runs: a `SHARP_IS_TYPE` of the value and the type text, and a
/// `SHARP_TYPE_OF` of the type text, written as `new` writes its type arguments. None runs as `instanceof` a class named
/// for a type parameter, which no value is.
#[test]
fn a_test_and_typeof_of_a_type_with_type_arguments_read_them_while_the_code_runs() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App;

        public class Box<TValue>
        {
            public Box() { }

            public bool holds(Any value) => value is TValue;
        }

        public class Shelf
        {
            public bool boxed(Any value) => value is Box<Order>;

            public bool kept<T>(Any value) => value is T;

            public T? cast<T>(Any value) => value as T;

            public int arm<T>(Any value) => match (value) { T => 1, default => 0 };

            public Class<T> kind<T>() => typeof(T);
        }
    "},
        &[(
            "src/App/Entities.php",
            "<?php namespace App; abstract class DatabaseEntity {} final class Order extends DatabaseEntity {}",
        )],
    );

    let test = |text: &str| {
        format!("STMT_LIST\n  RETURN\n    SHARP_IS_TYPE\n      VAR\n        ZVAL \"value\"\n      ZVAL \"{text}\"\n")
    };

    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
    assert_eq!(lowered.body_of("holds"), test("$0"));
    assert_eq!(lowered.body_of("boxed"), test("App.Box<App.Order>"));
    assert_eq!(lowered.body_of("kept"), test("#0"));
    assert_eq!(
        lowered.body_of("cast"),
        "STMT_LIST\n  RETURN\n    CONDITIONAL\n      SHARP_IS_TYPE\n        VAR\n          ZVAL \"value\"\n        ZVAL \"#0\"\n      VAR\n        ZVAL \"value\"\n      ZVAL null\n"
    );
    assert_eq!(
        lowered.body_of("arm"),
        "STMT_LIST\n  RETURN\n    MATCH\n      ZVAL true\n      MATCH_ARM_LIST\n        MATCH_ARM\n          EXPR_LIST\n            SHARP_IS_TYPE\n              VAR\n                ZVAL \"value\"\n              ZVAL \"#0\"\n          ZVAL 1\n        MATCH_ARM\n          null\n          ZVAL 0\n"
    );
    assert_eq!(lowered.body_of("kind"), "STMT_LIST\n  RETURN\n    SHARP_TYPE_OF\n      ZVAL \"#0\"\n");
}

/// `typeof` of a type that is no class is a `SHARP_TYPE_OF` of its type text, as `typeof(T)` is when `T` runs as that
/// type, so the engine gives its class value. A nullable or union type is one type, spelled whole. `typeof(Self)` is
/// PHP's `static::class`, and a static call through it is PHP's `static::method()`.
#[test]
fn typeof_of_a_type_that_is_no_class_spells_its_type() {
    let lowered = Lowered::new(indoc! {"
        namespace App;

        public class Order
        {
            public static string defaultTag() => \"div\";

            public static Any whole() => typeof(int);

            public static Any nullable() => typeof(int?);

            public static Any either() => typeof(string|int);

            public static Any held() => typeof((Order|int)?);

            public static Any orders() => typeof(List<Order>);

            public static Any counts() => typeof(Map<string, int>);

            public static Any numbers() => typeof(Set<int>);

            public static Any own() => typeof(Self);

            public static string tag() => typeof(Self).defaultTag();
        }
    "});
    let type_of = |text: &str| format!("STMT_LIST\n  RETURN\n    SHARP_TYPE_OF\n      ZVAL \"{text}\"\n");

    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
    assert_eq!(lowered.body_of("whole"), type_of("int"));
    assert_eq!(lowered.body_of("nullable"), type_of("int?"));
    assert_eq!(lowered.body_of("either"), type_of("int|string"));
    assert_eq!(lowered.body_of("held"), type_of("(App.Order|int)?"));
    assert_eq!(lowered.body_of("orders"), type_of("List<App.Order>"));
    assert_eq!(lowered.body_of("counts"), type_of("Map<string, int>"));
    assert_eq!(lowered.body_of("numbers"), type_of("Set<int>"));
    assert_eq!(lowered.body_of("own"), "STMT_LIST\n  RETURN\n    CLASS_NAME\n      ZVAL [1] \"static\"\n");
    assert_eq!(
        lowered.body_of("tag"),
        "STMT_LIST\n  RETURN\n    STATIC_CALL\n      ZVAL [1] \"static\"\n      ZVAL \"defaultTag\"\n      ARG_LIST\n"
    );
}

/// `new TItem(…)` is a `NEW` that names no class under the `SHARP_TYPE_ARGS` of `TItem`'s text, so the engine creates
/// the type argument with its own type arguments. A static member of `TItem` is read on the class `SHARP_TYPE_OF`
/// gives, as PHP's `$class::member` reads one, and a static method of it is never inlined, since the type argument may
/// declare its own.
#[test]
fn new_and_a_static_member_of_a_type_parameter_read_its_class_while_the_code_runs() {
    let lowered = Lowered::new(indoc! {"
        namespace App;

        public abstract class Maker
        {
            public const int LIMIT = 3;

            public static int count { get; set; } = 0;

            public required Maker(public int size { get; }) { }

            public static int make() => 1;
        }

        public class Builder<TItem : Maker>
        {
            public Builder() { }

            public TItem create() => new TItem(TItem.LIMIT);

            public int made<TOther : Maker>() => TOther.make();

            public void counted()
            {
                TItem.count = 2;
            }
        }
    "});

    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
    assert_eq!(
        lowered.body_of("create"),
        "STMT_LIST\n  RETURN\n    SHARP_TYPE_ARGS\n      NEW\n        null\n        ARG_LIST\n          CLASS_CONST\n            SHARP_TYPE_OF\n              ZVAL \"$0\"\n            ZVAL \"LIMIT\"\n      ZVAL \"$0\"\n"
    );
    assert_eq!(
        lowered.body_of("made"),
        "STMT_LIST\n  RETURN\n    STATIC_CALL\n      SHARP_TYPE_OF\n        ZVAL \"#0\"\n      ZVAL \"make\"\n      ARG_LIST\n"
    );
    assert_eq!(
        lowered.body_of("counted"),
        "STMT_LIST\n  ASSIGN\n    STATIC_PROP\n      SHARP_TYPE_OF\n        ZVAL \"$0\"\n      ZVAL \"count\"\n    ZVAL 2\n"
    );
}

/// A list runs its methods in the engine's built-in `Sharp\Collection`, so a call of a generic one such as `map` carries
/// no type arguments, as a call of any built-in method does.
#[test]
fn a_generic_method_call_on_a_list_carries_no_type_arguments() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App;

        public class Prices
        {
            public static List<string> labels(List<int> prices) => prices.map(price => (string) price);
        }
    "},
        &[],
    );

    // A call with type arguments is a `SHARP_TYPE_ARGS` around the call. The lambda's own holds its function type.
    assert!(
        lowered.body_of("labels").starts_with("STMT_LIST\n  RETURN\n    METHOD_CALL\n"),
        "{}",
        lowered.body_of("labels")
    );
}

/// A PHP# generic class ends with its metadata, a `SHARP_TYPE_ARGS` without a `new` whose second text is its bounds:
/// the type text of each type parameter's bound, `Any?` for one without a bound. It declares its hidden type-argument
/// slot from it. A class without type parameters, whose header gives no type arguments, has no metadata. `new` of the
/// class is a `SHARP_TYPE_ARGS` over the `NEW`, whose text is the type arguments the checker found, each in its full
/// dotted name. `new Self` gives the new object `this`'s type arguments, so its text is null. A type argument
/// that names a type parameter of the class writes it as `$` and its index, and the engine gives it `this`'s type
/// argument at that index. One that names a method's own type parameter writes it as `#` and its index, and the engine
/// gives it the method's own type argument at that index.
#[test]
fn new_of_a_generic_php_sharp_class_carries_its_type_arguments() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App;

        public class PaginatedList<TItem : DatabaseEntity, TKey>
        {
            public required PaginatedList(private List<TItem> rows)
            {
            }

            public PaginatedList<TItem, TKey> copy() => new Self(this.rows);

            public PaginatedList<TItem, List<TKey>?> nested() => new PaginatedList<TItem, List<TKey>?>(this.rows);
        }

        public class Report
        {
            public PaginatedList<Order, int> run(List<Order> rows) => new PaginatedList<Order, int>(rows);

            public PaginatedList<TItem, int> open<TItem : DatabaseEntity>(List<TItem> rows) => new PaginatedList<TItem, int>(rows);
        }
    "},
        &[(
            "src/App/Entities.php",
            "<?php namespace App; abstract class DatabaseEntity {} final class Order extends DatabaseEntity {}",
        )],
    );
    let last_member = |class: u32| {
        let members = lowered.child(lowered.child(lowered.root(), class), 2);

        lowered.child(members, lowered.nodes()[members as usize].child_count - 1)
    };

    assert_eq!(
        lowered.render(last_member(2)),
        indoc! {r#"
            SHARP_CLASS_METADATA
              null
              ZVAL "App.DatabaseEntity, Any?"
              null
        "#}
    );
    assert_eq!(lowered.nodes()[last_member(3) as usize].kind, sharp_kind::SHARP_AST_METHOD);
    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                SHARP_TYPE_ARGS
                  NEW
                    ZVAL "App\\PaginatedList"
                    ARG_LIST
                      VAR
                        ZVAL "rows"
                  ZVAL "App.Order, int"
        "#}
    );
    assert_eq!(
        lowered.body_of("copy"),
        indoc! {r#"
            STMT_LIST
              RETURN
                SHARP_TYPE_ARGS
                  NEW
                    ZVAL [1] "static"
                    ARG_LIST
                      PROP
                        VAR
                          ZVAL "this"
                        ZVAL "rows"
                  null
        "#}
    );
    assert_eq!(
        lowered.body_of("nested"),
        indoc! {r#"
            STMT_LIST
              RETURN
                SHARP_TYPE_ARGS
                  NEW
                    ZVAL "App\\PaginatedList"
                    ARG_LIST
                      PROP
                        VAR
                          ZVAL "this"
                        ZVAL "rows"
                  ZVAL "$0, List<$1>?"
        "#}
    );
    assert_eq!(
        lowered.body_of("open"),
        indoc! {r##"
            STMT_LIST
              RETURN
                SHARP_TYPE_ARGS
                  NEW
                    ZVAL "App\\PaginatedList"
                    ARG_LIST
                      VAR
                        ZVAL "rows"
                  ZVAL "#0, int"
        "##}
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
                    SHARP_TYPE_ARGS
                      ZVAL [1] "float"
                      ZVAL "float"
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
                    SHARP_TYPE_ARGS
                      ZVAL [1] "string"
                      ZVAL "string"
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
                    SHARP_TYPE_ARGS
                      ZVAL [1] "string"
                      ZVAL "string"
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

/// Spec section 15: an `on` clause adds nothing to its class, because `mago compile` lists each listener in
/// `.sharp/listeners.php`, so a listener lowers to its method alone.
#[test]
fn a_listener_lowers_to_its_method_alone() {
    let code = "namespace App;\n\npublic interface OrderEvent\n{\n    int orderId { get; }\n}\n\npublic event OrderPaid(int orderId) : OrderEvent;\n\npublic class Notices\n{\n    public void send(OrderEvent e) on OrderPaid { }\n\n    public void refresh() on OrderPaid, OrderEvent => this.send(new OrderPaid(1));\n}\n";

    assert_eq!(
        Lowered::new(code).tree(),
        Lowered::new(&code.replace(" on OrderPaid, OrderEvent", "").replace(" on OrderPaid", "")).tree()
    );
}

/// ```php
/// /** @extends \Lib\PaginatedList<\Lib\Order> */
/// class OrderPage extends \Lib\PaginatedList { }
/// ```
///
/// A header entry with type arguments is its class, as the same entry without them. The class's metadata keeps the
/// type arguments, as a type text list of each class its header gives some.
#[test]
fn a_header_entry_with_type_arguments_is_its_class() {
    let source = |header: &str| {
        format!(
            "namespace App.Tenant;\n\nimport Lib.Order;\nimport Lib.PaginatedList;\n\npublic class OrderPage : {header}\n{{\n}}\n"
        )
    };
    let generic = Lowered::with(
        &source("PaginatedList<Order>"),
        &[(
            "src/Lib/PaginatedList.php",
            "<?php namespace Lib; class Order {} /** @template T */ class PaginatedList {}",
        )],
    );
    let plain = Lowered::with(
        &source("PaginatedList"),
        &[("src/Lib/PaginatedList.php", "<?php namespace Lib; class Order {} class PaginatedList {}")],
    );

    let parent = |lowered: &Lowered| lowered.render(lowered.child(lowered.child(lowered.root(), 2), 0));
    let members = |lowered: &Lowered| lowered.render(lowered.child(lowered.child(lowered.root(), 2), 2));

    assert_eq!(parent(&generic), parent(&plain));
    assert_eq!(parent(&generic), "ZVAL \"Lib\\\\PaginatedList\"\n");
    assert_eq!(
        members(&generic),
        indoc! {r#"
            STMT_LIST
              SHARP_CLASS_METADATA
                ZVAL "Lib.PaginatedList<Lib.Order>"
                null
                null
        "#}
    );
    assert_eq!(members(&plain), "STMT_LIST\n");
}

/// An event's header keeps its type arguments as a class's does, after its constructor, so the engine knows
/// `Moved` is a `Holder<int>`.
#[test]
fn an_event_header_entry_with_type_arguments_keeps_them() {
    let lowered = Lowered::with(
        "namespace App;\n\npublic event Moved(int item) : Holder<int>;\n",
        &[(
            "app/Holder.sharp",
            "namespace App;\n\npublic interface Holder<TItem>\n{\n    public TItem item { get; }\n}\n",
        )],
    );
    let members = lowered.child(lowered.child(lowered.root(), 2), 2);

    assert_eq!(
        lowered.render(lowered.child(members, 1)),
        indoc! {r#"
            SHARP_CLASS_METADATA
              ZVAL "App.Holder<int>"
              null
              null
        "#}
    );
}

/// ```php
/// interface OrderEvent
/// {
///     public int $orderId { get; }
/// }
/// final class OrderPaid implements \App\OrderEvent
/// {
///     public function __construct(public readonly int $orderId, public readonly int $amount = 0) {}
/// }
/// class Order
/// {
///     public function pay(int $orderId): \App\OrderPaid { return new \App\OrderPaid($orderId); }
/// }
/// ```
///
/// ```php
/// abstract class CheckoutState
/// {
///     public function label(): string { return 'checkout'; }
///     public static function op_Equality(mixed $a, mixed $b): bool
///     {
///         if ($a instanceof \App\CheckoutState\Open && $b instanceof \App\CheckoutState\Open) { return true; }
///         if ($a instanceof \App\CheckoutState\Paying && $b instanceof \App\CheckoutState\Paying) { return $a->gateway === $b->gateway; }
///         return $a === $b;
///     }
/// }
/// final class CheckoutState\Open extends \App\CheckoutState implements \Sharp\Struct
/// {
///     private static $instance;
///     private function __construct() {}
///     public static function instance(): self { return self::$instance ??= new self(); }
/// }
/// final class CheckoutState\Paying extends \App\CheckoutState implements \Sharp\Struct
/// {
///     public function __construct(public readonly string $gateway) {}
/// }
/// class Shop
/// {
///     public function open(): \App\CheckoutState { return \App\CheckoutState\Open::instance(); }
///     public function pay(): \App\CheckoutState { return new \App\CheckoutState\Paying(gateway: 'stripe'); }
///     public function same(\App\CheckoutState $a, \App\CheckoutState $b): bool { return \App\CheckoutState::op_Equality($a, $b); }
/// }
/// ```
///
/// Spec section 20: an enum with a case that carries data is an abstract class, `[64]`
/// `ZEND_ACC_EXPLICIT_ABSTRACT_CLASS`, and each case a final subclass named in the enum's namespace. A plain case is the
/// one object its class's `instance()` gives, and `CheckoutState.Paying(…)` creates its case. An enum compares by value,
/// sections 19 and 20, so `==` runs the enum's `op_Equality`, which compares two of the same case field by field and
/// anything else as identical, as a struct's does. A plain case is a case with no fields, so the copy `unserialize`
/// makes of it is equal to it. Each case implements `\Sharp\Struct`, so a `Map` or a `Set` hashes it by its fields, as
/// it hashes a struct.
#[test]
fn an_enum_with_data_is_an_abstract_class_and_each_case_a_final_subclass() {
    let lowered = Lowered::new(
        "namespace App;\n\npublic enum CheckoutState\n{\n    case Open;\n    case Paying(string gateway);\n\n    public string label() => \"checkout\";\n}\n\npublic class Shop\n{\n    public CheckoutState open() => CheckoutState.Open;\n    public CheckoutState pay() => CheckoutState.Paying(gateway: \"stripe\");\n    public bool same(CheckoutState a, CheckoutState b) => a == b;\n}\n",
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
                ZVAL "App"
                null
              CLASS [64] "CheckoutState" @3-9
                null
                null
                STMT_LIST
                  METHOD [1] "label" @8-8
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        ZVAL "checkout"
                    SHARP_TYPE_ARGS
                      ZVAL [1] "string"
                      ZVAL "Function<string()>"
                    null
                  METHOD [17] "op_Equality" @3-3
                    PARAM_LIST
                      PARAM
                        ZVAL [1] "mixed"
                        ZVAL "a"
                        null
                        null
                        null
                        null
                      PARAM
                        ZVAL [1] "mixed"
                        ZVAL "b"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                      IF
                        IF_ELEM
                          AND
                            INSTANCEOF
                              VAR
                                ZVAL "a"
                              ZVAL "App\\CheckoutState\\Open"
                            INSTANCEOF
                              VAR
                                ZVAL "b"
                              ZVAL "App\\CheckoutState\\Open"
                          STMT_LIST
                            RETURN
                              ZVAL true
                      IF
                        IF_ELEM
                          AND
                            INSTANCEOF
                              VAR
                                ZVAL "a"
                              ZVAL "App\\CheckoutState\\Paying"
                            INSTANCEOF
                              VAR
                                ZVAL "b"
                              ZVAL "App\\CheckoutState\\Paying"
                          STMT_LIST
                            RETURN
                              BINARY_OP [16]
                                PROP
                                  VAR
                                    ZVAL "a"
                                  ZVAL "gateway"
                                PROP
                                  VAR
                                    ZVAL "b"
                                  ZVAL "gateway"
                      RETURN
                        BINARY_OP [16]
                          VAR
                            ZVAL "a"
                          VAR
                            ZVAL "b"
                    ZVAL [1] "bool"
                    null
                null
                null
              CLASS [32] "CheckoutState\\Open" @5-5
                ZVAL "App\\CheckoutState"
                NAME_LIST
                  ZVAL "Sharp\\Struct"
                STMT_LIST
                  PROP_GROUP [20]
                    null
                    PROP_DECL
                      PROP_ELEM
                        ZVAL "instance"
                        null
                        null
                        null
                    null
                  METHOD [4] "__construct" @5-5
                    PARAM_LIST
                    null
                    STMT_LIST
                    null
                    null
                  METHOD [17] "instance" @5-5
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        ASSIGN_COALESCE
                          STATIC_PROP
                            ZVAL [1] "self"
                            ZVAL "instance"
                          NEW
                            ZVAL [1] "self"
                            ARG_LIST
                    ZVAL [1] "self"
                    null
                null
                null
              CLASS [32] "CheckoutState\\Paying" @6-6
                ZVAL "App\\CheckoutState"
                NAME_LIST
                  ZVAL "Sharp\\Struct"
                STMT_LIST
                  METHOD [1] "__construct" @6-6
                    PARAM_LIST
                      PARAM [129]
                        ZVAL [1] "string"
                        ZVAL "gateway"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                    null
                    null
                null
                null
              CLASS "Shop" @11-16
                null
                null
                STMT_LIST
                  METHOD [1] "open" @13-13
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        STATIC_CALL
                          ZVAL "App\\CheckoutState\\Open"
                          ZVAL "instance"
                          ARG_LIST
                    SHARP_TYPE_ARGS
                      ZVAL "App\\CheckoutState"
                      ZVAL "Function<App.CheckoutState()>"
                    null
                  METHOD [1] "pay" @14-14
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        NEW
                          ZVAL "App\\CheckoutState\\Paying"
                          ARG_LIST
                            NAMED_ARG
                              ZVAL "gateway"
                              ZVAL "stripe"
                    SHARP_TYPE_ARGS
                      ZVAL "App\\CheckoutState"
                      ZVAL "Function<App.CheckoutState()>"
                    null
                  METHOD [1] "same" @15-15
                    PARAM_LIST
                      PARAM
                        ZVAL "App\\CheckoutState"
                        ZVAL "a"
                        null
                        null
                        null
                        null
                      PARAM
                        ZVAL "App\\CheckoutState"
                        ZVAL "b"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                      RETURN
                        STATIC_CALL
                          ZVAL "App\\CheckoutState"
                          ZVAL "op_Equality"
                          ARG_LIST
                            VAR
                              ZVAL "a"
                            VAR
                              ZVAL "b"
                    SHARP_TYPE_ARGS
                      ZVAL [1] "bool"
                      ZVAL "Function<bool(App.CheckoutState, App.CheckoutState)>"
                    null
                null
                null
        "#}
    );
}

/// Spec section 15: an event is a final class, `[32]` `ZEND_ACC_FINAL`, whose constructor promotes each parameter to a
/// public get-only property, `[129]` `ZEND_ACC_PUBLIC | ZEND_ACC_READONLY`, and its header is the `implements` name
/// list. An interface's get-only property, written bare, is a public property, `[1]`, with an abstract `get` hook.
#[test]
fn an_event_is_a_final_class_and_an_interface_property_has_an_abstract_get_hook() {
    let lowered = Lowered::new(
        "namespace App;\n\npublic interface OrderEvent\n{\n    int orderId { get; }\n}\n\npublic event OrderPaid(int orderId, int amount = 0) : OrderEvent;\n\npublic class Order\n{\n    public OrderPaid pay(int orderId) => new OrderPaid(orderId);\n}\n",
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
                ZVAL "App"
                null
              CLASS [1] "OrderEvent" @3-6
                null
                null
                STMT_LIST
                  PROP_GROUP [1]
                    ZVAL [1] "int"
                    PROP_DECL
                      PROP_ELEM
                        ZVAL "orderId"
                        null
                        null
                        STMT_LIST
                          PROPERTY_HOOK "get" @5-5
                            null
                            null
                            null
                            null
                            null
                    null
                null
                null
              CLASS [32] "OrderPaid" @8-8
                null
                NAME_LIST
                  ZVAL "App\\OrderEvent"
                STMT_LIST
                  METHOD [1] "__construct" @8-8
                    PARAM_LIST
                      PARAM [129]
                        ZVAL [1] "int"
                        ZVAL "orderId"
                        null
                        null
                        null
                        null
                      PARAM [129]
                        ZVAL [1] "int"
                        ZVAL "amount"
                        ZVAL 0
                        null
                        null
                        null
                    null
                    STMT_LIST
                    null
                    null
                null
                null
              CLASS "Order" @10-13
                null
                null
                STMT_LIST
                  METHOD [1] "pay" @12-12
                    PARAM_LIST
                      PARAM
                        ZVAL [1] "int"
                        ZVAL "orderId"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                      RETURN
                        NEW
                          ZVAL "App\\OrderPaid"
                          ARG_LIST
                            VAR
                              ZVAL "orderId"
                    SHARP_TYPE_ARGS
                      ZVAL "App\\OrderPaid"
                      ZVAL "Function<App.OrderPaid(int)>"
                    null
                null
                null
        "#}
    );
}

/// ```php
/// \Sharp\Internal\emit(new \App\OrderPaid(1));
/// \Sharp\Internal\emit($paid);
/// ```
///
/// Spec section 15: `emit` raises the event its expression gives, through the one function the engine adds, which runs
/// every listener of the event before it returns.
#[test]
fn emit_is_a_call_of_the_engine_emit() {
    let lowered = Lowered::with(
        "namespace App;\n\npublic class Order\n{\n    public void run(OrderPaid paid)\n    {\n        emit new OrderPaid(1);\n        emit paid;\n    }\n}\n",
        &[("src/OrderPaid.sharp", "namespace App;\n\npublic event OrderPaid(int orderId);\n")],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              CALL
                ZVAL "Sharp\\Internal\\emit"
                ARG_LIST
                  NEW
                    ZVAL "App\\OrderPaid"
                    ARG_LIST
                      ZVAL 1
              CALL
                ZVAL "Sharp\\Internal\\emit"
                ARG_LIST
                  VAR
                    ZVAL "paid"
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
                    SHARP_TYPE_ARGS
                      ZVAL [1] "int"
                      ZVAL "Function<int()>"
                    null
                  METHOD [1] "name" @9-12
                    PARAM_LIST
                    null
                    STMT_LIST
                      RETURN
                        ZVAL "thumbnail"
                    SHARP_TYPE_ARGS
                      ZVAL [1] "string"
                      ZVAL "Function<string()>"
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
/// An override is a property with `#[\Override]`, and an auto-property lowers as a field does, so PHP checks at link
/// time that the parent has the property. PHP refuses a type on a property whose parent's has none, so an override of
/// a property with no type has none either, as decision 028 writes. An override of a typed property keeps its type.
#[test]
fn an_override_of_a_property_with_no_type_has_no_type() {
    let lowered = Lowered::with(
        "class Order : Model\n{\n    protected override string table { get; set; } = \"orders\";\n    public override bool timestamps { get; set; } = false;\n}\n",
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

/// ```php
/// #[\Override] protected $table = 'orders' { set { $this->table = \trim($value); } }
/// abstract public string $kind { get; }
/// ```
///
/// An override with accessor bodies runs as PHP's hooks over the parent's property, untyped when the parent's is, and
/// an abstract property runs as PHP's, with a hook that has no body and no `readonly`.
#[test]
fn an_override_with_accessor_bodies_and_an_abstract_property_are_their_php_hooks() {
    let lowered = Lowered::with(
        "abstract class Order : Model\n{\n    protected override string table { get; set => field = global.trim(value); } = \"orders\";\n    public abstract string kind { get; }\n}\n",
        &[("src/Model.php", "<?php class Model { protected $table = ''; }")],
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
                    STMT_LIST
                      PROPERTY_HOOK "set" @3-3
                        null
                        null
                        STMT_LIST
                          ASSIGN
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "table"
                            CALL
                              ZVAL "trim"
                              ARG_LIST
                                VAR
                                  ZVAL "value"
                        null
                        null
                ATTRIBUTE_LIST
                  ATTRIBUTE_GROUP
                    ATTRIBUTE
                      ZVAL "Override"
                      null
              PROP_GROUP [65]
                ZVAL [1] "string"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "kind"
                    null
                    null
                    STMT_LIST
                      PROPERTY_HOOK "get" @4-4
                        null
                        null
                        null
                        null
                        null
                null
        "#}
    );
}

/// ```php
/// class Shop
/// {
///     public const int MAX = 3;
///     public int $views = 0;
///     public int $stock = 0;
///     public protected(set) int $id = 0;
///     public function secret(): int { return 1; }
/// }
/// ```
///
/// PHP has no namespace reach, so spec section 5's `internal` runs as public, and the analyzer checks who reaches it.
/// An `internal` class takes no flag, `internal set` takes no set visibility, and `internal init` takes the
/// `protected(set)` of every `init`, which only constructors run.
#[test]
fn internal_runs_as_public() {
    let lowered = Lowered::with(
        "internal class Shop\n{\n    internal const int MAX = 3;\n    public int views { get; internal set; } = 0;\n    internal int stock { get; set; } = 0;\n    public int id { get; internal init; } = 0;\n\n    internal int secret() => 1;\n}\n",
        &[],
    );
    let class = lowered.child(lowered.root(), 1);

    assert_eq!(
        lowered.render(class),
        indoc! {r#"
            CLASS "Shop" @1-9
              null
              null
              STMT_LIST
                CLASS_CONST_GROUP [1]
                  CLASS_CONST_DECL
                    CONST_ELEM
                      ZVAL "MAX"
                      ZVAL 3
                      null
                  null
                  ZVAL [1] "int"
                PROP_GROUP [1]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "views"
                      ZVAL 0
                      null
                      null
                  null
                PROP_GROUP [1]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "stock"
                      ZVAL 0
                      null
                      null
                  null
                PROP_GROUP [2049]
                  ZVAL [1] "int"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "id"
                      ZVAL 0
                      null
                      null
                  null
                METHOD [1] "secret" @8-8
                  PARAM_LIST
                  null
                  STMT_LIST
                    RETURN
                      ZVAL 1
                  SHARP_TYPE_ARGS
                    ZVAL [1] "int"
                    ZVAL "Function<int()>"
                  null
              null
              null
        "#}
    );
}

/// ```php
/// private bool ${'doubled#lazy'} = false;
/// public private(set) int $doubled { get { if (!$this->{'doubled#lazy'}) { $this->doubled = 2; $this->{'doubled#lazy'} = true; } return $this->doubled; } }
/// private bool ${'found#lazy'} = false;
/// public private(set) ?int $found { get { … } }
/// public function __clone(): void { $this->{'doubled#lazy'} = false; $this->{'found#lazy'} = false; }
/// ```
///
/// A `lazy` property keeps its first value in its own storage, which only its `get` hook writes, and a hidden flag
/// beside it says whether the body ran, since a kept null can't say so. A copy is a new value, so its `__clone`
/// clears each flag, and its first read computes the value again, spec section 6.5.
#[test]
fn a_lazy_property_keeps_its_first_value_in_its_storage() {
    let lowered = Lowered::with(
        "class Plan\n{\n    public lazy int doubled => 2;\n    public lazy int? found => null;\n}\n",
        &[],
    );
    let class = lowered.child(lowered.root(), 1);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [4]
                ZVAL [1] "bool"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "doubled#lazy"
                    ZVAL false
                    null
                    null
                null
              PROP_GROUP [4097]
                ZVAL [1] "int"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "doubled"
                    null
                    null
                    STMT_LIST
                      PROPERTY_HOOK "get" @3-3
                        null
                        null
                        STMT_LIST
                          IF
                            IF_ELEM
                              UNARY_OP [14]
                                PROP
                                  VAR
                                    ZVAL "this"
                                  ZVAL "doubled#lazy"
                              STMT_LIST
                                ASSIGN
                                  PROP
                                    VAR
                                      ZVAL "this"
                                    ZVAL "doubled"
                                  ZVAL 2
                                ASSIGN
                                  PROP
                                    VAR
                                      ZVAL "this"
                                    ZVAL "doubled#lazy"
                                  ZVAL true
                          RETURN
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "doubled"
                        null
                        null
                null
              PROP_GROUP [4]
                ZVAL [1] "bool"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "found#lazy"
                    ZVAL false
                    null
                    null
                null
              PROP_GROUP [4097]
                ZVAL [257] "int"
                PROP_DECL
                  PROP_ELEM
                    ZVAL "found"
                    null
                    null
                    STMT_LIST
                      PROPERTY_HOOK "get" @4-4
                        null
                        null
                        STMT_LIST
                          IF
                            IF_ELEM
                              UNARY_OP [14]
                                PROP
                                  VAR
                                    ZVAL "this"
                                  ZVAL "found#lazy"
                              STMT_LIST
                                ASSIGN
                                  PROP
                                    VAR
                                      ZVAL "this"
                                    ZVAL "found"
                                  ZVAL null
                                ASSIGN
                                  PROP
                                    VAR
                                      ZVAL "this"
                                    ZVAL "found#lazy"
                                  ZVAL true
                          RETURN
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "found"
                        null
                        null
                null
              METHOD [1] "__clone" @1-5
                PARAM_LIST
                null
                STMT_LIST
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "doubled#lazy"
                    ZVAL false
                  ASSIGN
                    PROP
                      VAR
                        ZVAL "this"
                      ZVAL "found#lazy"
                    ZVAL false
                ZVAL [1] "void"
                null
        "#}
    );
}

/// The members of `RushOrder : Order : Model`, where the PHP# class `Order` overrides `table` with `string?` and
/// `model` is the plain PHP class `Model`.
fn rush_order_members(model: &str) -> String {
    let lowered = Lowered::with(
        "class RushOrder : Order\n{\n    protected override string? table { get; set; } = \"rush_orders\";\n}\n",
        &[
            ("src/Model.php", model),
            (
                "src/Order.sharp",
                "public class Order : Model\n{\n    protected override string? table { get; set; } = \"orders\";\n}\n",
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
                    SHARP_TYPE_ARGS
                      TYPE_UNION
                        ZVAL [1] "int"
                        ZVAL [1] "float"
                      ZVAL "float|int"
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
/// return parent::pick($order);
/// ```
///
/// A generic `super` call carries its type arguments when PHP# declares the method, though the parent between is a plain
/// PHP class that inherits it.
#[test]
fn a_generic_super_call_through_a_plain_php_parent_carries_its_type_arguments() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App;

        public class Report : Plain
        {
            public Order run(Order order) => super.pick<Order>(order);
        }
    "},
        &[
            (
                "src/App/Base.sharp",
                "namespace App;\n\npublic class Base\n{\n    public T pick<T>(T item) => item;\n}\n",
            ),
            ("src/App/Plain.php", "<?php namespace App; class Plain extends Base {}"),
            ("src/App/Order.php", "<?php namespace App; final class Order {}"),
        ],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL [1] "parent"
                  ZVAL "pick"
                  ARG_LIST
                    VAR
                      ZVAL "order"
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "App.Order"
        "#}
    );
}

/// The body of `run`, which calls `wrapper.wrap(order)` through `Wrapper`, the plain PHP class-like `plain` declares,
/// whose `@template` method `wrap` the PHP# method `wrap` declares again in `SharpWrapper`.
fn a_call_through_plain_php(wrap: &str, plain: &str) -> String {
    let code = format!(
        "namespace App;\n\npublic class SharpWrapper : Wrapper\n{{\n    {wrap} Box<T> wrap<T>(T item) => new Box<T>();\n}}\n\npublic class Report\n{{\n    public Box<Order> run(Wrapper wrapper, Order order) => wrapper.wrap(order);\n}}\n"
    );

    Lowered::with(
        &code,
        &[
            ("src/App/Box.sharp", "namespace App;\n\npublic class Box<TValue>\n{\n    public Box() { }\n}\n"),
            ("src/App/Wrapper.php", plain),
            ("src/App/Order.php", "<?php namespace App; final class Order {}"),
        ],
    )
    .body_of("run")
}

/// The call resolves to plain PHP, so its result is checked too.
const CALL_WITH_TYPE_ARGUMENTS: &str = indoc! {r#"
    STMT_LIST
      RETURN
        VERIFY_TYPE_ARGS
          METHOD_CALL
            VAR
              ZVAL "wrapper"
            ZVAL "wrap"
            ARG_LIST
              VAR
                ZVAL "order"
              SHARP_TYPE_ARGS
                null
                ZVAL "App.Order"
          ZVAL "App.Box<App.Order>"
          ZVAL "App\\Wrapper::wrap()"
"#};

/// A generic call through a plain PHP `@template` interface carries the type arguments the checker found, since the
/// PHP# class that implements it reads them.
#[test]
fn a_generic_call_through_a_plain_php_interface_carries_its_type_arguments() {
    assert_eq!(
        a_call_through_plain_php(
            "public",
            "<?php namespace App; interface Wrapper {\n/**\n * @template T\n * @param T $item\n * @return Box<T>\n */\npublic function wrap(mixed $item): Box; }",
        ),
        CALL_WITH_TYPE_ARGUMENTS
    );
}

/// A generic call through a plain PHP parent's `@template` method carries the type arguments the checker found, since
/// the PHP# class that overrides it reads them.
#[test]
fn a_generic_call_through_a_plain_php_parent_carries_its_type_arguments() {
    assert_eq!(
        a_call_through_plain_php(
            "public override",
            "<?php namespace App; class Wrapper {\n/**\n * @template T\n * @param T $item\n * @return Box<T>\n */\npublic function wrap(mixed $item): Box { return new Box(); } }",
        ),
        CALL_WITH_TYPE_ARGUMENTS
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
                SHARP_TYPE_ARGS
                  TYPE [15]
                  ZVAL "Function<App.Tenant.DatabaseEntity(Lib.Row)>"
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
                SHARP_TYPE_ARGS
                  TYPE [271]
                  ZVAL "Function<App.Tenant.DatabaseEntity?(Lib.Row)>"
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
                SHARP_TYPE_ARGS
                  TYPE_UNION
                    TYPE [15]
                    ZVAL [1] "int"
                  ZVAL "Function<App.Tenant.DatabaseEntity|int(Lib.Row)>"
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
/// return 'total';
/// ```
///
/// `nameof(X.y)` is the string `'y'`: the checker proved `X` has the property, so the running program needs its name
/// alone.
#[test]
fn nameof_is_the_name_of_the_property() {
    assert_eq!(
        body_in(
            "string run(int extra)",
            "        return nameof(Calc.total);\n",
            &[("src/Lib/Calc.php", "<?php namespace Lib; final class Calc { public int $total = 0; }")]
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                ZVAL "total"
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
/// protected const LIMIT = \PHP_INT_MAX - 1;
/// ```
///
/// A constant is a class constant group of one constant, as php-src's grammar builds it: the constant list, no
/// attributes, then the type. `[1]` and `[2]` on the groups are `ZEND_ACC_PUBLIC` and `ZEND_ACC_PROTECTED`.
#[test]
fn a_class_constant_is_a_class_constant_group_of_one_constant() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public const int MAX = 3;\n    protected const LIMIT = global.PHP_INT_MAX - 1;\n}\n",
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
                        ZVAL "PHP_INT_MAX"
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
/// A static member's initial value is constant, so it is the default. `[32788]` is `ZEND_ACC_PRIVATE | ZEND_ACC_STATIC
/// | ZEND_ACC_FIELD`, and `[4113]` is `ZEND_ACC_PUBLIC | ZEND_ACC_STATIC | ZEND_ACC_PRIVATE_SET`.
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
            (32788, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"made\"\n    ZVAL 0\n    null\n    null\n".to_owned()),
            (4113, "PROP_DECL\n  PROP_ELEM\n    ZVAL \"last\"\n    ZVAL \"none\"\n    null\n    null\n".to_owned()),
        ]
    );
}

/// ```php
/// public static ?\App\Box $latest = null;
/// protected static ?\App\Box $inherited = null;
/// private static ?\App\Box $hidden = null;
/// ```
///
/// A static property whose type has type arguments has its PHP# type text around its PHP type, which PHP's own typed
/// property check reads on every write plain PHP makes. A `private` one has it too, since plain PHP writes it through
/// reflection.
#[test]
fn a_static_property_with_type_arguments_has_its_type_text_around_its_type() {
    let lowered = Lowered::new(
        "namespace App;\n\npublic class Box<T>\n{\n    public T? item() => null;\n}\n\npublic class Registry\n{\n    public static Box<int>? latest { get; set; } = null;\n\n    protected static Box<string>? inherited = null;\n\n    private static Box<int>? hidden = null;\n\n    public static Box<int>? last() => Registry.hidden;\n}\n",
    );
    let groups: Vec<String> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_PROP_GROUP)
        .map(|(index, _)| lowered.render(index as u32))
        .collect();

    assert_eq!(
        groups,
        [
            indoc! {r#"
                PROP_GROUP [17]
                  SHARP_TYPE_ARGS
                    ZVAL [256] "App\\Box"
                    ZVAL "App.Box<int>?"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "latest"
                      ZVAL null
                      null
                      null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [32786]
                  SHARP_TYPE_ARGS
                    ZVAL [256] "App\\Box"
                    ZVAL "App.Box<string>?"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "inherited"
                      ZVAL null
                      null
                      null
                  null
            "#},
            indoc! {r#"
                PROP_GROUP [32788]
                  SHARP_TYPE_ARGS
                    ZVAL [256] "App\\Box"
                    ZVAL "App.Box<int>?"
                  PROP_DECL
                    PROP_ELEM
                      ZVAL "hidden"
                      ZVAL null
                      null
                      null
                  null
            "#},
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
        "namespace App.Tenant;\n\nimport Lib.Bag;\n\nclass Report\n{\n    public int run(Bag bag)\n    {\n        return bag.size + bag.count() + global.strlen(global.gettype(bag.other)) + global.strlen(global.gettype(bag.untagged()));\n    }\n}\n",
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
/// $a = ($doubler === null ? null : $doubler(4));
/// return (($nullsafe#1 = $this->next()) === null ? null : $nullsafe#1($a ?? 0));
/// ```
///
/// Spec section 14.4: PHP has no `?->` that calls a function, so `f?.(x)` is a conditional on `f`, as a null-safe
/// property call is. A function other than a local goes into a hidden variable, so it runs once.
#[test]
fn a_null_safe_function_call_is_a_conditional_on_the_function() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public int? run(Function<int(int)>? doubler)\n    {\n        const a = doubler?.(4);\n        return this.next()?.(a ?? 0);\n    }\n\n    private Function<int(int)>? next() => null;\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    VAR
                      ZVAL "doubler"
                    ZVAL null
                  ZVAL null
                  CALL
                    VAR
                      ZVAL "doubler"
                    ARG_LIST
                      ZVAL 4
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
                  CALL
                    VAR
                      ZVAL "nullsafe#1"
                    ARG_LIST
                      COALESCE
                        VAR
                          ZVAL "a"
                        ZVAL 0
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
        "namespace App.Tenant;\n\nimport Lib.Bag;\n\nclass Report\n{\n    public int run(Bag bag)\n    {\n        return global.strlen(global.gettype(bag.handler())) + global.strlen(global.gettype(bag.untagged()));\n    }\n}\n",
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
/// $share = $item->share(...);
/// ```
///
/// A value of a type parameter bounded by an intersection reads a member any class of the intersection declares, as
/// the checker finds it.
#[test]
fn a_method_value_through_an_intersection_bound_is_the_method_one_of_its_classes_declares() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Lib.Entity;\nimport Lib.Shareable;\n\nclass Report\n{\n    public Function<string()> run<TItem : Entity & Shareable>(TItem item)\n    {\n        return item.share;\n    }\n}\n",
        &[
            (
                "src/Lib/Entity.php",
                "<?php namespace Lib; abstract class Entity { public function id(): int { return 1; } }",
            ),
            ("src/Lib/Shareable.php", "<?php namespace Lib; interface Shareable { public function share(): string; }"),
        ],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                METHOD_CALL
                  VAR
                    ZVAL "item"
                  ZVAL "share"
                  CALLABLE_CONVERT
        "#}
    );
}

/// ```php
/// return (function ($receiver) {
///     return function (...$arguments) use ($receiver) {
///         return $receiver->box(...$arguments, <App.Order>);
///     };
/// })($this);
/// ```
///
/// A method value of a generic method is a closure that calls the method with the type arguments the checker took
/// from its function type, on the receiver read once where the value is read. PHP's `$this->box(...)` would call it
/// with none.
#[test]
fn a_generic_method_value_is_a_closure_that_calls_the_method_with_its_type_arguments() {
    let lowered = Lowered::with(
        "namespace App;\n\nimport Lib.Order;\n\npublic class Box<T>\n{\n    public T? item() => null;\n}\n\nclass Report\n{\n    public Box<T> box<T>(T item) => new Box<T>();\n\n    public Function<Box<Order>(Order)> run() => this.box;\n}\n",
        &[("src/Lib/Order.php", "<?php namespace Lib; final class Order {}")],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  CLOSURE "" @14-14
                    PARAM_LIST
                      PARAM
                        null
                        ZVAL "receiver"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                      RETURN
                        CLOSURE "" @14-14
                          PARAM_LIST
                            PARAM [16]
                              null
                              ZVAL "arguments"
                              null
                              null
                              null
                              null
                          CLOSURE_USES
                            ZVAL "receiver"
                          STMT_LIST
                            RETURN
                              METHOD_CALL
                                VAR
                                  ZVAL "receiver"
                                ZVAL "box"
                                ARG_LIST
                                  UNPACK
                                    VAR
                                      ZVAL "arguments"
                                  SHARP_TYPE_ARGS
                                    null
                                    ZVAL "Lib.Order"
                          null
                          null
                    null
                    null
                  ARG_LIST
                    VAR
                      ZVAL "this"
        "#}
    );

    // Inside a generic method, the value passes the method's own type argument on, so both closures capture the method's
    // hidden local.
    let generic = Lowered::with(
        "namespace App;\n\npublic class Box<T>\n{\n    public T? item() => null;\n}\n\nclass Report\n{\n    public Box<T> box<T>(T item) => new Box<T>();\n\n    public Function<Box<T>(T)> boxer<T>() => this.box;\n}\n",
        &[],
    );
    let boxer = generic.body_of("boxer");

    assert_eq!(boxer.matches(r#"ZVAL "\0<sharp>\0types""#).count(), 2, "{boxer}");
    assert!(boxer.contains("SHARP_TYPE_ARGS\n"), "{boxer}");
    assert!(boxer.contains(r##"ZVAL "#0""##), "{boxer}");
}

/// ```php
/// $pick = $this->pick(...);
/// return $pick(5);
/// ```
///
/// A method value of a method that isn't generic stays PHP's first-class callable.
#[test]
fn a_method_value_of_a_method_that_is_not_generic_is_a_first_class_callable() {
    let lowered = Lowered::new(
        "namespace App;\n\nclass Report\n{\n    public int pick(int value) => value;\n\n    public int run()\n    {\n        let pick = this.pick;\n        return pick(5);\n    }\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "pick"
                METHOD_CALL
                  VAR
                    ZVAL "this"
                  ZVAL "pick"
                  CALLABLE_CONVERT
              RETURN
                CALL
                  VAR
                    ZVAL "pick"
                  ARG_LIST
                    ZVAL 5
        "#}
    );
}

/// ```php
/// return \Lib\Shapes::named()->name;
/// ```
///
/// A plain PHP object shape names no class, so a read of its property is a property read, and never a method value.
#[test]
fn a_property_of_a_plain_php_object_shape_is_a_property_read() {
    let lowered = Lowered::with(
        "namespace App;\n\nimport Lib.Shapes;\n\nclass Report\n{\n    public string run() => Shapes.named().name;\n}\n",
        &[(
            "src/Lib/Shapes.php",
            "<?php namespace Lib; final class Shapes { /** @return object{name: string} */ public static function named(): object { return (object) ['name' => 'a']; } }",
        )],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                PROP
                  STATIC_CALL
                    ZVAL "Lib\\Shapes"
                    ZVAL "named"
                    ARG_LIST
                  ZVAL "name"
        "#}
    );
}

/// ```php
/// return $status->label(...);
/// ```
///
/// An enum value's method value is the method's first-class callable, as a class value's is, not a read of a property
/// of that name.
#[test]
fn a_method_value_of_an_enum_value_is_a_first_class_callable() {
    let lowered = Lowered::new(
        "namespace App;\n\nenum Status\n{\n    case Active;\n\n    public string label() => \"active\";\n}\n\nclass Report\n{\n    public Function<string()> run(Status status) => status.label;\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                METHOD_CALL
                  VAR
                    ZVAL "status"
                  ZVAL "label"
                  CALLABLE_CONVERT
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
            "        const type = typeof(Calc);\n        return typeof(Calc).MAX + type.MAX + type.count + global.strlen(type.defaultTag());\n",
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
/// $type = \Sharp\Class::of($calc);
/// return \strlen($type::label());
/// ```
///
/// A class value of an enum, as `Class.of` gives for an enum case, calls the enum's static as a class's is called.
#[test]
fn a_static_call_through_a_class_value_of_an_enum_is_a_static_call_on_it() {
    assert_eq!(
        body_in(
            "int run(Calc calc)",
            "        const type = Class.of(calc);\n        return global.strlen(type.label());\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; enum Calc { case One; public static function label(): string { return 'calc'; } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "type"
                STATIC_CALL
                  ZVAL "Sharp\\Class"
                  ZVAL "of"
                  ARG_LIST
                    VAR
                      ZVAL "calc"
              RETURN
                CALL
                  ZVAL "strlen"
                  ARG_LIST
                    STATIC_CALL
                      VAR
                        ZVAL "type"
                      ZVAL "label"
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
            "        const type = extra > 0 ? typeof(Calc) : null;\n        const max = type?.MAX;\n        const count = type?.count;\n        const tag = type?.defaultTag();\n        return (max ?? 0) + (count ?? 0) + global.strlen(tag ?? \"\");\n",
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
/// return new \Sharp\Map([...\array_merge([], ...\array_map(null, $defaults->keys(), \iterator_to_array($defaults, false))), 'root', 0, ...…]);
/// ```
///
/// A `Map` literal with a spread is a new `Map` of each key followed by its value, each spread `Map`'s and each
/// entry's in order, so every key stays and a later one wins.
#[test]
fn a_map_spread_puts_each_key_and_value_of_the_literal_parts_in_order() {
    assert_eq!(
        body_in(
            "Map<string, int> run(Map<string, int> defaults, Map<string, int> overrides)",
            "        return [...defaults, \"root\": 0, ...overrides];\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "defaults"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "defaults"
                  NEW
                    ZVAL "Sharp\\Map"
                    ARG_LIST
                      CALL
                        ZVAL "array_merge"
                        ARG_LIST
                          ARRAY [3]
                          UNPACK
                            CALL
                              ZVAL "array_map"
                              ARG_LIST
                                ARROW_FUNC "" @7-7
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "key#1"
                                      null
                                      null
                                      null
                                      null
                                    PARAM
                                      null
                                      ZVAL "value#2"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  ARRAY [3]
                                    ARRAY_ELEM
                                      CAST [6]
                                        VAR
                                          ZVAL "key#1"
                                      null
                                    ARRAY_ELEM
                                      VAR
                                        ZVAL "value#2"
                                      null
                                  null
                                  null
                                CALL
                                  ZVAL "array_keys"
                                  ARG_LIST
                                    VAR
                                      ZVAL "defaults"
                                VAR
                                  ZVAL "defaults"
                  VAR
                    ZVAL "defaults"
              ASSIGN
                VAR
                  ZVAL "overrides"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "overrides"
                  NEW
                    ZVAL "Sharp\\Map"
                    ARG_LIST
                      CALL
                        ZVAL "array_merge"
                        ARG_LIST
                          ARRAY [3]
                          UNPACK
                            CALL
                              ZVAL "array_map"
                              ARG_LIST
                                ARROW_FUNC "" @7-7
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "key#1"
                                      null
                                      null
                                      null
                                      null
                                    PARAM
                                      null
                                      ZVAL "value#2"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  ARRAY [3]
                                    ARRAY_ELEM
                                      CAST [6]
                                        VAR
                                          ZVAL "key#1"
                                      null
                                    ARRAY_ELEM
                                      VAR
                                        ZVAL "value#2"
                                      null
                                  null
                                  null
                                CALL
                                  ZVAL "array_keys"
                                  ARG_LIST
                                    VAR
                                      ZVAL "overrides"
                                VAR
                                  ZVAL "overrides"
                  VAR
                    ZVAL "overrides"
              RETURN
                NEW
                  ZVAL "Sharp\\Map"
                  ARG_LIST
                    ARRAY [3]
                      UNPACK
                        CALL
                          ZVAL "array_merge"
                          ARG_LIST
                            ARRAY [3]
                            UNPACK
                              CALL
                                ZVAL "array_map"
                                ARG_LIST
                                  ZVAL null
                                  METHOD_CALL
                                    VAR
                                      ZVAL "defaults"
                                    ZVAL "keys"
                                    ARG_LIST
                                  CALL
                                    ZVAL "iterator_to_array"
                                    ARG_LIST
                                      VAR
                                        ZVAL "defaults"
                                      ZVAL false
                      ARRAY_ELEM
                        ZVAL "root"
                        null
                      ARRAY_ELEM
                        ZVAL 0
                        null
                      UNPACK
                        CALL
                          ZVAL "array_merge"
                          ARG_LIST
                            ARRAY [3]
                            UNPACK
                              CALL
                                ZVAL "array_map"
                                ARG_LIST
                                  ZVAL null
                                  METHOD_CALL
                                    VAR
                                      ZVAL "overrides"
                                    ZVAL "keys"
                                    ARG_LIST
                                  CALL
                                    ZVAL "iterator_to_array"
                                    ARG_LIST
                                      VAR
                                        ZVAL "overrides"
                                      ZVAL false
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
            "        const type = typeof(Calc);\n        const Function<string()> tag = type.defaultTag;\n        return global.strlen(tag());\n",
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
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "Function<Sharp.Position()>"
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
        "namespace App.Tenant;\n\nclass Report\n{\n    public string folder => Position.current().directory;\n    public string title { get; set { field = Position.current().function; } }\n\n    public Report()\n    {\n        this.title = \"\";\n    }\n}\n",
    );

    assert_eq!(positions(&lowered), ["5:29 App.Tenant.Report.folder", "6:46 App.Tenant.Report.title"]);
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
        .written()
        .into_iter()
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
/// $type = \Sharp\Class::of($this);
/// return 1;
/// ```
#[test]
fn environment_list_and_class_are_the_classes_of_the_sharp_namespace() {
    assert_eq!(
        body_in(
            RUN,
            "        const settings = new Environment();\n        const lines = List.wrap(extra);\n        const type = Class.of(this);\n        return 1;\n",
            &[common::ENVIRONMENT]
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
              ASSIGN
                VAR
                  ZVAL "type"
                STATIC_CALL
                  ZVAL "Sharp\\Class"
                  ZVAL "of"
                  ARG_LIST
                    VAR
                      ZVAL "this"
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
/// $b = \Sharp\Internal\equals($kind, $other); $b = !\Sharp\Internal\equals($kind, $other);
/// $found = array_find_key($kinds, (fn ($value#1) => fn ($element#2) => \Sharp\Internal\equals($element#2, $value#1))($kind)) !== null;
/// ```
///
/// Spec section 25: `==` compares two class values by their whole type, type arguments included, as C# compares two
/// `Type`s, so `typeof(Box<Order>) == typeof(Box<Invoice>)` is false though both name `App\Box`. The engine's
/// `\Sharp\Internal\equals` compares them, `!=` is its `!`, and a search of class values runs it for each element.
#[test]
fn equality_of_class_values_compares_their_whole_types() {
    assert_eq!(
        body_in(
            "bool run(Class<Order> kind, Class<Order> other, List<Class<Order>> kinds)",
            "        let b = kind == other;\n        b = kind != other;\n        return kinds.contains(kind);\n",
            &[("src/App/Tenant/Order.php", "<?php namespace App\\Tenant; final class Order {}")],
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "b"
                CALL
                  ZVAL "Sharp\\Internal\\equals"
                  ARG_LIST
                    VAR
                      ZVAL "kind"
                    VAR
                      ZVAL "other"
              ASSIGN
                VAR
                  ZVAL "b"
                UNARY_OP [14]
                  CALL
                    ZVAL "Sharp\\Internal\\equals"
                    ARG_LIST
                      VAR
                        ZVAL "kind"
                      VAR
                        ZVAL "other"
              RETURN
                BINARY_OP [17]
                  CALL
                    ZVAL "array_find_key"
                    ARG_LIST
                      VAR
                        ZVAL "kinds"
                      CALL
                        ARROW_FUNC "" @11-11
                          PARAM_LIST
                            PARAM
                              null
                              ZVAL "value#1"
                              null
                              null
                              null
                              null
                          null
                          ARROW_FUNC "" @11-11
                            PARAM_LIST
                              PARAM
                                null
                                ZVAL "element#2"
                                null
                                null
                                null
                                null
                            null
                            CALL
                              ZVAL "Sharp\\Internal\\equals"
                              ARG_LIST
                                VAR
                                  ZVAL "element#2"
                                VAR
                                  ZVAL "value#1"
                            null
                            null
                          null
                          null
                        ARG_LIST
                          VAR
                            ZVAL "kind"
                  ZVAL null
        "#}
    );
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
        "namespace Sharp.Text;\n\npublic static class Text\n{\n    public static string run(string x)\n    {\n        return @global.trim(x);\n    }\n}\n",
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
/// return \is_int(${'match#1'} = $sizes[0]) && \is_int(${'match#1'} = $counts['a'] ?? null);
/// ```
///
/// Spec section 12: a `List` index a pattern tests is a bare read, which throws on a missing index, and a `Map` index
/// reads a missing key as null, as `??` does.
#[test]
fn a_tested_list_index_is_read_bare_and_a_tested_map_key_quietly() {
    assert_eq!(
        body_in(
            "bool run(List<int?> sizes, Map<string, int> counts)",
            "        return sizes[0] is int && counts[\"a\"] is int;\n",
            &[]
        )
        .lines()
        .skip_while(|line| line.trim() != "RETURN")
        .map(str::trim_start)
        .collect::<Vec<_>>(),
        [
            "RETURN",
            "AND",
            "CALL",
            "ZVAL \"is_int\"",
            "ARG_LIST",
            "ASSIGN",
            "VAR",
            "ZVAL \"match#1\"",
            "DIM",
            "VAR",
            "ZVAL \"sizes\"",
            "ZVAL 0",
            "CALL",
            "ZVAL \"is_int\"",
            "ARG_LIST",
            "ASSIGN",
            "VAR",
            "ZVAL \"match#1\"",
            "COALESCE",
            "DIM",
            "VAR",
            "ZVAL \"counts\"",
            "ZVAL \"a\"",
            "ZVAL null",
        ]
    );
}

/// ```php
/// return $extra[0]?->value ?? $extra[1]?->total() ?? ($named['a'] ?? null)?->value;
/// ```
///
/// `?.` reads a missing key of the `Map` index directly before it as null, as `??` does (spec section 12), so that
/// index is the left side of a `COALESCE`. A `List` index is the bare index itself, which throws on a missing index.
#[test]
fn null_safe_access_reads_a_missing_map_key_as_null_and_a_list_index_bare() {
    assert_eq!(
        body_in(
            "int? run(List<Calc?> extra, Map<string, Calc?> named)",
            "        return extra[0]?.value ?? (extra[1])?.total() ?? named[\"a\"]?.value;\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { public int $value = 0; public function total(): int { return 0; } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "named"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "named"
                  NEW
                    ZVAL "Sharp\\Map"
                    ARG_LIST
                      CALL
                        ZVAL "array_merge"
                        ARG_LIST
                          ARRAY [3]
                          UNPACK
                            CALL
                              ZVAL "array_map"
                              ARG_LIST
                                ARROW_FUNC "" @7-7
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "key#1"
                                      null
                                      null
                                      null
                                      null
                                    PARAM
                                      null
                                      ZVAL "value#2"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  ARRAY [3]
                                    ARRAY_ELEM
                                      CAST [6]
                                        VAR
                                          ZVAL "key#1"
                                      null
                                    ARRAY_ELEM
                                      VAR
                                        ZVAL "value#2"
                                      null
                                  null
                                  null
                                CALL
                                  ZVAL "array_keys"
                                  ARG_LIST
                                    VAR
                                      ZVAL "named"
                                VAR
                                  ZVAL "named"
                  VAR
                    ZVAL "named"
              RETURN
                COALESCE
                  NULLSAFE_PROP
                    DIM
                      VAR
                        ZVAL "extra"
                      ZVAL 0
                    ZVAL "value"
                  COALESCE
                    NULLSAFE_METHOD_CALL
                      DIM
                        VAR
                          ZVAL "extra"
                        ZVAL 1
                      ZVAL "total"
                      ARG_LIST
                    NULLSAFE_PROP
                      COALESCE
                        DIM
                          VAR
                            ZVAL "named"
                          ZVAL "a"
                        ZVAL null
                      ZVAL "value"
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
///     $inner = \PHP_INT_MAX;
///     $own = LIMIT;
/// }
/// ```
///
/// `global.PHP_INT_MAX` names PHP's constant by its full name, `ZEND_NAME_FQ`, which is 0, spec section 23. A bare
/// name is the code's own constant, and `[1]` on its name is `ZEND_NAME_NOT_FQ`: the engine looks it up in the
/// file's namespace.
#[test]
fn a_block_is_a_statement_list_and_a_constant_is_looked_up_by_the_name_it_is_read_by() {
    let limit = ("src/App/Tenant/constants.php", "<?php\n\nnamespace App\\Tenant;\n\nconst LIMIT = 3;\n");

    assert_eq!(
        body_in(
            RUN,
            "        {\n            let inner = global.PHP_INT_MAX;\n            let own = LIMIT;\n        }\n        return 1;\n",
            &[limit]
        ),
        indoc! {r#"
            STMT_LIST
              STMT_LIST
                ASSIGN
                  VAR
                    ZVAL "inner"
                  CONST
                    ZVAL "PHP_INT_MAX"
                ASSIGN
                  VAR
                    ZVAL "own"
                  CONST
                    ZVAL [1] "LIMIT"
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
/// while ($again) {
///     continue;
/// }
/// while (true) {
///     break;
/// }
/// ```
///
/// A `break` or `continue` without a level has a null depth.
#[test]
fn break_and_continue_have_a_null_depth() {
    assert_eq!(
        body_in(
            "int run(bool again)",
            "        while (again) {\n            continue;\n        }\n        while (true) {\n            break;\n        }\n        return 1;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              WHILE
                VAR
                  ZVAL "again"
                STMT_LIST
                  CONTINUE
                    null
              WHILE
                ZVAL true
                STMT_LIST
                  BREAK
                    null
              RETURN
                ZVAL 1
        "#}
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
/// foreach (<check Map<int, int>>(\Lib\Calc::make(2)) as $key#1 => $value#2) {
///     $entry = [$key#1, $value#2];
///     $extra += $entry[1];
/// }
/// foreach (<check Map<int, int>>(\Lib\Calc::make(3)) as $key => $value) {
/// }
/// ```
///
/// `FOREACH` takes the collection, the value variable, the key variable or null, and the body. The map a plain PHP
/// docblock claims is checked entry by entry first, and one loop variable over it is each entry.
#[test]
fn for_of_loops_are_foreach_nodes_with_the_value_before_the_key() {
    assert_eq!(
        body_in(
            RUN,
            "        for (const entry of Calc.make(2)) {\n            extra += entry[1];\n        }\n        for (let [key, value] of Calc.make(3)) {\n        }\n        return extra;\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { /** @return array<int, int> */ public static function make(int $n): array { return [$n => $n]; } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              FOREACH
                VERIFY_TYPE_ARGS
                  STATIC_CALL
                    ZVAL "Lib\\Calc"
                    ZVAL "make"
                    ARG_LIST
                      ZVAL 2
                  ZVAL "Map<int, int>"
                  ZVAL "Lib\\Calc::make()"
                VAR
                  ZVAL "value#2"
                VAR
                  ZVAL "key#1"
                STMT_LIST
                  ASSIGN
                    VAR
                      ZVAL "entry"
                    ARRAY [3]
                      ARRAY_ELEM
                        VAR
                          ZVAL "key#1"
                        null
                      ARRAY_ELEM
                        VAR
                          ZVAL "value#2"
                        null
                  STMT_LIST
                    ASSIGN_OP [1]
                      VAR
                        ZVAL "extra"
                      DIM
                        VAR
                          ZVAL "entry"
                        ZVAL 1
              FOREACH
                VERIFY_TYPE_ARGS
                  STATIC_CALL
                    ZVAL "Lib\\Calc"
                    ZVAL "make"
                    ARG_LIST
                      ZVAL 3
                  ZVAL "Map<int, int>"
                  ZVAL "Lib\\Calc::make()"
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
/// $names = \VERIFY_TYPE_ARGS(\Lib\Calc::lazy(), 'Iterable<string>', 'Lib\Calc::lazy()');
/// return Report::tally(\VERIFY_TYPE_ARGS(\Lib\Calc::lazy(), …)) + \Lib\Calc::lazy()->count();
/// ```
///
/// Spec section 12: a lazy plain PHP source is checked element by element as it is read. A call that gives an object
/// of a plain PHP class that is a `Traversable`, as a generator or Laravel's `LazyCollection`, is checked in a
/// `VERIFY_TYPE_ARGS` of the place's type where an `Iterable<T>` holds it, a local or a PHP# parameter, which puts a
/// checking `Sequence` in its place. The engine checks the arguments of a call from plain PHP only, so a call from
/// PHP# checks the one it passes. Until then the value keeps its own class, so a call of its own `count` takes no check.
#[test]
fn a_lazy_plain_php_source_is_checked_where_an_iterable_holds_it() {
    assert_eq!(
        body_in(
            "int run()",
            "        Iterable<string> names = Calc.lazy();\n        return Report.tally(Calc.lazy()) + Calc.lazy().count();\n    }\n\n    private static int tally(Iterable<string> names)\n    {\n        return 0;\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib;\n/**\n * @template TKey of array-key\n * @template TValue\n * @implements \\IteratorAggregate<TKey, TValue>\n */\nfinal class Lazy implements \\IteratorAggregate { public function getIterator(): \\Generator { yield 'a'; } public function count(): int { return 1; } }\nfinal class Calc { /** @return Lazy<int, string> */ public static function lazy(): Lazy { return new Lazy(); } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "names"
                VERIFY_TYPE_ARGS
                  STATIC_CALL
                    ZVAL "Lib\\Calc"
                    ZVAL "lazy"
                    ARG_LIST
                  ZVAL "Iterable<string>"
                  ZVAL "Lib\\Calc::lazy()"
              RETURN
                BINARY_OP [1]
                  STATIC_CALL
                    ZVAL "App\\Tenant\\Report"
                    ZVAL "tally"
                    ARG_LIST
                      VERIFY_TYPE_ARGS
                        STATIC_CALL
                          ZVAL "Lib\\Calc"
                          ZVAL "lazy"
                          ARG_LIST
                        ZVAL "Iterable<string>"
                        ZVAL "Lib\\Calc::lazy()"
                  METHOD_CALL
                    STATIC_CALL
                      ZVAL "Lib\\Calc"
                      ZVAL "lazy"
                      ARG_LIST
                    ZVAL "count"
                    ARG_LIST
        "#}
    );
}

/// ```php
/// $pair = [1, 'a']; [$count, $label] = $pair;
/// ```
///
/// Spec section 30: a tuple is a PHP list while the code runs, so a tuple literal is a list literal, and unpacking one
/// is PHP's list assignment, an `ASSIGN` of an `ARRAY` of `ARRAY_ELEM`s, as php-src's grammar builds `[$a, $b] = $c`.
#[test]
fn an_unpacked_tuple_is_a_list_assignment() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public void run()\n    {\n        [int, string] pair = [1, \"a\"];\n        const [count, label] = pair;\n    }\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "pair"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL 1
                    null
                  ARRAY_ELEM
                    ZVAL "a"
                    null
              ASSIGN
                ARRAY [3]
                  ARRAY_ELEM
                    VAR
                      ZVAL "count"
                    null
                  ARRAY_ELEM
                    VAR
                      ZVAL "label"
                    null
                VAR
                  ZVAL "pair"
        "#}
    );
}

/// ```php
/// foreach ($plans as $key#1 => $value#2) { $entry = [$key#1, $value#2]; }
/// foreach ($rows as [$name, $count]) { }
/// foreach ($plans as $name => $plan) { }
/// ```
///
/// Spec sections 17 and 30: a loop variable is each element, and a `Map`'s element is its entry, so one variable over a
/// `Map` is the tuple of each key and value, which a `Sharp\Map` and a PHP array both give a `foreach` of both. `[a, b]`
/// unpacks each two-element tuple of any other collection, as PHP's `foreach ($rows as [$a, $b])` does, and reads a
/// `Map`'s key and value.
#[test]
fn a_loop_over_a_map_reads_each_entry_and_a_loop_unpacks_each_tuple() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public void run(Map<string, int> plans, List<[string, int]> rows)\n    {\n        for (const entry of plans) {\n        }\n        for (const [name, count] of rows) {\n        }\n        for (const [name, plan] of plans) {\n        }\n    }\n}\n",
    );

    // The class's generated `parse` loops over the fields after them.
    let loops: Vec<String> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_FOREACH)
        .map(|(index, _)| lowered.render(index as u32))
        .take(3)
        .collect();

    assert_eq!(
        loops,
        [
            indoc! {r#"
                FOREACH
                  VAR
                    ZVAL "plans"
                  VAR
                    ZVAL "value#2"
                  VAR
                    ZVAL "key#1"
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "entry"
                      ARRAY [3]
                        ARRAY_ELEM
                          VAR
                            ZVAL "key#1"
                          null
                        ARRAY_ELEM
                          VAR
                            ZVAL "value#2"
                          null
                    STMT_LIST
            "#},
            indoc! {r#"
                FOREACH
                  VAR
                    ZVAL "rows"
                  ARRAY [3]
                    ARRAY_ELEM
                      VAR
                        ZVAL "name"
                      null
                    ARRAY_ELEM
                      VAR
                        ZVAL "count"
                      null
                  null
                  STMT_LIST
            "#},
            indoc! {r#"
                FOREACH
                  VAR
                    ZVAL "plans"
                  VAR
                    ZVAL "plan"
                  VAR
                    ZVAL "name"
                  STMT_LIST
            "#},
        ]
    );
}

/// ```php
/// $entries = $entries instanceof \Sharp\Map ? \Sharp\Sequence::from($entries) : $entries;
/// yield from \Sharp\Sequence::from($plans);
/// return \Sharp\Sequence::from($plans);
/// $kept = \Sharp\Sequence::from($plans);
/// ```
///
/// Spec section 12: a `Map<TKey, TValue>` is an `Iterable<[TKey, TValue]>` of its entries, so a `Map` that lands in an
/// `Iterable` place, a local, a return or `yield ...`, is the `Sequence` that reads its entries. A parameter takes any
/// caller's `Map` the same way before the body runs.
#[test]
fn a_map_that_lands_in_an_iterable_gives_its_entries() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public int count(Iterable<[string, int]> entries) { return 1; }\n\n    public Iterable<[string, int]> all(Map<string, int> plans) { yield ...plans; }\n\n    public Iterable<[string, int]> some(Map<string, int> plans) { return plans; }\n\n    public void run(Map<string, int> plans)\n    {\n        Iterable<[string, int]> kept = plans;\n    }\n}\n",
    );

    assert_eq!(
        lowered.body_of("count"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "entries"
                CONDITIONAL
                  INSTANCEOF
                    VAR
                      ZVAL "entries"
                    ZVAL "Sharp\\Map"
                  STATIC_CALL
                    ZVAL "Sharp\\Sequence"
                    ZVAL "from"
                    ARG_LIST
                      VAR
                        ZVAL "entries"
                  VAR
                    ZVAL "entries"
              RETURN
                ZVAL 1
        "#}
    );
    // Each body first makes the object of a PHP array plain PHP passes as `plans`, and its last statement takes the
    // entries, a child of that statement.
    let entries = concat!(
        "    STATIC_CALL\n",
        "      ZVAL \"Sharp\\\\Sequence\"\n",
        "      ZVAL \"from\"\n",
        "      ARG_LIST\n",
        "        VAR\n",
        "          ZVAL \"plans\"\n",
    );
    for (method, statement) in
        [("all", "  YIELD_FROM\n"), ("some", "  RETURN\n"), ("run", "  ASSIGN\n    VAR\n      ZVAL \"kept\"\n")]
    {
        let body = lowered.body_of(method);

        assert!(body.ends_with(&format!("{statement}{entries}")), "{method}:\n{body}");
    }
}

/// ```php
/// return $summary[1] . $pair[0];
/// ```
///
/// Spec section 30: a named element of a tuple, which runs as a PHP list, is the index read of its position, and an
/// index read of a tuple is one already.
#[test]
fn a_named_tuple_element_is_an_index_read_of_its_position() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public string run([int total, string label] summary, [string, int] pair)\n    {\n        return summary.label + pair[0];\n    }\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [8]
                  DIM
                    VAR
                      ZVAL "summary"
                    ZVAL 1
                  DIM
                    VAR
                      ZVAL "pair"
                    ZVAL 0
        "#}
    );
}

/// ```php
/// yield from $pending; yield $latest; yield fn($x) => $x; return;
/// ```
///
/// `yield value;` is a `YIELD` of its value and a null key, alone as a statement, as php-src's grammar builds
/// `yield $value;`, and `yield ...other;` is PHP's `yield from`, a `YIELD_FROM` of `other`. `yield x => x;` yields a
/// lambda, because `=>` is the lambda arrow in PHP#.
#[test]
fn yield_is_a_yield_statement_and_yield_spread_is_yield_from() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass OrderImport\n{\n    public Iterable<Any> run(List<int> pending, int latest)\n    {\n        yield ...pending;\n        yield latest;\n        yield x => x;\n        return;\n    }\n}\n",
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              YIELD_FROM
                VAR
                  ZVAL "pending"
              YIELD
                VAR
                  ZVAL "latest"
                null
              YIELD
                ARROW_FUNC "" @9-9
                  PARAM_LIST
                    PARAM
                      null
                      ZVAL "x"
                      null
                      null
                      null
                      null
                  null
                  VAR
                    ZVAL "x"
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "Function<Any?(Any?)>"
                  null
                null
              RETURN
                null
        "#}
    );
}

/// A method that yields is a generator: `[16777217]` is `ZEND_ACC_PUBLIC | ZEND_ACC_GENERATOR`, which php-src's grammar
/// puts on a function whose body holds a `yield`, so the engine compiles it to start with `GENERATOR_CREATE`. A yield
/// nested in a loop or an `if` counts, and a method that returns an `Iterable<T>` without a yield stays `[1]`.
#[test]
fn a_method_that_yields_is_a_generator() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass OrderImport\n{\n    public Iterable<int> lines(List<int> rows)\n    {\n        for (const row of rows) {\n            if (row > 0) {\n                yield row;\n            }\n        }\n    }\n\n    public Iterable<int> all(List<int> rows)\n    {\n        yield ...rows;\n    }\n\n    public Iterable<int> kept(List<int> rows)\n    {\n        return rows;\n    }\n}\n",
    );

    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
    let methods: Vec<String> = lowered
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_METHOD)
        .map(|node| format!("{} [{}]", lowered.text(node.text), node.attr))
        .collect();
    assert_eq!(methods, ["lines [16777217]", "all [16777217]", "kept [1]", "parse [17]", "tryParse [17]"]);
}

/// ```php
/// $code = $extra > 0 ? $extra : throw new \App\Tenant\Failure(0);
/// throw new \App\Tenant\Failure($code);
/// ```
///
/// `throw` is an expression, and a `throw` statement is that expression alone.
#[test]
fn throw_is_a_throw_expression() {
    assert_eq!(
        body_in(
            RUN,
            "        let code = extra > 0 ? extra : throw new Failure(0);\n        throw new Failure(code);\n",
            &[(
                "src/App/Tenant/Failure.php",
                "<?php namespace App\\Tenant; final class Failure extends \\Exception { public function __construct(int $code) { parent::__construct('', $code); } }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "code"
                CONDITIONAL
                  GREATER
                    VAR
                      ZVAL "extra"
                    ZVAL 0
                  VAR
                    ZVAL "extra"
                  THROW
                    NEW
                      ZVAL "App\\Tenant\\Failure"
                      ARG_LIST
                        ZVAL 0
              THROW
                NEW
                  ZVAL "App\\Tenant\\Failure"
                  ARG_LIST
                    VAR
                      ZVAL "code"
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
/// try {
///     $extra += 1;
/// } catch (\Throwable $caught#1) {
///     if (sharp_is_type($caught#1, 'App.Failure<int>')) {
///         $numbers = $caught#1;
///         { $extra = 1; }
///     } elseif (sharp_is_type($caught#1, 'App.Failure<string>') || $caught#1 instanceof \LogicException) {
///         $texts = $caught#1;
///         { throw $texts; }
///     } elseif ($caught#1 instanceof \Exception) {
///         $extra = 0;
///     } else {
///         throw $caught#1;
///     }
/// } finally {
///     $extra -= 1;
/// }
/// ```
///
/// A `try` with a catch clause of a generic class catches every `Throwable` into a hidden variable, then tests each
/// clause in the order the code writes them, with the `SHARP_IS_TYPE` an `is` of a generic class runs. The first that
/// matches gives its variable the value and runs its block. When none matches, the value is thrown again, so a `try`
/// around this one still catches it.
#[test]
fn a_try_with_a_generic_catch_clause_catches_throwable_and_tests_each_clause_in_order() {
    let lowered = Lowered::new(indoc! {"
        namespace App;

        import Exception;
        import LogicException;
        import RuntimeException;

        public class Failure<TItem> : RuntimeException
        {
        }

        public class Report
        {
            public int run(int extra)
            {
                try {
                    extra += 1;
                } catch (Failure<int> numbers) {
                    extra = 1;
                } catch (Failure<string> | LogicException texts) {
                    throw texts;
                } catch (Exception) {
                    extra = 0;
                } finally {
                    extra -= 1;
                }
                return extra;
            }
        }
    "});

    assert_eq!(
        lowered.body(),
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
                      ZVAL "Throwable"
                    ZVAL "caught#1"
                    STMT_LIST
                      IF
                        IF_ELEM
                          SHARP_IS_TYPE
                            VAR
                              ZVAL "caught#1"
                            ZVAL "App.Failure<int>"
                          STMT_LIST
                            ASSIGN
                              VAR
                                ZVAL "numbers"
                              VAR
                                ZVAL "caught#1"
                            STMT_LIST
                              ASSIGN
                                VAR
                                  ZVAL "extra"
                                ZVAL 1
                        IF_ELEM
                          OR
                            SHARP_IS_TYPE
                              VAR
                                ZVAL "caught#1"
                              ZVAL "App.Failure<string>"
                            INSTANCEOF
                              VAR
                                ZVAL "caught#1"
                              ZVAL "LogicException"
                          STMT_LIST
                            ASSIGN
                              VAR
                                ZVAL "texts"
                              VAR
                                ZVAL "caught#1"
                            STMT_LIST
                              THROW
                                VAR
                                  ZVAL "texts"
                        IF_ELEM
                          INSTANCEOF
                            VAR
                              ZVAL "caught#1"
                            ZVAL "Exception"
                          STMT_LIST
                            ASSIGN
                              VAR
                                ZVAL "extra"
                              ZVAL 0
                        IF_ELEM
                          null
                          STMT_LIST
                            THROW
                              VAR
                                ZVAL "caught#1"
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

/// A `try` without a catch clause of a generic class lowers as it does in a file that has none, beside a `try` that
/// has one: the choice is made for each `try`.
#[test]
fn a_try_without_a_generic_catch_clause_keeps_its_catch_list_beside_one_with_it() {
    let source = |guarded: &str| {
        format!(
            "namespace App;\n\nimport Exception;\nimport RuntimeException;\n\npublic class Failure<TItem> : RuntimeException\n{{\n}}\n\npublic class Report\n{{\n    public int plain(int extra)\n    {{\n        try {{\n            extra += 1;\n        }} catch (RuntimeException failure) {{\n            throw failure;\n        }} catch (Exception) {{\n        }}\n        return extra;\n    }}\n{guarded}}}\n"
        )
    };
    let guarded = "\n    public int guarded(int extra)\n    {\n        try {\n            extra += 1;\n        } catch (Failure<int>) {\n        }\n        return extra;\n    }\n";
    let alone = Lowered::new(&source(""));

    assert_eq!(Lowered::new(&source(guarded)).body_of("plain"), alone.body_of("plain"));
    assert!(alone.body_of("plain").contains("CATCH\n        NAME_LIST\n          ZVAL \"RuntimeException\""));
}

/// A catch clause of a type parameter tests the type argument the method runs with, as `is` does, written as `new`
/// writes a method's type argument.
#[test]
fn a_catch_clause_of_a_type_parameter_tests_its_type_argument() {
    let lowered = Lowered::new(indoc! {"
        namespace App;

        import RuntimeException;

        public class Report
        {
            public int run<TError : RuntimeException>(int extra)
            {
                try {
                    extra += 1;
                } catch (TError) {
                    extra = 0;
                }
                return extra;
            }
        }
    "});

    assert!(
        lowered.body_of("run").contains(
            "IF_ELEM\n              SHARP_IS_TYPE\n                VAR\n                  ZVAL \"caught#1\"\n                ZVAL \"#0\"\n"
        ),
        "{}",
        lowered.body_of("run")
    );
}

/// ```php
/// return \strlen(\Vendor\Pdf\render(\sprintf("%d", \count($extra, mode: 0))));
/// ```
///
/// Spec section 23 calls a plain PHP function through its namespace: `global.strlen(…)` a function of PHP's root
/// namespace, and `Pdf.render(…)` after `import Vendor.Pdf;` a function of that namespace. Each is a call of the
/// function by its full name, with `ZEND_NAME_FQ`, which is 0.
#[test]
fn a_call_through_a_namespace_is_a_call_of_its_plain_php_function() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Vendor.Pdf;\n\nclass Report\n{\n    public int run(List<int> extra)\n    {\n        return global.strlen(Pdf.render(global.sprintf(\"%d\", global.count(extra, mode: 0))));\n    }\n}\n",
        &[(
            "src/Vendor/Pdf/functions.php",
            "<?php namespace Vendor\\Pdf; function render(string $html): string { return $html; }",
        )],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "strlen"
                  ARG_LIST
                    CALL
                      ZVAL "Vendor\\Pdf\\render"
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
/// return \Vendor\Pdf\DEFAULT_DPI + \PHP_INT_SIZE;
/// ```
///
/// Spec section 23 reads a plain PHP constant through its namespace, as it calls a function: `Pdf.DEFAULT_DPI` after
/// `import Vendor.Pdf;`, and `global.PHP_INT_SIZE` from PHP's root namespace. Each is a read of the constant by its
/// full name, with `ZEND_NAME_FQ`, which is 0.
#[test]
fn a_read_through_a_namespace_is_a_read_of_its_plain_php_constant() {
    let lowered = Lowered::with(
        "namespace App.Tenant;\n\nimport Vendor.Pdf;\n\nclass Report\n{\n    public int run()\n    {\n        return Pdf.DEFAULT_DPI + global.PHP_INT_SIZE;\n    }\n}\n",
        &[("src/Vendor/Pdf/constants.php", "<?php namespace Vendor\\Pdf; const DEFAULT_DPI = 300;")],
    );

    assert_eq!(
        lowered.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [1]
                  CONST
                    ZVAL "Vendor\\Pdf\\DEFAULT_DPI"
                  CONST
                    ZVAL "PHP_INT_SIZE"
        "#}
    );
}

/// ```php
/// return \run($extra);
/// ```
///
/// Spec section 4 writes every member as `this.m()` or `Class.m()`, so `global.run(extra)` inside `run` calls the
/// global function `run`, not the method.
#[test]
fn a_global_call_named_like_a_method_of_its_class_is_a_call_of_the_global_function() {
    assert_eq!(
        body_in(
            RUN,
            "        return global.run(extra);\n",
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
/// exit($extra);
/// ```
///
/// PHP 8.4 makes `exit` a function, so `global.exit(…)` is a call of the global function `exit`, named with
/// `ZEND_NAME_FQ`, which is 0, as php-src's grammar builds `exit(…)`.
#[test]
fn exit_is_a_call_of_the_global_function_exit() {
    assert_eq!(
        body("        global.exit(extra);\n"),
        indoc! {r#"
            STMT_LIST
              CALL
                ZVAL "exit"
                ARG_LIST
                  VAR
                    ZVAL "extra"
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
            "        const label = `Order ${extra}: ${global.strlen(\"x\")}!`;\n        const alone = `${extra}`;\n        const plain = `plain \\x41`;\n        return extra;\n"
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
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "Function<int(int)>"
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
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "Function<int(int)>"
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
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "Function<void(int)>"
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
                  SHARP_TYPE_ARGS
                    null
                    ZVAL "Function<int()>"
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
                      SHARP_TYPE_ARGS
                        null
                        ZVAL "Function<void()>"
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
///     #[\Lib\Entity(true, \PHP_INT_MAX)]
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
        "namespace App.Tenant;\n\nimport Lib.Field;\nimport Lib.Entity;\n\n[Entity(label: \"Orders\", order: 2 * 3), Searchable]\n[Entity(null)]\nclass Report\n{\n    [Field] private int count = 0;\n\n    public Report([Field(-1.5)] public int total { get; })\n    {\n    }\n\n    [Entity(true, global.PHP_INT_MAX)]\n    public void run([Field] int extra)\n    {\n    }\n}\n",
        &[common::FIELD, common::ENTITY, common::SEARCHABLE],
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.root(), 2)),
        indoc! {r#"
            CLASS "Report" @8-20
              null
              null
              STMT_LIST
                PROP_GROUP [32772]
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
                  SHARP_TYPE_ARGS
                    ZVAL [1] "void"
                    ZVAL "Function<void(int)>"
                  ATTRIBUTE_LIST
                    ATTRIBUTE_GROUP
                      ATTRIBUTE
                        ZVAL "Lib\\Entity"
                        ARG_LIST
                          ZVAL true
                          CONST
                            ZVAL "PHP_INT_MAX"
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
/// public \Sharp\Map $stock;    #[\Sharp\Crossing(['map', 'string', null])]
/// public \Sharp\Map $names;    #[\Sharp\Crossing(['map', null, null])]
/// public \Sharp\Map $counts;   #[\Sharp\Crossing(['map', 'App\Tenant\Status', null])]
/// public ?\Sharp\Map $spare;   #[\Sharp\Crossing(['map', 'string', null])]
/// public static \Sharp\Set $tags;   #[\Sharp\Crossing(['set', null])]
/// public \Sharp\Map $shelves;  #[\Sharp\Crossing(['map', 'string', ['list', ['set', null]]])]
/// public int $count;           no attribute
/// ```
///
/// A property PHP# declares a `Map` or a `Set` carries `#[\Sharp\Crossing]`, the plan the engine crosses a PHP array
/// plain PHP writes there by: an `int` key back as a `string` for a key type of `string`, a backing value back as its
/// case, and each `Map` or `Set` it holds, at any depth, crossed too.
#[test]
fn a_map_or_set_property_carries_the_plan_a_php_array_crosses_by() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nenum Status : string\n{\n    case Open = \"open\";\n}\n\nclass Ledger\n{\n    public Map<string, int> stock { get; set; } = [:];\n    public Map<int, string> names { get; set; } = [:];\n    public Map<Status, int> counts { get; set; } = [:];\n    public Map<string, int>? spare { get; set; } = null;\n    public static Set<string> tags { get; set; } = [];\n    public Map<string, List<Set<int>>> shelves { get; set; } = [:];\n    public int count { get; set; } = 0;\n}\n",
    );
    // The attribute list of `#[\Sharp\Crossing(plan)]`, with the plan's rendering indented under its `ARG_LIST`.
    let crossing = |plan: &str| {
        let mut attribute = String::from(
            "ATTRIBUTE_LIST\n  ATTRIBUTE_GROUP\n    ATTRIBUTE\n      ZVAL \"Sharp\\\\Crossing\"\n      ARG_LIST\n",
        );
        for line in plan.lines() {
            attribute.push_str("        ");
            attribute.push_str(line);
            attribute.push('\n');
        }

        attribute
    };
    let map_by_string = indoc! {r#"
        ARRAY [3]
          ARRAY_ELEM
            ZVAL "map"
            null
          ARRAY_ELEM
            ZVAL "string"
            null
          ARRAY_ELEM
            ZVAL null
            null
    "#};
    let attributes: Vec<String> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_PROP_GROUP)
        .map(|(index, _)| lowered.render(lowered.child(index as u32, 2)))
        .collect();

    assert_eq!(lowered.diagnostics(), Vec::<String>::new(), "the source lowers");
    assert_eq!(
        attributes,
        [
            crossing(map_by_string),
            crossing(indoc! {r#"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "map"
                    null
                  ARRAY_ELEM
                    ZVAL null
                    null
                  ARRAY_ELEM
                    ZVAL null
                    null
            "#}),
            crossing(indoc! {r#"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "map"
                    null
                  ARRAY_ELEM
                    ZVAL "App\\Tenant\\Status"
                    null
                  ARRAY_ELEM
                    ZVAL null
                    null
            "#}),
            crossing(map_by_string),
            crossing(indoc! {r#"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "set"
                    null
                  ARRAY_ELEM
                    ZVAL null
                    null
            "#}),
            crossing(indoc! {r#"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL "map"
                    null
                  ARRAY_ELEM
                    ZVAL "string"
                    null
                  ARRAY_ELEM
                    ARRAY [3]
                      ARRAY_ELEM
                        ZVAL "list"
                        null
                      ARRAY_ELEM
                        ARRAY [3]
                          ARRAY_ELEM
                            ZVAL "set"
                            null
                          ARRAY_ELEM
                            ZVAL null
                            null
                        null
                    null
            "#}),
            "null\n".to_string(),
        ]
    );
}

/// `Lib\Calc`, a plain PHP class whose methods take and give PHP arrays and closures that take them.
const ARRAYS: (&str, &str) = (
    "src/Lib/Calc.php",
    "<?php namespace Lib; final class Calc {
        public static function total(?array $items): int { return 0; }
        public static function sum(array ...$items): int { return 0; }
        /** @param array<int, string> $items */
        public static function sort(array &$items): void {}
        /** @param-out array<int, string> $value */
        public static function fill(&$value): void {}
        /** @param-out int $value */
        public static function bump(&$value): void {}
        /** @return array<int, string> */
        public static function names(): array { return []; }
        /** @return \\Closure(array $items, array ...$rest): int */
        public static function counter(): \\Closure { return fn (array $items, array ...$rest): int => 0; }
        public static function plain(): \\Closure { return fn () => 0; }
    }",
);

/// The statements of `run` at `positions` in `code`, lowered beside `ARRAYS` and the `library` files.
fn statements_at(code: &str, library: &[(&str, &str)], positions: std::ops::Range<u32>) -> Vec<String> {
    let files: Vec<(&str, &str)> = std::iter::once(ARRAYS).chain(library.iter().copied()).collect();
    let lowered = Lowered::with(code, &files);
    let body = lowered.child(lowered.method_named("run"), 2);

    positions.map(|position| lowered.render(lowered.child(body, position))).collect()
}

/// ```php
/// \Lib\Calc::total(\iterator_to_array($stock));
/// \Lib\Calc::total(\iterator_to_array($tags, false));
/// \Lib\Calc::total(\is_object($spare) ? \iterator_to_array($spare) : $spare);
/// \Lib\Calc::sum(...\array_map(fn ($element#1) => \is_object($element#1) ? \iterator_to_array($element#1) : $element#1, $shelves));
/// \implode(",", \iterator_to_array($tags, false));
/// return \Lib\Calc::total(items: \iterator_to_array($stock));
/// ```
///
/// A `Map` or a `Set` goes into a plain PHP parameter that takes `array` as its PHP array, spec section 12, and a `Set`
/// without its positions, whether a method or a function called through its namespace takes it. A value that may be
/// null goes in as it is when it is null, and each `Map` of a spread `List` goes in as its PHP array.
#[test]
fn a_map_or_set_goes_into_a_plain_php_array_parameter_as_its_php_array() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        Map<string, int> stock = this.stock();\n        Set<string> tags = this.tags();\n        Map<string, int>? spare = this.spare();\n        List<Map<string, int>> shelves = this.shelves();\n        Calc.total(stock);\n        Calc.total(tags);\n        Calc.total(spare);\n        Calc.sum(...shelves);\n        global.implode(\",\", tags);\n        return Calc.total(items: stock);\n    }\n\n    private Map<string, int> stock() => [:];\n\n    private Set<string> tags() => [];\n\n    private Map<string, int>? spare() => null;\n\n    private List<Map<string, int>> shelves() => [];\n}\n";

    assert_eq!(
        statements_at(code, &[], 4..10),
        [
            indoc! {r#"
                STATIC_CALL
                  ZVAL "Lib\\Calc"
                  ZVAL "total"
                  ARG_LIST
                    CALL
                      ZVAL "iterator_to_array"
                      ARG_LIST
                        VAR
                          ZVAL "stock"
            "#},
            indoc! {r#"
                STATIC_CALL
                  ZVAL "Lib\\Calc"
                  ZVAL "total"
                  ARG_LIST
                    CALL
                      ZVAL "iterator_to_array"
                      ARG_LIST
                        VAR
                          ZVAL "tags"
                        ZVAL false
            "#},
            indoc! {r#"
                STATIC_CALL
                  ZVAL "Lib\\Calc"
                  ZVAL "total"
                  ARG_LIST
                    CONDITIONAL
                      CALL
                        ZVAL "is_object"
                        ARG_LIST
                          VAR
                            ZVAL "spare"
                      CALL
                        ZVAL "iterator_to_array"
                        ARG_LIST
                          VAR
                            ZVAL "spare"
                      VAR
                        ZVAL "spare"
            "#},
            indoc! {r#"
                STATIC_CALL
                  ZVAL "Lib\\Calc"
                  ZVAL "sum"
                  ARG_LIST
                    UNPACK
                      CALL
                        ZVAL "array_map"
                        ARG_LIST
                          ARROW_FUNC "" @16-16
                            PARAM_LIST
                              PARAM
                                null
                                ZVAL "element#1"
                                null
                                null
                                null
                                null
                            null
                            CONDITIONAL
                              CALL
                                ZVAL "is_object"
                                ARG_LIST
                                  VAR
                                    ZVAL "element#1"
                              CALL
                                ZVAL "iterator_to_array"
                                ARG_LIST
                                  VAR
                                    ZVAL "element#1"
                              VAR
                                ZVAL "element#1"
                            null
                            null
                          VAR
                            ZVAL "shelves"
            "#},
            indoc! {r#"
                CALL
                  ZVAL "implode"
                  ARG_LIST
                    ZVAL ","
                    CALL
                      ZVAL "iterator_to_array"
                      ARG_LIST
                        VAR
                          ZVAL "tags"
                        ZVAL false
            "#},
            indoc! {r#"
                RETURN
                  STATIC_CALL
                    ZVAL "Lib\\Calc"
                    ZVAL "total"
                    ARG_LIST
                      NAMED_ARG
                        ZVAL "items"
                        CALL
                          ZVAL "iterator_to_array"
                          ARG_LIST
                            VAR
                              ZVAL "stock"
            "#},
        ]
    );
}

/// ```php
/// [$sorted = \is_object($sorted) ? \iterator_to_array($sorted) : $sorted, \Lib\Calc::sort($sorted), $sorted = \is_array($sorted) ? \Sharp\Map::from($sorted) : $sorted][1];
/// [\Lib\Calc::fill($filled), $filled = \is_array($filled) ? \Sharp\Map::from($filled) : $filled][0];
/// \Lib\Calc::bump($count);
/// ```
///
/// A typed local that holds a `Map` or a `Set` goes into a plain PHP reference parameter and comes back as the object
/// of what plain PHP wrote there: as its PHP array first where the parameter takes `array`, and as itself otherwise.
/// A local of any other type goes in as it is.
#[test]
fn a_typed_local_crosses_into_and_back_from_a_plain_php_reference_parameter() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        Map<int, string> sorted = this.names();\n        Map<int, string> filled = this.names();\n        int count = 0;\n        Calc.sort(sorted);\n        Calc.fill(filled);\n        Calc.bump(count);\n        return count;\n    }\n\n    private Map<int, string> names() => [:];\n}\n";

    assert_eq!(
        statements_at(code, &[], 3..6),
        [
            indoc! {r#"
                DIM
                  ARRAY [3]
                    ARRAY_ELEM
                      ASSIGN
                        VAR
                          ZVAL "sorted"
                        CONDITIONAL
                          CALL
                            ZVAL "is_object"
                            ARG_LIST
                              VAR
                                ZVAL "sorted"
                          CALL
                            ZVAL "iterator_to_array"
                            ARG_LIST
                              VAR
                                ZVAL "sorted"
                          VAR
                            ZVAL "sorted"
                      null
                    ARRAY_ELEM
                      STATIC_CALL
                        ZVAL "Lib\\Calc"
                        ZVAL "sort"
                        ARG_LIST
                          VAR
                            ZVAL "sorted"
                      null
                    ARRAY_ELEM
                      ASSIGN
                        VAR
                          ZVAL "sorted"
                        CONDITIONAL
                          CALL
                            ZVAL "is_array"
                            ARG_LIST
                              VAR
                                ZVAL "sorted"
                          STATIC_CALL
                            ZVAL "Sharp\\Map"
                            ZVAL "from"
                            ARG_LIST
                              VAR
                                ZVAL "sorted"
                          VAR
                            ZVAL "sorted"
                      null
                  ZVAL 1
            "#},
            indoc! {r#"
                DIM
                  ARRAY [3]
                    ARRAY_ELEM
                      STATIC_CALL
                        ZVAL "Lib\\Calc"
                        ZVAL "fill"
                        ARG_LIST
                          VAR
                            ZVAL "filled"
                      null
                    ARRAY_ELEM
                      ASSIGN
                        VAR
                          ZVAL "filled"
                        CONDITIONAL
                          CALL
                            ZVAL "is_array"
                            ARG_LIST
                              VAR
                                ZVAL "filled"
                          STATIC_CALL
                            ZVAL "Sharp\\Map"
                            ZVAL "from"
                            ARG_LIST
                              VAR
                                ZVAL "filled"
                          VAR
                            ZVAL "filled"
                      null
                  ZVAL 0
            "#},
            indoc! {r#"
                STATIC_CALL
                  ZVAL "Lib\\Calc"
                  ZVAL "bump"
                  ARG_LIST
                    VAR
                      ZVAL "count"
            "#},
        ]
    );
}

/// ```php
/// $count(\iterator_to_array($names), \iterator_to_array($names), \iterator_to_array($names));
/// $count(items: \iterator_to_array($names));
/// $plain($names);
/// $doubler(\iterator_to_array($names));
/// ```
///
/// A call of a plain PHP closure a local holds crosses each `Map` by the parameters of the closure's signature, as a
/// call of a plain PHP method does: by position, by name and into its variadic parameter. A closure whose signature
/// its type doesn't give takes the object, and a plain PHP object a local holds crosses each by its `__invoke`.
#[test]
fn a_call_of_a_plain_php_callable_a_local_holds_crosses_each_map_by_its_parameters() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\nimport Lib.Doubler;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        Map<int, string> names = this.names();\n        const count = Calc.counter();\n        const plain = Calc.plain();\n        const doubler = new Doubler();\n        count(names, names, names);\n        count(items: names);\n        plain(names);\n        doubler(names);\n        return 0;\n    }\n\n    private Map<int, string> names() => [:];\n}\n";
    let doubler = (
        "src/Lib/Doubler.php",
        "<?php namespace Lib; final class Doubler { public function __invoke(array $items): int { return 0; } }",
    );
    let crossed = indoc! {r#"
        CALL
          ZVAL "iterator_to_array"
          ARG_LIST
            VAR
              ZVAL "names"
    "#};
    let passed = "VAR\n  ZVAL \"names\"\n";
    // The tree of `$local(…)` with `arguments`, each a rendered node, below its `ARG_LIST`.
    let called = |local: &str, arguments: &[&str]| {
        let mut call = format!("CALL\n  VAR\n    ZVAL \"{local}\"\n  ARG_LIST\n");
        for argument in arguments {
            call.push_str(&nested(argument, 2));
        }

        call
    };
    let named = format!("NAMED_ARG\n  ZVAL \"items\"\n{}", nested(crossed, 1));

    assert_eq!(
        statements_at(code, &[doubler], 4..8),
        [
            called("count", &[crossed, crossed, crossed]),
            called("count", &[named.as_str()]),
            called("plain", &[passed]),
            called("doubler", &[crossed]),
        ]
    );
}

/// `tree`, a rendered node, as it renders `levels` below its parent.
fn nested(tree: &str, levels: usize) -> String {
    let mut nested = String::new();
    for line in tree.lines() {
        nested.push_str(&"  ".repeat(levels));
        nested.push_str(line);
        nested.push('\n');
    }

    nested
}

/// ```php
/// $levels = \is_array($levels) ? new \Sharp\Map(\array_merge([], ...\array_map(fn ($key#1, $value#2) => [\Lib\Level::from($key#1), $value#2], \array_keys($levels), $levels))) : $levels;
/// $names = \is_array(${'crossing#1'} = \Lib\Calc::names()) ? \Sharp\Map::from(${'crossing#1'}) : ${'crossing#1'};
/// return \is_array(${'crossing#1'} = \Lib\Calc::names()) ? \Sharp\Map::from(${'crossing#1'}) : ${'crossing#1'};
/// ```
///
/// A value plain PHP gives enters PHP# with each `Map` it holds made the object, spec section 12: a parameter, a typed
/// local and a `return`. A call's result is checked in its `VERIFY_TYPE_ARGS` first, then made the object. A `Map` keyed by an `int`-backed enum takes each backing value back as its case as it is,
/// where a `string`-backed one casts it first, as `a_backed_enum_key_beside_a_map_spread_goes_in_as_its_case` shows.
#[test]
fn a_value_plain_php_gives_enters_php_sharp_as_the_object() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\nimport Lib.Level;\n\nclass Report\n{\n    public Map<int, string> run(Map<Level, int> levels)\n    {\n        Map<int, string> names = Calc.names();\n        return Calc.names();\n    }\n}\n";
    let level = ("src/Lib/Level.php", "<?php namespace Lib; enum Level: int { case Low = 1; }");
    let crossing = indoc! {r#"
        CONDITIONAL
          CALL
            ZVAL "is_array"
            ARG_LIST
              ASSIGN
                VAR
                  ZVAL "crossing#1"
                VERIFY_TYPE_ARGS
                  STATIC_CALL
                    ZVAL "Lib\\Calc"
                    ZVAL "names"
                    ARG_LIST
                  ZVAL "Map<int, string>"
                  ZVAL "Lib\\Calc::names()"
          STATIC_CALL
            ZVAL "Sharp\\Map"
            ZVAL "from"
            ARG_LIST
              VAR
                ZVAL "crossing#1"
          VAR
            ZVAL "crossing#1"
    "#};

    assert_eq!(
        statements_at(code, &[level], 0..3),
        [
            indoc! {r#"
                ASSIGN
                  VAR
                    ZVAL "levels"
                  CONDITIONAL
                    CALL
                      ZVAL "is_array"
                      ARG_LIST
                        VAR
                          ZVAL "levels"
                    NEW
                      ZVAL "Sharp\\Map"
                      ARG_LIST
                        CALL
                          ZVAL "array_merge"
                          ARG_LIST
                            ARRAY [3]
                            UNPACK
                              CALL
                                ZVAL "array_map"
                                ARG_LIST
                                  ARROW_FUNC "" @8-8
                                    PARAM_LIST
                                      PARAM
                                        null
                                        ZVAL "key#1"
                                        null
                                        null
                                        null
                                        null
                                      PARAM
                                        null
                                        ZVAL "value#2"
                                        null
                                        null
                                        null
                                        null
                                    null
                                    ARRAY [3]
                                      ARRAY_ELEM
                                        STATIC_CALL
                                          ZVAL "Lib\\Level"
                                          ZVAL "from"
                                          ARG_LIST
                                            VAR
                                              ZVAL "key#1"
                                        null
                                      ARRAY_ELEM
                                        VAR
                                          ZVAL "value#2"
                                        null
                                    null
                                    null
                                  CALL
                                    ZVAL "array_keys"
                                    ARG_LIST
                                      VAR
                                        ZVAL "levels"
                                  VAR
                                    ZVAL "levels"
                    VAR
                      ZVAL "levels"
            "#}
            .to_owned(),
            format!("ASSIGN\n  VAR\n    ZVAL \"names\"\n{}", nested(crossing, 1)),
            format!("RETURN\n{}", nested(crossing, 1)),
        ]
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

/// A `match` without arms has no PHP, so the checker refuses it before the lowering meets it.
#[test]
fn a_match_without_arms_is_refused_before_lowering() {
    let lowered = Lowered::new(&method_with("void run(Any? extra)", "        match (extra) {}\n"));

    assert_eq!(lowered.diagnostics(), ["9:9 compile error: A `match` needs a `default` arm."]);
}

/// Spec section 21: a function type is tested as any type is, by the engine's test of its type text, its effects
/// included, which compares a closure's own function type with it.
#[test]
fn a_test_of_a_function_type_is_the_engine_test_of_its_type_text() {
    assert_eq!(
        body_in("bool run(Any? extra)", "        return extra is Function<int(int) uses Http>;\n", &[]),
        indoc! {r#"
            STMT_LIST
              RETURN
                SHARP_IS_TYPE
                  VAR
                    ZVAL "extra"
                  ZVAL "Function<int(int) uses Sharp.Http>"
        "#}
    );
}

/// A plain PHP class's objects carry no type arguments while the code runs, so `is` and `as` that give one type
/// arguments are refused before the lowering would test the class alone. The class alone is its `instanceof`.
#[test]
fn is_and_as_that_give_a_plain_php_class_type_arguments_are_refused_before_lowering() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App.Tenant;

        import Lib.Collection;
        import Lib.Order;

        class Report
        {
            public void run(Any extra)
            {
                const tested = extra is Collection<Order>;
                const kept = extra as List<Collection<Order>>;
            }
        }
    "},
        &[(
            "src/Lib/Collection.php",
            "<?php namespace Lib; class Order {} /** @template T */ final class Collection { /** @param list<T> $items */ public function __construct(array $items) {} }",
        )],
    );

    assert_eq!(
        lowered.diagnostics(),
        [
            "10:33 compile error: `Collection<Order>` can't be tested: `Collection` is a plain PHP class, so its objects carry no type arguments while the code runs.",
            "11:36 compile error: `Collection<Order>` can't be tested: `Collection` is a plain PHP class, so its objects carry no type arguments while the code runs.",
        ]
    );
}

/// ```php
/// $present = <is Any>($extra);
/// $either = <is int|string>($extra) ? $extra : null;
/// ```
///
/// `is` and `as` of `Any` and of a union are the engine's test of the type, as a collection's are.
#[test]
fn is_and_as_of_any_or_a_union_are_the_engine_test_of_its_type() {
    assert_eq!(
        body_in(
            "void run(Any? extra)",
            "        const present = extra is Any;\n        const either = extra as int|string;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "present"
                SHARP_IS_TYPE
                  VAR
                    ZVAL "extra"
                  ZVAL "Any"
              ASSIGN
                VAR
                  ZVAL "either"
                CONDITIONAL
                  SHARP_IS_TYPE
                    VAR
                      ZVAL "extra"
                    ZVAL "int|string"
                  VAR
                    ZVAL "extra"
                  ZVAL null
        "#}
    );
}

/// ```php
/// $numbers = <is List<int>>($extra);
/// $kept = <is Map<string, int>>($extra) ? $extra : null;
/// $tags = <is Set<string>>($extra);
/// ```
///
/// `is` and `as` of a `List`, a `Map` or a `Set` are the engine's test of the type, as a generic class's are, which
/// walks every element.
#[test]
fn is_and_as_of_a_collection_are_the_engine_test_of_its_type() {
    assert_eq!(
        body_in(
            "void run(Any? extra)",
            "        const numbers = extra is List<int>;\n        const kept = extra as Map<string, int>;\n        const tags = extra is Set<string>;\n",
            &[],
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "numbers"
                SHARP_IS_TYPE
                  VAR
                    ZVAL "extra"
                  ZVAL "List<int>"
              ASSIGN
                VAR
                  ZVAL "kept"
                CONDITIONAL
                  SHARP_IS_TYPE
                    VAR
                      ZVAL "extra"
                    ZVAL "Map<string, int>"
                  VAR
                    ZVAL "extra"
                  ZVAL null
              ASSIGN
                VAR
                  ZVAL "tags"
                SHARP_IS_TYPE
                  VAR
                    ZVAL "extra"
                  ZVAL "Set<string>"
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
              ASSIGN
                VAR
                  ZVAL "counts"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "counts"
                  NEW
                    ZVAL "Sharp\\Map"
                    ARG_LIST
                      CALL
                        ZVAL "array_merge"
                        ARG_LIST
                          ARRAY [3]
                          UNPACK
                            CALL
                              ZVAL "array_map"
                              ARG_LIST
                                ARROW_FUNC "" @7-7
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "key#1"
                                      null
                                      null
                                      null
                                      null
                                    PARAM
                                      null
                                      ZVAL "value#2"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  ARRAY [3]
                                    ARRAY_ELEM
                                      CAST [6]
                                        VAR
                                          ZVAL "key#1"
                                      null
                                    ARRAY_ELEM
                                      VAR
                                        ZVAL "value#2"
                                      null
                                  null
                                  null
                                CALL
                                  ZVAL "array_keys"
                                  ARG_LIST
                                    VAR
                                      ZVAL "counts"
                                VAR
                                  ZVAL "counts"
                  VAR
                    ZVAL "counts"
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
/// $a = $extra === 200;
/// $b = \is_int($extra) && $extra >= 1 && $extra < 10 || !($extra === -1);
/// $c = \is_object($extra) && (${'match#1'} = $extra->count) > 0 && \is_string($label = $extra->name);
/// ```
///
/// A value is compared with `===`, a comparison keeps its operator, and `and`, `or` and `not` are `&&`, `||` and
/// `!`. A properties pattern tests that the value is an object, then reads each property once. A comparison orders a
/// `Calc`, which declares no `operator <=>`, only once `int` has ruled it out.
#[test]
fn values_comparisons_and_properties_are_the_php_comparisons_they_name() {
    assert_eq!(
        body_in(
            "Calc|int run(Calc|int extra)",
            "        const a = extra is 200;\n        const b = extra is int and >= 1 and < 10 or (not -1);\n        const c = extra is { count: > 0, name: string label };\n        return extra;\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { public int $count = 0; public ?string $name = null; }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
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
                      ZVAL 10
                  UNARY_OP [14]
                    BINARY_OP [16]
                      VAR
                        ZVAL "extra"
                      UNARY_MINUS
                        ZVAL 1
              ASSIGN
                VAR
                  ZVAL "c"
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
/// $a = ($big = $extra) instanceof \Lib\Calc && (${'match#1'} = $big->count) > 1000;
/// $b = (${'match#1'} = $this->calc()) instanceof \Lib\Calc && (${'match#2'} = ${'match#1'}->count) > 0;
/// ```
///
/// Spec section 21: a type pattern with properties tests the type, then each property on the value the name holds, or
/// on the subject's hidden variable when it has no name. Each property is read once, as in a properties pattern.
/// `a ?? return`, `a ?? continue` and `a ?? break` lower to a RETURN, CONTINUE or BREAK node in the place of the right side
/// of `??`, which the engine compiles in the middle of the expression. PHP has no spelling for it.
#[test]
fn a_leave_after_null_coalesce_is_its_statement_inside_the_expression() {
    assert_eq!(
        body_in(
            "int run(Map<int, int> values)",
            "        const a = values[0] ?? return -1;\n        for (int i = 0; i < 2; i++) {\n            const b = values[i] ?? continue;\n            const c = values[i + 1] ?? break;\n        }\n        return a;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "values"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "values"
                  STATIC_CALL
                    ZVAL "Sharp\\Map"
                    ZVAL "from"
                    ARG_LIST
                      VAR
                        ZVAL "values"
                  VAR
                    ZVAL "values"
              ASSIGN
                VAR
                  ZVAL "a"
                COALESCE
                  DIM
                    VAR
                      ZVAL "values"
                    ZVAL 0
                  RETURN
                    UNARY_MINUS
                      ZVAL 1
              FOR
                EXPR_LIST
                  ASSIGN
                    VAR
                      ZVAL "i"
                    ZVAL 0
                EXPR_LIST
                  BINARY_OP [20]
                    VAR
                      ZVAL "i"
                    ZVAL 2
                EXPR_LIST
                  POST_INC
                    VAR
                      ZVAL "i"
                STMT_LIST
                  ASSIGN
                    VAR
                      ZVAL "b"
                    COALESCE
                      DIM
                        VAR
                          ZVAL "values"
                        VAR
                          ZVAL "i"
                      CONTINUE
                        null
                  ASSIGN
                    VAR
                      ZVAL "c"
                    COALESCE
                      DIM
                        VAR
                          ZVAL "values"
                        BINARY_OP [1]
                          VAR
                            ZVAL "i"
                          ZVAL 1
                      BREAK
                        null
              RETURN
                VAR
                  ZVAL "a"
            "#}
    );
}

/// ```php
/// $a = \is_array($values) && \count($values) === 0;
/// $b = \is_array($values) && \count($values) >= 1 && \is_int($first = $values[0])
///     && ($rest = \array_slice($values, 1)) is List<int>;
/// ```
///
/// The rest is tested as a typed element is, by the engine's test of its type, so `...List<Circle> rest` holds only
/// circles.
#[test]
fn a_list_pattern_tests_the_count_then_each_element_then_takes_the_rest() {
    assert_eq!(
        body_in(
            "int run(List<int> values)",
            "        const a = values is [];\n        const b = values is [int first, ...List<int> rest];\n        return 0;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                AND
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "values"
                  BINARY_OP [16]
                    CALL
                      ZVAL "count"
                      ARG_LIST
                        VAR
                          ZVAL "values"
                    ZVAL 0
              ASSIGN
                VAR
                  ZVAL "b"
                AND
                  AND
                    AND
                      CALL
                        ZVAL "is_array"
                        ARG_LIST
                          VAR
                            ZVAL "values"
                      GREATER_EQUAL
                        CALL
                          ZVAL "count"
                          ARG_LIST
                            VAR
                              ZVAL "values"
                        ZVAL 1
                    CALL
                      ZVAL "is_int"
                      ARG_LIST
                        ASSIGN
                          VAR
                            ZVAL "first"
                          DIM
                            VAR
                              ZVAL "values"
                            ZVAL 0
                  SHARP_IS_TYPE
                    ASSIGN
                      VAR
                        ZVAL "rest"
                      CALL
                        ZVAL "array_slice"
                        ARG_LIST
                          VAR
                            ZVAL "values"
                          ZVAL 1
                    ZVAL "List<int>"
              RETURN
                ZVAL 0
            "#}
    );
}

/// ```php
/// $a = $value is List<Any?> && \count($value) === 1 && \is_int($one = $value[0]);
/// ```
///
/// A value that may be a list or anything else is tested with the engine's test of `List<Any?>` first, where a `List`
/// needs only `\is_array`.
#[test]
fn a_list_pattern_on_any_value_tests_it_is_a_list_first() {
    assert_eq!(
        body_in("int run(Any value)", "        const a = value is [int one];\n        return 0;\n", &[]),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                AND
                  AND
                    SHARP_IS_TYPE
                      VAR
                        ZVAL "value"
                      ZVAL "List<Any?>"
                    BINARY_OP [16]
                      CALL
                        ZVAL "count"
                        ARG_LIST
                          VAR
                            ZVAL "value"
                      ZVAL 1
                  CALL
                    ZVAL "is_int"
                    ARG_LIST
                      ASSIGN
                        VAR
                          ZVAL "one"
                        DIM
                          VAR
                            ZVAL "value"
                          ZVAL 0
              RETURN
                ZVAL 0
            "#}
    );
}

#[test]
fn a_type_with_properties_tests_the_type_then_each_property() {
    assert_eq!(
        body_in(
            "Calc|int run(Calc|int extra)",
            "        const a = extra is Calc { count: > 1000 } big;\n        const b = this.calc() is Calc { count: > 0 };\n        return extra;\n    }\n\n    public Calc calc()\n    {\n        return new Calc();\n",
            &[(
                "src/Lib/Calc.php",
                "<?php namespace Lib; final class Calc { public int $count = 0; public ?string $name = null; }",
            )]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                AND
                  INSTANCEOF
                    ASSIGN
                      VAR
                        ZVAL "big"
                      VAR
                        ZVAL "extra"
                    ZVAL "Lib\\Calc"
                  GREATER
                    ASSIGN
                      VAR
                        ZVAL "match#1"
                      PROP
                        VAR
                          ZVAL "big"
                        ZVAL "count"
                    ZVAL 1000
              ASSIGN
                VAR
                  ZVAL "b"
                AND
                  INSTANCEOF
                    ASSIGN
                      VAR
                        ZVAL "match#1"
                      METHOD_CALL
                        VAR
                          ZVAL "this"
                        ZVAL "calc"
                        ARG_LIST
                    ZVAL "Lib\\Calc"
                  GREATER
                    ASSIGN
                      VAR
                        ZVAL "match#2"
                      PROP
                        VAR
                          ZVAL "match#1"
                        ZVAL "count"
                    ZVAL 0
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
        .written()
        .into_iter()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_IF_ELEM)
        .map(|(_, node)| node.line)
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
                    SHARP_TYPE_ARGS
                      ZVAL [1] "string"
                      ZVAL "Function<string()>"
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
                    SHARP_TYPE_ARGS
                      ZVAL "App\\Tenant\\Status"
                      ZVAL "Function<App.Tenant.Status(string)>"
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
        "enum Suit\n{\n    case Hearts;\n    case Spades;\n\n    public string label() => this.name;\n\n    public static int size() => global.count(Suit.cases());\n}\n",
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
                    SHARP_TYPE_ARGS
                      ZVAL [1] "string"
                      ZVAL "Function<string()>"
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
                    SHARP_TYPE_ARGS
                      ZVAL [1] "int"
                      ZVAL "int"
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
                    SHARP_TYPE_ARGS
                      TYPE [7]
                      ZVAL "Function<List<Suit>()>"
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
                          SHARP_TYPE_ARGS
                            null
                            ZVAL "Function<bool(Suit)>"
                          null
                    SHARP_TYPE_ARGS
                      ZVAL "Closure"
                      ZVAL "Function<Function<bool(Suit)>()>"
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
                  SHARP_TYPE_ARGS
                    TYPE [15]
                    ZVAL "Function<App.Status()>"
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
                  SHARP_TYPE_ARGS
                    TYPE [7]
                    ZVAL "Function<List<App.Status>()>"
                  null
              null
              ZVAL [1] "string"
        "#}
    );
}

/// ```php
/// abstract class Shape { public const int Sides = 4; }
/// ```
///
/// A constant of an enum whose cases carry data is a constant of the abstract class the enum runs as, first among its
/// members, as a class's constant is.
#[test]
fn an_enum_with_data_holds_its_constant_as_a_class_does() {
    let lowered = Lowered::new(
        "namespace App;\n\nenum Shape\n{\n    public const int Sides = 4;\n\n    case Square(int side);\n}\n",
    );
    let class = lowered.nodes().iter().position(|node| node.kind == sharp_kind::SHARP_AST_CLASS).expect("the enum");
    let members = lowered.child(class as u32, 2);

    assert_eq!(
        lowered.render(lowered.child(members, 0)),
        indoc! {r#"
            CLASS_CONST_GROUP [1]
              CLASS_CONST_DECL
                CONST_ELEM
                  ZVAL "Sides"
                  ZVAL 4
                  null
              null
              ZVAL [1] "int"
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
            "namespace Sharp;\n\npublic class Here\n{\n    public static Position now() => Position.current();\n\n    public static string shout(string text) => global.strtoupper(text);\n}\n"
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

/// `Sharp\Padding` at the standard library's path, with a method whose body reads PHP's constant `STR_PAD_LEFT`.
const PADDING: (&str, &str) = (
    "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Padding.sharp",
    "namespace Sharp;\n\npublic class Padding\n{\n    public static string left(string text, int width) => global.str_pad(text, width, \" \", global.STR_PAD_LEFT);\n\n    public static string after(string text, int start) => global.substr(text, start, null);\n}\n",
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
/// A form may read PHP's constant through `global`, as it reads a literal. `global.STR_PAD_LEFT` names the constant by
/// its full name, `ZEND_NAME_FQ`, which is 0, so the copy of the form in the caller's namespace reads the same one.
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
/// return \str_pad($name, WIDTH, ' ', \STR_PAD_LEFT);
/// ```
///
/// A constant of its own the caller passes keeps the caller's short name, `[1]` `ZEND_NAME_NOT_FQ`, which the engine
/// looks up in the caller's namespace, as the caller wrote it. The form's PHP constant keeps its full name.
#[test]
fn a_constant_the_caller_passes_keeps_its_short_name_beside_the_forms_full_name() {
    let width = ("src/App/Tenant/constants.php", "<?php\n\nnamespace App\\Tenant;\n\nconst WIDTH = 4;\n");
    let code = "namespace App.Tenant;\n\nimport Sharp.Padding;\n\nclass Report\n{\n    public string run(string name)\n    {\n        return Padding.left(name, WIDTH);\n    }\n}\n";

    assert_eq!(
        Lowered::inlining(code, &[width], &[PADDING]).body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                CALL
                  ZVAL "str_pad"
                  ARG_LIST
                    VAR
                      ZVAL "name"
                    CONST
                      ZVAL [1] "WIDTH"
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

/// The unit names the function type of each method its file declares with the xxh3-64 of the type text `is` tests a
/// closure of it by, which `Reads::function_types` takes, since the effects the text lists come from other files.
#[test]
fn the_unit_names_each_function_type_with_the_hash_of_its_text() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int twice(int x) => x * 2;\n}\n";
    let lowered = Lowered::new(code);
    let function_types: Vec<(String, u64)> = lowered
        .unit()
        .expect("the source lowers")
        .function_types()
        .iter()
        .map(|read| (String::from_utf8_lossy(&read.name).into_owned(), read.fingerprint))
        .collect();

    assert_eq!(function_types, [("App\\Tenant\\Report::twice".to_owned(), xxh3_64(b"Function<int(int)>"))]);
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
const MONEY: &str = "namespace App;\n\npublic class Money\n{\n    public int hash() => 1;\n\n    public static bool operator ==(Money a, Money b) => true;\n\n    public static Money operator +(Money a, Money b) => new Money();\n\n    public static Money operator -(Money a) => new Money();\n\n    public static int operator <=>(Money a, Money b) => 0;\n}\n";

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
/// public static function op_Addition(\App\Money $a, \App\Money $b): \App\Money { return new \App\Money(); }
/// public static function op_UnaryNegation(\App\Money $a): \App\Money { return new \App\Money(); }
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
                  NEW
                    ZVAL "App\\Money"
                    ARG_LIST
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
                  NEW
                    ZVAL "App\\Money"
                    ARG_LIST
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
/// return \App\Money::op_Addition($a, $b);
/// ```
///
/// A value of a type parameter bounded by `Money` runs the operator `Money` declares.
#[test]
fn arithmetic_on_a_type_parameter_calls_the_operator_its_bound_declares() {
    assert_eq!(
        ledger_body("Money run<T : Money>(T a, T b)", "        return a + b;\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
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

/// Spec section 10: the struct `Point`, whose `==` compares an `int`, a `float` and a nullable `Money`.
/// ```php
/// $a = …;
/// $b = …;
/// return $a == $b
///     && !(fn (${'lhs#1'}, ${'rhs#2'}) => ${'lhs#1'} === null || ${'rhs#2'} === null ? ${'lhs#1'} === ${'rhs#2'}
///         : \count(${'lhs#1'}) === \count(${'rhs#2'}) && \array_all(${'lhs#1'}, fn (${'element#3'}, ${'key#4'}) =>
///             \array_key_exists(${'key#4'}, ${'rhs#2'}) && (float) ${'element#3'} === ${'rhs#2'}[${'key#4'}]))($c, $d)
///     && $c === $e;
/// ```
///
/// Spec section 19 compares two collections element by element, each with its element's own `==`. Two `Map`s are
/// PHP's `==` on the objects, which the engine runs key by key in any order with each value's `==`. An int list
/// compared with a float list compares each pair as two floats, and a side that may be null equals only null. Two lists
/// whose elements compare by `===` are one `===`, which PHP runs on arrays in order.
#[test]
fn equality_of_two_collections_compares_each_element_with_its_own_equality() {
    assert_eq!(
        body_in(
            "bool run(Map<string, int> a, Map<string, int> b, List<int> c, List<float>? d, List<int>? e)",
            "        return a == b && c != d && c == e;\n",
            &[]
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "a"
                  NEW
                    ZVAL "Sharp\\Map"
                    ARG_LIST
                      CALL
                        ZVAL "array_merge"
                        ARG_LIST
                          ARRAY [3]
                          UNPACK
                            CALL
                              ZVAL "array_map"
                              ARG_LIST
                                ARROW_FUNC "" @7-7
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "key#1"
                                      null
                                      null
                                      null
                                      null
                                    PARAM
                                      null
                                      ZVAL "value#2"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  ARRAY [3]
                                    ARRAY_ELEM
                                      CAST [6]
                                        VAR
                                          ZVAL "key#1"
                                      null
                                    ARRAY_ELEM
                                      VAR
                                        ZVAL "value#2"
                                      null
                                  null
                                  null
                                CALL
                                  ZVAL "array_keys"
                                  ARG_LIST
                                    VAR
                                      ZVAL "a"
                                VAR
                                  ZVAL "a"
                  VAR
                    ZVAL "a"
              ASSIGN
                VAR
                  ZVAL "b"
                CONDITIONAL
                  CALL
                    ZVAL "is_array"
                    ARG_LIST
                      VAR
                        ZVAL "b"
                  NEW
                    ZVAL "Sharp\\Map"
                    ARG_LIST
                      CALL
                        ZVAL "array_merge"
                        ARG_LIST
                          ARRAY [3]
                          UNPACK
                            CALL
                              ZVAL "array_map"
                              ARG_LIST
                                ARROW_FUNC "" @7-7
                                  PARAM_LIST
                                    PARAM
                                      null
                                      ZVAL "key#1"
                                      null
                                      null
                                      null
                                      null
                                    PARAM
                                      null
                                      ZVAL "value#2"
                                      null
                                      null
                                      null
                                      null
                                  null
                                  ARRAY [3]
                                    ARRAY_ELEM
                                      CAST [6]
                                        VAR
                                          ZVAL "key#1"
                                      null
                                    ARRAY_ELEM
                                      VAR
                                        ZVAL "value#2"
                                      null
                                  null
                                  null
                                CALL
                                  ZVAL "array_keys"
                                  ARG_LIST
                                    VAR
                                      ZVAL "b"
                                VAR
                                  ZVAL "b"
                  VAR
                    ZVAL "b"
              RETURN
                AND
                  AND
                    BINARY_OP [18]
                      VAR
                        ZVAL "a"
                      VAR
                        ZVAL "b"
                    UNARY_OP [14]
                      CALL
                        ARROW_FUNC "" @9-9
                          PARAM_LIST
                            PARAM
                              null
                              ZVAL "lhs#1"
                              null
                              null
                              null
                              null
                            PARAM
                              null
                              ZVAL "rhs#2"
                              null
                              null
                              null
                              null
                          null
                          CONDITIONAL
                            OR
                              BINARY_OP [16]
                                VAR
                                  ZVAL "lhs#1"
                                ZVAL null
                              BINARY_OP [16]
                                VAR
                                  ZVAL "rhs#2"
                                ZVAL null
                            BINARY_OP [16]
                              VAR
                                ZVAL "lhs#1"
                              VAR
                                ZVAL "rhs#2"
                            AND
                              BINARY_OP [16]
                                CALL
                                  ZVAL "count"
                                  ARG_LIST
                                    VAR
                                      ZVAL "lhs#1"
                                CALL
                                  ZVAL "count"
                                  ARG_LIST
                                    VAR
                                      ZVAL "rhs#2"
                              CALL
                                ZVAL "array_all"
                                ARG_LIST
                                  VAR
                                    ZVAL "lhs#1"
                                  ARROW_FUNC "" @9-9
                                    PARAM_LIST
                                      PARAM
                                        null
                                        ZVAL "element#3"
                                        null
                                        null
                                        null
                                        null
                                      PARAM
                                        null
                                        ZVAL "key#4"
                                        null
                                        null
                                        null
                                        null
                                    null
                                    AND
                                      CALL
                                        ZVAL "array_key_exists"
                                        ARG_LIST
                                          VAR
                                            ZVAL "key#4"
                                          VAR
                                            ZVAL "rhs#2"
                                      BINARY_OP [16]
                                        CAST [5]
                                          VAR
                                            ZVAL "element#3"
                                        DIM
                                          VAR
                                            ZVAL "rhs#2"
                                          VAR
                                            ZVAL "key#4"
                                    null
                                    null
                          null
                          null
                        ARG_LIST
                          VAR
                            ZVAL "c"
                          VAR
                            ZVAL "d"
                  BINARY_OP [16]
                    VAR
                      ZVAL "c"
                    VAR
                      ZVAL "e"
        "#}
    );
}

/// ```php
/// public static function slug(string $value, string $separator): string
/// {
///     return $value . $separator;
/// }
///
/// public static function isBlank(string $value): bool
/// {
///     return $value === '';
/// }
/// ```
///
/// Spec section 26: an extension's method is the public static method of its static class that takes the receiver
/// first, and a computed property is the one that takes only the receiver and returns its expression. The method
/// carries its result type, which a generator's elements are read from, spec section 12.
#[test]
fn an_extension_member_is_the_static_method_that_takes_the_receiver_first() {
    let lowered = Lowered::new(
        "namespace App;\n\npublic static class TextExtensions\n{\n    extension(string value)\n    {\n        public string slug(string separator) => value + separator;\n\n        public bool isBlank => value == \"\";\n    }\n}\n",
    );

    assert_eq!(
        (lowered.declared("slug"), lowered.declared("isBlank")),
        (
            indoc! {r#"
                METHOD [17] "slug" @7-7
                  PARAM_LIST
                    PARAM
                      ZVAL [1] "string"
                      ZVAL "value"
                      null
                      null
                      null
                      null
                    PARAM
                      ZVAL [1] "string"
                      ZVAL "separator"
                      null
                      null
                      null
                      null
                  null
                  STMT_LIST
                    RETURN
                      BINARY_OP [8]
                        VAR
                          ZVAL "value"
                        VAR
                          ZVAL "separator"
                  SHARP_TYPE_ARGS
                    ZVAL [1] "string"
                    ZVAL "string"
                  null
            "#}
            .to_owned(),
            indoc! {r#"
                METHOD [17] "isBlank" @9-9
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
                    RETURN
                      BINARY_OP [16]
                        VAR
                          ZVAL "value"
                        ZVAL ""
                  ZVAL [1] "bool"
                  null
            "#}
            .to_owned()
        )
    );
}

/// Spec section 26: a member of `extension<T>(List<T> items)` runs as a generic static method whose type parameters
/// are the block's and then its own, so its parameter list ends with their metadata, as any generic method's does. A
/// call passes the type arguments the checker inferred from the receiver, and those the member names after them.
#[test]
fn a_generic_extension_member_takes_the_type_arguments_of_its_block_and_its_own() {
    let lowered = Lowered::with(
        indoc! {"
        namespace App;

        import App.Lists.ListExtensions;

        public class Report
        {
            public List<string> twice(List<string> names) => names.twice();

            public List<string> converted(List<Order> orders) => orders.converted<string>(order => \"order\");
        }
    "},
        &[
            ("src/App/Order.sharp", "namespace App;\n\npublic class Order\n{\n}\n"),
            (
                "src/App/Lists/ListExtensions.sharp",
                "namespace App.Lists;\n\npublic static class ListExtensions\n{\n    extension<T>(List<T> items)\n    {\n        public List<T> twice() => items;\n\n        public List<TOut> converted<TOut>(Function<TOut(T)> convert) => [];\n    }\n}\n",
            ),
        ],
    );

    assert_eq!(
        lowered.body_of("twice"),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "App\\Lists\\ListExtensions"
                  ZVAL "twice"
                  ARG_LIST
                    VAR
                      ZVAL "names"
                    SHARP_TYPE_ARGS
                      null
                      ZVAL "string"
        "#}
    );
    let converted = lowered.body_of("converted");
    assert!(
        converted.ends_with("        SHARP_TYPE_ARGS\n          null\n          ZVAL \"App.Order, string\"\n"),
        "{converted}"
    );

    let extensions = Lowered::with(
        "namespace App.Lists;\n\nimport App.Order;\n\npublic static class ListExtensions\n{\n    extension<T : Order>(List<T> items)\n    {\n        public List<TOut> converted<TOut>(Function<TOut(T)> convert) => [];\n    }\n}\n",
        &[("src/App/Order.sharp", "namespace App;\n\npublic class Order\n{\n}\n")],
    );
    assert_eq!(
        extensions.declared("converted"),
        indoc! {r#"
            METHOD [17] "converted" @9-9
              PARAM_LIST
                PARAM
                  TYPE [7]
                  ZVAL "items"
                  null
                  null
                  null
                  null
                PARAM
                  ZVAL "Closure"
                  ZVAL "convert"
                  null
                  null
                  null
                  null
                SHARP_TYPE_ARGS
                  ZVAL "App.Order, Any?"
                  null
              null
              STMT_LIST
                RETURN
                  ARRAY [3]
              SHARP_TYPE_ARGS
                TYPE [7]
                ZVAL "List<#1>"
              null
        "#}
    );
}

/// `App\Order`, a PHP class, `App\RushOrder`, its subclass with its own `total`, and `App\Receipt`, a final class.
const ORDERS: (&str, &str) = (
    "src/App/Order.php",
    "<?php namespace App; class Order { public int $amount = 0; } class RushOrder extends Order { public function total(int $extra): int { return 9; } } final class Receipt { public int $amount = 0; }",
);

/// `App\Text\Extensions`, a project's extensions on `string`, on `App\Order`, on `App\Receipt` and on `Any`.
const EXTENSIONS: (&str, &str) = (
    "src/App/Text/Extensions.sharp",
    "namespace App.Text;\n\nimport App.Order;\nimport App.Receipt;\n\npublic static class Extensions\n{\n    extension(string value)\n    {\n        public string slug(string separator) => value + separator;\n\n        public bool isBlank => value == \"\";\n    }\n\n    extension(Order order)\n    {\n        public int total(int extra) => order.amount + extra;\n\n        public bool isEmpty => order.amount == 0;\n    }\n\n    extension(Receipt receipt)\n    {\n        public int paid() => receipt.amount;\n    }\n\n    extension(Any value)\n    {\n        public string kind => \"value\";\n    }\n}\n",
);

/// The statements of `run`, declared with `signature` and holding `statements`, in a class that imports `Order`,
/// `Receipt` and the extensions on them, lowered beside them.
fn extension_body(signature: &str, statements: &str) -> String {
    let code = format!(
        "namespace App.Tenant;\n\nimport App.Order;\nimport App.Receipt;\nimport App.Text.Extensions;\n\nclass Report\n{{\n    public {signature}\n    {{\n{statements}    }}\n}}\n"
    );

    Lowered::with(&code, &[ORDERS, EXTENSIONS]).body()
}

/// ```php
/// return \App\Text\Extensions::slug($title, '-') . \App\Text\Extensions::isBlank($title)
///     . \App\Text\Extensions::paid($receipt) . \App\Text\Extensions::total($order, 2)
///     . \App\Text\Extensions::isEmpty($order) . \App\Text\Extensions::kind($value);
/// ```
///
/// Spec section 26: a member an extension adds runs as the static call of its method with the receiver first, for a
/// method and a computed property alike, on any receiver: a `string`, an object of a final class or of a class a
/// subclass can extend, and a value of `Any`. The checker chose it from the type the code shows. Called through its
/// class, as `Extensions.total(order, 2)`, it is that static call already, as in C#.
#[test]
fn an_extension_member_is_the_static_call_of_its_method() {
    assert_eq!(
        extension_body(
            "string run(string title, Receipt receipt, Order order, Any value)",
            "        return title.slug(\"-\") + (title.isBlank ? \"y\" : \"n\") + (string) receipt.paid()\n            + (string) order.total(2) + (order.isEmpty ? \"y\" : \"n\") + value.kind;\n"
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [8]
                  BINARY_OP [8]
                    BINARY_OP [8]
                      BINARY_OP [8]
                        BINARY_OP [8]
                          STATIC_CALL
                            ZVAL "App\\Text\\Extensions"
                            ZVAL "slug"
                            ARG_LIST
                              VAR
                                ZVAL "title"
                              ZVAL "-"
                          CONDITIONAL [1]
                            STATIC_CALL
                              ZVAL "App\\Text\\Extensions"
                              ZVAL "isBlank"
                              ARG_LIST
                                VAR
                                  ZVAL "title"
                            ZVAL "y"
                            ZVAL "n"
                        CAST [6]
                          STATIC_CALL
                            ZVAL "App\\Text\\Extensions"
                            ZVAL "paid"
                            ARG_LIST
                              VAR
                                ZVAL "receipt"
                      CAST [6]
                        STATIC_CALL
                          ZVAL "App\\Text\\Extensions"
                          ZVAL "total"
                          ARG_LIST
                            VAR
                              ZVAL "order"
                            ZVAL 2
                    CONDITIONAL [1]
                      STATIC_CALL
                        ZVAL "App\\Text\\Extensions"
                        ZVAL "isEmpty"
                        ARG_LIST
                          VAR
                            ZVAL "order"
                      ZVAL "y"
                      ZVAL "n"
                  STATIC_CALL
                    ZVAL "App\\Text\\Extensions"
                    ZVAL "kind"
                    ARG_LIST
                      VAR
                        ZVAL "value"
        "#}
    );
    assert_eq!(
        extension_body(
            "string run(Order order)",
            "        return (string) Extensions.total(order, 2) + (Extensions.isEmpty(order) ? \"y\" : \"n\");\n"
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [8]
                  CAST [6]
                    STATIC_CALL
                      ZVAL "App\\Text\\Extensions"
                      ZVAL "total"
                      ARG_LIST
                        VAR
                          ZVAL "order"
                        ZVAL 2
                  CONDITIONAL [1]
                    STATIC_CALL
                      ZVAL "App\\Text\\Extensions"
                      ZVAL "isEmpty"
                      ARG_LIST
                        VAR
                          ZVAL "order"
                    ZVAL "y"
                    ZVAL "n"
        "#}
    );
}

/// ```php
/// return match (true) {
///     ($q = $p) instanceof \App\Point && \App\Point::op_Equality($q, $origin) => "origin",
///     default => "elsewhere",
/// };
/// ```
///
/// A pattern takes types and constants only, so a local's value is compared in `when`, spec section 21. Its `==` is
/// section 19's, so a struct subject matches an equal struct through the struct's `op_Equality`.
#[test]
fn a_struct_compared_in_when_runs_its_op_equality() {
    assert_eq!(
        Lowered::with(
            "namespace App;\n\nclass Report\n{\n    public string run(Point p, Point origin)\n    {\n        return match (p) {\n            Point q when q == origin => \"origin\",\n            default => \"elsewhere\",\n        };\n    }\n}\n",
            &[("src/App/Point.sharp", POINT), ("src/App/Money.sharp", MONEY)]
        )
        .body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                MATCH
                  ZVAL true
                  MATCH_ARM_LIST
                    MATCH_ARM
                      EXPR_LIST
                        AND
                          INSTANCEOF
                            ASSIGN
                              VAR
                                ZVAL "q"
                              VAR
                                ZVAL "p"
                            ZVAL "App\\Point"
                          STATIC_CALL
                            ZVAL "App\\Point"
                            ZVAL "op_Equality"
                            ARG_LIST
                              VAR
                                ZVAL "q"
                              VAR
                                ZVAL "origin"
                      ZVAL "origin"
                    MATCH_ARM
                      null
                      ZVAL "elsewhere"
        "#}
    );
}

/// ```php
/// $order = new \App\RushOrder();
/// $rush = new \App\RushOrder();
/// return \App\Text\Extensions::total($order, 2) + $rush->total(2);
/// ```
///
/// Spec section 26: the type the code shows chooses the member, as in C# and Kotlin. On a value shown as an `Order`,
/// the extension runs though the object is a `RushOrder` with its own `total`. On a value shown as a `RushOrder`, its
/// own `total` runs.
#[test]
fn the_type_the_code_shows_chooses_between_an_extension_and_a_subclass_member() {
    let code = "namespace App.Tenant;\n\nimport App.Order;\nimport App.RushOrder;\nimport App.Text.Extensions;\n\nclass Report\n{\n    public int run()\n    {\n        Order order = new RushOrder();\n        RushOrder rush = new RushOrder();\n        return order.total(2) + rush.total(2);\n    }\n}\n";

    assert_eq!(
        Lowered::with(code, &[ORDERS, EXTENSIONS]).body(),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "order"
                NEW
                  ZVAL "App\\RushOrder"
                  ARG_LIST
              ASSIGN
                VAR
                  ZVAL "rush"
                NEW
                  ZVAL "App\\RushOrder"
                  ARG_LIST
              RETURN
                BINARY_OP [1]
                  STATIC_CALL
                    ZVAL "App\\Text\\Extensions"
                    ZVAL "total"
                    ARG_LIST
                      VAR
                        ZVAL "order"
                      ZVAL 2
                  METHOD_CALL
                    VAR
                      ZVAL "rush"
                    ZVAL "total"
                    ARG_LIST
                      ZVAL 2
        "#}
    );
}

/// `Sharp\Key`, the attribute the standard library declares, as php-sharp writes it.
const KEY: (&str, &str) = (
    "src/Sharp/Key.sharp",
    "namespace Sharp;\n\nimport Attribute;\n\n[Attribute(Attribute.TARGET_PARAMETER | Attribute.TARGET_PROPERTY)]\npublic class Key\n{\n    public Key(public string name { get; }) { }\n}\n",
);

/// ```php
/// public static function parse(array|object $value): \App\Ticket
/// {
///     ${'input#1'} = \is_object($value) ? \get_object_vars($value) : $value;
///     ${'fields#2'} = [];
///     ${'arguments#3'} = [];
///     ${'properties#4'} = [];
///     if (\array_key_exists('ticket_id', ${'input#1'})) {
///         ${'read#8'} = ${'input#1'}['ticket_id'];
///         \Sharp\Internal\accepts('int', ${'read#8'}, 'ticket_id', ${'fields#2'});
///         ${'arguments#3'}['id'] = ${'read#8'};
///     } else { ${'fields#2'}['ticket_id'] = 'missing'; }
///     if (\array_key_exists('seats', ${'input#1'})) { …the read of an int… }
///     if (\array_key_exists('note', ${'input#1'})) { …the read of a string into ${'properties#4'}['note']… }
///     if (\array_key_exists('owner', ${'input#1'})) { …the read of a string? … } else { ${'properties#4'}['owner'] = null; }
///     if (${'fields#2'} !== []) { throw new \Sharp\InvalidFields(${'fields#2'}); }
///     ${'object#5'} = new \App\Ticket(...${'arguments#3'});
///     foreach (${'properties#4'} as ${'name#6'} => ${'property#7'}) { ${'object#5'}->${'name#6'} = ${'property#7'}; }
///     return ${'object#5'};
/// }
///
/// public static function tryParse(array|object $value): ?\App\Ticket
/// {
///     try { return \App\Ticket::parse($value); } catch (\Sharp\InvalidFields) { return null; }
/// }
/// ```
///
/// Spec section 10: `parse` reads each main constructor parameter from its key, which `[Key]` renames, then each
/// property an object initializer may set, records every bad or missing field, and throws them all at once. The
/// arguments go in by name, so a missing key takes its parameter's default, and a missing `T?` property is null. A
/// parameter that reads the key of a property the main constructor doesn't set is refused, so `parse` never leaves
/// that property unset.
#[test]
fn a_struct_parses_each_field_from_its_key_and_creates_itself_through_its_main_constructor() {
    let lowered = Lowered::with(
        "namespace App;\n\nimport Sharp.Key;\n\npublic struct Ticket\n{\n    public Ticket([Key(\"ticket_id\")] public int id { get; }, public int seats = 1 { get; }) { }\n\n    public string note { get; init; } = \"\";\n\n    public string? owner { get; init; }\n}\n",
        &[KEY],
    );

    assert_eq!(
        [lowered.declared("parse"), lowered.declared("tryParse")].join("\n"),
        indoc! {r#"
            METHOD [17] "parse" @5-5
              PARAM_LIST
                PARAM
                  TYPE_UNION
                    TYPE [7]
                    ZVAL [1] "object"
                  ZVAL "value"
                  null
                  null
                  null
                  null
              null
              STMT_LIST
                ASSIGN
                  VAR
                    ZVAL "input#1"
                  CONDITIONAL
                    INSTANCEOF
                      VAR
                        ZVAL "value"
                      ZVAL "Sharp\\Map"
                    CALL
                      ZVAL "iterator_to_array"
                      ARG_LIST
                        VAR
                          ZVAL "value"
                    CONDITIONAL [1]
                      CALL
                        ZVAL "is_object"
                        ARG_LIST
                          VAR
                            ZVAL "value"
                      CALL
                        ZVAL "get_object_vars"
                        ARG_LIST
                          VAR
                            ZVAL "value"
                      VAR
                        ZVAL "value"
                ASSIGN
                  VAR
                    ZVAL "fields#2"
                  ARRAY [3]
                ASSIGN
                  VAR
                    ZVAL "arguments#3"
                  ARRAY [3]
                ASSIGN
                  VAR
                    ZVAL "properties#4"
                  ARRAY [3]
                IF
                  IF_ELEM
                    CALL
                      ZVAL "array_key_exists"
                      ARG_LIST
                        ZVAL "ticket_id"
                        VAR
                          ZVAL "input#1"
                    STMT_LIST
                      STMT_LIST
                        ASSIGN
                          VAR
                            ZVAL "read#8"
                          DIM
                            VAR
                              ZVAL "input#1"
                            ZVAL "ticket_id"
                        CALL
                          ZVAL "Sharp\\Internal\\accepts"
                          ARG_LIST
                            ZVAL "int"
                            VAR
                              ZVAL "read#8"
                            ZVAL "ticket_id"
                            VAR
                              ZVAL "fields#2"
                        ASSIGN
                          DIM
                            VAR
                              ZVAL "arguments#3"
                            ZVAL "id"
                          VAR
                            ZVAL "read#8"
                  IF_ELEM
                    null
                    STMT_LIST
                      ASSIGN
                        DIM
                          VAR
                            ZVAL "fields#2"
                          ZVAL "ticket_id"
                        ZVAL "missing"
                IF
                  IF_ELEM
                    CALL
                      ZVAL "array_key_exists"
                      ARG_LIST
                        ZVAL "seats"
                        VAR
                          ZVAL "input#1"
                    STMT_LIST
                      STMT_LIST
                        ASSIGN
                          VAR
                            ZVAL "read#10"
                          DIM
                            VAR
                              ZVAL "input#1"
                            ZVAL "seats"
                        CALL
                          ZVAL "Sharp\\Internal\\accepts"
                          ARG_LIST
                            ZVAL "int"
                            VAR
                              ZVAL "read#10"
                            ZVAL "seats"
                            VAR
                              ZVAL "fields#2"
                        ASSIGN
                          DIM
                            VAR
                              ZVAL "arguments#3"
                            ZVAL "seats"
                          VAR
                            ZVAL "read#10"
                IF
                  IF_ELEM
                    CALL
                      ZVAL "array_key_exists"
                      ARG_LIST
                        ZVAL "note"
                        VAR
                          ZVAL "input#1"
                    STMT_LIST
                      STMT_LIST
                        ASSIGN
                          VAR
                            ZVAL "read#12"
                          DIM
                            VAR
                              ZVAL "input#1"
                            ZVAL "note"
                        CALL
                          ZVAL "Sharp\\Internal\\accepts"
                          ARG_LIST
                            ZVAL "string"
                            VAR
                              ZVAL "read#12"
                            ZVAL "note"
                            VAR
                              ZVAL "fields#2"
                        ASSIGN
                          DIM
                            VAR
                              ZVAL "properties#4"
                            ZVAL "note"
                          VAR
                            ZVAL "read#12"
                IF
                  IF_ELEM
                    CALL
                      ZVAL "array_key_exists"
                      ARG_LIST
                        ZVAL "owner"
                        VAR
                          ZVAL "input#1"
                    STMT_LIST
                      STMT_LIST
                        ASSIGN
                          VAR
                            ZVAL "read#14"
                          DIM
                            VAR
                              ZVAL "input#1"
                            ZVAL "owner"
                        CALL
                          ZVAL "Sharp\\Internal\\accepts"
                          ARG_LIST
                            ZVAL "string?"
                            VAR
                              ZVAL "read#14"
                            ZVAL "owner"
                            VAR
                              ZVAL "fields#2"
                        ASSIGN
                          DIM
                            VAR
                              ZVAL "properties#4"
                            ZVAL "owner"
                          VAR
                            ZVAL "read#14"
                  IF_ELEM
                    null
                    STMT_LIST
                      ASSIGN
                        DIM
                          VAR
                            ZVAL "properties#4"
                          ZVAL "owner"
                        ZVAL null
                IF
                  IF_ELEM
                    BINARY_OP [17]
                      VAR
                        ZVAL "fields#2"
                      ARRAY [3]
                    STMT_LIST
                      THROW
                        NEW
                          ZVAL "Sharp\\InvalidFields"
                          ARG_LIST
                            VAR
                              ZVAL "fields#2"
                ASSIGN
                  VAR
                    ZVAL "object#5"
                  NEW
                    ZVAL "App\\Ticket"
                    ARG_LIST
                      UNPACK
                        VAR
                          ZVAL "arguments#3"
                FOREACH
                  VAR
                    ZVAL "properties#4"
                  VAR
                    ZVAL "property#7"
                  VAR
                    ZVAL "name#6"
                  STMT_LIST
                    ASSIGN
                      PROP
                        VAR
                          ZVAL "object#5"
                        VAR
                          ZVAL "name#6"
                      VAR
                        ZVAL "property#7"
                RETURN
                  VAR
                    ZVAL "object#5"
              ZVAL "App\\Ticket"
              null

            METHOD [17] "tryParse" @5-5
              PARAM_LIST
                PARAM
                  TYPE_UNION
                    TYPE [7]
                    ZVAL [1] "object"
                  ZVAL "value"
                  null
                  null
                  null
                  null
              null
              STMT_LIST
                TRY
                  STMT_LIST
                    RETURN
                      STATIC_CALL
                        ZVAL "App\\Ticket"
                        ZVAL "parse"
                        ARG_LIST
                          VAR
                            ZVAL "value"
                  CATCH_LIST
                    CATCH
                      NAME_LIST
                        ZVAL "Sharp\\InvalidFields"
                      null
                      STMT_LIST
                        RETURN
                          ZVAL null
                  null
              ZVAL [256] "App\\Ticket"
              null
        "#}
    );
    assert_eq!(
        Lowered::new(
            "namespace App;\n\npublic struct Split\n{\n    public Split(int celsius) { this.reading = celsius; }\n\n    public int celsius { get; init; }\n\n    public int reading { get; init; }\n}\n"
        )
        .diagnostics(),
        [
            "7:16 compile error: `celsius` is read twice: by the main constructor parameter `celsius` and the property `celsius`."
        ]
    );
}

/// ```php
/// ${'read#N'} = ${'input#1'}['amount']; \Sharp\Internal\accepts('float', ${'read#N'}, 'amount', ${'fields#2'});
/// if (\is_string(${'read#N'}) && (${'case#N'} = \App\Status::tryFrom((string) ${'read#N'})) !== null) { ${'read#N'} = ${'case#N'}; }
/// ${'failed#N'} = [];
/// if (!${'read#N'} instanceof \App\Address && (\is_array(${'read#N'}) || \is_object(${'read#N'}))) {
///     try { ${'read#N'} = \App\Address::parse(${'read#N'}); }
///     catch (\Sharp\InvalidFields ${'inner#N'}) { …each problem under 'address.' . ${'key#N'}…; ${'failed#N'}[] = 'address'; }
/// }
/// \Sharp\Internal\accepts('App.Address', ${'read#N'}, 'address', ${'fields#2'});
/// foreach (${'failed#N'} as ${'path#N'}) { unset(${'fields#2'}[${'path#N'}]); }
/// if (\is_array(${'read#N'})) { foreach (${'read#N'} as ${'key#N'} => ${'element#N'}) { …built under 'addresses[' . ${'key#N'} . ']'… } }
/// \Sharp\Internal\accepts('Map<App.Status, int>', …);
/// …the PHP# class App\Plan built through its own parse, as App\Address is…
/// ${'arguments#3'}['extra'] = ${'input#1'}['extra'];   // Any?
/// \Sharp\Internal\accepts('int|string', …);
/// if (!… instanceof \App\Address && !… instanceof \App\Point && …) { try { …Address… } catch (\Sharp\InvalidFields) { try { …Point… } catch (\Sharp\InvalidFields) { } } }
/// …(Point|Address)? and each value of a Map<string, Point|Address> try Point first…
/// if (\Sharp\Internal\accepts('Set<App.Status>', ${'read#N'}, 'seen', ${'fields#2'})) { ${'read#N'} = \is_array(${'read#N'}) ? new \Sharp\Set(${'read#N'}) : ${'read#N'}; }
/// if (\Sharp\Internal\accepts('List<List<Set<int>>>', …)) { if (\is_array(…)) { foreach (…) { …each list's Sets built… } } }
/// \Sharp\Internal\accepts('Any', …); \Sharp\Internal\accepts('bool', …);
/// if (\is_int(${'read#N'}) && (${'case#N'} = \App\Rank::tryFrom(${'read#N'})) !== null) { ${'read#N'} = ${'case#N'}; }
/// \Sharp\Internal\accepts('App.Mood', …);
/// …Point|List<Point> tries Point, then each element of a list as a Point…
/// …List<List<Point>> builds each Point of each inner list…
/// …the property Point|Address corner tries Point first, into ${'properties#4'}['corner']…
/// ```
///
/// Spec section 10 and generics R3c: `parse` checks each field through the one walk, `\Sharp\Internal\accepts`, with
/// the field's type text and its key as the path. First it builds what the walk takes only as an object: a backed
/// enum case from its backing value, and a nested struct or PHP# class through its own `parse`, its problems under the
/// field's path, where the walk's own problem for it is then dropped, element by element inside a collection at any
/// depth. A union of structs takes the first that builds, in the order the field's main constructor parameter or
/// property writes them, which a method's parameter of the same name doesn't change. A private field, a get-only
/// property and one whose `init` is private are no field, and a property the main constructor sets may be left out.
/// Once the walk accepts a value, each `Set` in it that the input gave as an array becomes a `Sharp\Set`, through
/// nested collections. An `int`-backed enum case is built from its value uncast, and an enum with no backing value
/// isn't built: the checker refuses `parse` where it is called. `Any?` takes the value as it is.
#[test]
fn parse_reads_each_field_as_its_type() {
    let lowered = Lowered::with(
        "namespace App;\n\npublic struct Order\n{\n    public Point|Address corner { get; init; }\n\n    private int secret = 0;\n\n    public int fixed { get; } = 1;\n\n    public int serial { get; private init; } = 0;\n\n    public bool near(Point|Address place) => true;\n\n    public Order(public float amount { get; }, public Status status { get; }, public Address address { get; }, public List<Address> addresses { get; }, public Map<Status, int> prices { get; }, public Plan plan { get; }, public Any? extra { get; }, public int|string reference { get; }, public Address|Point place { get; }, public (Point|Address)? spot { get; }, public Map<string, Point|Address> spots { get; }, public Set<Status> seen { get; }, public List<List<Set<int>>> groups { get; }, public Any note { get; }, public bool paid { get; }, public Rank rank { get; }, public Mood mood { get; }, public Point|List<Point> trail { get; }, public List<List<Point>> rows { get; }) { this.corner = place; }\n}\n",
        &[
            (
                "src/App/Status.sharp",
                "namespace App;\n\npublic enum Status : string\n{\n    case Open = \"open\";\n}\n",
            ),
            ("src/App/Rank.sharp", "namespace App;\n\npublic enum Rank : int\n{\n    case First = 1;\n}\n"),
            ("src/App/Mood.sharp", "namespace App;\n\npublic enum Mood\n{\n    case Calm;\n}\n"),
            (
                "src/App/Address.sharp",
                "namespace App;\n\npublic struct Address\n{\n    public Address(public string city { get; }) { }\n}\n",
            ),
            (
                "src/App/Point.sharp",
                "namespace App;\n\npublic struct Point\n{\n    public Point(public int x { get; }) { }\n}\n",
            ),
            ("src/App/Plan.sharp", "namespace App;\n\npublic class Plan\n{\n}\n"),
        ],
    );

    assert_eq!(
        lowered.parse_reads(),
        indoc! {r#"
            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "amount"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#8"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "amount"
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "float"
                        VAR
                          ZVAL "read#8"
                        ZVAL "amount"
                        VAR
                          ZVAL "fields#2"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "amount"
                      VAR
                        ZVAL "read#8"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "amount"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "status"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#10"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "status"
                    IF
                      IF_ELEM
                        AND
                          CALL
                            ZVAL "is_string"
                            ARG_LIST
                              VAR
                                ZVAL "read#10"
                          BINARY_OP [17]
                            ASSIGN
                              VAR
                                ZVAL "case#12"
                              STATIC_CALL
                                ZVAL "App\\Status"
                                ZVAL "tryFrom"
                                ARG_LIST
                                  CAST [6]
                                    VAR
                                      ZVAL "read#10"
                            ZVAL null
                        STMT_LIST
                          ASSIGN
                            VAR
                              ZVAL "read#10"
                            VAR
                              ZVAL "case#12"
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "App.Status"
                        VAR
                          ZVAL "read#10"
                        ZVAL "status"
                        VAR
                          ZVAL "fields#2"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "status"
                      VAR
                        ZVAL "read#10"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "status"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "address"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#13"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "address"
                    ASSIGN
                      VAR
                        ZVAL "failed#14"
                      ARRAY [3]
                    IF
                      IF_ELEM
                        AND
                          UNARY_OP [14]
                            INSTANCEOF
                              VAR
                                ZVAL "read#13"
                              ZVAL "App\\Address"
                          OR
                            CALL
                              ZVAL "is_array"
                              ARG_LIST
                                VAR
                                  ZVAL "read#13"
                            CALL
                              ZVAL "is_object"
                              ARG_LIST
                                VAR
                                  ZVAL "read#13"
                        STMT_LIST
                          TRY
                            STMT_LIST
                              ASSIGN
                                VAR
                                  ZVAL "read#13"
                                STATIC_CALL
                                  ZVAL "App\\Address"
                                  ZVAL "parse"
                                  ARG_LIST
                                    VAR
                                      ZVAL "read#13"
                            CATCH_LIST
                              CATCH
                                NAME_LIST
                                  ZVAL "Sharp\\InvalidFields"
                                ZVAL "inner#15"
                                STMT_LIST
                                  FOREACH
                                    PROP
                                      VAR
                                        ZVAL "inner#15"
                                      ZVAL "fields"
                                    VAR
                                      ZVAL "problem#17"
                                    VAR
                                      ZVAL "key#16"
                                    STMT_LIST
                                      ASSIGN
                                        DIM
                                          VAR
                                            ZVAL "fields#2"
                                          BINARY_OP [8]
                                            BINARY_OP [8]
                                              ZVAL "address"
                                              ZVAL "."
                                            VAR
                                              ZVAL "key#16"
                                        VAR
                                          ZVAL "problem#17"
                                  ASSIGN
                                    DIM
                                      VAR
                                        ZVAL "failed#14"
                                      null
                                    ZVAL "address"
                            null
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "App.Address"
                        VAR
                          ZVAL "read#13"
                        ZVAL "address"
                        VAR
                          ZVAL "fields#2"
                    FOREACH
                      VAR
                        ZVAL "failed#14"
                      VAR
                        ZVAL "path#18"
                      null
                      STMT_LIST
                        UNSET
                          DIM
                            VAR
                              ZVAL "fields#2"
                            VAR
                              ZVAL "path#18"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "address"
                      VAR
                        ZVAL "read#13"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "address"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "addresses"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#19"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "addresses"
                    ASSIGN
                      VAR
                        ZVAL "failed#20"
                      ARRAY [3]
                    IF
                      IF_ELEM
                        CALL
                          ZVAL "is_array"
                          ARG_LIST
                            VAR
                              ZVAL "read#19"
                        STMT_LIST
                          FOREACH
                            VAR
                              ZVAL "read#19"
                            VAR
                              ZVAL "element#22"
                            VAR
                              ZVAL "key#21"
                            STMT_LIST
                              IF
                                IF_ELEM
                                  AND
                                    UNARY_OP [14]
                                      INSTANCEOF
                                        VAR
                                          ZVAL "element#22"
                                        ZVAL "App\\Address"
                                    OR
                                      CALL
                                        ZVAL "is_array"
                                        ARG_LIST
                                          VAR
                                            ZVAL "element#22"
                                      CALL
                                        ZVAL "is_object"
                                        ARG_LIST
                                          VAR
                                            ZVAL "element#22"
                                  STMT_LIST
                                    TRY
                                      STMT_LIST
                                        ASSIGN
                                          VAR
                                            ZVAL "element#22"
                                          STATIC_CALL
                                            ZVAL "App\\Address"
                                            ZVAL "parse"
                                            ARG_LIST
                                              VAR
                                                ZVAL "element#22"
                                      CATCH_LIST
                                        CATCH
                                          NAME_LIST
                                            ZVAL "Sharp\\InvalidFields"
                                          ZVAL "inner#23"
                                          STMT_LIST
                                            FOREACH
                                              PROP
                                                VAR
                                                  ZVAL "inner#23"
                                                ZVAL "fields"
                                              VAR
                                                ZVAL "problem#25"
                                              VAR
                                                ZVAL "key#24"
                                              STMT_LIST
                                                ASSIGN
                                                  DIM
                                                    VAR
                                                      ZVAL "fields#2"
                                                    BINARY_OP [8]
                                                      BINARY_OP [8]
                                                        BINARY_OP [8]
                                                          BINARY_OP [8]
                                                            BINARY_OP [8]
                                                              ZVAL "addresses"
                                                              ZVAL "["
                                                            VAR
                                                              ZVAL "key#21"
                                                          ZVAL "]"
                                                        ZVAL "."
                                                      VAR
                                                        ZVAL "key#24"
                                                  VAR
                                                    ZVAL "problem#25"
                                            ASSIGN
                                              DIM
                                                VAR
                                                  ZVAL "failed#20"
                                                null
                                              BINARY_OP [8]
                                                BINARY_OP [8]
                                                  BINARY_OP [8]
                                                    ZVAL "addresses"
                                                    ZVAL "["
                                                  VAR
                                                    ZVAL "key#21"
                                                ZVAL "]"
                                      null
                              ASSIGN
                                DIM
                                  VAR
                                    ZVAL "read#19"
                                  VAR
                                    ZVAL "key#21"
                                VAR
                                  ZVAL "element#22"
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "List<App.Address>"
                        VAR
                          ZVAL "read#19"
                        ZVAL "addresses"
                        VAR
                          ZVAL "fields#2"
                    FOREACH
                      VAR
                        ZVAL "failed#20"
                      VAR
                        ZVAL "path#26"
                      null
                      STMT_LIST
                        UNSET
                          DIM
                            VAR
                              ZVAL "fields#2"
                            VAR
                              ZVAL "path#26"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "addresses"
                      VAR
                        ZVAL "read#19"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "addresses"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "prices"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#27"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "prices"
                    IF
                      IF_ELEM
                        CALL
                          ZVAL "Sharp\\Internal\\accepts"
                          ARG_LIST
                            ZVAL "Map<App.Status, int>"
                            VAR
                              ZVAL "read#27"
                            ZVAL "prices"
                            VAR
                              ZVAL "fields#2"
                        STMT_LIST
                          ASSIGN
                            VAR
                              ZVAL "read#27"
                            CONDITIONAL
                              CALL
                                ZVAL "is_array"
                                ARG_LIST
                                  VAR
                                    ZVAL "read#27"
                              NEW
                                ZVAL "Sharp\\Map"
                                ARG_LIST
                                  CALL
                                    ZVAL "array_merge"
                                    ARG_LIST
                                      ARRAY [3]
                                      UNPACK
                                        CALL
                                          ZVAL "array_map"
                                          ARG_LIST
                                            ARROW_FUNC "" @3-3
                                              PARAM_LIST
                                                PARAM
                                                  null
                                                  ZVAL "key#29"
                                                  null
                                                  null
                                                  null
                                                  null
                                                PARAM
                                                  null
                                                  ZVAL "value#30"
                                                  null
                                                  null
                                                  null
                                                  null
                                              null
                                              ARRAY [3]
                                                ARRAY_ELEM
                                                  STATIC_CALL
                                                    ZVAL "App\\Status"
                                                    ZVAL "from"
                                                    ARG_LIST
                                                      CAST [6]
                                                        VAR
                                                          ZVAL "key#29"
                                                  null
                                                ARRAY_ELEM
                                                  VAR
                                                    ZVAL "value#30"
                                                  null
                                              null
                                              null
                                            CALL
                                              ZVAL "array_keys"
                                              ARG_LIST
                                                VAR
                                                  ZVAL "read#27"
                                            VAR
                                              ZVAL "read#27"
                              VAR
                                ZVAL "read#27"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "prices"
                      VAR
                        ZVAL "read#27"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "prices"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "plan"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#29"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "plan"
                    ASSIGN
                      VAR
                        ZVAL "failed#30"
                      ARRAY [3]
                    IF
                      IF_ELEM
                        AND
                          UNARY_OP [14]
                            INSTANCEOF
                              VAR
                                ZVAL "read#29"
                              ZVAL "App\\Plan"
                          OR
                            CALL
                              ZVAL "is_array"
                              ARG_LIST
                                VAR
                                  ZVAL "read#29"
                            CALL
                              ZVAL "is_object"
                              ARG_LIST
                                VAR
                                  ZVAL "read#29"
                        STMT_LIST
                          TRY
                            STMT_LIST
                              ASSIGN
                                VAR
                                  ZVAL "read#29"
                                STATIC_CALL
                                  ZVAL "App\\Plan"
                                  ZVAL "parse"
                                  ARG_LIST
                                    VAR
                                      ZVAL "read#29"
                            CATCH_LIST
                              CATCH
                                NAME_LIST
                                  ZVAL "Sharp\\InvalidFields"
                                ZVAL "inner#31"
                                STMT_LIST
                                  FOREACH
                                    PROP
                                      VAR
                                        ZVAL "inner#31"
                                      ZVAL "fields"
                                    VAR
                                      ZVAL "problem#33"
                                    VAR
                                      ZVAL "key#32"
                                    STMT_LIST
                                      ASSIGN
                                        DIM
                                          VAR
                                            ZVAL "fields#2"
                                          BINARY_OP [8]
                                            BINARY_OP [8]
                                              ZVAL "plan"
                                              ZVAL "."
                                            VAR
                                              ZVAL "key#32"
                                        VAR
                                          ZVAL "problem#33"
                                  ASSIGN
                                    DIM
                                      VAR
                                        ZVAL "failed#30"
                                      null
                                    ZVAL "plan"
                            null
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "App.Plan"
                        VAR
                          ZVAL "read#29"
                        ZVAL "plan"
                        VAR
                          ZVAL "fields#2"
                    FOREACH
                      VAR
                        ZVAL "failed#30"
                      VAR
                        ZVAL "path#34"
                      null
                      STMT_LIST
                        UNSET
                          DIM
                            VAR
                              ZVAL "fields#2"
                            VAR
                              ZVAL "path#34"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "plan"
                      VAR
                        ZVAL "read#29"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "plan"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "extra"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "arguments#3"
                      ZVAL "extra"
                    DIM
                      VAR
                        ZVAL "input#1"
                      ZVAL "extra"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "arguments#3"
                      ZVAL "extra"
                    ZVAL null

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "reference"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#35"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "reference"
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "int|string"
                        VAR
                          ZVAL "read#35"
                        ZVAL "reference"
                        VAR
                          ZVAL "fields#2"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "reference"
                      VAR
                        ZVAL "read#35"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "reference"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "place"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#37"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "place"
                    IF
                      IF_ELEM
                        AND
                          UNARY_OP [14]
                            INSTANCEOF
                              VAR
                                ZVAL "read#37"
                              ZVAL "App\\Point"
                          AND
                            UNARY_OP [14]
                              INSTANCEOF
                                VAR
                                  ZVAL "read#37"
                                ZVAL "App\\Address"
                            OR
                              CALL
                                ZVAL "is_array"
                                ARG_LIST
                                  VAR
                                    ZVAL "read#37"
                              CALL
                                ZVAL "is_object"
                                ARG_LIST
                                  VAR
                                    ZVAL "read#37"
                        STMT_LIST
                          STMT_LIST
                            TRY
                              STMT_LIST
                                ASSIGN
                                  VAR
                                    ZVAL "read#37"
                                  STATIC_CALL
                                    ZVAL "App\\Address"
                                    ZVAL "parse"
                                    ARG_LIST
                                      VAR
                                        ZVAL "read#37"
                              CATCH_LIST
                                CATCH
                                  NAME_LIST
                                    ZVAL "Sharp\\InvalidFields"
                                  null
                                  STMT_LIST
                                    TRY
                                      STMT_LIST
                                        ASSIGN
                                          VAR
                                            ZVAL "read#37"
                                          STATIC_CALL
                                            ZVAL "App\\Point"
                                            ZVAL "parse"
                                            ARG_LIST
                                              VAR
                                                ZVAL "read#37"
                                      CATCH_LIST
                                        CATCH
                                          NAME_LIST
                                            ZVAL "Sharp\\InvalidFields"
                                          null
                                          STMT_LIST
                                      null
                              null
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "App.Address|App.Point"
                        VAR
                          ZVAL "read#37"
                        ZVAL "place"
                        VAR
                          ZVAL "fields#2"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "place"
                      VAR
                        ZVAL "read#37"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "place"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "spot"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#39"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "spot"
                    IF
                      IF_ELEM
                        AND
                          UNARY_OP [14]
                            INSTANCEOF
                              VAR
                                ZVAL "read#39"
                              ZVAL "App\\Address"
                          AND
                            UNARY_OP [14]
                              INSTANCEOF
                                VAR
                                  ZVAL "read#39"
                                ZVAL "App\\Point"
                            OR
                              CALL
                                ZVAL "is_array"
                                ARG_LIST
                                  VAR
                                    ZVAL "read#39"
                              CALL
                                ZVAL "is_object"
                                ARG_LIST
                                  VAR
                                    ZVAL "read#39"
                        STMT_LIST
                          STMT_LIST
                            TRY
                              STMT_LIST
                                ASSIGN
                                  VAR
                                    ZVAL "read#39"
                                  STATIC_CALL
                                    ZVAL "App\\Point"
                                    ZVAL "parse"
                                    ARG_LIST
                                      VAR
                                        ZVAL "read#39"
                              CATCH_LIST
                                CATCH
                                  NAME_LIST
                                    ZVAL "Sharp\\InvalidFields"
                                  null
                                  STMT_LIST
                                    TRY
                                      STMT_LIST
                                        ASSIGN
                                          VAR
                                            ZVAL "read#39"
                                          STATIC_CALL
                                            ZVAL "App\\Address"
                                            ZVAL "parse"
                                            ARG_LIST
                                              VAR
                                                ZVAL "read#39"
                                      CATCH_LIST
                                        CATCH
                                          NAME_LIST
                                            ZVAL "Sharp\\InvalidFields"
                                          null
                                          STMT_LIST
                                      null
                              null
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "(App.Address|App.Point)?"
                        VAR
                          ZVAL "read#39"
                        ZVAL "spot"
                        VAR
                          ZVAL "fields#2"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "spot"
                      VAR
                        ZVAL "read#39"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "arguments#3"
                      ZVAL "spot"
                    ZVAL null

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "spots"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#41"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "spots"
                    IF
                      IF_ELEM
                        CALL
                          ZVAL "is_array"
                          ARG_LIST
                            VAR
                              ZVAL "read#41"
                        STMT_LIST
                          FOREACH
                            VAR
                              ZVAL "read#41"
                            VAR
                              ZVAL "element#44"
                            VAR
                              ZVAL "key#43"
                            STMT_LIST
                              IF
                                IF_ELEM
                                  AND
                                    UNARY_OP [14]
                                      INSTANCEOF
                                        VAR
                                          ZVAL "element#44"
                                        ZVAL "App\\Address"
                                    AND
                                      UNARY_OP [14]
                                        INSTANCEOF
                                          VAR
                                            ZVAL "element#44"
                                          ZVAL "App\\Point"
                                      OR
                                        CALL
                                          ZVAL "is_array"
                                          ARG_LIST
                                            VAR
                                              ZVAL "element#44"
                                        CALL
                                          ZVAL "is_object"
                                          ARG_LIST
                                            VAR
                                              ZVAL "element#44"
                                  STMT_LIST
                                    STMT_LIST
                                      TRY
                                        STMT_LIST
                                          ASSIGN
                                            VAR
                                              ZVAL "element#44"
                                            STATIC_CALL
                                              ZVAL "App\\Point"
                                              ZVAL "parse"
                                              ARG_LIST
                                                VAR
                                                  ZVAL "element#44"
                                        CATCH_LIST
                                          CATCH
                                            NAME_LIST
                                              ZVAL "Sharp\\InvalidFields"
                                            null
                                            STMT_LIST
                                              TRY
                                                STMT_LIST
                                                  ASSIGN
                                                    VAR
                                                      ZVAL "element#44"
                                                    STATIC_CALL
                                                      ZVAL "App\\Address"
                                                      ZVAL "parse"
                                                      ARG_LIST
                                                        VAR
                                                          ZVAL "element#44"
                                                CATCH_LIST
                                                  CATCH
                                                    NAME_LIST
                                                      ZVAL "Sharp\\InvalidFields"
                                                    null
                                                    STMT_LIST
                                                null
                                        null
                              ASSIGN
                                DIM
                                  VAR
                                    ZVAL "read#41"
                                  VAR
                                    ZVAL "key#43"
                                VAR
                                  ZVAL "element#44"
                    IF
                      IF_ELEM
                        CALL
                          ZVAL "Sharp\\Internal\\accepts"
                          ARG_LIST
                            ZVAL "Map<string, App.Address|App.Point>"
                            VAR
                              ZVAL "read#41"
                            ZVAL "spots"
                            VAR
                              ZVAL "fields#2"
                        STMT_LIST
                          ASSIGN
                            VAR
                              ZVAL "read#41"
                            CONDITIONAL
                              CALL
                                ZVAL "is_array"
                                ARG_LIST
                                  VAR
                                    ZVAL "read#41"
                              NEW
                                ZVAL "Sharp\\Map"
                                ARG_LIST
                                  CALL
                                    ZVAL "array_merge"
                                    ARG_LIST
                                      ARRAY [3]
                                      UNPACK
                                        CALL
                                          ZVAL "array_map"
                                          ARG_LIST
                                            ARROW_FUNC "" @3-3
                                              PARAM_LIST
                                                PARAM
                                                  null
                                                  ZVAL "key#45"
                                                  null
                                                  null
                                                  null
                                                  null
                                                PARAM
                                                  null
                                                  ZVAL "value#46"
                                                  null
                                                  null
                                                  null
                                                  null
                                              null
                                              ARRAY [3]
                                                ARRAY_ELEM
                                                  CAST [6]
                                                    VAR
                                                      ZVAL "key#45"
                                                  null
                                                ARRAY_ELEM
                                                  VAR
                                                    ZVAL "value#46"
                                                  null
                                              null
                                              null
                                            CALL
                                              ZVAL "array_keys"
                                              ARG_LIST
                                                VAR
                                                  ZVAL "read#41"
                                            VAR
                                              ZVAL "read#41"
                              VAR
                                ZVAL "read#41"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "spots"
                      VAR
                        ZVAL "read#41"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "spots"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "seen"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#45"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "seen"
                    IF
                      IF_ELEM
                        CALL
                          ZVAL "is_array"
                          ARG_LIST
                            VAR
                              ZVAL "read#45"
                        STMT_LIST
                          FOREACH
                            VAR
                              ZVAL "read#45"
                            VAR
                              ZVAL "element#48"
                            VAR
                              ZVAL "key#47"
                            STMT_LIST
                              IF
                                IF_ELEM
                                  AND
                                    CALL
                                      ZVAL "is_string"
                                      ARG_LIST
                                        VAR
                                          ZVAL "element#48"
                                    BINARY_OP [17]
                                      ASSIGN
                                        VAR
                                          ZVAL "case#49"
                                        STATIC_CALL
                                          ZVAL "App\\Status"
                                          ZVAL "tryFrom"
                                          ARG_LIST
                                            CAST [6]
                                              VAR
                                                ZVAL "element#48"
                                      ZVAL null
                                  STMT_LIST
                                    ASSIGN
                                      VAR
                                        ZVAL "element#48"
                                      VAR
                                        ZVAL "case#49"
                              ASSIGN
                                DIM
                                  VAR
                                    ZVAL "read#45"
                                  VAR
                                    ZVAL "key#47"
                                VAR
                                  ZVAL "element#48"
                    IF
                      IF_ELEM
                        CALL
                          ZVAL "Sharp\\Internal\\accepts"
                          ARG_LIST
                            ZVAL "Set<App.Status>"
                            VAR
                              ZVAL "read#45"
                            ZVAL "seen"
                            VAR
                              ZVAL "fields#2"
                        STMT_LIST
                          ASSIGN
                            VAR
                              ZVAL "read#45"
                            CONDITIONAL
                              CALL
                                ZVAL "is_array"
                                ARG_LIST
                                  VAR
                                    ZVAL "read#45"
                              NEW
                                ZVAL "Sharp\\Set"
                                ARG_LIST
                                  VAR
                                    ZVAL "read#45"
                              VAR
                                ZVAL "read#45"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "seen"
                      VAR
                        ZVAL "read#45"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "seen"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "groups"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#50"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "groups"
                    IF
                      IF_ELEM
                        CALL
                          ZVAL "Sharp\\Internal\\accepts"
                          ARG_LIST
                            ZVAL "List<List<Set<int>>>"
                            VAR
                              ZVAL "read#50"
                            ZVAL "groups"
                            VAR
                              ZVAL "fields#2"
                        STMT_LIST
                          ASSIGN
                            VAR
                              ZVAL "read#50"
                            CONDITIONAL
                              CALL
                                ZVAL "is_array"
                                ARG_LIST
                                  VAR
                                    ZVAL "read#50"
                              CALL
                                ZVAL "array_map"
                                ARG_LIST
                                  ARROW_FUNC "" @3-3
                                    PARAM_LIST
                                      PARAM
                                        null
                                        ZVAL "element#52"
                                        null
                                        null
                                        null
                                        null
                                    null
                                    CONDITIONAL
                                      CALL
                                        ZVAL "is_array"
                                        ARG_LIST
                                          VAR
                                            ZVAL "element#52"
                                      CALL
                                        ZVAL "array_map"
                                        ARG_LIST
                                          ARROW_FUNC "" @3-3
                                            PARAM_LIST
                                              PARAM
                                                null
                                                ZVAL "element#53"
                                                null
                                                null
                                                null
                                                null
                                            null
                                            CONDITIONAL
                                              CALL
                                                ZVAL "is_array"
                                                ARG_LIST
                                                  VAR
                                                    ZVAL "element#53"
                                              NEW
                                                ZVAL "Sharp\\Set"
                                                ARG_LIST
                                                  VAR
                                                    ZVAL "element#53"
                                              VAR
                                                ZVAL "element#53"
                                            null
                                            null
                                          VAR
                                            ZVAL "element#52"
                                      VAR
                                        ZVAL "element#52"
                                    null
                                    null
                                  VAR
                                    ZVAL "read#50"
                              VAR
                                ZVAL "read#50"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "groups"
                      VAR
                        ZVAL "read#50"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "groups"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "note"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#52"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "note"
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "Any"
                        VAR
                          ZVAL "read#52"
                        ZVAL "note"
                        VAR
                          ZVAL "fields#2"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "note"
                      VAR
                        ZVAL "read#52"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "note"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "paid"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#54"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "paid"
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "bool"
                        VAR
                          ZVAL "read#54"
                        ZVAL "paid"
                        VAR
                          ZVAL "fields#2"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "paid"
                      VAR
                        ZVAL "read#54"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "paid"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "rank"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#56"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "rank"
                    IF
                      IF_ELEM
                        AND
                          CALL
                            ZVAL "is_int"
                            ARG_LIST
                              VAR
                                ZVAL "read#56"
                          BINARY_OP [17]
                            ASSIGN
                              VAR
                                ZVAL "case#58"
                              STATIC_CALL
                                ZVAL "App\\Rank"
                                ZVAL "tryFrom"
                                ARG_LIST
                                  VAR
                                    ZVAL "read#56"
                            ZVAL null
                        STMT_LIST
                          ASSIGN
                            VAR
                              ZVAL "read#56"
                            VAR
                              ZVAL "case#58"
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "App.Rank"
                        VAR
                          ZVAL "read#56"
                        ZVAL "rank"
                        VAR
                          ZVAL "fields#2"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "rank"
                      VAR
                        ZVAL "read#56"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "rank"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "mood"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#59"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "mood"
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "App.Mood"
                        VAR
                          ZVAL "read#59"
                        ZVAL "mood"
                        VAR
                          ZVAL "fields#2"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "mood"
                      VAR
                        ZVAL "read#59"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "mood"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "trail"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#61"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "trail"
                    ASSIGN
                      VAR
                        ZVAL "failed#62"
                      ARRAY [3]
                    IF
                      IF_ELEM
                        AND
                          UNARY_OP [14]
                            INSTANCEOF
                              VAR
                                ZVAL "read#61"
                              ZVAL "App\\Point"
                          OR
                            CALL
                              ZVAL "is_array"
                              ARG_LIST
                                VAR
                                  ZVAL "read#61"
                            CALL
                              ZVAL "is_object"
                              ARG_LIST
                                VAR
                                  ZVAL "read#61"
                        STMT_LIST
                          STMT_LIST
                            TRY
                              STMT_LIST
                                ASSIGN
                                  VAR
                                    ZVAL "read#61"
                                  STATIC_CALL
                                    ZVAL "App\\Point"
                                    ZVAL "parse"
                                    ARG_LIST
                                      VAR
                                        ZVAL "read#61"
                              CATCH_LIST
                                CATCH
                                  NAME_LIST
                                    ZVAL "Sharp\\InvalidFields"
                                  null
                                  STMT_LIST
                              null
                    IF
                      IF_ELEM
                        CALL
                          ZVAL "is_array"
                          ARG_LIST
                            VAR
                              ZVAL "read#61"
                        STMT_LIST
                          FOREACH
                            VAR
                              ZVAL "read#61"
                            VAR
                              ZVAL "element#64"
                            VAR
                              ZVAL "key#63"
                            STMT_LIST
                              IF
                                IF_ELEM
                                  AND
                                    UNARY_OP [14]
                                      INSTANCEOF
                                        VAR
                                          ZVAL "element#64"
                                        ZVAL "App\\Point"
                                    OR
                                      CALL
                                        ZVAL "is_array"
                                        ARG_LIST
                                          VAR
                                            ZVAL "element#64"
                                      CALL
                                        ZVAL "is_object"
                                        ARG_LIST
                                          VAR
                                            ZVAL "element#64"
                                  STMT_LIST
                                    TRY
                                      STMT_LIST
                                        ASSIGN
                                          VAR
                                            ZVAL "element#64"
                                          STATIC_CALL
                                            ZVAL "App\\Point"
                                            ZVAL "parse"
                                            ARG_LIST
                                              VAR
                                                ZVAL "element#64"
                                      CATCH_LIST
                                        CATCH
                                          NAME_LIST
                                            ZVAL "Sharp\\InvalidFields"
                                          ZVAL "inner#65"
                                          STMT_LIST
                                            FOREACH
                                              PROP
                                                VAR
                                                  ZVAL "inner#65"
                                                ZVAL "fields"
                                              VAR
                                                ZVAL "problem#67"
                                              VAR
                                                ZVAL "key#66"
                                              STMT_LIST
                                                ASSIGN
                                                  DIM
                                                    VAR
                                                      ZVAL "fields#2"
                                                    BINARY_OP [8]
                                                      BINARY_OP [8]
                                                        BINARY_OP [8]
                                                          BINARY_OP [8]
                                                            BINARY_OP [8]
                                                              ZVAL "trail"
                                                              ZVAL "["
                                                            VAR
                                                              ZVAL "key#63"
                                                          ZVAL "]"
                                                        ZVAL "."
                                                      VAR
                                                        ZVAL "key#66"
                                                  VAR
                                                    ZVAL "problem#67"
                                            ASSIGN
                                              DIM
                                                VAR
                                                  ZVAL "failed#62"
                                                null
                                              BINARY_OP [8]
                                                BINARY_OP [8]
                                                  BINARY_OP [8]
                                                    ZVAL "trail"
                                                    ZVAL "["
                                                  VAR
                                                    ZVAL "key#63"
                                                ZVAL "]"
                                      null
                              ASSIGN
                                DIM
                                  VAR
                                    ZVAL "read#61"
                                  VAR
                                    ZVAL "key#63"
                                VAR
                                  ZVAL "element#64"
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "App.Point|List<App.Point>"
                        VAR
                          ZVAL "read#61"
                        ZVAL "trail"
                        VAR
                          ZVAL "fields#2"
                    FOREACH
                      VAR
                        ZVAL "failed#62"
                      VAR
                        ZVAL "path#68"
                      null
                      STMT_LIST
                        UNSET
                          DIM
                            VAR
                              ZVAL "fields#2"
                            VAR
                              ZVAL "path#68"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "trail"
                      VAR
                        ZVAL "read#61"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "trail"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "rows"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#69"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "rows"
                    ASSIGN
                      VAR
                        ZVAL "failed#70"
                      ARRAY [3]
                    IF
                      IF_ELEM
                        CALL
                          ZVAL "is_array"
                          ARG_LIST
                            VAR
                              ZVAL "read#69"
                        STMT_LIST
                          FOREACH
                            VAR
                              ZVAL "read#69"
                            VAR
                              ZVAL "element#72"
                            VAR
                              ZVAL "key#71"
                            STMT_LIST
                              IF
                                IF_ELEM
                                  CALL
                                    ZVAL "is_array"
                                    ARG_LIST
                                      VAR
                                        ZVAL "element#72"
                                  STMT_LIST
                                    FOREACH
                                      VAR
                                        ZVAL "element#72"
                                      VAR
                                        ZVAL "element#74"
                                      VAR
                                        ZVAL "key#73"
                                      STMT_LIST
                                        IF
                                          IF_ELEM
                                            AND
                                              UNARY_OP [14]
                                                INSTANCEOF
                                                  VAR
                                                    ZVAL "element#74"
                                                  ZVAL "App\\Point"
                                              OR
                                                CALL
                                                  ZVAL "is_array"
                                                  ARG_LIST
                                                    VAR
                                                      ZVAL "element#74"
                                                CALL
                                                  ZVAL "is_object"
                                                  ARG_LIST
                                                    VAR
                                                      ZVAL "element#74"
                                            STMT_LIST
                                              TRY
                                                STMT_LIST
                                                  ASSIGN
                                                    VAR
                                                      ZVAL "element#74"
                                                    STATIC_CALL
                                                      ZVAL "App\\Point"
                                                      ZVAL "parse"
                                                      ARG_LIST
                                                        VAR
                                                          ZVAL "element#74"
                                                CATCH_LIST
                                                  CATCH
                                                    NAME_LIST
                                                      ZVAL "Sharp\\InvalidFields"
                                                    ZVAL "inner#75"
                                                    STMT_LIST
                                                      FOREACH
                                                        PROP
                                                          VAR
                                                            ZVAL "inner#75"
                                                          ZVAL "fields"
                                                        VAR
                                                          ZVAL "problem#77"
                                                        VAR
                                                          ZVAL "key#76"
                                                        STMT_LIST
                                                          ASSIGN
                                                            DIM
                                                              VAR
                                                                ZVAL "fields#2"
                                                              BINARY_OP [8]
                                                                BINARY_OP [8]
                                                                  BINARY_OP [8]
                                                                    BINARY_OP [8]
                                                                      BINARY_OP [8]
                                                                        BINARY_OP [8]
                                                                          BINARY_OP [8]
                                                                            BINARY_OP [8]
                                                                              ZVAL "rows"
                                                                              ZVAL "["
                                                                            VAR
                                                                              ZVAL "key#71"
                                                                          ZVAL "]"
                                                                        ZVAL "["
                                                                      VAR
                                                                        ZVAL "key#73"
                                                                    ZVAL "]"
                                                                  ZVAL "."
                                                                VAR
                                                                  ZVAL "key#76"
                                                            VAR
                                                              ZVAL "problem#77"
                                                      ASSIGN
                                                        DIM
                                                          VAR
                                                            ZVAL "failed#70"
                                                          null
                                                        BINARY_OP [8]
                                                          BINARY_OP [8]
                                                            BINARY_OP [8]
                                                              BINARY_OP [8]
                                                                BINARY_OP [8]
                                                                  BINARY_OP [8]
                                                                    ZVAL "rows"
                                                                    ZVAL "["
                                                                  VAR
                                                                    ZVAL "key#71"
                                                                ZVAL "]"
                                                              ZVAL "["
                                                            VAR
                                                              ZVAL "key#73"
                                                          ZVAL "]"
                                                null
                                        ASSIGN
                                          DIM
                                            VAR
                                              ZVAL "element#72"
                                            VAR
                                              ZVAL "key#73"
                                          VAR
                                            ZVAL "element#74"
                              ASSIGN
                                DIM
                                  VAR
                                    ZVAL "read#69"
                                  VAR
                                    ZVAL "key#71"
                                VAR
                                  ZVAL "element#72"
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "List<List<App.Point>>"
                        VAR
                          ZVAL "read#69"
                        ZVAL "rows"
                        VAR
                          ZVAL "fields#2"
                    FOREACH
                      VAR
                        ZVAL "failed#70"
                      VAR
                        ZVAL "path#78"
                      null
                      STMT_LIST
                        UNSET
                          DIM
                            VAR
                              ZVAL "fields#2"
                            VAR
                              ZVAL "path#78"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "rows"
                      VAR
                        ZVAL "read#69"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "rows"
                    ZVAL "missing"

            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "corner"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#79"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "corner"
                    IF
                      IF_ELEM
                        AND
                          UNARY_OP [14]
                            INSTANCEOF
                              VAR
                                ZVAL "read#79"
                              ZVAL "App\\Address"
                          AND
                            UNARY_OP [14]
                              INSTANCEOF
                                VAR
                                  ZVAL "read#79"
                                ZVAL "App\\Point"
                            OR
                              CALL
                                ZVAL "is_array"
                                ARG_LIST
                                  VAR
                                    ZVAL "read#79"
                              CALL
                                ZVAL "is_object"
                                ARG_LIST
                                  VAR
                                    ZVAL "read#79"
                        STMT_LIST
                          STMT_LIST
                            TRY
                              STMT_LIST
                                ASSIGN
                                  VAR
                                    ZVAL "read#79"
                                  STATIC_CALL
                                    ZVAL "App\\Point"
                                    ZVAL "parse"
                                    ARG_LIST
                                      VAR
                                        ZVAL "read#79"
                              CATCH_LIST
                                CATCH
                                  NAME_LIST
                                    ZVAL "Sharp\\InvalidFields"
                                  null
                                  STMT_LIST
                                    TRY
                                      STMT_LIST
                                        ASSIGN
                                          VAR
                                            ZVAL "read#79"
                                          STATIC_CALL
                                            ZVAL "App\\Address"
                                            ZVAL "parse"
                                            ARG_LIST
                                              VAR
                                                ZVAL "read#79"
                                      CATCH_LIST
                                        CATCH
                                          NAME_LIST
                                            ZVAL "Sharp\\InvalidFields"
                                          null
                                          STMT_LIST
                                      null
                              null
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "App.Address|App.Point"
                        VAR
                          ZVAL "read#79"
                        ZVAL "corner"
                        VAR
                          ZVAL "fields#2"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "properties#4"
                        ZVAL "corner"
                      VAR
                        ZVAL "read#79"
        "#}
    );
}

const SIZE: &str = "namespace App;\n\npublic struct Size\n{\n    public int width { get; init; }\n    public int depth { get; init; }\n}\n";

/// ```php
/// public function copies(\App\Size $size, int $depth): \App\Size
/// {
///     $made = \Sharp\Internal\build(new \App\Size(),
///         function ($value#1, $value#2) { $this->width = $value#1; $this->depth = $value#2; return $this; }, 2, 1);
///     return (function ($value#1) { return clone($this, ['depth' => $value#1]); })->call($size, $depth);
/// }
/// ```
///
/// Spec section 16: an object initializer and `with` run in a closure bound to the object, as a named constructor
/// runs, so they write an `init` property at its accessor's access wherever the code is. The initializer writes each
/// value after `new`, through `\Sharp\Internal\build`, which binds the closure as `Closure::call` does while the
/// object is still being built, so its writes run no observer (spec section 6.2). `with` is PHP 8.5's `clone` with
/// the values, which runs each `init` hook on the copy. Each value is evaluated in the caller, as an argument, and the
/// shorthand `depth` is the local `depth`.
#[test]
fn with_and_an_initializer_run_in_a_closure_bound_to_the_object() {
    assert_eq!(
        Lowered::with(
            &method_in(
                "Report",
                "Size copies(Size size, int depth)",
                "        Size made = new Size { width: 2, depth: 1 };\n        return size with { depth };\n"
            )
            .replace("import Lib.Calc;", "import App.Size;"),
            &[("src/App/Size.sharp", SIZE)]
        )
        .declared("copies"),
        indoc! {r#"
            METHOD [1] "copies" @7-11
              PARAM_LIST
                PARAM
                  ZVAL "App\\Size"
                  ZVAL "size"
                  null
                  null
                  null
                  null
                PARAM
                  ZVAL [1] "int"
                  ZVAL "depth"
                  null
                  null
                  null
                  null
              null
              STMT_LIST
                ASSIGN
                  VAR
                    ZVAL "made"
                  CALL
                    ZVAL "Sharp\\Internal\\build"
                    ARG_LIST
                      NEW
                        ZVAL "App\\Size"
                        ARG_LIST
                      CLOSURE "" @9-9
                        PARAM_LIST
                          PARAM
                            null
                            ZVAL "value#1"
                            null
                            null
                            null
                            null
                          PARAM
                            null
                            ZVAL "value#2"
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
                              ZVAL "width"
                            VAR
                              ZVAL "value#1"
                          ASSIGN
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "depth"
                            VAR
                              ZVAL "value#2"
                          RETURN
                            VAR
                              ZVAL "this"
                        null
                        null
                      ZVAL 2
                      ZVAL 1
                RETURN
                  METHOD_CALL
                    CLOSURE "" @10-10
                      PARAM_LIST
                        PARAM
                          null
                          ZVAL "value#1"
                          null
                          null
                          null
                          null
                      null
                      STMT_LIST
                        RETURN
                          CALL
                            ZVAL "clone"
                            ARG_LIST
                              VAR
                                ZVAL "this"
                              ARRAY [3]
                                ARRAY_ELEM
                                  VAR
                                    ZVAL "value#1"
                                  ZVAL "depth"
                      null
                      null
                    ZVAL "call"
                    ARG_LIST
                      VAR
                        ZVAL "size"
                      VAR
                        ZVAL "depth"
              SHARP_TYPE_ARGS
                ZVAL "App\\Size"
                ZVAL "Function<App.Size(App.Size, int)>"
              null
        "#}
    );
}

/// ```php
/// public static function tryParse(array|object $value): ?\App\Ticket
/// {
///     try { return \App\Ticket::parse($value); } catch (\Sharp\InvalidFields) { return null; }
/// }
/// ```
///
/// Spec section 10: a PHP# class that can be created parses the same way a struct does, so the same members give the
/// same `parse` and `tryParse`, and `tryParse` gives null where `parse` throws. A class with no constructor of its own
/// reads the main constructor it inherits, `[Key]` included, and a list of a class is built element by element through
/// that class's `parse`. A class whose parent declares `parse`, an abstract or a
/// static class, and a class or a struct that declares its own `parse` get neither method.
#[test]
fn a_class_parses_as_a_struct_does() {
    let members = "Ticket([Key(\"ticket_id\")] public int id { get; }, public int seats = 1 { get; }) { }\n\n    public string note { get; init; } = \"\";\n}\n";
    let lowered = |kind: &str| {
        Lowered::with(
            &format!("namespace App;\n\nimport Sharp.Key;\n\npublic {kind} Ticket\n{{\n    public {members}"),
            &[KEY],
        )
    };
    let (class, r#struct) = (lowered("class"), lowered("struct"));

    assert_eq!(class.declared("parse"), r#struct.declared("parse"));
    assert_eq!(class.declared("tryParse"), r#struct.declared("tryParse"));
    assert_eq!(
        class.declared("tryParse"),
        indoc! {r#"
            METHOD [17] "tryParse" @5-5
              PARAM_LIST
                PARAM
                  TYPE_UNION
                    TYPE [7]
                    ZVAL [1] "object"
                  ZVAL "value"
                  null
                  null
                  null
                  null
              null
              STMT_LIST
                TRY
                  STMT_LIST
                    RETURN
                      STATIC_CALL
                        ZVAL "App\\Ticket"
                        ZVAL "parse"
                        ARG_LIST
                          VAR
                            ZVAL "value"
                  CATCH_LIST
                    CATCH
                      NAME_LIST
                        ZVAL "Sharp\\InvalidFields"
                      null
                      STMT_LIST
                        RETURN
                          ZVAL null
                  null
              ZVAL [256] "App\\Ticket"
              null
        "#}
    );

    let note = (
        "src/App/Note.sharp",
        "namespace App;\n\nimport Sharp.Key;\n\npublic class Note\n{\n    public Note([Key(\"body\")] public string text { get; }) { }\n}\n",
    );
    assert_eq!(
        Lowered::with("namespace App;\n\npublic class Memo : Note\n{\n}\n", &[KEY, note]).parse_reads(),
        indoc! {r#"
            IF
              IF_ELEM
                CALL
                  ZVAL "array_key_exists"
                  ARG_LIST
                    ZVAL "body"
                    VAR
                      ZVAL "input#1"
                STMT_LIST
                  STMT_LIST
                    ASSIGN
                      VAR
                        ZVAL "read#8"
                      DIM
                        VAR
                          ZVAL "input#1"
                        ZVAL "body"
                    CALL
                      ZVAL "Sharp\\Internal\\accepts"
                      ARG_LIST
                        ZVAL "string"
                        VAR
                          ZVAL "read#8"
                        ZVAL "body"
                        VAR
                          ZVAL "fields#2"
                    ASSIGN
                      DIM
                        VAR
                          ZVAL "arguments#3"
                        ZVAL "text"
                      VAR
                        ZVAL "read#8"
              IF_ELEM
                null
                STMT_LIST
                  ASSIGN
                    DIM
                      VAR
                        ZVAL "fields#2"
                      ZVAL "body"
                    ZVAL "missing"
        "#}
    );

    let letter = Lowered::with(
        "namespace App;\n\npublic class Letter\n{\n    public Letter(public List<Note> copies { get; }) { }\n}\n",
        &[KEY, note],
    );
    let parsed: Vec<String> = letter
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_STATIC_CALL)
        .map(|(index, _)| letter.text(letter.nodes()[letter.child(index as u32, 0) as usize].text))
        .collect();
    assert_eq!(parsed, ["App\\Note", "App\\Letter"], "each copy through `Note::parse`, then `tryParse`'s own call");

    let methods = |code: &str| {
        let clock = (
            "src/Lib/Clock.php",
            "<?php namespace Lib; class Clock { public static function parse(string $text): static { return new static(); } }",
        );
        let lowered = Lowered::with(code, &[clock]);

        lowered
            .nodes()
            .iter()
            .filter(|node| node.kind == sharp_kind::SHARP_AST_METHOD)
            .map(|node| lowered.text(node.text))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        methods(
            "namespace App;\n\nimport Lib.Clock;\n\npublic class Deadline : Clock\n{\n    public Deadline(public int at { get; }) { }\n}\n"
        ),
        ["__construct"]
    );
    assert_eq!(
        methods(
            "namespace App;\n\npublic abstract class Shape\n{\n    public Shape(public int sides { get; }) { }\n}\n"
        ),
        ["__construct"]
    );
    assert_eq!(
        methods("namespace App;\n\npublic static class Rates\n{\n    public static int base() => 1;\n}\n"),
        ["base"]
    );
    assert_eq!(
        methods(
            "namespace App;\n\npublic class Money\n{\n    public Money(public int cents { get; }) { }\n\n    public static Money parse(string text) => new Money(1);\n}\n"
        ),
        ["__construct", "parse"]
    );
    assert_eq!(
        methods(
            "namespace App;\n\npublic struct Cents\n{\n    public Cents(public int value { get; }) { }\n\n    public static Cents parse(string text) => new Cents(1);\n}\n"
        ),
        ["__construct", "parse", "op_Equality"]
    );
}

/// Spec section 10: no input holds a function, so a struct with a function field, also in a list, lowers with no
/// `parse` or `tryParse`, which the checker refuses where they are called. A struct with a pure enum field keeps both,
/// as the enum has a type text, and the checker refuses those calls too.
#[test]
fn a_struct_that_holds_a_function_has_no_parse() {
    let methods = |code: &str| {
        let lowered = Lowered::with(
            code,
            &[("src/App/Mood.sharp", "namespace App;\n\npublic enum Mood\n{\n    case Calm;\n}\n")],
        );

        lowered
            .nodes()
            .iter()
            .filter(|node| node.kind == sharp_kind::SHARP_AST_METHOD)
            .map(|node| lowered.text(node.text))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        methods(
            "namespace App;\n\npublic struct Rule\n{\n    public Rule(public Function<bool(int)> check { get; }) { }\n\n    public bool allows(int value) => this.check(value);\n}\n"
        ),
        ["__construct", "allows", "op_Equality"]
    );
    assert_eq!(
        methods(
            "namespace App;\n\npublic struct Makers\n{\n    public Makers(public List<Function<int()>> builders { get; }) { }\n}\n"
        ),
        ["__construct", "op_Equality"]
    );
    assert_eq!(
        methods("namespace App;\n\npublic struct Feeling\n{\n    public Feeling(public Mood mood { get; }) { }\n}\n"),
        ["__construct", "op_Equality", "parse", "tryParse"]
    );
}

const POINT: &str = "namespace App;\n\npublic struct Point\n{\n    public Point(public int x { get; }, public float y { get; }, public Money? total { get; }) { }\n\n    public Point moved(int dx) => new Point(this.x + dx, this.y, this.total);\n}\n";

/// ```php
/// final class Point implements \Sharp\Struct
/// {
///     public function __construct(public readonly int $x, public readonly float $y, public readonly ?\App\Money $total) {}
///
///     public function moved(int $dx): \App\Point { return new \App\Point($this->x + $dx, $this->y, $this->total); }
///
///     public static function op_Equality(mixed $a, mixed $b): bool
///     {
///         if ($a instanceof \App\Point && $b instanceof \App\Point) {
///             return $a->x === $b->x && $a->y === $b->y && \App\Money::op_Equality($a->total, $b->total);
///         }
///         return $a === $b;
///     }
/// }
/// ```
///
/// A struct is a final class, `ZEND_ACC_FINAL` being 32, that implements its header's interfaces and `\Sharp\Struct`,
/// which the engine lets only a `.sharp` class implement, and whose get-only properties are `readonly`, 128. Its `==` is the
/// `op_Equality` the bridge writes: two instances of the struct are equal when each property equals its pair by the
/// property's own `==`, and any other pair, `null` included, is equal only when identical. The parameters take `mixed`,
/// so an `Any?` compares with a struct. A static field and a computed property hold no part of the value, so `==`
/// compares neither. The static field `made` is `[32788]`, `ZEND_ACC_FIELD | ZEND_ACC_STATIC | ZEND_ACC_PRIVATE`. A struct that declares `operator ==` gets no `op_Equality` of the bridge's: `==` calls its own,
/// whatever it holds.
#[test]
fn a_struct_is_a_final_class_whose_op_equality_compares_each_property() {
    assert_eq!(
        Lowered::with(
            &POINT.replace("public struct Point\n", "public struct Point : Shape\n").replace(
                "    public Point moved(",
                "    private static int made = 0;\n\n    public float sum { get => this.x + this.y; }\n\n    public Point moved("
            ),
            &[
                ("src/App/Money.sharp", MONEY),
                ("src/App/Shape.sharp", "namespace App;\n\npublic interface Shape\n{\n}\n")
            ]
        )
        .tree(),
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
                ZVAL "App"
                null
              CLASS [32] "Point" @3-12
                null
                NAME_LIST
                  ZVAL "App\\Shape"
                  ZVAL "Sharp\\Struct"
                STMT_LIST
                  METHOD [1] "__construct" @5-5
                    PARAM_LIST
                      PARAM [129]
                        ZVAL [1] "int"
                        ZVAL "x"
                        null
                        null
                        null
                        null
                      PARAM [129]
                        ZVAL [1] "float"
                        ZVAL "y"
                        null
                        null
                        null
                        null
                      PARAM [129]
                        ZVAL [256] "App\\Money"
                        ZVAL "total"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                    null
                    null
                  PROP_GROUP [32788]
                    ZVAL [1] "int"
                    PROP_DECL
                      PROP_ELEM
                        ZVAL "made"
                        ZVAL 0
                        null
                        null
                    null
                  PROP_GROUP [1]
                    ZVAL [1] "float"
                    PROP_DECL
                      PROP_ELEM
                        ZVAL "sum"
                        null
                        null
                        STMT_LIST
                          PROPERTY_HOOK "get" @9-9
                            null
                            null
                            PROPERTY_HOOK_SHORT_BODY
                              BINARY_OP [1]
                                PROP
                                  VAR
                                    ZVAL "this"
                                  ZVAL "x"
                                PROP
                                  VAR
                                    ZVAL "this"
                                  ZVAL "y"
                            null
                            null
                    null
                  METHOD [1] "moved" @11-11
                    PARAM_LIST
                      PARAM
                        ZVAL [1] "int"
                        ZVAL "dx"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                      RETURN
                        NEW
                          ZVAL "App\\Point"
                          ARG_LIST
                            BINARY_OP [1]
                              PROP
                                VAR
                                  ZVAL "this"
                                ZVAL "x"
                              VAR
                                ZVAL "dx"
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "y"
                            PROP
                              VAR
                                ZVAL "this"
                              ZVAL "total"
                    SHARP_TYPE_ARGS
                      ZVAL "App\\Point"
                      ZVAL "Function<App.Point(int)>"
                    null
                  METHOD [17] "op_Equality" @3-3
                    PARAM_LIST
                      PARAM
                        ZVAL [1] "mixed"
                        ZVAL "a"
                        null
                        null
                        null
                        null
                      PARAM
                        ZVAL [1] "mixed"
                        ZVAL "b"
                        null
                        null
                        null
                        null
                    null
                    STMT_LIST
                      IF
                        IF_ELEM
                          AND
                            INSTANCEOF
                              VAR
                                ZVAL "a"
                              ZVAL "App\\Point"
                            INSTANCEOF
                              VAR
                                ZVAL "b"
                              ZVAL "App\\Point"
                          STMT_LIST
                            RETURN
                              AND
                                AND
                                  BINARY_OP [16]
                                    PROP
                                      VAR
                                        ZVAL "a"
                                      ZVAL "x"
                                    PROP
                                      VAR
                                        ZVAL "b"
                                      ZVAL "x"
                                  BINARY_OP [16]
                                    PROP
                                      VAR
                                        ZVAL "a"
                                      ZVAL "y"
                                    PROP
                                      VAR
                                        ZVAL "b"
                                      ZVAL "y"
                                STATIC_CALL
                                  ZVAL "App\\Money"
                                  ZVAL "op_Equality"
                                  ARG_LIST
                                    PROP
                                      VAR
                                        ZVAL "a"
                                      ZVAL "total"
                                    PROP
                                      VAR
                                        ZVAL "b"
                                      ZVAL "total"
                      RETURN
                        BINARY_OP [16]
                          VAR
                            ZVAL "a"
                          VAR
                            ZVAL "b"
                    ZVAL [1] "bool"
                    null
                null
                null
        "#}
    );

    let declared = Lowered::new(
        "namespace App;\n\npublic struct Rule\n{\n    public Rule(public string name { get; }, public Function<bool(int)> check { get; }) { }\n\n    public static bool operator ==(Rule a, Rule b) => a.name == b.name;\n\n    public int hash() => 7;\n\n    public bool run(Rule other) => this == other;\n}\n",
    );
    assert_eq!(
        declared
            .nodes()
            .iter()
            .filter(|node| node.kind == sharp_kind::SHARP_AST_METHOD)
            .map(|node| declared.text(node.text))
            .collect::<Vec<_>>(),
        ["__construct", "op_Equality", "hash", "run"]
    );
    assert_eq!(
        declared.body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                STATIC_CALL
                  ZVAL "App\\Rule"
                  ZVAL "op_Equality"
                  ARG_LIST
                    VAR
                      ZVAL "this"
                    VAR
                      ZVAL "other"
        "#}
    );
}

/// ```php
/// return $rush->total(2) + \App\Text\Extensions::total($order, 2);
/// ```
///
/// Spec section 26: a receiver typed by a type parameter shows its bound, so a member the bound declares runs, as
/// `RushOrder`'s own `total` does for `TRush : RushOrder`, and an extension runs only where the bound declares none, as
/// for `TOrder : Order`.
#[test]
fn a_type_parameter_receiver_runs_the_member_its_bound_declares_over_an_extension() {
    let code = "namespace App.Tenant;\n\nimport App.Order;\nimport App.RushOrder;\nimport App.Text.Extensions;\n\nclass Report\n{\n    public int run<TRush : RushOrder, TOrder : Order>(TRush rush, TOrder order)\n    {\n        return rush.total(2) + order.total(2);\n    }\n}\n";

    assert_eq!(
        Lowered::with(code, &[ORDERS, EXTENSIONS]).body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [1]
                  METHOD_CALL
                    VAR
                      ZVAL "rush"
                    ZVAL "total"
                    ARG_LIST
                      ZVAL 2
                  STATIC_CALL
                    ZVAL "App\\Text\\Extensions"
                    ZVAL "total"
                    ARG_LIST
                      VAR
                        ZVAL "order"
                      ZVAL 2
        "#}
    );
}

/// ```php
/// return \App\Rush\Ranks::rank($rush) + \App\Text\Ranks::rank($order);
/// ```
///
/// Spec section 26: of two imported extensions that fit, the one whose receiver is the most specific runs, as in C# and
/// Kotlin: `Rush.Ranks` on a value shown as a `RushOrder`, and `Text.Ranks` on one shown as an `Order`.
#[test]
fn the_extension_with_the_most_specific_receiver_runs() {
    let orders = (
        "src/App/Text/Ranks.sharp",
        "namespace App.Text;\n\nimport App.Order;\n\npublic static class Ranks\n{\n    extension(Order order)\n    {\n        public int rank() => 1;\n    }\n}\n",
    );
    let rush = (
        "src/App/Rush/Ranks.sharp",
        "namespace App.Rush;\n\nimport App.RushOrder;\n\npublic static class Ranks\n{\n    extension(RushOrder order)\n    {\n        public int rank() => 2;\n    }\n}\n",
    );
    let code = "namespace App.Tenant;\n\nimport App.Order;\nimport App.RushOrder;\nimport App.Rush.Ranks as RushRanks;\nimport App.Text.Ranks;\n\nclass Report\n{\n    public int run(RushOrder rush, Order order)\n    {\n        return rush.rank() + order.rank();\n    }\n}\n";

    assert_eq!(
        Lowered::with(code, &[ORDERS, orders, rush]).body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [1]
                  STATIC_CALL
                    ZVAL "App\\Rush\\Ranks"
                    ZVAL "rank"
                    ARG_LIST
                      VAR
                        ZVAL "rush"
                  STATIC_CALL
                    ZVAL "App\\Text\\Ranks"
                    ZVAL "rank"
                    ARG_LIST
                      VAR
                        ZVAL "order"
        "#}
    );
}

/// ```php
/// return \App\Text\Extensions::slug($title, separator: '-')
///     . (string) \App\Text\Extensions::total($order, extra: 2);
/// ```
///
/// Spec section 26: a named argument passes to the extension's method by its name, as it does to any call.
#[test]
fn an_extension_member_passes_a_named_argument_by_its_name() {
    assert_eq!(
        extension_body(
            "string run(string title, Order order)",
            "        return title.slug(separator: \"-\") + (string) order.total(extra: 2);\n"
        ),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [8]
                  STATIC_CALL
                    ZVAL "App\\Text\\Extensions"
                    ZVAL "slug"
                    ARG_LIST
                      VAR
                        ZVAL "title"
                      NAMED_ARG
                        ZVAL "separator"
                        ZVAL "-"
                  CAST [6]
                    STATIC_CALL
                      ZVAL "App\\Text\\Extensions"
                      ZVAL "total"
                      ARG_LIST
                        VAR
                          ZVAL "order"
                        NAMED_ARG
                          ZVAL "extra"
                          ZVAL 2
        "#}
    );
}

/// `Sharp\Text\TextExtensions` at the standard library's path, beside a constant: its extension on `string` wraps
/// `trim` with a method, `strlen` with a computed property and `strtoupper` with a method whose block returns it.
const TEXT_EXTENSIONS: (&str, &str) = (
    "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text/TextExtensions.sharp",
    "namespace Sharp.Text;\n\npublic static class TextExtensions\n{\n    public const int LIMIT = 80;\n\n    extension(string text)\n    {\n        public string trim() => global.trim(text);\n\n        public int length => global.strlen(text);\n\n        public string upper()\n        {\n            return global.strtoupper(text);\n        }\n    }\n}\n",
);

/// ```php
/// return trim($name) . strlen(trim($name)) . strtoupper($name);
/// ```
///
/// Spec section 8: a standard library extension member whose body is one call inlines as that call, so `name.trim()`
/// runs as `trim($name)`, with no call of the library's method. Every file has the library's extensions.
#[test]
fn a_standard_library_extension_member_inlines_as_the_call_its_body_makes() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public string run(string name)\n    {\n        return name.trim() + (string) name.trim().length + name.upper();\n    }\n}\n";

    assert_eq!(
        Lowered::inlining(code, &[], &[TEXT_EXTENSIONS]).body(),
        indoc! {r#"
            STMT_LIST
              RETURN
                BINARY_OP [8]
                  BINARY_OP [8]
                    CALL
                      ZVAL "trim"
                      ARG_LIST
                        VAR
                          ZVAL "name"
                    CAST [6]
                      CALL
                        ZVAL "strlen"
                        ARG_LIST
                          CALL
                            ZVAL "trim"
                            ARG_LIST
                              VAR
                                ZVAL "name"
                  CALL
                    ZVAL "strtoupper"
                    ARG_LIST
                      VAR
                        ZVAL "name"
        "#}
    );
}

/// ```php
/// $total = ($order === null ? null : \App\Text\Extensions::total($order, 2)) ?? 0;
/// $blank = ($title === null ? null : \App\Text\Extensions::isBlank($title)) ?? false;
/// $kind = $order === null ? null : \App\Text\Extensions::kind($order->amount);
/// return $blank ? ($title === null ? null : \App\Text\Extensions::slug($title, '-')) : (string) $total;
/// ```
///
/// Spec section 26: `?.` reaches an extension's member only when the receiver is not null, as it does any member, so
/// the call runs under the conditional that tests the receiver. A member read with `.` later in the chain runs under
/// it too, since PHP's `?->` would skip only its own link and pass `null` to the static call.
#[test]
fn a_null_safe_extension_member_runs_only_on_a_receiver_that_is_not_null() {
    assert_eq!(
        extension_body(
            "string? run(string? title, Order? order)",
            "        const total = order?.total(2) ?? 0;\n        const blank = title?.isBlank ?? false;\n        const kind = order?.amount.kind;\n        return blank ? title?.slug(\"-\") : (string) total;\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "total"
                COALESCE
                  CONDITIONAL [1]
                    BINARY_OP [16]
                      VAR
                        ZVAL "order"
                      ZVAL null
                    ZVAL null
                    STATIC_CALL
                      ZVAL "App\\Text\\Extensions"
                      ZVAL "total"
                      ARG_LIST
                        VAR
                          ZVAL "order"
                        ZVAL 2
                  ZVAL 0
              ASSIGN
                VAR
                  ZVAL "blank"
                COALESCE
                  CONDITIONAL [1]
                    BINARY_OP [16]
                      VAR
                        ZVAL "title"
                      ZVAL null
                    ZVAL null
                    STATIC_CALL
                      ZVAL "App\\Text\\Extensions"
                      ZVAL "isBlank"
                      ARG_LIST
                        VAR
                          ZVAL "title"
                  ZVAL false
              ASSIGN
                VAR
                  ZVAL "kind"
                CONDITIONAL [1]
                  BINARY_OP [16]
                    VAR
                      ZVAL "order"
                    ZVAL null
                  ZVAL null
                  STATIC_CALL
                    ZVAL "App\\Text\\Extensions"
                    ZVAL "kind"
                    ARG_LIST
                      PROP
                        VAR
                          ZVAL "order"
                        ZVAL "amount"
              RETURN
                CONDITIONAL
                  VAR
                    ZVAL "blank"
                  CONDITIONAL [1]
                    BINARY_OP [16]
                      VAR
                        ZVAL "title"
                      ZVAL null
                    ZVAL null
                    STATIC_CALL
                      ZVAL "App\\Text\\Extensions"
                      ZVAL "slug"
                      ARG_LIST
                        VAR
                          ZVAL "title"
                        ZVAL "-"
                  CAST [6]
                    VAR
                      ZVAL "total"
        "#}
    );
}

/// Spec section 13: `readonly` is a promise the checker keeps about PHP# code, so the engine runs a readonly type as
/// the type and a readonly method as the method. A constructor's readonly parameter declares no property, and a
/// readonly `Set` parameter still makes its `Set` before the body runs.
#[test]
fn readonly_lowers_to_the_same_php_as_without_it() {
    let order = (
        "src/Order.sharp",
        "namespace App;\n\npublic class Order\n{\n    public int paid { get; set; } = 0;\n\n    public static bool operator ==(Order a, Order b) => a.paid == b.paid;\n\n    public int hash() => this.paid;\n}\n",
    );
    let shop = |readonly: &str, marker: &str| {
        format!(
            "namespace App;\n\npublic class Shop\n{{\n    private Order kept = new Order();\n\n    public Shop({readonly}Order order, {readonly}List<Order> orders) {{ }}\n\n    public {readonly}Order customer => this.kept;\n\n    public {readonly}Order? pick({readonly}Order order, {readonly}Set<Order> ids, Function<int({readonly}Order)> paid){marker}\n    {{\n        {readonly}Order held = order;\n        const count = ({readonly}Order o) => o.paid;\n        return ids.contains(held) && count(held) > 0 ? held : null;\n    }}\n}}\n"
        )
    };

    let marked = Lowered::with(&shop("readonly ", " readonly"), &[order]);
    let plain = Lowered::with(&shop("", ""), &[order]);

    // The container fills `orders` with the service its PHP type, `array`, drops, and builds `ids` as a `Set`.
    assert!(plain.tree().contains(r#"ZVAL "list<app\\order>""#), "{}", plain.tree());
    assert_eq!(marked.tree(), plain.tree());
}
