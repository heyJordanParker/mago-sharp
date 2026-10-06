#![allow(clippy::panic, clippy::expect_used)]

use std::borrow::Cow;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_names::resolver::NameResolver;
use mago_php_version::PHPVersion;
use mago_reporting::Issue;
use mago_semantics::SemanticsChecker;
use mago_syntax::parser::parse_file;

/// Every semantic issue in the source, in the dialect its path names.
fn check(path: &'static str, code: &'static str) -> Vec<Issue> {
    let arena = LocalArena::new();
    let file = File::ephemeral(Cow::Borrowed(path.as_bytes()), Cow::Borrowed(code.as_bytes()));
    let program = parse_file(&arena, &file);
    assert!(program.errors.is_empty(), "test source did not parse: {:#?}", program.errors);

    let names = NameResolver::new(&arena).resolve(program);

    SemanticsChecker::new(PHPVersion::new(8, 4, 0)).check(&file, program, &names).into_iter().collect()
}

/// Every semantic issue in the PHP# source, as `line:column message` at its primary span.
fn issues(code: &'static str) -> Vec<String> {
    issues_in("src/Report.sharp", code)
}

/// Every semantic issue in the source, in the dialect its path names, as `line:column message` at its primary span.
fn issues_in(path: &'static str, code: &'static str) -> Vec<String> {
    check(path, code)
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

/// The bridge writes every class type by its full name, so the engine never sees a `parent` type in a class whose
/// parent its header names. PHP's `self` is not part of PHP#, spec section 25.
#[test]
fn a_parent_type_is_not_supported_yet_and_a_self_type_is_an_error() {
    let code = "namespace App.Tenant;\n\nimport Lib.Entity;\n\nclass Report : Entity\n{\n    public parent copy(self other)\n    {\n        return other;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "7:12 This type is not supported yet in PHP#.",
            "7:24 PHP# has no `self`: write the class's own name, `Report`, for the declaring class.",
        ]
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

/// Spec section 25's example: `Self` is the class a static method is called on, and `new Self(…)` needs a `required`
/// constructor. A subclass calls its parent's constructor with `super.__construct(…)`, because `: super(…)` does not
/// parse yet.
#[test]
fn self_is_a_return_type_creates_with_a_required_constructor_and_calls_static_methods() {
    let code = "namespace App.Tenant;\n\nimport Lib.Row;\nimport Lib.Clock;\n\npublic abstract class DatabaseEntity\n{\n    public required DatabaseEntity(Row row)\n    {\n    }\n\n    public static Self fromSchema(Row row)\n    {\n        return new Self(row);\n    }\n\n    public static Self? find(Row row) => Self.fromSchema(row);\n\n    public static Self|int counted(Row row) => Self.fromSchema(row);\n\n    public static (Self|int)? either(Row row) => null;\n}\n\npublic class Order : DatabaseEntity\n{\n    public Order(Row row, Clock? clock = null)\n    {\n        super.__construct(row);\n    }\n}\n\npublic interface Copyable\n{\n    Self copy();\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn self_is_only_a_return_type() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private Self owner;\n    public Self? next { get; set; }\n    public const Self SAME = 1;\n\n    public required Report(Self other, int|Self either)\n    {\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:13 `Self` is only a return type in PHP#.",
            "6:12 `Self` is only a return type in PHP#.",
            "7:18 `Self` is only a return type in PHP#.",
            "9:28 `Self` is only a return type in PHP#.",
            "9:44 `Self` is only a return type in PHP#.",
        ]
    );
}

#[test]
fn new_self_needs_a_required_constructor() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(int count)\n    {\n    }\n\n    public static Self make() => new Self(1);\n}\n\nclass Total\n{\n    public static Self make() => new Self();\n}\n\nclass Entity\n{\n    public required Entity(int count)\n    {\n    }\n\n    public static Self make() => new Self;\n}\n";

    assert_eq!(
        issues(code),
        [
            "9:34 `new Self(…)` needs a `required` constructor: `Self` can be any subclass, so every subclass must keep a constructor that `new Self(…)` can call.",
            "14:34 `new Self(…)` needs a `required` constructor: `Self` can be any subclass, so every subclass must keep a constructor that `new Self(…)` can call.",
            "23:34 This `new` without arguments is not supported yet in PHP#.",
        ]
    );
    let help: Vec<_> = check("src/Report.sharp", code).into_iter().filter_map(|issue| issue.help).collect();
    assert_eq!(
        help,
        [
            "Declare the constructor of `Report` `required`, as in `public required Report(…)`.",
            "Declare the constructor of `Total` `required`, as in `public required Total(…)`.",
        ]
    );
}

/// `Self.y` is not ruled yet, as `super.y` is not.
#[test]
fn a_member_of_self_read_or_written_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private static int count = 0;\n\n    public static int run()\n    {\n        Self.count = 1;\n        return Self.count + Self;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "9:9 This expression is not supported yet in PHP#.",
            "10:16 This expression is not supported yet in PHP#.",
            "10:29 This expression is not supported yet in PHP#.",
        ]
    );
}

/// PHP's `self` is removed, spec section 25: a class writes its own name, or `Self` where `Self` goes.
#[test]
fn self_names_the_class_and_self_where_it_goes() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private static int count = 0;\n    private self other;\n\n    public required Report(self copy)\n    {\n    }\n\n    public static self make()\n    {\n        self.count = 1;\n        let made = self.make();\n        return new self(self.count);\n    }\n}\n\ninterface Copyable\n{\n    self copy();\n}\n";
    let without_self = "PHP# has no `self`: write the class's own name, `Report`, for the declaring class.";
    let with_self = "PHP# has no `self`: write the class's own name, `Report`, for the declaring class, or `Self` for the class a static method is called on.";

    assert_eq!(
        issues(code),
        [
            format!("6:13 {without_self}"),
            format!("8:28 {without_self}"),
            format!("12:19 {with_self}"),
            format!("14:9 {without_self}"),
            format!("15:20 {with_self}"),
            format!("16:20 {with_self}"),
            "22:5 PHP# has no `self`: write the class's own name, `Copyable`, for the declaring class, or `Self` for the class a static method is called on.".to_owned(),
        ]
    );
}

#[test]
fn static_is_written_self() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private static int count = 0;\n\n    public required Report(int|static copy)\n    {\n    }\n\n    public static int|static make()\n    {\n        let count = static.count;\n        let made = static.make();\n        return new static(static.count);\n    }\n}\n";
    let message = "PHP# writes `Self` for PHP's `static`.";

    assert_eq!(
        issues(code),
        [
            format!("7:32 {message}"),
            format!("11:23 {message}"),
            format!("13:21 {message}"),
            format!("14:20 {message}"),
            format!("15:20 {message}"),
        ]
    );
}

#[test]
fn required_is_a_modifier_of_the_constructor_only() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public required const int MAX = 1;\n\n    public required Report(int count)\n    {\n    }\n}\n";

    assert_eq!(issues(code), ["5:12 This modifier is not supported yet in PHP#."]);
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
fn an_optional_parameter_before_a_variadic_one_is_in_the_slice_as_in_php() {
    let code = leak(method("        return extra;\n").replace("int extra", "int extra = 1, int ...rest"));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_interface_method_takes_union_types_and_a_variadic_parameter() {
    let code = "namespace App.Tenant;\n\ninterface Measured\n{\n    int|float total(int|float ...values);\n\n    (int|string)? key(string? name, int ...rest);\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
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

#[test]
fn an_interface_property_is_not_supported_yet_with_or_without_accessor_bodies() {
    let code = "namespace App.Tenant;\n\ninterface Named\n{\n    public string name { get; }\n    public string label { get => \"label\"; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "6:31 Interface virtual property `Named::label` must be abstract.",
            "5:5 This class member is not supported yet in PHP#.",
            "6:5 This class member is not supported yet in PHP#."
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
            "5:90 The property `c` has no storage for the constructor to set: give it an auto accessor, such as `get;`, or use `field` in an accessor body.",
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

/// A field written `override` replaces a plain PHP parent's property, spec section 6.1, with the parent's access
/// level, `public` included. The analyzer checks it against the parent.
#[test]
fn a_field_may_override_a_parent_property_with_its_access_level_and_a_constant_value() {
    let code = "namespace App.Store;\n\nimport Lib.Model;\n\npublic class Order : Model\n{\n    protected override List<string> fillable = [\"number\", \"total\"];\n    public override bool timestamps = false;\n    protected override string table = \"orders\";\n    protected static override int count = 1 + 2;\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

/// An override replaces the default of the parent's property, so it writes one: without it, the engine would give the
/// property PHP's default for an untyped one, `null`, which its written type may not hold.
#[test]
fn an_override_without_an_initial_value_is_an_error() {
    let code = "namespace App.Store;\n\nimport Lib.Model;\n\npublic class Order : Model\n{\n    protected override List<string> hidden;\n}\n";

    assert_eq!(
        issues(code),
        ["7:37 An override needs an initial value: it replaces the default of the parent's property."]
    );
}

/// The parent's constructor may read an overridden property before this class's code runs, so an override's initial
/// value is constant, which PHP stores as the default.
#[test]
fn an_override_whose_initial_value_is_not_constant_is_an_error() {
    let code = "namespace App.Store;\n\nimport Lib.Calc;\nimport Lib.Model;\n\npublic class Order : Model\n{\n    protected override List<string> fillable = this.columns();\n    protected override int perPage = Calc.make();\n\n    public List<string> columns() => [\"number\"];\n}\n";

    assert_eq!(
        issues(code),
        [
            "8:48 The initial value of an override must be constant: the parent's constructor may read it before this class's code runs.",
            "9:38 The initial value of an override must be constant: the parent's constructor may read it before this class's code runs.",
        ]
    );
}

/// `override` takes a field, the one member that replaces a plain PHP parent's property. A PHP# parent's property is
/// overridden as a property, spec section 6.1, which is not supported yet.
#[test]
fn an_overriding_property_is_not_supported_yet() {
    let code = "namespace App.Store;\n\nimport Lib.Model;\n\npublic class Order : Model\n{\n    public override int views { get; set; } = 0;\n    public override string slug => \"order\";\n}\n";

    assert_eq!(
        issues(code),
        [
            "7:25 Overriding a property is not supported yet in PHP#.",
            "8:28 Overriding a property is not supported yet in PHP#.",
        ]
    );
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
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private readonly int count = 0;\n    private int first, second;\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:13 This modifier is not supported yet in PHP#.",
            "6:5 A field declaring several names is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_nullable_field_or_auto_property_is_in_the_slice_with_or_without_an_initial_value() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int? total;\n    protected Calc? owner;\n    private (int|string)? key;\n    private int? start = 1;\n    public Calc? helper { get; set; }\n    public int? count { get; private set; }\n    public (int|string)? code { get; protected set; } = 2;\n    public int? limit { get; } = null;\n    public string? label { get; } = \"none\";\n\n    public Report(public int? id { get; }, private int? flag, public (int|string)? tag { get; })\n    {\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_field_or_a_property_may_be_static_with_a_constant_initial_value() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private static int count = 0;\n    protected static string label = \"none\";\n    public static int views { get; private set; } = 0;\n    public static string last { get; set; } = \"\";\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_nullable_field_of_a_type_outside_the_slice_is_not_supported_yet() {
    let code =
        "namespace App.Tenant;\n\nclass Report\n{\n    private iterable? items;\n    private (int|iterable)? key;\n}\n";

    assert_eq!(
        issues(code),
        ["5:13 This type is not supported yet in PHP#.", "6:18 This type is not supported yet in PHP#."]
    );
}

#[test]
fn a_void_field_reports_only_the_php_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private void? nothing;\n    private void plain;\n}\n";

    assert_eq!(
        issues(code),
        ["6:13 Property `Report::plain` cannot have type `void`.", "5:13 Type `void` cannot be nullable."]
    );
}

#[test]
fn a_get_only_nullable_property_without_an_initial_value_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int? total { get; }\n    private (int|string)? key { get; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:17 A get-only nullable property without an initial value is not supported yet in PHP#.",
            "6:27 A get-only nullable property without an initial value is not supported yet in PHP#.",
        ]
    );
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
fn a_get_only_nullable_property_without_an_initial_value_names_the_initial_value_or_set_to_write() {
    let notes: Vec<Vec<String>> =
        check("src/Report.sharp", "namespace App.Tenant;\n\nclass Report\n{\n    public int? total { get; }\n}\n")
            .into_iter()
            .map(|issue| issue.notes)
            .collect();

    assert_eq!(
        notes,
        [[
            "A get-only property runs as PHP's `readonly`, which takes no default, so it cannot start as null: give it an initial value, as in `public int? total { get; } = null;`, or a `set` accessor."
        ]]
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

/// Spec section 24 writes a union anywhere a type goes, and PHP 8.3 types a class constant with one.
#[test]
fn a_class_constant_may_have_a_union_type_or_a_nullable_union_type() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public const int|string KEY = 1;\n    public const (int|string)? CODE = null;\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_union_typed_class_constant_follows_the_union_rules() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public const int|null NONE = null;\n    public const int|iterable ITEMS = 1;\n    public const int|string|int TWICE = 1;\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:22 This union that holds null is not supported yet in PHP#.",
            "6:22 This type is not supported yet in PHP#.",
            "7:29 Duplicate type `int` is redundant.",
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
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    int a { get; set; }\n    public static int b { get; }\n    public int c { private get; set; }\n    public abstract int d { get; }\n    public override int e { get => 1; }\n    public int f { get; init; }\n    public int g = 0 { get; }\n    public int h { get; set(int value); }\n}\n";

    assert_eq!(
        issues(code),
        [
            "8:12 Property `Report::d` cannot be declared abstract",
            "5:9 A property without `public`, `protected` or `private` is not supported yet in PHP#.",
            "6:23 A get-only static property is not supported yet in PHP#.",
            "7:20 This accessor is not supported yet in PHP#.",
            "8:12 This modifier is not supported yet in PHP#.",
            "9:25 Overriding a property is not supported yet in PHP#.",
            "10:25 This accessor is not supported yet in PHP#.",
            "11:16 An initial value before the accessors is not supported yet in PHP#.",
            "12:25 This accessor is not supported yet in PHP#.",
        ]
    );
}

/// Spec section 6.1: `get` and `set` take a body, written `=> expr;` or as a block, which uses `field` for the
/// property's storage and `value` for the incoming value. Each accessor keeps its own access level, and a
/// constructor parameter declares the same property.
#[test]
fn accessor_bodies_are_in_the_slice() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public string name { get => field; set => field = trim(value); }\n    public int count { get; private set { if (value < 0) { throw new Negative(value); } field = value; } }\n    public int total { get { return this.count + 1; } }\n    public string label { get => this.name; set => this.rename(value); }\n    public List<string> tags { get => field; set { field = value; } } = [];\n    public Map<string, int> hits { get => field; set { field = value; field[\"all\"] = 1; field.set(\"one\", 1); } } = [:];\n    public int seen { get => field; set { field += value; field++; } } = 0;\n\n    public Report(public string title { get => field; set => field = value; })\n    {\n    }\n\n    public void rename(string text)\n    {\n        this.name = text;\n        this.tags.add(text);\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

/// Spec section 24 puts `?` on a property as on a parameter, so a property with accessor bodies takes a nullable type.
/// A property without storage is computed by its accessors, so it starts as nothing and needs no initial value even
/// when get-only, and one whose body uses `field` starts as null as an auto-property with `set` does.
#[test]
fn nullable_properties_with_accessor_bodies_are_in_the_slice() {
    let code = "namespace App.Tenant;\n\nimport Lib.Model;\n\nclass Order : Model\n{\n    public Address? shipping { get => this.getAttribute(\"shipping\"); set => this.setAttribute(\"shipping\", value); }\n    public string? note { get => field; set => field = value; }\n    public string? summary { get => this.note; }\n}\n\nclass Address\n{\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

/// A lambda is a function of its own, so PHP would call the accessor again where it reads `$this->count`.
#[test]
fn field_inside_a_lambda_in_an_accessor_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int count { get { const read = () => field; const twice = () => () => field; return read(); } set; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:49 `field` cannot be used in a lambda: PHP would call the accessor again instead of reading the storage.",
            "5:82 `field` cannot be used in a lambda: PHP would call the accessor again instead of reading the storage.",
        ]
    );
}

/// PHP reads and writes the storage where a property's own accessor names it, while C# calls the accessor again, so
/// the accessor writes `field`. A lambda calls the accessor again in both.
#[test]
fn the_property_read_through_this_inside_its_own_accessor_is_an_error_that_names_field() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int count { get => this.count + 1; set => this.count = value; }\n    public int other { get { const read = () => this.other; return read(); } }\n    public int size { get => (this).size + (this?.size ?? 0); }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:31 Write `field` instead of `this.count` inside `count`'s own accessor.",
            "5:54 Write `field` instead of `this.count` inside `count`'s own accessor.",
            "7:30 Write `field` instead of `this.size` inside `size`'s own accessor.",
            "7:45 Write `field` instead of `this.size` inside `size`'s own accessor.",
        ]
    );
}

/// PHP gives a `get` hook the property's type and a `set` hook `void`, so `get` returns a value and `set` returns
/// none. A lambda in an accessor returns for itself.
#[test]
fn a_get_returns_a_value_and_a_set_returns_none_as_php_hooks_do() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int a { get { return; } }\n    public int b { get => field; set { return value; } }\n    public int c { get => field; set { const run = () => { return 1; }; return; } }\n    public int d { get { const run = () => { return; }; return 1; } }\n}\n";

    assert_eq!(
        issues(code),
        ["5:26 A `get` accessor must return a value.", "6:47 A `set` accessor must not return a value.",]
    );
}

/// A property uses `field` where an accessor body names it outside a lambda, in a nested block too, as php-src finds
/// `$this->name` in a hook. `field` only in a lambda leaves the property without storage.
#[test]
fn field_in_a_nested_block_gives_storage_and_field_only_in_a_lambda_does_not() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int a { get { if (true) { return field; } return 0; } } = 1;\n    public int b { get { const read = () => field; return read(); } } = 2;\n}\n";

    assert_eq!(
        issues(code),
        [
            "6:45 `field` cannot be used in a lambda: PHP would call the accessor again instead of reading the storage.",
            "6:73 The property `b` has no storage for its initial value: give it an auto accessor, such as `get;`, or use `field` in an accessor body.",
        ]
    );
}

/// Overriding a parent's property, a plain PHP one included, is outside the slice, with or without accessor bodies.
#[test]
fn a_property_with_accessor_bodies_that_overrides_a_parent_property_is_not_supported_yet() {
    let code = "namespace App.Store;\n\nimport Lib.Model;\n\nclass Order : Model\n{\n    public override string status { get => field; set => field = value; }\n}\n";

    assert_eq!(issues(code), ["7:28 Overriding a property is not supported yet in PHP#."]);
}

/// A property whose accessors all have bodies and never use `field` has no storage, as PHP's virtual property, so
/// neither an initial value nor a constructor can set it.
#[test]
fn an_initial_value_or_a_constructor_parameter_needs_storage() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int a { get => 1; } = 2;\n    public int b { get => field; } = 3;\n    public int c { get; set => this.save(value); } = 4;\n\n    public Report(public int d { get => 1; set => this.save(value); }, public int e { get => field; })\n    {\n    }\n\n    public void save(int value)\n    {\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:34 The property `a` has no storage for its initial value: give it an auto accessor, such as `get;`, or use `field` in an accessor body.",
            "9:30 The property `d` has no storage for the constructor to set: give it an auto accessor, such as `get;`, or use `field` in an accessor body.",
        ]
    );
}

/// A constant initial value is the property's default, but any other would run the `set` body in the constructor.
#[test]
fn an_initial_value_that_is_not_constant_on_a_property_whose_set_has_a_body_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int a { get; set => field = value; } = strlen(\"x\");\n    public int b { get => field; set; } = strlen(\"x\");\n    public int c { get; set => field = value; } = 1;\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:51 An initial value that is not constant, on a property whose `set` has a body, is not supported yet in PHP#."
        ]
    );
}

#[test]
fn a_static_property_with_an_accessor_body_is_not_supported_yet() {
    let code =
        "namespace App.Tenant;\n\nclass Report\n{\n    public static int a { get; set => field = value; } = 0;\n}\n";

    assert_eq!(issues(code), ["5:12 A static property with an accessor body is not supported yet in PHP#."]);
}

#[test]
fn a_variadic_parameter_is_in_the_slice_on_a_method_and_a_constructor() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(string label, int ...values)\n    {\n    }\n\n    public static int sum(int|float ...values)\n    {\n        return 0;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_variadic_parameter_that_is_not_last_or_has_a_default_is_an_error_as_in_php() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int run(int ...values, int extra)\n    {\n        return extra;\n    }\n\n    public int sum(int ...values = 1)\n    {\n        return 0;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:39 Invalid parameter order: parameter `extra` is defined after variadic parameter `values`.",
            "10:34 Invalid parameter definition: variadic parameter `values` cannot have a default value.",
        ]
    );
}

#[test]
fn a_void_variadic_parameter_reports_only_the_php_error() {
    let code = leak(method("        return 1;\n").replace("int extra", "void ...extra"));

    assert_eq!(issues(code), ["5:20 Invalid parameter type: bottom type `void` cannot be used as a parameter type."]);
}

#[test]
fn a_variadic_constructor_parameter_that_declares_a_member_is_an_error_as_in_php() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(private int ...values)\n    {\n    }\n}\n\nclass Total\n{\n    public Total(public int ...values { get; })\n    {\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:19 Cannot declare variadic promoted property `values`.",
            "12:18 Cannot declare variadic promoted property `values`.",
        ]
    );
}

/// Upstream Mago checks neither the member nor the type of a variadic parameter, nor a type written twice, so a `.php`
/// file keeps its results. The PHP# twins of these lines are errors.
#[test]
fn a_php_file_keeps_upstream_results_for_a_variadic_parameter_and_a_type_written_twice() {
    let code = "<?php\n\nclass Report\n{\n    public function __construct(private int ...$values)\n    {\n    }\n}\n\nfunction f(void ...$x) {}\n\nfunction g(int|int $x) {}\n";

    assert_eq!(issues_in("src/Report.php", code), Vec::<String>::new());
}

#[test]
fn a_union_type_is_in_the_slice_wherever_a_type_is() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private int|string key = 1;\n    public int|float amount { get; set; } = 0;\n\n    public Report(private bool|Calc flag, int|string start)\n    {\n    }\n\n    public int|string run(int|float|Calc extra)\n    {\n        int|string local = 1;\n        for (int|bool step = 0; ; ) {\n        }\n        return local;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_union_that_holds_null_is_not_supported_yet() {
    let code = leak(
        method("        return 1;\n").replace("int extra", "int?|string a, int|string? b, int|null c, null|Calc d"),
    );

    assert_eq!(
        issues(code),
        [
            "5:20 Type `int?` cannot be part of a union.",
            "5:39 Type `string?` cannot be part of a union.",
            "5:54 This union that holds null is not supported yet in PHP#.",
            "5:62 This union that holds null is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn void_in_a_union_reports_only_the_php_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public void|int run(int|void extra)\n    {\n        return 1;\n    }\n}\n";

    assert_eq!(
        issues(code),
        ["5:12 Type `void` cannot be part of a union.", "5:29 Type `void` cannot be part of a union."]
    );
}

#[test]
fn a_type_written_twice_in_a_union_is_an_error_as_in_php() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public int|string|int run(Calc|string|calc extra)\n    {\n        return 1;\n    }\n}\n";

    assert_eq!(issues(code), ["7:23 Duplicate type `int` is redundant.", "7:43 Duplicate type `calc` is redundant."]);
}

#[test]
fn a_nullable_union_holds_null_where_a_nullable_type_does() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    private (int|string)? key = null;\n    public (int|string)? amount { get; set; } = null;\n\n    public Report(private (bool|Calc)? flag, (int|string)? start)\n    {\n    }\n\n    public (int|string)? run((int|float|Calc)? extra)\n    {\n        (int|string)? local = null;\n        const (int|bool)? fixed = null;\n        for ((int|bool)? step = null; ; ) {\n        }\n        return local;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_union_that_holds_null_names_the_nullable_union_to_write() {
    let notes: Vec<Vec<String>> =
        check("src/Report.sharp", leak(method("        return 1;\n").replace("int extra", "int|null a")))
            .into_iter()
            .map(|issue| issue.notes)
            .collect();

    assert_eq!(
        notes,
        [["PHP# writes a union that holds null in parentheses with `?` after it, as in `(int|string)?`."]]
    );
}

#[test]
fn a_nullable_union_is_checked_as_its_union_and_never_joins_another_union() {
    let code = leak(method("        return 1;\n").replace(
        "int extra",
        "(int|string|int)? a, (int|null)? b, (int?|string)? c, (int|string)?|float d, (void|int)? e",
    ));

    assert_eq!(
        issues(code),
        [
            "5:32 Duplicate type `int` is redundant.",
            "5:46 This union that holds null is not supported yet in PHP#.",
            "5:57 Type `int?` cannot be part of a union.",
            "5:74 Type `(int|string)?` cannot be part of a union.",
            "5:98 Type `void` cannot be part of a union.",
        ]
    );
}

#[test]
fn a_single_type_in_parentheses_with_a_question_mark_after_it_reports_only_the_php_errors() {
    let code = leak(method("        return 1;\n").replace("int extra", "(Calc)? extra"));

    assert_eq!(issues(code), ["5:20 Type `(Calc)` cannot be nullable.", "5:21 Type `Calc` cannot be parenthesized."]);
}

#[test]
fn a_php_file_keeps_refusing_a_nullable_union_in_parentheses() {
    let code = "<?php\n\nfunction f(?(int|string) $x) {}\n";

    assert_eq!(issues_in("src/Report.php", code), ["3:13 Type `(int|string)` cannot be nullable."]);
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
    let code = "class Report\n{\n    public mixed run(iterable? a, int|iterable b, iterable c, callable d, (Lib&Other)|null e)\n    {\n        return 1;\n    }\n\n    public self make(Lib f, float g, bool h, string i)\n    {\n        return this;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:12 This type is not supported yet in PHP#.",
            "3:22 This type is not supported yet in PHP#.",
            "3:39 This type is not supported yet in PHP#.",
            "3:51 This type is not supported yet in PHP#.",
            "3:63 This type is not supported yet in PHP#.",
            "3:75 This type is not supported yet in PHP#.",
            "3:87 This union that holds null is not supported yet in PHP#.",
            "8:12 PHP# has no `self`: write the class's own name, `Report`, for the declaring class, or `Self` for the class a static method is called on.",
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
fn a_typed_loop_variable_takes_the_types_of_the_slice_but_not_void() {
    let code = leak(method(
        "        for (const [Status status, int n] of Store.counts()) {\n        }\n        for (const Map<string, List<int>> group of Store.groups()) {\n        }\n        for (const void step of Store.values()) {\n        }\n        for (const iterable items of Store.values()) {\n        }\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "11:20 A local cannot be `void`: `void` is only a return type.",
            "13:20 This type is not supported yet in PHP#.",
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

    assert_eq!(
        issues(code),
        ["5:9 A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value."]
    );
}

#[test]
fn an_empty_literal_needs_a_type() {
    let code = leak(method(
        "        let names = [];\n        const sizes = [:];\n        List<string> kept = [];\n        const Map<string, int> counts = [:];\n        let filled = [1];\n        names = [];\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:21 An empty literal needs a type: write `List<T> names = []`.",
            "8:23 An empty literal needs a type: write `const Map<TKey, TValue> sizes = [:]`.",
        ]
    );
}

#[test]
fn a_null_start_needs_a_type() {
    let code = leak(method(
        "        let value = null;\n        const other = null;\n        string? kept = null;\n        let either = extra > 1 ? null : 1;\n        return 1;\n",
    ));

    assert_eq!(issues(code), ["7:21 A null start needs a type: write `T? value = null`."]);
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

/// A named key type may have a backing value, which only the analyzer knows, so it passes here.
#[test]
fn a_map_key_that_is_not_int_string_or_a_named_type_is_an_error() {
    let code = "class Report\n{\n    public Map<float, int> run(Map<Line, int> a, Map<int?, int> b, Map<string, Map<bool, int>> c)\n    {\n        return [:];\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:16 A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value.",
            "3:54 A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value.",
            "3:84 A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value.",
        ]
    );
}

#[test]
fn a_literal_element_outside_the_slice_is_not_supported_yet() {
    let code = leak(method(
        "        const reference = [&extra];\n        const missing = [, extra];\n        let list = [1];\n        list[] = 2;\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:28 This operator is not supported yet in PHP#.",
            "8:26 This construct is not supported yet in PHP#.",
            "10:9 This write target is not supported yet in PHP#.",
        ]
    );
}

/// The analyzer decides whether a spread is a `List`'s or a `Map`'s, since the spread value's type does.
#[test]
fn a_spread_in_a_literal_is_in_the_slice() {
    let code = "class Report\n{\n    private List<int> all = [...Defaults.SMALL, 3, ...Defaults.LARGE];\n\n    public List<int> run(List<int> extra)\n    {\n        const all = [...extra, 1, ...this.all];\n        return all;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
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
    let code =
        leak(method("        const kind = new (Report);\n        const other = new class {};\n        return 1;\n"));

    assert_eq!(
        issues(code),
        ["7:22 This expression is not supported yet in PHP#.", "8:23 This expression is not supported yet in PHP#."]
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
fn a_positional_argument_after_a_spread_or_a_spread_after_a_named_argument_is_an_error_as_in_php() {
    let code = leak(method(
        "        const parts = this.parts();\n        this.total(...parts, 1);\n        this.total(first: 1, ...parts);\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "8:30 Cannot use positional argument after argument unpacking.",
            "9:30 Cannot use argument unpacking after a named argument.",
        ]
    );
}

#[test]
fn an_assert_whose_only_argument_is_a_spread_is_an_error_as_in_php() {
    let code = leak(method(
        "        const parts = this.parts();\n        assert(...parts);\n        ASSERT(...parts);\n        assert(true, ...parts);\n        assert(...parts, ...parts);\n        assert(...parts, description: \"parts\");\n        return 1;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "8:16 Cannot use positional argument after argument unpacking.",
            "9:16 Cannot use positional argument after argument unpacking.",
        ]
    );
}

#[test]
fn a_spread_in_attribute_arguments_reports_only_the_php_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    [Field(...PARTS)]\n    public int run(int extra)\n    {\n        return extra;\n    }\n}\n";

    assert_eq!(issues(code), ["5:12 Cannot use argument unpacking in attribute arguments."]);
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
fn is_as_and_match_with_their_patterns_are_in_the_slice() {
    let code = leak(method(
        "        if (extra is int count && count > 0) {\n            return count;\n        }\n        const small = extra is >= 1 and < 10 or 100 ? 1 : 0;\n        const report = this as Report;\n        const counted = this is { count: int total } && total > 0;\n        match (extra) {\n            0 => {\n                return 0;\n            },\n            int n when n < 0 => this.run(n),\n            default => {},\n        }\n        return match (extra) {\n            < 0 => -1,\n            not (0 or 1) => 2,\n            default => small,\n        };\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_match_without_a_default_arm_is_an_error() {
    let code =
        leak(method("        match (extra) {\n            0 => this.run(1),\n        }\n        return extra;\n"));

    assert_eq!(issues(code), ["7:9 A `match` needs a `default` arm."]);
}

#[test]
fn a_match_whose_arms_name_class_values_leaves_its_default_to_the_analyzer() {
    let code = "namespace App.Tenant;\n\nimport Lib.Status;\n\nclass Report\n{\n    public string label(Status? status)\n    {\n        match (status) {\n            Status.Open when status !== null => {},\n            Status.Closed or null => {},\n        }\n        return match (status) {\n            Status.Open => \"open\",\n            0 => \"zero\",\n        };\n    }\n}\n";

    assert_eq!(issues(code), ["13:16 A `match` needs a `default` arm."]);
}

#[test]
fn a_second_default_arm_or_a_block_arm_in_a_match_that_gives_a_value_is_an_error() {
    let code = leak(method(
        "        const a = match (extra) {\n            default => 1,\n            default => 2,\n        };\n        return match (extra) {\n            0 => { return 1; },\n            default => 2,\n        };\n",
    ));

    assert_eq!(
        issues(code),
        [
            "9:13 A `match` has one `default` arm.",
            "12:18 A block arm is only in a `match` statement: a `match` that gives a value gives an expression in each arm.",
        ]
    );
}

#[test]
fn a_nullable_type_pattern_or_as_to_a_nullable_type_is_an_error() {
    let code =
        leak(method("        const a = extra is int? n;\n        const b = extra as int?;\n        return extra;\n"));

    assert_eq!(
        issues(code),
        [
            "7:28 A type pattern is never nullable: null never matches a type.",
            "8:28 `as` converts to a type that is not nullable or `void`.",
        ]
    );
}

#[test]
fn a_nullable_type_pattern_names_the_null_check_to_write() {
    let code = leak(method("        const a = extra is int? n;\n        return extra;\n"));

    let help: Vec<_> = check("src/Report.sharp", code).into_iter().filter_map(|issue| issue.help).collect();
    assert_eq!(help, ["Test for null with `x == null`, or join both with `or`, as in `x is int or null`."]);
}

#[test]
fn a_pattern_variable_under_or_or_not_is_an_error_but_under_the_not_that_starts_is() {
    let code = leak(method(
        "        const a = extra is int x or string y;\n        const b = extra is not (int z and > 0);\n        const c = match (extra) {\n            not int w => 1,\n            default => 0,\n        };\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:32 `x` is declared under `or` or `not`, where the pattern can match without a value for it.",
            "7:44 `y` is declared under `or` or `not`, where the pattern can match without a value for it.",
            "10:21 `w` is declared under `or` or `not`, where the pattern can match without a value for it.",
        ]
    );
}

#[test]
fn a_pattern_variable_used_where_its_test_does_not_hold_names_its_test() {
    let code = leak(method(
        "        if (extra is int count) {\n        }\n        if (extra is not int other) {\n            return other;\n        }\n        const a = match (extra) {\n            int n => n,\n            default => n,\n        };\n        return count;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "10:20 `other` exists only where `extra is not int other` is false.",
            "14:24 `n` exists only where `int n` is true.",
            "16:16 `count` exists only where `extra is int count` is true.",
        ]
    );
}

#[test]
fn a_pattern_variable_named_this_is_an_error() {
    let code = leak(method("        if (extra is int this) {\n        }\n        return extra;\n"));

    assert_eq!(issues(code), ["7:26 Cannot name a pattern variable `this`: `this` is the object the method runs on."]);
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
        "        const changed = (&value) => value;\n        const marked = (readonly int value) => value;\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:26 A by-reference parameter is not supported yet in PHP#.",
            "8:25 This modifier is not supported yet in PHP#.",
        ]
    );
}

/// A lambda's parameter is a method's, so it takes a union, a nullable union and a variadic last parameter, and a
/// function type takes unions as its return and parameter types.
#[test]
fn a_lambda_and_a_function_type_take_unions_and_a_variadic_parameter() {
    let code = leak(method(
        "        const pick = (int|string key, (int|Calc)? fallback, int ...rest) => count(rest);\n        Function<(int|string)?(int|string, List<int>|Calc)> choose = (a, b) => a;\n        const sum = (int ...values) => array_sum(values);\n        return pick(1, null, ...[2, 3]) + sum(...[extra]);\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
}

/// `Self` is a method's return type only, so a function type, whose return is not a method's, refuses it anywhere,
/// while a `List` a method returns may hold it.
#[test]
fn self_in_a_function_type_is_an_error_and_a_list_of_self_is_a_return_type() {
    let code = "namespace App;\n\nclass Report\n{\n    private Function<Self()> make;\n\n    public static List<Self>|Self all(Function<int(Self)> pick) => [];\n}\n";

    assert_eq!(
        issues(code),
        ["5:22 `Self` is only a return type in PHP#.", "7:52 `Self` is only a return type in PHP#.",]
    );
}

/// An enum's method returns `Self`, which is the enum, and its constants take union types, as a class's do. An enum
/// has no constructor, so `new Self(…)` in it is an error.
#[test]
fn an_enum_returns_self_holds_union_constants_and_refuses_new_self() {
    let code = "namespace App;\n\npublic enum Status : string\n{\n    public const int|string Key = 1;\n    public const (int|string)? Code = null;\n\n    case Active = \"a\";\n\n    public static Self first() => Status.Active;\n\n    public static Self? find(string ...codes) => Status.tryFrom(codes[0] ?? \"\");\n\n    public static Self make() => new Self(\"a\");\n}\n";

    assert_eq!(issues(code), ["14:34 An enum has no constructor: its cases are its only values."]);
}

/// `List` and `Map` both run as PHP's `array`, and every function type as `\Closure`, so a union of two of either is
/// a type written twice, which the engine refuses.
#[test]
fn a_union_of_two_collections_or_two_function_types_is_a_type_written_twice() {
    let code = leak(method("        return 1;\n").replace(
        "int extra",
        "List<int>|Map<string, int> items, Function<int()>|Function<string()> make, List<int>|string fine",
    ));

    assert_eq!(
        issues(code),
        [
            "5:30 Duplicate type `Map<string, int>` is redundant.",
            "5:70 Duplicate type `Function<string()>` is redundant."
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
