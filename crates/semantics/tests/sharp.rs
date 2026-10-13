#![allow(clippy::panic, clippy::expect_used)]

use std::borrow::Cow;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_names::resolver::NameResolver;
use mago_php_version::PHPVersion;
use mago_semantics::SemanticsChecker;
use mago_syntax::parser::parse_file;

/// Every semantic issue in the PHP# source, as `line:column message` at its primary span.
fn issues(code: &'static str) -> Vec<String> {
    issues_in("src/Report.sharp", code)
}

/// Every semantic issue in the source of the file `name`, whose extension picks the dialect, as `issues` prints them.
fn issues_in(name: &'static str, code: &'static str) -> Vec<String> {
    let arena = LocalArena::new();
    let file = File::ephemeral(Cow::Borrowed(name.as_bytes()), Cow::Borrowed(code.as_bytes()));
    let program = parse_file(&arena, &file);
    assert!(program.errors.is_empty(), "test source did not parse: {:#?}", program.errors);

    let names = NameResolver::new(&arena).resolve(program);
    let issues = SemanticsChecker::new(PHPVersion::new(8, 4, 0)).check(&file, program, &names);

    issues
        .iter()
        .map(|issue| {
            let span = issue.primary_span().expect("a primary span");
            let offset = span.start.offset as usize;
            let line = code[..offset].matches('\n').count() + 1;
            let column = offset - code[..offset].rfind('\n').map_or(0, |newline| newline + 1) + 1;

            format!("{line}:{column} {}", issue.message)
        })
        .collect()
}

fn method(body: &str) -> String {
    format!("namespace App.Tenant;\n\nclass Report\n{{\n    public int run(int extra)\n    {{\n{body}    }}\n}}\n")
}

fn leak(code: String) -> &'static str {
    Box::leak(code.into_boxed_str())
}

/// One file that uses every construct `check_slice` accepts. The engine's bridge lowers the same file.
#[test]
fn the_slice_fixture_has_no_semantic_issues() {
    assert_eq!(issues(include_str!("fixtures/slice.sharp")), Vec::<String>::new());
}

#[test]
fn every_construct_outside_the_slice_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nlet top = 1;\necho 1;\n\ninterface Shape\n{\n}\n\ntrait Named\n{\n}\n\nclass Report\n{\n    public int run(int extra)\n    {\n        switch (extra) {\n            default: return 1;\n        }\n        echo extra;\n        const made = new Report;\n        const partial = this.run(...);\n        const text = <<<TEXT\ntotal\nTEXT;\n        return extra;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:1 This statement is not supported yet in PHP#.",
            "4:1 This statement is not supported yet in PHP#.",
            "10:1 This statement is not supported yet in PHP#.",
            "18:9 This statement is not supported yet in PHP#.",
            "21:9 This statement is not supported yet in PHP#.",
            "22:22 This `new` without arguments is not supported yet in PHP#.",
            "23:25 This expression is not supported yet in PHP#.",
            "24:22 This expression is not supported yet in PHP#.",
        ]
    );
}

/// The bridge writes every class type by its full name, so the engine never sees a `self` or `parent` type in a class
/// whose parent its header names.
#[test]
fn self_and_parent_types_are_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nimport Lib.Entity;\n\nclass Report : Entity\n{\n    public parent copy(self other)\n    {\n        return other;\n    }\n}\n";

    assert_eq!(
        issues(code),
        ["7:12 This type is not supported yet in PHP#.", "7:24 This type is not supported yet in PHP#."]
    );
}

#[test]
fn a_final_enum_or_a_public_trait_is_not_supported_yet_where_it_starts() {
    let code = "namespace App.Tenant;\n\nfinal enum Suit\n{\n}\n\npublic trait Tagged\n{\n}\n\ntrait Bare\n{\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:1 This modifier is not supported yet in PHP#.",
            "7:1 This statement is not supported yet in PHP#.",
            "11:1 This statement is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn if_else_if_and_else_with_braces_are_in_the_slice() {
    let code = leak(method(
        "        if (extra > 1) {\n            return 1;\n        } else if (extra < 0) {\n            return 2;\n        } else {\n            return 3;\n        }\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_template_is_in_the_slice_with_any_slice_expression_in_its_interpolations() {
    let code =
        leak(method("        const label = `Order ${extra}: ${this.run(extra + 1)} items`;\n        return extra;\n"));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_interpolation_outside_the_slice_is_not_supported_yet() {
    let code = leak(method("        const label = `made ${this.run(...)}`;\n        return extra;\n"));

    assert_eq!(issues(code), ["7:31 This expression is not supported yet in PHP#."]);
}

#[test]
fn throw_is_an_expression_in_the_slice() {
    let code = leak(method(
        "        if (extra < 0) {\n            throw new Failure(extra);\n        }\n        return extra ?? throw new Failure(0);\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn try_with_catch_and_finally_is_in_the_slice() {
    let code = leak(method(
        "        try {\n            extra += 1;\n        } catch (Missing | Broken failure) {\n            throw failure;\n        } catch (Throwable) {\n            extra = 0;\n        } finally {\n            extra -= 1;\n        }\n        return extra;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_catch_type_that_is_not_a_class_reports_only_the_php_error() {
    let code = leak(method(
        "        try {\n            extra += 1;\n        } catch (int failure) {\n        } catch (Missing | string failure) {\n        }\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        ["9:18 Invalid type hint in `catch` clause.", "10:28 Invalid type hint in `catch` clause."]
    );
}

#[test]
fn a_catch_variable_named_this_or_a_superglobal_is_an_error() {
    let code = leak(method(
        "        try {\n        } catch (Missing this) {\n        } catch (Broken _GET) {\n        }\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "8:26 Cannot name a catch variable `this`: `this` is the object the method runs on.",
            "9:25 `_GET` is the name of a PHP superglobal: rename this catch variable.",
        ]
    );
}

#[test]
fn a_catch_variable_is_out_of_scope_after_its_catch_block() {
    let code = leak(method("        try {\n        } catch (Missing failure) {\n        }\n        throw failure;\n"));

    assert_eq!(issues(code), ["10:15 `failure` is used after the block that declares it closes."]);
}

#[test]
fn an_if_or_else_without_braces_is_not_supported_yet() {
    let code = leak(method(
        "        if (extra > 1) return 1;\n        if (extra > 2) {\n            return 2;\n        } else return 3;\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 This statement without braces is not supported yet in PHP#.",
            "8:9 This statement without braces is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn elseif_and_a_colon_delimited_if_are_not_supported_yet() {
    let code = leak(method(
        "        if (extra > 1) {\n            return 1;\n        } elseif (extra > 2) {\n            return 2;\n        }\n        if (extra > 3):\n            return 3;\n        endif;\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        ["9:11 This `elseif` is not supported yet in PHP#.", "12:23 This construct is not supported yet in PHP#."]
    );
}

#[test]
fn a_local_declared_in_an_if_branch_is_out_of_scope_after_it() {
    let code = leak(method(
        "        if (extra > 1) {\n            let inner = 1;\n        } else {\n            let inner = 2;\n        }\n        return inner;\n",
    ));

    assert_eq!(issues(code), ["12:16 `inner` is used after the block that declares it closes."]);
}

#[test]
fn while_and_do_while_with_braces_are_in_the_slice() {
    let code = leak(method(
        "        while (extra > 0) {\n            extra -= 1;\n        }\n        do {\n            extra += 1;\n        } while (extra < 3);\n        return extra;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_loop_without_braces_or_with_a_colon_body_is_not_supported_yet() {
    let code = leak(method(
        "        while (extra > 0) extra -= 1;\n        do extra += 1; while (extra < 3);\n        while (extra > 1):\n            extra -= 1;\n        endwhile;\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 This statement without braces is not supported yet in PHP#.",
            "8:9 This statement without braces is not supported yet in PHP#.",
            "9:26 This construct is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_local_declared_in_a_loop_body_is_out_of_scope_after_it() {
    let code = leak(method(
        "        while (extra > 0) {\n            let step = 1;\n            extra -= step;\n        }\n        do {\n            let step = 2;\n        } while (extra < 0);\n        return step;\n",
    ));

    assert_eq!(issues(code), ["14:16 `step` is used after the block that declares it closes."]);
}

#[test]
fn break_and_continue_without_a_level_are_in_the_slice() {
    let code = leak(method(
        "        while (extra > 0) {\n            extra -= 1;\n            if (extra > 5) {\n                continue;\n            }\n            break;\n        }\n        return extra;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn break_and_continue_with_a_level_are_not_supported_yet() {
    let code = leak(method(
        "        while (extra > 0) {\n            while (extra > 1) {\n                break 2;\n            }\n            continue 1;\n        }\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        ["9:17 This statement is not supported yet in PHP#.", "11:13 This statement is not supported yet in PHP#."]
    );
}

#[test]
fn a_for_loop_with_a_let_counter_or_expressions_is_in_the_slice() {
    let code = leak(method(
        "        let total = 0;\n        for (let step = 0; step < extra; step++) {\n            total += step;\n        }\n        for (total = 0; total < 3; total++, extra--) {\n        }\n        for (;;) {\n            break;\n        }\n        return total;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_for_counter_is_out_of_scope_after_its_loop_and_const_when_declared_so() {
    let code = leak(method("        for (const step = 0; step < extra; step++) {\n        }\n        return step;\n"));

    assert_eq!(
        issues(code),
        [
            "9:16 `step` is used after the block that declares it closes.",
            "7:44 Cannot increment `step`: it is declared with `const`.",
        ]
    );
}

#[test]
fn a_for_loop_without_braces_or_with_a_colon_body_is_not_supported_yet() {
    let code = leak(method(
        "        for (let step = 0; step < 3; step++) extra++;\n        for (;;):\n            break;\n        endfor;\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 This statement without braces is not supported yet in PHP#.",
            "8:17 This construct is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn for_of_over_values_or_keys_and_values_is_in_the_slice() {
    let code = leak(method(
        "        for (const value of Store.values()) {\n            extra += value;\n        }\n        for (let [key, value] of Store.values()) {\n            value += key;\n            extra += value;\n        }\n        return extra;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_const_loop_variable_cannot_be_assigned_and_is_out_of_scope_after_its_loop() {
    let code = leak(method(
        "        for (const [key, value] of Store.values()) {\n            value = key;\n        }\n        return value;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "10:16 `value` is used after the block that declares it closes.",
            "8:13 Cannot assign to `value`: it is declared with `const`.",
        ]
    );
}

#[test]
fn a_loop_variable_named_this_or_a_superglobal_is_an_error() {
    let code = leak(method(
        "        for (const this of Store.values()) {\n        }\n        for (const [_GET, value] of Store.values()) {\n        }\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:20 Cannot name a loop variable `this`: `this` is the object the method runs on.",
            "9:21 `_GET` is the name of a PHP superglobal: rename this loop variable.",
        ]
    );
}

#[test]
fn a_for_of_loop_without_braces_is_not_supported_yet() {
    let code = leak(method("        for (const value of Store.values()) extra += value;\n        return extra;\n"));

    assert_eq!(issues(code), ["7:9 This statement without braces is not supported yet in PHP#."]);
}

#[test]
fn reassigning_a_const_local_is_an_error() {
    let code = leak(method("        const base = 2;\n        base = 3;\n        return base;\n"));

    assert_eq!(issues(code), ["8:9 Cannot assign to `base`: it is declared with `const`."]);
}

#[test]
fn null_coalescing_assignment_to_a_const_local_is_an_error() {
    let code = leak(method("        const base = null;\n        base ??= 3;\n        return base;\n"));

    assert_eq!(issues(code), ["8:9 Cannot assign to `base`: it is declared with `const`."]);
}

#[test]
fn reassigning_a_const_local_spelled_in_capitals_is_an_error() {
    let code = leak(method("        CONST base = 2;\n        base = 3;\n        return base;\n"));

    assert_eq!(issues(code), ["8:9 Cannot assign to `base`: it is declared with `const`."]);
}

#[test]
fn incrementing_or_decrementing_a_const_local_is_an_error() {
    let code = leak(method(
        "        const base = 2;\n        base++;\n        ++base;\n        base--;\n        --base;\n        return base;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "8:9 Cannot increment `base`: it is declared with `const`.",
            "9:11 Cannot increment `base`: it is declared with `const`.",
            "10:9 Cannot decrement `base`: it is declared with `const`.",
            "11:11 Cannot decrement `base`: it is declared with `const`.",
        ]
    );
}

#[test]
fn redeclaring_a_name_an_enclosing_block_declares_is_an_error() {
    let code = leak(method(
        "        let total = 1;\n        {\n            let total = 2;\n            let extra = 3;\n        }\n        return total;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "9:17 `total` is already declared in an enclosing block of this method.",
            "10:17 `extra` is already declared in an enclosing block of this method.",
        ]
    );
}

#[test]
fn using_a_local_after_its_block_closes_is_an_error() {
    let code = leak(method("        {\n            let inner = 1;\n        }\n        return inner;\n"));

    assert_eq!(issues(code), ["10:16 `inner` is used after the block that declares it closes."]);
}

#[test]
fn writing_a_const_local_after_its_block_closes_reports_only_the_scope_error() {
    let code =
        leak(method("        {\n            const inner = 1;\n        }\n        inner = 2;\n        return 1;\n"));

    assert_eq!(issues(code), ["10:9 `inner` is used after the block that declares it closes."]);
}

#[test]
fn a_top_level_function_is_an_error() {
    let code = "namespace App.Tenant;\n\nfunction total(int extra) { return extra; }\n";

    assert_eq!(issues(code), ["3:10 PHP# has no top-level functions: move `total` into a class as a static method."]);
}

#[test]
fn dollar_variables_are_errors() {
    let code = leak(method("        $total = 1;\n        $$total;\n        return 1;\n"));

    assert_eq!(
        issues(code),
        ["7:9 PHP# variables have no `$`: write `total`.", "8:9 Variable variables are not part of PHP#.",]
    );
}

#[test]
fn a_dollar_parameter_is_an_error() {
    let code = "class Report\n{\n    public int run(int $extra) { return 1; }\n}\n";

    assert_eq!(issues(code), ["3:24 PHP# variables have no `$`: write `extra`."]);
}

#[test]
fn a_function_called_by_its_bare_name_is_in_the_slice() {
    let code = leak(method(
        "        const name = sprintf(\"%d items\", count(this.items(), mode: 0));\n        return strlen(name) + random_int(1, extra);\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_construct_outside_the_slice_is_not_supported_yet_in_a_catch_block_or_a_call_argument() {
    let code = leak(method(
        "        try {\n        } catch (Missing failure) {\n            extra = extra & 1;\n        }\n        return count(this.run(...));\n",
    ));

    assert_eq!(
        issues(code),
        ["9:27 This operator is not supported yet in PHP#.", "11:22 This expression is not supported yet in PHP#."]
    );
}

#[test]
fn a_function_called_through_an_expression_is_not_supported_yet() {
    let code = leak(method("        return this.callback()(extra);\n"));

    assert_eq!(issues(code), ["7:16 This expression is not supported yet in PHP#."]);
}

#[test]
fn compact_extract_and_global_are_errors() {
    let code =
        leak(method("        compact(\"extra\");\n        extract([]);\n        global $config;\n        return 1;\n"));

    assert_eq!(
        issues(code),
        [
            "7:9 `compact()` is not part of PHP#.",
            "8:9 `extract()` is not part of PHP#.",
            "9:9 `global` is not part of PHP#.",
        ]
    );
}

#[test]
fn a_dollar_variable_in_a_double_quoted_string_is_text() {
    let code = leak(method("        return \"{$extra} $extra ${extra}\";\n"));

    assert_eq!(issues(code), Vec::<String>::new());
}

/// One file cannot tell `Status.Active`, a class and its case, from `App.Status`, a namespace and a class, so the
/// analyzer, which knows the codebase, reports a full name. A class of the same namespace needs no import.
#[test]
fn a_chain_of_capitalized_names_on_a_class_of_another_file_is_a_member_read() {
    let code = leak(method("        return Status.Active.label().length + App.Shared.Money.of(extra);\n"));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_abstract_class_with_an_abstract_method_and_a_final_class_are_in_the_slice() {
    let code = "namespace App.Tenant;\n\npublic abstract class Shape\n{\n    public abstract float area();\n\n    protected abstract string name();\n}\n\nfinal class Unit\n{\n    public int one()\n    {\n        return 1;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_interface_declares_methods_without_an_access_modifier_or_a_body() {
    let code = "namespace App.Tenant;\n\npublic interface Measured\n{\n    float area();\n\n    string label(int digits);\n}\n\ninterface Sized\n{\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_class_or_an_interface_names_its_base_class_and_interfaces_after_a_colon() {
    let code = "namespace App.Tenant;\n\nimport Lib.Entity;\n\npublic interface Linkable : Named\n{\n    string link();\n}\n\npublic class Page : Entity, Linkable\n{\n    public string link()\n    {\n        return \"page\";\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_method_opens_with_virtual_and_replaces_with_override() {
    let code = "namespace App.Tenant;\n\npublic class Image\n{\n    public virtual int size()\n    {\n        return 1;\n    }\n}\n\npublic class Thumbnail : Image\n{\n    public override int size()\n    {\n        return 2;\n    }\n\n    private virtual int cached;\n}\n";

    assert_eq!(issues(code), ["18:13 This modifier is not supported yet in PHP#."]);
}

#[test]
fn super_calls_the_parent_method() {
    let code = "namespace App.Tenant;\n\npublic class Thumbnail : Image\n{\n    public override int size()\n    {\n        let base = super.count;\n        return super.size() + base;\n    }\n}\n";

    assert_eq!(issues(code), ["7:20 This expression is not supported yet in PHP#."]);
}

#[test]
fn an_optional_parameter_before_a_required_one_is_an_error() {
    let code = leak(method("        return extra;\n").replace("int extra", "int first = 1, int extra, int last = 2"));

    assert_eq!(
        issues(code),
        [
            "5:20 The optional parameter `first` comes before the required parameter `extra`: PHP would make it required."
        ]
    );
}

#[test]
fn a_modifier_on_an_interface_member_is_an_error() {
    let code =
        "namespace App.Tenant;\n\ninterface Measured\n{\n    public float area();\n\n    static float unit();\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:5 An interface method takes no modifier: every interface method is public.",
            "7:5 An interface method takes no modifier: every interface method is public."
        ]
    );
}

#[test]
fn an_interface_constant_or_another_interface_modifier_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nabstract interface Measured\n{\n    const int SIDES = 4;\n}\n";

    assert_eq!(
        issues(code),
        ["3:1 This modifier is not supported yet in PHP#.", "5:5 This class member is not supported yet in PHP#."]
    );
}

/// Spec section 22: an interface declares abstract properties, `{ get; }` or `{ get; set; }`, and gives a method or a
/// computed property a default body, which is checked as a class method's body.
#[test]
fn an_interface_declares_abstract_properties_and_gives_methods_and_properties_default_bodies() {
    let code = "namespace App;\n\npublic interface HasDesign\n{\n    List<string> claims { get; set; }\n    string designColumn { get; }\n    string designKey() => `design:${this.designColumn}`;\n    string designLabel => `label:${this.designColumn}`;\n    string designName()\n    {\n        return this.designColumn;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_default_body_outside_the_slice_is_not_supported_yet_as_in_a_class_method() {
    let code = "namespace App;\n\ninterface HasDesign\n{\n    string designColumn { get; }\n    string designKey()\n    {\n        echo 1;\n        return designColumn;\n    }\n    string designLabel => $label;\n}\n";

    assert_eq!(
        issues(code),
        [
            "8:9 This statement is not supported yet in PHP#.",
            "9:16 Write `this.designColumn`: members of the same object are always written with `this.`.",
            "11:27 PHP# variables have no `$`: write `label`."
        ]
    );
}

#[test]
fn a_php_interface_method_with_a_body_is_still_an_error() {
    let code = "<?php\n\ninterface HasDesign\n{\n    public function designKey(): string\n    {\n        return 'design';\n    }\n}\n";

    assert_eq!(
        issues_in("src/HasDesign.php", code),
        [
            "5:21 Interface method `HasDesign::designKey` cannot have a body.",
            "6:5 Interface method `HasDesign::designKey` is implicitly abstract and cannot have a concrete body."
        ]
    );
}

#[test]
fn an_interface_field_constant_initial_value_or_accessor_body_is_still_not_supported() {
    let code = "namespace App;\n\ninterface HasDesign\n{\n    private int count = 0;\n\n    const int SIDES = 4;\n\n    string label { get { return \"label\"; } }\n\n    int views { get; } = 0;\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:5 Interface `HasDesign` cannot have non-hooked properties.",
            "9:24 Interface virtual property `HasDesign::label` must be abstract.",
            "5:5 This class member is not supported yet in PHP#.",
            "7:5 This class member is not supported yet in PHP#.",
            "9:5 This class member is not supported yet in PHP#.",
            "11:5 This class member is not supported yet in PHP#."
        ]
    );
}

#[test]
fn a_constant_or_an_enum_case_is_read_in_a_default_a_constant_value_and_an_initial_value() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public const int MAX = Calc.MAX + 1;\n    private Order sort = Order.Ascending;\n\n    [Field(Mode.Write)]\n    public int run(Order extra = Order.Descending)\n    {\n        return 1;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_static_member_read_in_a_constant_expression_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private static int count = 0;\n    public const int MAX = Report.count;\n\n    public int run(int extra = Report.count)\n    {\n        return 1;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "6:28 `Report.count` is a static member, which a constant value cannot read.",
            "8:32 `Report.count` is a static member, which a constant value cannot read."
        ]
    );
}

#[test]
fn a_member_chain_in_a_constant_expression_is_not_supported_yet() {
    let code = leak(method("        return extra;\n").replace("int extra", "int extra = Calc.rate.cents"));

    assert_eq!(issues(code), ["5:32 This expression is not supported yet in PHP#."]);
}

#[test]
fn a_constant_an_enum_case_or_a_static_member_is_read_through_its_class_name() {
    let code = leak(method("        Store.keep(Calc.rate, Order.Descending);\n        return Calc.MAX;\n"));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_static_member_is_written_through_its_class_name() {
    let code = leak(method(
        "        Calc.rate = 2;\n        Calc.count++;\n        --Calc.count;\n        Calc.rate += 1;\n        Calc.rate ??= 1;\n        Calc.rate.cents = 3;\n        return 1;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn null_safe_access_on_a_class_is_an_error_that_names_the_dot() {
    let code = leak(method("        Calc?.make();\n        return Calc?.rate;\n"));

    assert_eq!(
        issues(code),
        [
            "7:9 `Calc` is a class, which is never null: write `Calc.make`.",
            "8:16 `Calc` is a class, which is never null: write `Calc.rate`.",
        ]
    );
}

#[test]
fn a_write_through_null_safe_access_is_not_supported() {
    let code = leak(method(
        "        this?.count = 1;\n        this?.count ??= 2;\n        this?.count++;\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 This write target is not supported yet in PHP#.",
            "8:9 This write target is not supported yet in PHP#.",
            "9:9 This write target is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_reserved_class_name_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Mixed\n{\n}\n";

    assert_eq!(issues(code), ["3:7 Cannot use `Mixed` as a class name: it is reserved."]);
}

#[test]
fn methods_whose_names_differ_only_in_case_are_an_error() {
    let code = "class Report\n{\n    public int run() { return 1; }\n\n    public int Run() { return 2; }\n}\n";

    // The PHP checks already reject this, so PHP# adds no second error.
    assert_eq!(issues(code), ["5:16 class method `Report::Run` has already been defined"]);
}

#[test]
fn an_import_whose_short_name_is_reserved_is_an_error() {
    let code = "namespace App.Tenant;\n\nimport Lib.Int;\nimport Lib.Mixed;\n\nclass Report\n{\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:8 Cannot import `Lib.Int` as `Int`: PHP reserves `Int` for a type.",
            "4:8 Cannot import `Lib.Mixed` as `Mixed`: PHP reserves `Mixed` for a type.",
        ]
    );
}

#[test]
fn two_imports_with_the_same_short_name_are_an_error() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\nimport Other.CALC;\n\nclass Report\n{\n}\n";

    assert_eq!(issues(code), ["4:8 Cannot import `Other.CALC` as `CALC`: `Lib.Calc` is already imported as `Calc`."]);
}

#[test]
fn a_parameter_or_local_named_this_is_an_error() {
    let code = "class Report\n{\n    public int run(int this) { return 1; }\n\n    public int total() { let this = 1; return 1; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:24 Cannot name a parameter `this`: `this` is the object the method runs on.",
            "5:30 Cannot name a local `this`: `this` is the object the method runs on.",
        ]
    );
}

#[test]
fn importing_the_class_the_file_declares_is_valid() {
    let code = "namespace App.Tenant;\n\nimport App.Tenant.Report;\n\nclass Report\n{\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_import_named_like_a_class_of_the_same_file_is_an_error() {
    let code = "namespace App.Tenant;\n\nimport App.Shared.Report;\n\nclass Report\n{\n}\n";

    assert_eq!(
        issues(code),
        ["3:8 Cannot import `App.Shared.Report` as `Report`: this file declares a class named `Report`."]
    );
}

#[test]
fn a_local_or_parameter_named_after_a_superglobal_is_an_error() {
    let code = leak(method("        let _GET = 1;\n        return _GET;\n").replace("int extra", "int GLOBALS"));

    assert_eq!(
        issues(code),
        [
            "5:24 `GLOBALS` is the name of a PHP superglobal: rename this parameter.",
            "7:13 `_GET` is the name of a PHP superglobal: rename this local.",
        ]
    );
}

#[test]
fn a_bare_member_name_is_an_error_that_names_this() {
    let code =
        "class Report\n{\n    public int count() { return 0; }\n\n    public int run() { return count + run(); }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:31 Write `this.count()`: members of the same object are always written with `this.`.",
            "5:39 Write `this.run()`: members of the same object are always written with `this.`.",
        ]
    );
}

#[test]
fn a_bare_method_name_matches_ignoring_case_as_in_php() {
    let code = "class Report\n{\n    public int total() { return 0; }\n\n    public int run() { return Total(); }\n}\n";

    assert_eq!(
        issues(code),
        ["5:31 Write `this.total()`: members of the same object are always written with `this.`."]
    );
}

#[test]
fn a_bare_member_in_a_static_method_names_the_class() {
    let code = "class Report\n{\n    public static int helper() { return 0; }\n\n    public static int run() { return helper(); }\n}\n";

    assert_eq!(
        issues(code),
        ["5:38 Write `Report.helper()`: a static method reaches the members of its class through the class name."]
    );
}

#[test]
fn extends_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Base\n{\n}\n\nclass Report extends Base\n{\n}\n";

    assert_eq!(issues(code), ["7:14 This `extends` clause is not supported yet in PHP#."]);
}

#[test]
fn implements_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nimport Lib.Shape;\n\nclass Report implements Shape\n{\n}\n";

    assert_eq!(issues(code), ["5:14 This `implements` clause is not supported yet in PHP#."]);
}

#[test]
fn attributes_on_a_class_its_members_and_their_parameters_are_in_the_slice() {
    let code = "namespace App.Tenant;\n\n[Entity(label: \"Order Items\"), Searchable]\n[Table(\"orders\")]\nclass Report\n{\n    [Field] private int count = 0;\n    [Field(label: \"Name\", width: 2 * 3, limit: PHP_INT_MAX)] public string name { get; set; }\n\n    public Report([Field(label: null)] public string tenant { get; }, [Field] private int page = 1)\n    {\n    }\n\n    [Action(true, -1.5)]\n    [Retry(3)]\n    public int run([Field(\"extra\")] int extra)\n    {\n        return extra;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn attribute_arguments_outside_the_slice_are_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    [Field(typeof(Report).name)]\n    [Field(Mode.Write)]\n    [Field([this.run(1)])]\n    [Field(label: new Report())]\n    public int run(int extra)\n    {\n        return extra;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:12 This expression is not supported yet in PHP#.",
            "7:13 This expression is not supported yet in PHP#.",
            "8:19 This expression is not supported yet in PHP#.",
        ]
    );
}

/// `typeof(X)` is `X::class`, which PHP takes as a constant expression, so an attribute argument may name a class.
#[test]
fn typeof_is_an_attribute_argument() {
    let code = "namespace App.Tenant;\n\nimport Lib.Access;\nimport Lib.Authenticated;\n\n[Access(typeof(Authenticated))]\nclass Report\n{\n    [Access(role: typeof(Report))]\n    public int run(int extra)\n    {\n        return extra;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_class_may_be_public() {
    let code = "namespace App.Tenant;\n\npublic class Report\n{\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_enum_may_be_public() {
    let code = "namespace App.Tenant;\n\npublic enum Status : string\n{\n    case Active = \"a\";\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_protected_or_private_class_reports_only_the_php_error() {
    let code = "namespace App.Tenant;\n\nprotected class Report\n{\n}\n\nprivate class Calc\n{\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:1 Class `Report` cannot have the `protected` visibility modifier.",
            "7:1 Class `Calc` cannot have the `private` visibility modifier.",
        ]
    );
}

#[test]
fn a_final_method_and_a_readonly_class_are_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nfinal class Report\n{\n    final public int run()\n    {\n        return 1;\n    }\n}\n\nabstract class Shape\n{\n    abstract public int area();\n}\n\nreadonly class Point\n{\n}\n";

    assert_eq!(
        issues(code),
        ["5:5 This modifier is not supported yet in PHP#.", "16:1 This modifier is not supported yet in PHP#."]
    );
}

#[test]
fn a_method_name_starting_with_two_underscores_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public void __clone() { }\n    public int __get(string name) { return 1; }\n    public int __invoke() { return 1; }\n    public string __toString() { return \"report\"; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:17 This method name is not supported yet in PHP#.",
            "6:16 This method name is not supported yet in PHP#.",
            "7:16 This method name is not supported yet in PHP#.",
            "8:19 This method name is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_method_without_an_access_modifier_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    int run() { return 1; }\n    static int make() { return 1; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:9 A method without `public`, `protected` or `private` is not supported yet in PHP#.",
            "6:16 A method without `public`, `protected` or `private` is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_method_named_after_its_class_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int report() { return 1; }\n}\n\nclass Calc\n{\n    public int CALC() { return 1; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:16 A method named after its class is not supported yet in PHP#.",
            "10:16 A method named after its class is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn the_constructor_is_named_after_its_class_without_a_return_type() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private int count;\n\n    public Report(int start)\n    {\n        this.count = start;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_constructor_parameter_with_an_access_modifier_declares_a_field_or_a_property() {
    let code = "namespace App.Tenant;\n\nimport Lib.Planner;\n\nclass Report\n{\n    public Report(\n        private Planner planner,\n        public int id { get; },\n        public string name { get; private set; },\n        private int hidden { get; set; },\n        int extra,\n        protected int count = 0,\n    ) {\n        this.id = extra + count;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_promoted_member_read_without_this_is_an_error_that_names_this() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(private int count)\n    {\n        count = 1;\n    }\n\n    public int total()\n    {\n        return count;\n    }\n}\n";

    assert_eq!(issues(code), ["12:16 Write `this.count`: members of the same object are always written with `this.`."]);
}

#[test]
fn a_public_constructor_parameter_without_accessors_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(public int id) {}\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:19 A `public` constructor parameter needs accessors: a public member is a property, as in `public int id { get; }`."
        ]
    );
}

#[test]
fn a_promoted_member_follows_the_rules_of_the_same_declaration_in_the_class_body() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(public int a { set; }, private int b { get; private set; }, public int c { get => 1; }) {}\n\n    public void reset()\n    {\n        this.a = 1;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:30 A PHP# property needs a `get` accessor.",
            "5:63 The `set` accessor of a PHP# property must be narrower than the property.",
            "5:94 This accessor is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_promoted_member_outside_the_slice_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(private readonly int a, private static int b) {}\n\n    public void run(private int c) {}\n}\n";

    assert_eq!(
        issues(code),
        [
            "7:21 Promoted properties are not allowed outside of constructors.",
            "5:51 Parameter `b` cannot have the `static` modifier.",
            "5:27 This modifier is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_method_without_a_return_type_named_otherwise_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public report() {}\n    public Calc() {}\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:12 A PHP# method needs a return type: only the constructor, named after its class, has none.",
            "6:12 A PHP# method needs a return type: only the constructor, named after its class, has none.",
        ]
    );
}

#[test]
fn a_static_constructor_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public static Report() {}\n}\n";

    assert_eq!(issues(code), ["5:19 A static constructor is not supported yet in PHP#."]);
}

#[test]
fn a_method_without_a_body_reports_only_the_php_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int run();\n}\n";

    assert_eq!(issues(code), ["5:21 Non-Abstract method `Report::run` must have a concrete body."]);
}

#[test]
fn a_field_is_private_or_protected_with_a_type_and_an_optional_constant_initial_value() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private int count = 0;\n    protected float rate = 1.5 * -PHP_INT_MAX;\n    private string label;\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_initial_value_may_be_any_expression_a_method_body_has_without_this() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private Calc calc = new Calc(1, rate: 2);\n    private int made = Calc.make() + 1;\n    public int total { get; set; } = new Calc(2).add(1, 2);\n    private int count = this.made;\n}\n";

    assert_eq!(
        issues(code),
        ["10:25 An initial value cannot use `this`: it runs before the constructor body, while the object is built."]
    );
}

#[test]
fn a_public_field_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int count = 0;\n}\n";

    assert_eq!(issues(code), ["5:5 A PHP# field cannot be `public`: a field is `private` or `protected`."]);
}

#[test]
fn a_field_without_an_access_modifier_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    int count = 0;\n}\n";

    assert_eq!(issues(code), ["5:9 A field without `private` or `protected` is not supported yet in PHP#."]);
}

#[test]
fn a_field_ended_by_a_closing_tag_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private int count = 0 ?><?php\n}\n";

    assert_eq!(issues(code), ["5:27 This construct is not supported yet in PHP#."]);
}

#[test]
fn fields_outside_the_slice_are_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private readonly int count = 0;\n    private int first, second;\n    private int? maybe;\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:13 This modifier is not supported yet in PHP#.",
            "6:5 A field declaring several names is not supported yet in PHP#.",
            "7:13 This type is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_field_or_a_property_may_be_static_with_a_constant_initial_value() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private static int count = 0;\n    protected static string label = \"none\";\n    public static int views { get; private set; } = 0;\n    public static string last { get; set; } = \"\";\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_static_member_outside_the_slice_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public static int id { get; }\n    private static Calc calc = new Calc();\n    public static int views { get; set; } = Calc.make();\n}\n";

    assert_eq!(
        issues(code),
        [
            "7:23 A get-only static property is not supported yet in PHP#.",
            "8:32 A static member's initial value that is not constant is not supported yet in PHP#.",
            "9:45 A static member's initial value that is not constant is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_class_constant_has_an_access_modifier_an_optional_type_and_a_constant_value() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public const int MAX = 3;\n    protected const LIMIT = PHP_INT_MAX - 1;\n    private const string NAME = \"report\";\n    public const float? RATE = null;\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_class_constant_outside_the_slice_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    const A = 1;\n    public const B = 1, C = 2;\n    final public const D = 1;\n    public const E = 2 << 3;\n    public const iterable F = [];\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:11 A constant without `public`, `protected` or `private` is not supported yet in PHP#.",
            "6:5 A constant declaring several names is not supported yet in PHP#.",
            "7:5 This modifier is not supported yet in PHP#.",
            "8:24 This operator is not supported yet in PHP#.",
            "9:18 This type is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_constant_or_static_member_named_without_its_class_is_an_error_that_names_the_class() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public const int MAX = 3;\n    private static int count = 0;\n\n    public int run()\n    {\n        count = MAX;\n        return count;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "10:9 Write `Report.count`: a static member is reached through its class name.",
            "10:17 Write `Report.MAX`: a static member is reached through its class name.",
            "11:16 Write `Report.count`: a static member is reached through its class name.",
        ]
    );
}

#[test]
fn an_auto_property_has_get_an_optional_narrower_set_and_an_optional_constant_initial_value() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int views { get; private set; } = 0;\n    public string name { get; set; } = \"none\";\n    public float rate { get; protected set; }\n    protected int total { get; private set; }\n    public int id { get; }\n    private int hidden { get; set; }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_auto_property_breaking_the_accessor_rules_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int a { get; public set; }\n    private int b { get; private set; }\n    protected int c { get; public set; }\n    public int d { set; }\n    public int e { get; get; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:25 The `set` accessor of a PHP# property must be narrower than the property.",
            "6:26 The `set` accessor of a PHP# property must be narrower than the property.",
            "7:28 The `set` accessor of a PHP# property must be narrower than the property.",
            "8:16 A PHP# property needs a `get` accessor.",
            "9:25 A PHP# property declares each accessor once.",
        ]
    );
}

#[test]
fn a_method_with_an_expression_body_is_in_the_slice() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private int count = 0;\n\n    public Report(int count) => this.count = count;\n\n    public int total() => this.count > 0 ? this.count : 0;\n\n    protected void touch() => this.count++;\n\n    public static string name(string text) => strtolower(text);\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_computed_property_is_in_the_slice_with_any_access_modifier() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private string name = \"\";\n\n    public string slug => strtolower(this.name);\n    protected bool named => this.name != \"\";\n    private int size => strlen(this.name) > 3 ? 1 : 0;\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_expression_body_is_checked_as_a_method_body() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private int count = 0;\n\n    public int total() => this.count ?: 1;\n    public int size => this.count ?: 1;\n}\n";

    assert_eq!(
        issues(code),
        [
            "7:38 PHP# has no `?:`: write `a ?? b` to replace null, or `c ? a : b` with a `bool` condition.",
            "8:35 PHP# has no `?:`: write `a ?? b` to replace null, or `c ? a : b` with a `bool` condition.",
        ]
    );
}

#[test]
fn a_static_computed_property_or_one_without_an_access_modifier_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public static int count => 1;\n    int size => 2;\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:12 A static computed property is not supported yet in PHP#.",
            "6:9 A property without `public`, `protected` or `private` is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn properties_outside_the_slice_are_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    int a { get; set; }\n    public static int b { get; }\n    public int c { private get; set; }\n    public int d { get => 1; }\n    public int e { get; set { } }\n    public int f { get; init; }\n    public int g = 0 { get; }\n    public int h { get; set(int value); }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:9 A property without `public`, `protected` or `private` is not supported yet in PHP#.",
            "6:23 A get-only static property is not supported yet in PHP#.",
            "7:20 This accessor is not supported yet in PHP#.",
            "8:20 This accessor is not supported yet in PHP#.",
            "9:25 This accessor is not supported yet in PHP#.",
            "10:25 This accessor is not supported yet in PHP#.",
            "11:16 An initial value before the accessors is not supported yet in PHP#.",
            "12:25 This accessor is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_variadic_parameter_is_not_supported_yet() {
    let code = leak(method("        return 1;\n").replace("int extra", "int ...extra"));

    assert_eq!(issues(code), ["5:24 This variadic parameter is not supported yet in PHP#."]);
}

#[test]
fn exponentiation_and_its_compound_assignment_are_in_the_slice() {
    let code = leak(method("        let a = extra ** 2 ** -1;\n        a **= 2;\n        return a;\n"));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn operators_outside_the_slice_are_not_supported_yet() {
    let code = leak(method(
        "        let a = extra;\n        a = @extra;\n        a = extra & 1;\n        a = extra | 1;\n        a = extra ^ 1;\n        a = extra << 1;\n        a = extra >> 1;\n        a = ~extra;\n        a = extra xor true;\n        a = extra and true;\n        a = extra or true;\n        a = extra <=> 1;\n        a = extra <> 1;\n        a %= 2;\n        a &= 2;\n        return a;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "8:13 This operator is not supported yet in PHP#.",
            "9:19 This operator is not supported yet in PHP#.",
            "10:19 This operator is not supported yet in PHP#.",
            "11:19 This operator is not supported yet in PHP#.",
            "12:19 This operator is not supported yet in PHP#.",
            "13:19 This operator is not supported yet in PHP#.",
            "14:13 This operator is not supported yet in PHP#.",
            "15:19 This operator is not supported yet in PHP#.",
            "16:19 This operator is not supported yet in PHP#.",
            "17:19 This operator is not supported yet in PHP#.",
            "18:19 This operator is not supported yet in PHP#.",
            "19:19 This operator is not supported yet in PHP#.",
            "20:11 This operator is not supported yet in PHP#.",
            "21:11 This operator is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_member_name_written_as_an_expression_is_not_supported_yet() {
    let code = leak(method("        return this.{\"run\"}(extra);\n"));

    assert_eq!(issues(code), ["7:21 This member name is not supported yet in PHP#."]);
}

#[test]
fn types_outside_the_slice_are_not_supported_yet() {
    let code = "class Report\n{\n    public mixed run(iterable? a, int|string b, iterable c, callable d, (Lib&Other)|null e)\n    {\n        return 1;\n    }\n\n    public self make(Lib f, float g, bool h, string i)\n    {\n        return this;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:12 This type is not supported yet in PHP#.",
            "3:22 This type is not supported yet in PHP#.",
            "3:35 This type is not supported yet in PHP#.",
            "3:49 This type is not supported yet in PHP#.",
            "3:61 This type is not supported yet in PHP#.",
            "3:73 This type is not supported yet in PHP#.",
            "8:12 This type is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_typed_local_takes_the_types_of_the_slice_but_not_void() {
    let code = leak(method(
        "        void nothing = null;\n        iterable items = null;\n        mixed? anything = null;\n        const int? kept = null;\n        kept = 1;\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 A local cannot be `void`: `void` is only a return type.",
            "8:9 This type is not supported yet in PHP#.",
            "9:9 Type `mixed` cannot be nullable.",
            "9:9 This type is not supported yet in PHP#.",
            "11:9 Cannot assign to `kept`: it is declared with `const`.",
        ]
    );
}

#[test]
fn a_typed_for_counter_takes_the_types_of_the_slice_but_not_void() {
    let code = leak(method(
        "        for (void step = null; ; ) {\n        }\n        for (iterable items = null; ; ) {\n        }\n        for (const int? kept = null; ; kept = 1) {\n        }\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:14 A local cannot be `void`: `void` is only a return type.",
            "9:14 This type is not supported yet in PHP#.",
            "11:40 Cannot assign to `kept`: it is declared with `const`.",
        ]
    );
}

#[test]
fn a_nullable_void_reports_only_the_php_error() {
    let code = "class Report\n{\n    public void? run()\n    {\n    }\n}\n";

    assert_eq!(issues(code), ["3:12 Type `void` cannot be nullable."]);
}

#[test]
fn a_by_reference_parameter_is_not_supported_yet() {
    let code = leak(method("        return 1;\n").replace("int extra", "&extra"));

    assert_eq!(issues(code), ["5:20 A by-reference parameter is not supported yet in PHP#."]);
}

#[test]
fn a_dollar_parameter_without_a_type_reports_only_the_dollar_error() {
    let code = leak(method("        return 1;\n").replace("int extra", "$extra"));

    assert_eq!(issues(code), ["5:20 PHP# variables have no `$`: write `extra`."]);
}

#[test]
fn a_void_parameter_reports_only_the_php_error() {
    let code = "class Report\n{\n    public void run(void nothing)\n    {\n    }\n}\n";

    assert_eq!(issues(code), ["3:21 Invalid parameter type: bottom type `void` cannot be used as a parameter type."]);
}

#[test]
fn a_parameter_default_is_a_constant_expression() {
    let code = "class Report\n{\n    public int run(int a = -1 + 2 * PHP_INT_MAX, int b = a, int c = this.run(), bool d = !true)\n    {\n        return a;\n    }\n}\n";

    assert_eq!(
        issues(code),
        ["3:58 This expression is not supported yet in PHP#.", "3:69 This expression is not supported yet in PHP#.",]
    );
}

#[test]
fn a_write_goes_only_to_a_local_a_parameter_or_a_member() {
    let code = leak(method(
        "        1 = 2;\n        FOO = 1;\n        Calc = 1;\n        this = extra;\n        this++;\n        extra.total() = 3;\n        --FOO;\n        FOO *= 2;\n        this.count = 1;\n        extra = 2;\n        extra++;\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:9 This write target is not supported yet in PHP#.",
            "8:9 This write target is not supported yet in PHP#.",
            "9:9 This write target is not supported yet in PHP#.",
            "10:9 This write target is not supported yet in PHP#.",
            "11:9 This write target is not supported yet in PHP#.",
            "12:9 This write target is not supported yet in PHP#.",
            "13:11 This write target is not supported yet in PHP#.",
            "14:9 This write target is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn list_and_map_types_literals_and_indexes_are_in_the_slice() {
    let code = "class Report\n{\n    private Map<string, int> counts = [:];\n    public List<Line> lines { get; private set; } = [];\n\n    public Map<int, List<string>>? group(List<Line> items, Map<string, int> sizes = [\"a\": 1], List<int> none = [])\n    {\n        List<int> numbers = [1, 2];\n        const named = [\"a\": 1, 2: numbers[0]];\n        numbers[0] = items[1].size;\n        this.counts[\"a\"] += sizes[\"a\"];\n        this.lines[0] = items[0];\n        const nested = [[1], [2]];\n        nested[0][0]++;\n        return null;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_interface_method_takes_and_returns_list_and_map_types() {
    let code = "interface Grouped\n{\n    List<int> sizes(Map<string, List<int>> groups);\n\n    Map<float, int> rounded();\n}\n";

    assert_eq!(issues(code), ["5:9 A `Map`'s keys are `int` or `string`, as a PHP array's keys are."]);
}

#[test]
fn a_type_with_type_arguments_other_than_list_or_map_is_not_supported_yet() {
    let code = "class Report\n{\n    public List<void> run(Set<string> a, List<int, int> b, Map<string> c, Paged<Line> d)\n    {\n        return [];\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:12 This type is not supported yet in PHP#.",
            "3:27 This type is not supported yet in PHP#.",
            "3:42 This type is not supported yet in PHP#.",
            "3:60 This type is not supported yet in PHP#.",
            "3:75 This type is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_map_key_that_is_not_int_or_string_is_an_error() {
    let code = "class Report\n{\n    public Map<float, int> run(Map<Line, int> a, Map<int?, int> b, Map<string, Map<bool, int>> c)\n    {\n        return [:];\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:16 A `Map`'s keys are `int` or `string`, as a PHP array's keys are.",
            "3:36 A `Map`'s keys are `int` or `string`, as a PHP array's keys are.",
            "3:54 A `Map`'s keys are `int` or `string`, as a PHP array's keys are.",
            "3:84 A `Map`'s keys are `int` or `string`, as a PHP array's keys are.",
        ]
    );
}

#[test]
fn a_literal_element_outside_the_slice_is_not_supported_yet() {
    let code = leak(method(
        "        const spread = [...extra];\n        const reference = [&extra];\n        const missing = [, extra];\n        let list = [1];\n        list[] = 2;\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:25 This construct is not supported yet in PHP#.",
            "8:28 This operator is not supported yet in PHP#.",
            "9:26 This construct is not supported yet in PHP#.",
            "11:9 This write target is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_second_namespace_is_not_supported_yet() {
    let code = "namespace A;\n\nclass Y\n{\n}\n\nnamespace B;\n\nimport B.X;\n\nclass X\n{\n}\n";

    assert_eq!(issues(code), ["7:1 This namespace is not supported yet in PHP#."]);
}

#[test]
fn a_braced_or_global_namespace_is_not_supported_yet() {
    for code in ["namespace A\n{\n    class Y\n    {\n    }\n}\n", "namespace\n{\n    class Y\n    {\n    }\n}\n"] {
        assert_eq!(issues(code), ["1:1 This namespace is not supported yet in PHP#."], "{code}");
    }
}

#[test]
fn an_invalid_codepoint_escape_is_an_error_as_in_php() {
    let code =
        leak(method("        const big = \"\\u{110000}\";\n        const empty = \"\\u{}\";\n        return 1;\n"));

    assert_eq!(
        issues(code),
        ["7:21 Invalid UTF-8 codepoint escape sequence.", "8:23 Invalid UTF-8 codepoint escape sequence.",]
    );
}

#[test]
fn new_creates_a_class_written_by_its_short_name_with_its_arguments() {
    let code = leak(method("        const made = new Report(extra, total: 2);\n        return made.run(1);\n"));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn new_outside_the_slice_is_not_supported_yet() {
    let code = leak(method(
        "        const kind = new (Report);\n        const other = new class {};\n        const spread = new Report(...extra);\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:22 This expression is not supported yet in PHP#.",
            "8:23 This expression is not supported yet in PHP#.",
            "9:35 This spread argument is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn typeof_names_a_class_in_a_method_body() {
    let code = leak(method("        Store.keep(typeof(Order));\n        return extra;\n"));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_member_of_typeof_is_not_supported_yet() {
    let code = leak(method(
        "        const name = typeof(Order).name;\n        typeof(Order).attributes();\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:22 Reading a member of `typeof(Order)` is not supported yet.",
            "8:9 Reading a member of `typeof(Order)` is not supported yet.",
        ]
    );
}

#[test]
fn the_ternary_is_in_the_slice_with_a_nested_ternary_in_parentheses() {
    let code = leak(method(
        "        const sign = extra > 0 ? 1 : (extra < 0 ? -1 : 0);\n        return (extra > 9 ? true : false) ? sign : 0;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn the_two_operand_ternary_is_not_part_of_php_sharp() {
    let code = leak(method("        return extra ?: 1;\n"));

    assert_eq!(
        issues(code),
        ["7:22 PHP# has no `?:`: write `a ?? b` to replace null, or `c ? a : b` with a `bool` condition."]
    );
}

#[test]
fn a_ternary_nested_in_a_condition_without_parentheses_is_an_error_as_in_php() {
    let code = leak(method("        return extra > 1 ? 1 : extra > 0 ? 2 : 3;\n"));

    assert_eq!(
        issues(code),
        [
            "7:16 Unparenthesized `a ? b : c ? d : e` is not supported. Use either `(a ? b : c) ? d : e` or `a ? b : (c ? d : e)`."
        ]
    );
}

#[test]
fn casts_between_numbers_are_in_the_slice() {
    let code = leak(method(
        "        const cents = (int)(extra * 1.5);\n        const share = (float)extra;\n        const label = (string)share;\n        return cents;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn bool_array_and_object_casts_do_not_exist_in_php_sharp() {
    let code = leak(method(
        "        const a = (bool)extra;\n        const b = (array)extra;\n        const c = (object)extra;\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:19 PHP# has no `(bool)`: compare the value instead, as in `count > 0` or `flag == \"1\"`.",
            "8:19 PHP# has no `(array)`: `(int)`, `(float)` and `(string)` convert between numbers only.",
            "9:19 PHP# has no `(object)`: `(int)`, `(float)` and `(string)` convert between numbers only.",
        ]
    );
}

#[test]
fn php_cast_aliases_do_not_exist_in_php_sharp() {
    let code = leak(method(
        "        const a = (integer)extra;\n        const b = (double)extra;\n        const c = (real)extra;\n        const d = (boolean)extra;\n        const e = (binary)extra;\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:19 PHP# has no `(integer)`: write `(int)`.",
            "8:19 PHP# has no `(double)`: write `(float)`.",
            "9:19 PHP# has no `(real)`: write `(float)`.",
            "10:19 PHP# has no `(boolean)`: compare the value instead, as in `count > 0` or `flag == \"1\"`.",
            "11:19 PHP# has no `(binary)`: write `(string)`.",
        ]
    );
}

#[test]
fn a_cast_or_a_ternary_in_a_parameter_default_is_not_supported_yet() {
    let cast = leak(method("        return extra;\n").replace("int extra", "int extra = (int)1.5"));
    let ternary = leak(method("        return extra;\n").replace("int extra", "int extra = true ? 1 : 2"));

    assert_eq!(issues(cast), ["5:32 This operator is not supported yet in PHP#."]);
    assert_eq!(issues(ternary), ["5:32 This expression is not supported yet in PHP#."]);
}

#[test]
fn a_spread_argument_is_not_supported_yet() {
    let code = leak(method("        const parts = this.parts();\n        return this.total(...parts);\n"));

    assert_eq!(issues(code), ["8:27 This spread argument is not supported yet in PHP#."]);
}

#[test]
fn implements_or_a_member_modifier_in_an_enum_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nimport Lib.Shape;\n\nenum Status : string implements Shape\n{\n    case Active = \"a\";\n\n    final public string label()\n    {\n        return this.name;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:22 This `implements` clause is not supported yet in PHP#.",
            "9:5 This modifier is not supported yet in PHP#.",
        ]
    );
}

/// An enum's constant follows a class constant's rules: an access modifier, an optional type, one name and a constant
/// value, which may read a case of the enum.
#[test]
fn an_enum_constant_follows_the_class_constant_rules() {
    let code = "namespace App.Tenant;\n\nenum Status : string\n{\n    case Active = \"a\";\n\n    public const Status Default = Status.Active;\n    private const int LIMIT = 3;\n    const string BARE = \"b\";\n    public const int ONE = 1, TWO = 2;\n}\n";

    assert_eq!(
        issues(code),
        [
            "9:18 A constant without `public`, `protected` or `private` is not supported yet in PHP#.",
            "10:5 A constant declaring several names is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn an_enum_names_its_backing_type_then_its_interfaces_after_a_colon() {
    let code = "namespace App.Tenant;\n\nimport Lib.HasLabel;\n\nenum Status : string, HasLabel\n{\n    case Active = \"a\";\n}\n\nenum Suit : HasLabel\n{\n    case Hearts;\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

/// The engine refuses a declaration whose header names an interface twice. It adds `UnitEnum` to every enum, and
/// `BackedEnum` to a backed one, so it refuses an enum header that names them as well.
#[test]
fn a_header_naming_an_interface_twice_or_an_enum_header_naming_unit_enum_is_an_error() {
    let code = "namespace App.Tenant;\n\nimport Lib.HasLabel;\nimport UnitEnum;\nimport BackedEnum;\n\nclass Card : HasLabel, HasLabel\n{\n}\n\ninterface Shown : HasLabel, HasLabel\n{\n}\n\nenum Status : string, HasLabel, BackedEnum\n{\n    case Active = \"a\";\n}\n\nenum Suit : UnitEnum\n{\n    case Hearts;\n}\n";

    assert_eq!(
        issues(code),
        [
            "7:24 This header names `HasLabel` twice.",
            "11:29 This header names `HasLabel` twice.",
            "15:33 Every enum implements `UnitEnum`, and every backed enum `BackedEnum`, so an enum header never names them.",
            "20:13 Every enum implements `UnitEnum`, and every backed enum `BackedEnum`, so an enum header never names them.",
        ]
    );
}

#[test]
fn a_property_in_an_enum_reports_only_the_php_error() {
    let code = "namespace App.Tenant;\n\nenum Status\n{\n    case Active;\n\n    private int count = 0;\n}\n";

    assert_eq!(issues(code), ["7:5 Enum `Status` cannot have properties."]);
}

#[test]
fn a_backing_type_other_than_int_or_string_reports_only_the_php_error() {
    let code = "namespace App.Tenant;\n\nenum Status : float\n{\n    case Active = 1.5;\n}\n";

    assert_eq!(issues(code), ["3:15 Enum `Status` backing type must be either `string` or `int`, but found `float`."]);
}

#[test]
fn an_enum_method_without_a_return_type_is_an_error() {
    let code = "namespace App.Tenant;\n\nenum Status\n{\n    case Active;\n\n    public status() {}\n    public label() {}\n}\n";

    assert_eq!(
        issues(code),
        [
            "7:12 An enum has no constructor: its cases are its only values.",
            "8:12 A PHP# method needs a return type: only the constructor, named after its class, has none.",
        ]
    );
}

#[test]
fn an_enum_case_named_class_is_an_error_as_in_php() {
    let code = "namespace App.Tenant;\n\nenum Status\n{\n    case class;\n}\n\nenum Mode : string\n{\n    case CLASS = \"c\";\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:10 An enum case cannot be named `class`: PHP reserves `class` for the class name.",
            "10:10 An enum case cannot be named `class`: PHP reserves `class` for the class name.",
        ]
    );
}

#[test]
fn a_bare_case_name_in_an_enum_method_is_an_error_that_names_the_enum() {
    let code = "namespace App.Tenant;\n\nenum Status\n{\n    case Active;\n\n    public bool active()\n    {\n        return this === Active && label() != \"\";\n    }\n\n    public string label() => this.name;\n\n    public static Status first() => Active;\n}\n";

    assert_eq!(
        issues(code),
        [
            "9:25 Write `Status.Active`: an enum case is reached through its enum's name.",
            "9:35 Write `this.label()`: members of the same object are always written with `this.`.",
            "14:37 Write `Status.Active`: an enum case is reached through its enum's name.",
        ]
    );
}

#[test]
fn an_enum_case_of_the_same_file_is_read_through_its_enum_name_in_a_class_and_in_the_enum() {
    let code = "namespace App.Tenant;\n\nenum Status\n{\n    case Active;\n\n    public static Status first() => Status.Active;\n}\n\nclass Report\n{\n    public bool run(Status status = Status.Active)\n    {\n        return status === Status.Active;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

/// A case is read as `Status.Active` in a class, in the enum's own method, in a parameter default, in a constant, and
/// with a method called on it. A case's value reads another class's constant the same way.
#[test]
fn a_case_is_read_through_its_enum_name_in_every_place() {
    let code = "namespace App.Tenant;\n\nimport Lib.Registry;\n\npublic enum Status : string\n{\n    case Active = \"a\";\n    case Paused = Registry.PAUSED;\n\n    public const Status Default = Status.Active;\n\n    public bool active() => this === Status.Active;\n\n    public string label() => this.name;\n}\n\nclass Report\n{\n    public string run(Status status = Status.Active)\n    {\n        if (status === Status.Active) {\n            return Status.Active.label();\n        }\n        return Status.Default.label();\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_static_member_read_in_an_enum_case_value_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private static int count = 0;\n}\n\nenum Tier : int\n{\n    case Low = Report.count;\n}\n";

    assert_eq!(issues(code), ["10:16 `Report.count` is a static member, which a constant value cannot read."]);
}

#[test]
fn an_enum_named_like_a_reserved_class_name_or_an_import_is_an_error_as_a_class_is() {
    let code = "namespace App.Tenant;\n\nimport App.Shared.Status;\n\nenum Status\n{\n    case Active;\n}\n\nenum Mixed\n{\n    case One;\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:8 Cannot import `App.Shared.Status` as `Status`: this file declares a class named `Status`.",
            "10:6 Cannot use `Mixed` as a class name: it is reserved.",
        ]
    );
}

/// An enum's method follows a class's method rules, so it takes `List`, `Map` and function types and holds lambdas,
/// and a lambda in it captures a loop variable that changes only as a class's does.
#[test]
fn an_enum_method_takes_collection_and_function_types_and_holds_lambdas_as_a_class_method_does() {
    let code = "namespace App.Tenant;\n\nenum Status : string\n{\n    case Active = \"a\";\n\n    public static Map<string, Status> byValue(List<Status> all)\n    {\n        const Function<string(Status)> key = s => s.value;\n        return all.associateBy(key);\n    }\n\n    public List<Function<bool()>> checks(List<Status> all)\n    {\n        List<Function<bool()>> checks = [];\n        for (let other of all) {\n            other = Status.Active;\n            checks.add(() => other === this);\n        }\n        return checks;\n    }\n}\n";

    assert_eq!(issues(code), ["18:30 This capture of a loop variable that changes is not supported yet in PHP#."]);
}

#[test]
fn lambdas_with_an_expression_or_a_block_body_and_calls_of_function_locals_are_in_the_slice() {
    let code = leak(method(
        "        let count = 0;\n        const add = (a, b) => a + b;\n        const check = (int value) => value > extra;\n        const increment = () => { count += 1; };\n        increment();\n        return add(count, check(1) ? 1 : 0);\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn function_types_are_in_the_slice_as_field_parameter_local_and_return_types() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private Function<int?(Line, string)> priceOf;\n    private Function<void(Order)> onPaid;\n\n    public Function<bool(Order)> eligible(Map<string, Function<int()>> counters, Function<List<int>(List<Line>)> ids)\n    {\n        const Function<void()> log = () => {};\n        Function<int(int)> twice = n => n * 2;\n        return o => true;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_void_parameter_of_a_function_type_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private Function<int(void)> priceOf;\n}\n";

    assert_eq!(issues(code), ["5:13 This type is not supported yet in PHP#."]);
}

/// A call `x.priceOf(line)` runs the property's function only when the class has no method `priceOf`, and PHP finds
/// a method ignoring case, so a method named as a field, a property or a constructor-declared member is an error.
#[test]
fn a_method_named_as_a_property_of_its_class_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private Function<int(int)> priceOf;\n    public int views { get; set; }\n\n    public Report(private Function<bool()> ready) {}\n\n    public int priceof(int amount) => amount;\n    public int views() => 1;\n    public bool ready() => true;\n    public int other() => 2;\n}\n";

    assert_eq!(
        issues(code),
        [
            "10:16 The class `Report` declares a method and a property named `priceof`.",
            "11:16 The class `Report` declares a method and a property named `views`.",
            "12:17 The class `Report` declares a method and a property named `ready`.",
        ]
    );
}

#[test]
fn a_lambda_parameter_outside_the_slice_is_not_supported_yet() {
    let code = leak(method(
        "        const spread = (int ...values) => 1;\n        const marked = (readonly int value) => value;\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:29 This variadic parameter is not supported yet in PHP#.",
            "8:25 This modifier is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_lambda_parameter_named_like_an_enclosing_local_is_an_error() {
    let code = leak(method("        const twice = (extra) => extra * 2;\n        return twice(1);\n"));

    assert_eq!(issues(code), ["7:24 `extra` is already declared in an enclosing block of this method."]);
}

#[test]
fn a_lambda_capturing_a_loop_variable_that_changes_is_not_supported_yet() {
    let code = leak(method(
        "        for (let step = 0; step < extra; step++) {\n            const show = () => step;\n        }\n        for (let value of Store.values()) {\n            value += 1;\n            const read = () => value;\n        }\n        for (const item of Store.values()) {\n            const keep = () => item;\n        }\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "8:32 This capture of a loop variable that changes is not supported yet in PHP#.",
            "12:32 This capture of a loop variable that changes is not supported yet in PHP#.",
        ]
    );
}
