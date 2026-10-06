//! Each test lowers a PHP# snippet and compares the node tree with the `zend_ast` that php-src's
//! `zend_language_parser.y` builds for the equivalent PHP, quoted above each test.
//!
//! A tree prints one node per line: its kind without `SHARP_AST_`, its attr in brackets when it is not 0, a ZVAL's
//! value, and a declaration's name and lines. `null` is a null child.

#![allow(clippy::panic, clippy::expect_used, clippy::use_debug)]

use std::ffi::c_char;
use std::fmt::Write;
use std::slice;

use indoc::indoc;

use mago_sharp_bridge::sharp_kind;
use mago_sharp_bridge::sharp_lower;
use mago_sharp_bridge::sharp_node;
use mago_sharp_bridge::sharp_severity;
use mago_sharp_bridge::sharp_str;
use mago_sharp_bridge::sharp_unit;
use mago_sharp_bridge::sharp_unit_free;
use mago_sharp_bridge::sharp_value;

/// A unit `sharp_lower` returned, freed on drop.
struct Lowered(*mut sharp_unit);

impl Lowered {
    fn new(code: &str) -> Self {
        Self::named("src/Report.sharp", code)
    }

    fn named(path: &str, code: &str) -> Self {
        // SAFETY: both pointers point to as many bytes as their lengths say.
        Self(unsafe {
            sharp_lower(path.as_ptr().cast::<c_char>(), path.len(), code.as_ptr().cast::<c_char>(), code.len())
        })
    }

    fn unit(&self) -> &sharp_unit {
        // SAFETY: `sharp_lower` returns a valid unit, freed only on drop.
        unsafe { &*self.0 }
    }

    fn nodes(&self) -> &[sharp_node] {
        // SAFETY: the unit owns `node_count` nodes.
        unsafe { array(self.unit().nodes, self.unit().node_count) }
    }

    fn children(&self) -> &[u32] {
        // SAFETY: the unit owns `children_count` children.
        unsafe { array(self.unit().children, self.unit().children_count) }
    }

    /// Every diagnostic, as `line:column severity message`.
    fn diagnostics(&self) -> Vec<String> {
        // SAFETY: the unit owns `diagnostic_count` diagnostics.
        let diagnostics = unsafe { array(self.unit().diagnostics, self.unit().diagnostic_count) };

        diagnostics
            .iter()
            .map(|diagnostic| {
                let severity = match diagnostic.severity {
                    sharp_severity::SHARP_PARSE_ERROR => "parse error",
                    sharp_severity::SHARP_COMPILE_ERROR => "compile error",
                };

                format!("{}:{} {severity}: {}", diagnostic.line, diagnostic.column, text(diagnostic.message))
            })
            .collect()
    }

    fn tree(&self) -> String {
        assert_eq!(self.diagnostics(), Vec::<String>::new(), "the source lowers");

        self.render(self.unit().root)
    }

    /// The statements of the method `run`.
    fn body(&self) -> String {
        let method = self
            .nodes()
            .iter()
            .position(|node| node.kind == sharp_kind::SHARP_AST_METHOD && text(node.text) == "run")
            .expect("the source declares `run`");

        self.render(self.child(method as u32, 2))
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
                    sharp_value::SHARP_STRING => write!(tree, " {:?}", text(node.text)),
                };
            }
            sharp_kind::SHARP_AST_CLASS
            | sharp_kind::SHARP_AST_METHOD
            | sharp_kind::SHARP_AST_PROPERTY_HOOK
            | sharp_kind::SHARP_AST_CLOSURE
            | sharp_kind::SHARP_AST_ARROW_FUNC => {
                let _ = write!(tree, " {:?} @{}-{}", text(node.text), node.line, node.end_line);
            }
            _ => {}
        }

        tree.push('\n');
        for child in 0..node.child_count {
            self.render_into(self.child(index, child), depth + 1, tree);
        }
    }
}

impl Drop for Lowered {
    fn drop(&mut self) {
        // SAFETY: `sharp_lower` returned the unit, and only this drop frees it.
        unsafe { sharp_unit_free(self.0) };
    }
}

/// # Safety
///
/// When `len` is not 0, `pointer` points to `len` values.
unsafe fn array<'unit, T>(pointer: *const T, len: usize) -> &'unit [T] {
    if len == 0 {
        return &[];
    }

    // SAFETY: the caller passes `len` values at `pointer`.
    unsafe { slice::from_raw_parts(pointer, len) }
}

fn text(text: sharp_str) -> String {
    // SAFETY: the unit owns `len` bytes at `ptr`.
    String::from_utf8_lossy(unsafe { array(text.ptr.cast::<u8>(), text.len) }).into_owned()
}

/// A file declaring the method `run` with `body`. Its body starts on line 9.
fn method(body: &str) -> String {
    format!(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{{\n    public int run(int extra)\n    {{\n{body}    }}\n}}\n"
    )
}

fn body(statements: &str) -> String {
    Lowered::new(&method(statements)).body()
}

/// ```php
/// $base = 2; $base = 3;
/// ```
#[test]
fn a_const_local_reassigned_returns_one_compile_error_and_no_nodes() {
    let lowered = Lowered::new(&method("        const base = 2;\n        base = 3;\n        return base;\n"));

    assert_eq!(lowered.diagnostics(), ["10:9 compile error: Cannot assign to `base`: it is declared with `const`."]);
    assert_eq!(lowered.unit().node_count, 0);
    assert_eq!(lowered.unit().children_count, 0);
}

#[test]
fn php_syntax_returns_a_parse_error_and_no_nodes() {
    let lowered = Lowered::new(&method("        return this->run(1);\n"));

    assert_eq!(lowered.diagnostics(), ["9:20 parse error: `->` is PHP syntax: PHP# writes member access with `.`"]);
    assert_eq!(lowered.unit().node_count, 0);
}

#[test]
fn a_construct_outside_the_slice_returns_its_not_supported_error() {
    let lowered = Lowered::new(&method("        echo extra;\n        return 1;\n"));

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
    assert_eq!(lowered.unit().node_count, 0);
}

#[test]
fn a_method_without_a_body_returns_the_checker_error() {
    let lowered = Lowered::new("class Report\n{\n    public int run();\n}\n");

    assert_eq!(
        lowered.diagnostics(),
        ["3:21 compile error: Non-Abstract method `Report::run` must have a concrete body."]
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
        assert_eq!(lowered.unit().node_count, 0);
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
    assert_eq!(lowered.unit().node_count, 0);
}

/// A 509-term sum or `??` chain in a method nests its innermost term 512 levels deep, the most the checker allows, and
/// so do a null-safe call chain of 509 calls and a 510-term sum as a field's initial value. The bridge lowers each on a
/// thread whose stack is far smaller than the recursion needs, as a PHP thread or fiber may be.
#[test]
fn the_deepest_file_the_checker_accepts_lowers_on_a_small_stack() {
    let codes = [
        method(&format!("        return {};\n", vec!["extra"; 509].join(" + "))),
        method(&format!("        return {};\n", vec!["extra"; 509].join(" ?? "))),
        method(&format!("        this{};\n        return extra;\n", "?.total(extra)".repeat(508))),
        format!(
            "namespace App.Tenant;\n\nclass Report\n{{\n    private int total = {};\n}}\n",
            vec!["1"; 510].join(" + ")
        ),
    ];

    for code in codes {
        let node_count = std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                let lowered = Lowered::new(&code);
                assert_eq!(lowered.diagnostics(), Vec::<String>::new());

                lowered.unit().node_count
            })
            .expect("the thread starts")
            .join()
            .expect("the lowering returns");

        assert!(node_count > 509 * 2, "{node_count} nodes");
    }
}

/// `ext/sharp` decides the dialect from the file name, so the bridge parses and checks PHP# whatever name it gets.
#[test]
fn a_file_of_any_name_is_parsed_and_checked_as_php_sharp() {
    let lowered = Lowered::named("src/Upper.SHARP", "namespace App.Tenant;\n\nclass Report\n{\n}\n");
    assert_eq!(lowered.diagnostics(), Vec::<String>::new());

    let refused = Lowered::named("src/Upper.SHARP", &method("        echo extra;\n        return 1;\n"));
    assert_eq!(refused.diagnostics(), ["9:9 compile error: This statement is not supported yet in PHP#."]);
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

        lowered.nodes()[lowered.unit().root as usize].end_line
    })
    .collect();

    assert_eq!(end_lines, [1, 2, 2, 2, 5]);
}

/// Node lines, declaration lines and diagnostic lines count a lone `\r` as a line ending too.
#[test]
fn every_line_is_the_line_the_zend_scanner_counts() {
    let class = Lowered::new("class Report\r\n{\r}\n\n");
    let root = &class.nodes()[class.unit().root as usize];
    let declaration = &class.nodes()[class.child(class.unit().root, 1) as usize];
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
    assert_eq!(refused.diagnostics(), ["3:25 compile error: This statement is not supported yet in PHP#."]);
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
    let class = lowered.child(lowered.unit().root, 2);

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
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int count = 0;\n    protected Calc calc;\n}\n",
    );
    let class = lowered.child(lowered.unit().root, 2);

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
    let lowered = Lowered::new(
        "class Report\n{\n    private int count = 0;\n\n    public int total() => this.count + 1;\n\n    public void touch() => this.save();\n\n    public Report(int count) => this.count = count;\n}\n",
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
    let class = lowered.child(lowered.unit().root, 1);

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
            "3:45 compile error: `field` cannot be used in a lambda: PHP would call the accessor again instead of reading the storage."
        ]
    );
    assert_eq!(lambda.unit().node_count, 0);
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
    let lowered = Lowered::new(
        "import Lib.Model;\nimport InvalidArgumentException;\n\nclass Order : Model\n{\n    public int price { get; set { if (value < 0) { throw new InvalidArgumentException(\"negative\"); } field = value; } } = 0;\n    public Address shipping { get => this.getAttribute(\"shipping\"); set => this.setAttribute(\"shipping\", value); }\n}\n\nclass Address\n{\n}\n",
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
    let lowered = Lowered::new(
        "import Lib.Model;\n\nclass Order : Model\n{\n    public Address? shipping { get => this.getAttribute(\"shipping\"); set => this.setAttribute(\"shipping\", value); }\n    public string? note { get => field; set => field = value; }\n    public string? summary { get => this.note; }\n}\n\nclass Address\n{\n}\n",
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

            (text(lowered.nodes()[lowered.child(index as u32, 1) as usize].text), node.attr)
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
///     $this->count = $start;
/// }
/// ```
///
/// A constant initial value is the property's default. Any other runs at the start of the constructor.
#[test]
fn a_non_constant_initial_value_runs_at_the_start_of_the_constructor() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private Calc calc = new Calc(1);\n    public int total { get; set; } = 1 + 1;\n\n    public Report(int start)\n    {\n        this.count = start;\n    }\n}\n",
    );
    let class = lowered.child(lowered.unit().root, 2);

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
                      ZVAL "count"
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
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private Calc calc = new Calc(1);\n}\n",
    );
    let class = lowered.child(lowered.unit().root, 2);

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
    let class = lowered.child(lowered.unit().root, 1);

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
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int? total;\n    public Calc? owner { get; set; }\n    private (int|string)? key;\n    private int? start = 1;\n    public int? limit { get; } = null;\n}\n",
    );
    let class = lowered.child(lowered.unit().root, 2);

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
        .map(|node| (text(node.text), node.attr))
        .collect();

    assert_eq!(methods, [("make".to_owned(), 17), ("hide".to_owned(), 4), ("share".to_owned(), 2)]);
}

/// ```php
/// public function make(\Lib\Calc $other, float $rate = 1.5, bool $loud = PHP_DEBUG): \Lib\Calc {}
/// ```
///
/// A class type is its full name with `ZEND_NAME_FQ`, which is 0.
#[test]
fn parameters_carry_their_type_name_and_default() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public Calc make(Calc other, float rate = 1.5, bool loud = PHP_DEBUG) { return other; }\n}\n",
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
                ZVAL [1] "bool"
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
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public Calc? find(int? id, Calc? other = null) { return null; }\n}\n",
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
/// $counts = [\Lib\Calc::Active => 1];
/// $counts[\Lib\Calc::Closed] = 2;
/// return $counts[\Lib\Calc::Active] ?? 0;
/// ```
///
/// A `Map` keyed by a backed enum lowers as any `Map` does. The engine stores each case as its backing value, because
/// `ext/sharp` marks every PHP# index and literal.
#[test]
fn a_map_keyed_by_a_backed_enum_lowers_as_any_map() {
    assert_eq!(
        body(
            "        Map<Calc, int> counts = [Calc.Active: 1];\n        counts[Calc.Closed] = 2;\n        return counts[Calc.Active] ?? 0;\n"
        ),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "counts"
                ARRAY [3]
                  ARRAY_ELEM
                    ZVAL 1
                    CLASS_CONST [32768]
                      ZVAL "Lib\\Calc"
                      ZVAL "Active"
              ASSIGN
                DIM
                  VAR
                    ZVAL "counts"
                  CLASS_CONST [32768]
                    ZVAL "Lib\\Calc"
                    ZVAL "Closed"
                ZVAL 2
              RETURN
                COALESCE
                  DIM
                    VAR
                      ZVAL "counts"
                    CLASS_CONST [32768]
                      ZVAL "Lib\\Calc"
                      ZVAL "Active"
                  ZVAL 0
        "#}
    );
}

/// ```php
/// public function apply(\Closure $step, ?\Closure $other = null): \Closure { return $step; }
/// ```
///
/// A function type runs as PHP's `\Closure`, so its type is the full name `Closure` with `ZEND_NAME_FQ`, which is 0.
#[test]
fn a_function_type_is_the_closure_class() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nclass Report\n{\n    public Function<bool(int)> apply(Function<int(string, int)> step, Function<void()>? other = null) { return step; }\n}\n",
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
/// $numbers[0] = $named['a'];
/// $this->sizes['a'] += 1;
/// return $numbers[1];
/// ```
///
/// A list or map literal is an `ARRAY` with `ZEND_ARRAY_SYNTAX_SHORT`, which is 3, of `ARRAY_ELEM`s that take the value
/// before the key. An index is a `DIM` of the value and the key, read or written as its place in the tree decides.
#[test]
fn literals_are_short_arrays_and_an_index_is_a_dim() {
    assert_eq!(
        body(
            "        List<int> numbers = [1, extra];\n        const named = [\"a\": 1, 2: numbers[0]];\n        const Map<string, int> empty = [:];\n        numbers[0] = named[\"a\"];\n        this.sizes[\"a\"] += 1;\n        return numbers[1];\n"
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
                    ZVAL "numbers"
                  ZVAL 0
                DIM
                  VAR
                    ZVAL "named"
                  ZVAL "a"
              ASSIGN_OP [1]
                DIM
                  PROP
                    VAR
                      ZVAL "this"
                    ZVAL "sizes"
                  ZVAL "a"
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
        body("        this.count += 1;\n        return 1;\n"),
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
    assert_eq!(
        body("        return Calc.make(2);\n"),
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
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int|string key = 1;\n\n    public Calc|string find(\n        int|float|Calc id,\n    ) { return \"none\"; }\n}\n",
    );
    let members = lowered.child(lowered.child(lowered.unit().root, 2), 2);
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
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public (Calc|string)? find(\n        (int|string)? id,\n    ) { return null; }\n}\n",
    );
    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
    let method = lowered.child(lowered.child(lowered.child(lowered.unit().root, 2), 2), 0);
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
        body(
            "        const pick = (int|string key, (int|Calc)? fallback, int ...rest) => count(rest);\n        return pick(1, null, ...extra);\n"
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
        body("        const all = [...extra, 1, ...Calc.make()];\n        return 1;\n"),
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
        body(
            "        Calc.sum(...extra);\n        this.run(1, ...extra);\n        const made = new Calc(...extra);\n        return max(...extra);\n"
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
        body("        return new Calc(extra, rate: 2);\n"),
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
/// interface Linkable extends \Lib\Named { }
/// class Page implements \Lib\Entity, \App\Tenant\Linkable { }
/// ```
///
/// The header names are a name list in the interface list, with `ZEND_NAME_FQ`, which is 0. A class's header can
/// hold its parent class, which only `zend_do_link_class` can tell from the interfaces, so the class carries
/// php-sharp's `ZEND_ACC_PARENT_IN_INTERFACES`, `[2147483648]`, `1 << 31`. An interface's header holds only
/// interfaces, as PHP's `extends` list does.
#[test]
fn a_header_is_the_interface_name_list_and_marks_a_class_to_find_its_parent_there() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Entity;\nimport Lib.Named;\n\ninterface Linkable : Named\n{\n}\n\nclass Page : Entity, Linkable\n{\n}\n",
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
              CLASS [2147483648] "Page" @10-12
                null
                NAME_LIST
                  ZVAL "Lib\\Entity"
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
    let lowered = Lowered::new(
        "class Thumbnail : Image\n{\n    public virtual int size()\n    {\n        return 1;\n    }\n\n    [Deprecated]\n    public override string name()\n    {\n        return \"thumbnail\";\n    }\n}\n",
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
              CLASS [2147483648] "Thumbnail" @1-13
                null
                NAME_LIST
                  ZVAL "Image"
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
/// #[\Override] protected string $table = 'orders';   // runs as `protected $table` when Model's $table has no type
/// #[\Override] public bool $timestamps = false;
/// ```
///
/// An override is a field with `#[\Override]`, so PHP checks at link time that the parent has the property, and with
/// php-sharp's `ZEND_ACC_TYPE_FOLLOWS_PARENT`, `1 << 13`. One file cannot tell whether the parent's property has a
/// type, so the engine drops the written type when the class links if it has none. `[8194]` is
/// `ZEND_ACC_PROTECTED | ZEND_ACC_TYPE_FOLLOWS_PARENT`, and `[8193]` is `ZEND_ACC_PUBLIC | ZEND_ACC_TYPE_FOLLOWS_PARENT`.
#[test]
fn an_override_is_a_field_marked_to_follow_the_parent_type() {
    let lowered = Lowered::new(
        "class Order : Model\n{\n    protected override string table = \"orders\";\n    public override bool timestamps = false;\n}\n",
    );
    let class = lowered.child(lowered.unit().root, 1);

    assert_eq!(
        lowered.render(lowered.child(class, 2)),
        indoc! {r#"
            STMT_LIST
              PROP_GROUP [8194]
                ZVAL [1] "string"
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
              PROP_GROUP [8193]
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
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Ledger;\n\nclass Account : Ledger\n{\n    public override int|float total(string label, int|float ...amounts)\n    {\n        return super.total(label, ...amounts);\n    }\n}\n",
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
              CLASS [2147483648] "Account" @5-11
                null
                NAME_LIST
                  ZVAL "Lib\\Ledger"
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
        body("        return super.size(2);\n"),
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
/// public static function find(\Lib\Row $row): ?static { return static::fromSchema($row); }
/// public static function counted(\Lib\Row $row): static|int { return static::fromSchema($row); }
/// ```
///
/// `Self` is `static`. As a return type it is a `TYPE` node with `IS_STATIC`, which is 15, and `Self?` adds
/// `ZEND_TYPE_NULLABLE`, which is 256. As a class it is the name `static` with `ZEND_NAME_NOT_FQ`, which is 1, as
/// php-src's grammar builds `new static` and `static::`. `required` adds no flag, as PHP has no `required`.
#[test]
fn self_is_static_and_required_adds_no_flag() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Row;\n\npublic abstract class DatabaseEntity\n{\n    public required DatabaseEntity(Row row)\n    {\n    }\n\n    public static Self fromSchema(Row row)\n    {\n        return new Self(row);\n    }\n\n    public static Self? find(Row row) => Self.fromSchema(row);\n\n    public static Self|int counted(Row row) => Self.fromSchema(row);\n}\n",
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.child(lowered.unit().root, 2), 2)),
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
                    STATIC_CALL
                      ZVAL [1] "static"
                      ZVAL "fromSchema"
                      ARG_LIST
                        VAR
                          ZVAL "row"
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
        body("        return Self.make(2);\n"),
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
        body("        return typeof(Calc);\n"),
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
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Access;\nimport Lib.Calc;\n\n[Access(typeof(Calc), role: typeof(Report))]\nclass Report\n{\n}\n",
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.child(lowered.unit().root, 2), 3)),
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
    let class = lowered.child(lowered.unit().root, 2);

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
    let class = lowered.child(lowered.unit().root, 2);

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
/// \Lib\Calc::$rate ??= 1;
/// ```
///
/// A static member written through its class is a static property, as php-src's grammar builds `Class::$name`. The
/// class is its full name with `ZEND_NAME_FQ`, which is 0.
#[test]
fn a_static_member_written_through_its_class_is_a_static_property() {
    assert_eq!(
        body("        Calc.rate = 2;\n        Calc.count++;\n        Calc.rate ??= 1;\n        return 1;\n"),
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
                  ZVAL "rate"
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
/// PHP evaluates a constant expression without the static property fallback, so a class member read in a constant's
/// value, a constant initial value, a parameter default and an attribute argument is an unmarked class constant.
#[test]
fn a_class_member_read_in_a_constant_expression_is_an_unmarked_class_constant() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\nimport Lib.Field;\nimport Lib.Mode;\n\nclass Report\n{\n    public const int MAX = Calc.MAX;\n    private Mode mode = Mode.Read;\n\n    [Field(Mode.Write)]\n    public int run(Mode extra = Mode.Read)\n    {\n        return 1;\n    }\n}\n",
    );

    let class_constants = lowered
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_CLASS_CONST)
        .map(|node| node.attr)
        .collect::<Vec<_>>();

    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
    assert_eq!(class_constants, [0, 0, 0, 0]);
}

/// ```php
/// return \Lib\Calc::rate->cents;
/// ```
///
/// A class member read is a class constant fetch that the engine falls back from to the static property of the same
/// name, as spec section 4 decides. `[32768]` is php-sharp's `ZEND_FETCH_CLASS_MEMBER_SYNTAX`, `1 << 15`, which marks
/// the fallback above the fetch flags a constant expression passes in the same attr.
#[test]
fn a_class_member_read_is_a_class_constant_marked_to_fall_back_to_the_static_property() {
    assert_eq!(
        body("        return Calc.rate.cents;\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                PROP
                  CLASS_CONST [32768]
                    ZVAL "Lib\\Calc"
                    ZVAL "rate"
                  ZVAL "cents"
        "#}
    );
}

/// ```php
/// return \App\Tenant\Report::make();
/// ```
#[test]
fn a_class_of_the_same_namespace_is_called_by_its_full_name() {
    assert_eq!(
        body("        return Report.make();\n"),
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
        body("        return Int.parse(extra) + Float.tryParse(extra);\n"),
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
/// return $this->total($extra, rate: 2)->value;
/// ```
#[test]
fn member_calls_and_reads_on_values_are_instance_access() {
    assert_eq!(
        body("        return this.total(extra, rate: 2).value;\n"),
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
        body("        return Calc.make().add(extra);\n"),
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
/// $a + $a - $a * $a / $a % $a;
/// ```
///
/// `[1]` to `[5]` are `ZEND_ADD`, `ZEND_SUB`, `ZEND_MUL`, `ZEND_DIV` and `ZEND_MOD`.
#[test]
fn arithmetic_operators_are_binary_ops() {
    assert_eq!(
        body("        let a = 1;\n        a + a - a * a / a % a;\n        return a;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                ZVAL 1
              BINARY_OP [2]
                BINARY_OP [1]
                  VAR
                    ZVAL "a"
                  VAR
                    ZVAL "a"
                BINARY_OP [5]
                  BINARY_OP [4]
                    BINARY_OP [3]
                      VAR
                        ZVAL "a"
                      VAR
                        ZVAL "a"
                    VAR
                      ZVAL "a"
                  VAR
                    ZVAL "a"
              RETURN
                VAR
                  ZVAL "a"
        "#}
    );
}

/// ```php
/// $a == $a; $a != $a; $a === $a; $a !== $a; $a < $a; $a <= $a; $a > $a; $a >= $a;
/// ```
///
/// `[18]`, `[19]`, `[16]`, `[17]`, `[20]` and `[21]` are `ZEND_IS_EQUAL`, `ZEND_IS_NOT_EQUAL`, `ZEND_IS_IDENTICAL`,
/// `ZEND_IS_NOT_IDENTICAL`, `ZEND_IS_SMALLER` and `ZEND_IS_SMALLER_OR_EQUAL`. `>` and `>=` have kinds of their own.
#[test]
fn comparison_operators_are_the_kinds_php_gives_them() {
    let tree = body(
        "        let a = 1;\n        a == a;\n        a != a;\n        a === a;\n        a !== a;\n        a < a;\n        a <= a;\n        a > a;\n        a >= a;\n        return a;\n",
    );
    let operators: Vec<&str> = tree.lines().filter(|line| line.starts_with("  ") && !line.starts_with("   ")).collect();

    assert_eq!(
        operators,
        [
            "  ASSIGN",
            "  BINARY_OP [18]",
            "  BINARY_OP [19]",
            "  BINARY_OP [16]",
            "  BINARY_OP [17]",
            "  BINARY_OP [20]",
            "  BINARY_OP [21]",
            "  GREATER",
            "  GREATER_EQUAL",
            "  RETURN",
        ]
    );
}

/// ```php
/// $a && $a || !$a;
/// ```
///
/// `[14]` is `ZEND_BOOL_NOT`.
#[test]
fn logical_operators_are_and_or_and_bool_not() {
    assert_eq!(
        body("        let a = true;\n        a && a || !a;\n        return 1;\n"),
        indoc! {r#"
            STMT_LIST
              ASSIGN
                VAR
                  ZVAL "a"
                ZVAL true
              OR
                AND
                  VAR
                    ZVAL "a"
                  VAR
                    ZVAL "a"
                UNARY_OP [14]
                  VAR
                    ZVAL "a"
              RETURN
                ZVAL 1
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
/// $a -= 1; $a *= 2; $a /= 3; $a **= 4;
/// ```
///
/// `[2]`, `[3]`, `[4]` and `[12]` are `ZEND_SUB`, `ZEND_MUL`, `ZEND_DIV` and `ZEND_POW`.
#[test]
fn compound_assignments_are_assign_ops() {
    let tree = body(
        "        let a = 1;\n        a -= 1;\n        a *= 2;\n        a /= 3;\n        a **= 4;\n        return a;\n",
    );
    let operators: Vec<&str> = tree.lines().filter(|line| line.starts_with("  ") && !line.starts_with("   ")).collect();

    assert_eq!(
        operators,
        ["  ASSIGN", "  ASSIGN_OP [2]", "  ASSIGN_OP [3]", "  ASSIGN_OP [4]", "  ASSIGN_OP [12]", "  RETURN"]
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
        body("        return extra ?? this.total() ?? 0;\n"),
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
/// $cents = (int)($extra * 1.5); return (string)(float)$cents;
/// ```
///
/// A cast's attr is the type it converts to, `IS_LONG`, `IS_DOUBLE` or `IS_STRING`, as php-src's grammar builds it.
#[test]
fn casts_between_numbers_are_casts_to_their_types() {
    assert_eq!(
        body("        const cents = (int)(extra * 1.5);\n        return (string)(float)cents;\n"),
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
        body("        extra ??= 1;\n        this.count ??= extra;\n        return extra;\n"),
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
/// return $this?->total($extra)?->value->cents;
/// ```
#[test]
fn null_safe_calls_and_reads_are_nullsafe_kinds() {
    assert_eq!(
        body("        return this?.total(extra)?.value.cents;\n"),
        indoc! {r#"
            STMT_LIST
              RETURN
                PROP
                  NULLSAFE_PROP
                    NULLSAFE_METHOD_CALL
                      VAR
                        ZVAL "this"
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
        body("        return extra[0]?.value ?? (extra[1])?.total();\n"),
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
        body(
            "        for (const value of Calc.make(2)) {\n            extra += value;\n        }\n        for (let [key, value] of Calc.make(3)) {\n        }\n        return extra;\n"
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
/// foreach ($extra as $status => $n) {
///     $status = \Lib\Calc::from($status);
///     {
///         $extra += $n;
///     }
/// }
/// foreach ($extra as $sku => $n) {
///     $sku = (string) $sku;
///     {
///     }
/// }
/// foreach ($extra as $key => $n) {
/// }
/// ```
///
/// A `Map` keyed by a backed enum holds each key as its backing value, so a loop whose key names a class reads the key
/// back as that class's case, through `from` on the class as it runs. PHP stores an all-digit `string` key as an
/// `int`, so a key written `string` reads back through `(string)`. An `int` key type changes nothing.
#[test]
fn a_loop_key_written_as_a_class_or_string_reads_back_through_from_or_a_cast() {
    assert_eq!(
        body(
            "        for (const [Calc status, int n] of extra) {\n            extra += n;\n        }\n        for (const [string sku, n] of extra) {\n        }\n        for (const [int key, n] of extra) {\n        }\n        return 1;\n"
        ),
        indoc! {r#"
            STMT_LIST
              FOREACH
                VAR
                  ZVAL "extra"
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
                  ZVAL "extra"
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
                  ZVAL "extra"
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
        body("        throw new Failure(extra);\n        return extra ?? throw new Failure(0);\n"),
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
        body(
            "        try {\n            extra += 1;\n        } catch (Calc | Missing failure) {\n            throw failure;\n        } catch (Broken) {\n        } finally {\n            extra -= 1;\n        }\n        return extra;\n"
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
        body("        return strlen(sprintf(\"%d\", count(extra, mode: 0)));\n"),
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
/// $twice = fn (int $a) => $a * $extra; $half = fn ($b) => $b / 2; return $twice($half(4));
/// ```
///
/// A lambda that writes none of the locals it captures is PHP's `fn`, which captures by value what its body reads.
/// php-src's grammar gives an arrow function no name, no `use` list and its expression as its body. A call of a local
/// calls the closure the local holds.
#[test]
fn a_lambda_that_writes_no_capture_is_an_arrow_function() {
    assert_eq!(
        body(
            "        const twice = (int a) => a * extra;\n        const half = b => b / 2;\n        return twice(half(4));\n"
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
                  BINARY_OP [4]
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
    let lowered = Lowered::new(&method("        return `total:\n${extra} more`;\n"));
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
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Field;\nimport Lib.Entity;\n\n[Entity(label: \"Orders\", order: 2 * 3), Searchable]\n[Entity(null)]\nclass Report\n{\n    [Field] private int count = 0;\n\n    public Report([Field(-1.5)] public int total { get; })\n    {\n    }\n\n    [Entity(true, PHP_INT_MAX)]\n    public void run([Field] int extra)\n    {\n    }\n}\n",
    );

    assert_eq!(
        lowered.render(lowered.child(lowered.unit().root, 2)),
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
        body(
            "        const a = extra is Calc;\n        const b = extra is int;\n        const c = extra is string;\n        const d = this.total() is Calc;\n        return extra;\n"
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
        body(
            "        if (extra is Calc calc) {\n            return 1;\n        }\n        if (extra is not int count) {\n            return 0;\n        }\n        return count;\n"
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
/// $calc = $extra instanceof \Lib\Calc ? $extra : null;
/// $made = (${'as#1'} = \Lib\Calc::make()) instanceof \Lib\Calc ? ${'as#1'} : null;
/// ```
///
/// `as` reads a local or a parameter twice. Any other value is assigned to a variable PHP# cannot name, so it runs
/// once.
#[test]
fn as_is_a_conditional_that_gives_the_value_or_null() {
    assert_eq!(
        body("        const calc = extra as Calc;\n        const made = Calc.make() as Calc;\n        return extra;\n"),
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
/// $limit = 10;
/// $a = $extra === 200;
/// $b = $extra >= 1 && $extra < $limit || !($extra === -1);
/// $c = $extra === $limit;
/// $d = \is_object($extra) && (${'match#1'} = $extra->count) > 0 && \is_string($label = $extra->name);
/// ```
///
/// A value is compared with `===`, a comparison keeps its operator, and `and`, `or` and `not` are `&&`, `||` and
/// `!`. A bare name is the local's value when a local of that name is in scope. A properties pattern tests that the
/// value is an object, then reads each property once.
#[test]
fn values_comparisons_and_properties_are_the_php_comparisons_they_name() {
    assert_eq!(
        body(
            "        let limit = 10;\n        const a = extra is 200;\n        const b = extra is >= 1 and < limit or not -1;\n        const c = extra is limit;\n        const d = extra is { count: > 0, name: string label };\n        return extra;\n"
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
        body(
            "        return match (this.total()) {\n            0 => 1,\n            int n when n > 9 => n,\n            default => 2,\n        };\n"
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
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Label;\n\n[Label(\"Order status\")]\nenum Status : string\n{\n    case Active = \"active\";\n    [Label(\"Gone\")]\n    case Archived = \"archived\";\n\n    public string title()\n    {\n        return this.value;\n    }\n\n    public static Status parse(string value)\n    {\n        const all = Status.cases();\n        return Status.tryFrom(value) ?? Status.from(\"active\");\n    }\n}\n",
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
/// An enum's header is the interface name list, its 2nd child, with `ZEND_NAME_FQ`, which is 0. It carries no
/// `ZEND_ACC_PARENT_IN_INTERFACES`: an enum has no parent, so every name there is an interface.
#[test]
fn an_enum_header_is_the_interface_name_list_without_a_parent_mark() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.HasLabel;\n\nenum Status : string, HasLabel, Sorted\n{\n    case Active = \"a\";\n}\n\nenum Suit : HasLabel\n{\n    case Hearts;\n}\n",
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
/// An enum's constant is a class constant group of one constant, as a class's is. Its value is a constant expression,
/// so its read of a case is an unmarked class constant.
#[test]
fn an_enum_constant_is_a_class_constant_group_that_reads_a_case_unmarked() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nenum Status : string\n{\n    case Active = \"a\";\n\n    public const Status Default = Status.Active;\n}\n",
    );
    let r#enum = lowered.child(lowered.unit().root, 2);

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
/// A case read is a class constant fetch in every place: unmarked in a case's value, a constant and a parameter
/// default, which are constant expressions, and marked with `[32768]`, `1 << 15`, to fall back to the static property
/// in a method body. A method called on a case is an instance call on that fetch.
#[test]
fn a_case_read_is_a_class_constant_in_every_place() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Registry;\n\npublic enum Status : string\n{\n    case Active = \"a\";\n    case Paused = Registry.PAUSED;\n\n    public const Status Default = Status.Active;\n\n    public bool active() => this === Status.Active;\n\n    public string label() => this.name;\n}\n\nclass Report\n{\n    public string run(Status status = Status.Active)\n    {\n        if (status === Status.Active) {\n            return Status.Active.label();\n        }\n        return Status.Default.label();\n    }\n}\n",
    );
    let reads: Vec<(u32, String)> = lowered
        .nodes()
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == sharp_kind::SHARP_AST_CLASS_CONST)
        .map(|(index, node)| (node.attr, lowered.render(lowered.child(index as u32, 1)).trim_end().to_owned()))
        .collect();

    assert_eq!(
        reads,
        [
            (0, "ZVAL \"PAUSED\"".to_owned()),
            (0, "ZVAL \"Active\"".to_owned()),
            (1 << 15, "ZVAL \"Active\"".to_owned()),
            (0, "ZVAL \"Active\"".to_owned()),
            (1 << 15, "ZVAL \"Active\"".to_owned()),
            (1 << 15, "ZVAL \"Active\"".to_owned()),
            (1 << 15, "ZVAL \"Default\"".to_owned()),
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
                    CLASS_CONST [32768]
                      ZVAL "App\\Tenant\\Status"
                      ZVAL "Active"
                  STMT_LIST
                    RETURN
                      METHOD_CALL
                        CLASS_CONST [32768]
                          ZVAL "App\\Tenant\\Status"
                          ZVAL "Active"
                        ZVAL "label"
                        ARG_LIST
              RETURN
                METHOD_CALL
                  CLASS_CONST [32768]
                    ZVAL "App\\Tenant\\Status"
                    ZVAL "Default"
                  ZVAL "label"
                  ARG_LIST
        "#}
    );
}

/// An enum case's value is a constant expression, so a class member read there is an unmarked class constant, while a
/// read of the enum's own case in a method is marked to fall back to the static property, as in a class.
#[test]
fn a_class_member_read_in_an_enum_case_value_is_an_unmarked_class_constant() {
    let lowered = Lowered::new(
        "namespace App.Tenant;\n\nimport Lib.Calc;\n\nenum Rank : int\n{\n    case Top = Calc.MAX;\n\n    public static Rank first() => Rank.Top;\n}\n",
    );
    let class_constants = lowered
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_CLASS_CONST)
        .map(|node| node.attr)
        .collect::<Vec<_>>();

    assert_eq!(lowered.diagnostics(), Vec::<String>::new());
    assert_eq!(class_constants, [0, 1 << 15]);
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
/// either is the marked class constant of any method body.
#[test]
fn an_enum_method_lowers_a_list_literal_a_function_type_and_a_lambda_as_a_class_method_does() {
    let lowered = Lowered::new(
        "enum Suit\n{\n    case Hearts;\n    case Spades;\n\n    public static List<Suit> all() => [Suit.Hearts, Suit.Spades];\n\n    public Function<bool(Suit)> matches()\n    {\n        const first = Suit.Hearts;\n        return other => other === this || other === first;\n    }\n}\n",
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
                            CLASS_CONST [32768]
                              ZVAL "Suit"
                              ZVAL "Hearts"
                            null
                          ARRAY_ELEM
                            CLASS_CONST [32768]
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
                        CLASS_CONST [32768]
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
                      CLASS_CONST [32768]
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

/// The fixture holds every construct the checker accepts, so the bridge lowers all of them, and each fixed-size node
/// has the child count of its kind.
#[test]
fn every_construct_of_the_slice_lowers_into_nodes_of_their_kinds_child_count() {
    let lowered = Lowered::new(include_str!("../../semantics/tests/fixtures/slice.sharp"));
    assert_eq!(lowered.diagnostics(), Vec::<String>::new());

    let wrong: Vec<String> = lowered
        .nodes()
        .iter()
        .filter(|node| fixed_child_count(node.kind).is_some_and(|count| count != node.child_count))
        .map(|node| format!("{:?} has {} children", node.kind, node.child_count))
        .collect();

    assert_eq!(wrong, Vec::<String>::new());
}

#[test]
fn each_node_carries_the_line_of_its_first_token() {
    let lowered = Lowered::new(&method("        let total =\n            extra;\n        return total;\n"));
    let lines: Vec<(String, u32)> = lowered
        .nodes()
        .iter()
        .filter(|node| node.kind == sharp_kind::SHARP_AST_ZVAL && node.value == sharp_value::SHARP_STRING)
        .map(|node| (text(node.text), node.line))
        .filter(|(name, _)| name == "total" || name == "extra")
        .collect();

    assert_eq!(
        lines,
        [("extra".to_owned(), 7), ("total".to_owned(), 9), ("extra".to_owned(), 10), ("total".to_owned(), 11)]
    );
}
