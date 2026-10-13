#![allow(clippy::panic, clippy::expect_used)]

use std::borrow::Cow;
use std::path::Path;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_names::resolver::NameResolver;
use mago_php_version::PHPVersion;
use mago_reporting::Issue;
use mago_semantics::SemanticsChecker;
use mago_syntax::parser::parse_file;
use mago_syntax::token::SHARP_KEYWORDS;

#[path = "../../../tests/support/sharp_words.rs"]
mod sharp_words;

/// Every semantic issue in the source, in the dialect its path names. Panics when an issue of a `.sharp` file names a
/// word only PHP has, in its message, help, notes or annotations.
fn check(path: &'static str, code: &'static str) -> Vec<Issue> {
    let arena = LocalArena::new();
    let file = File::ephemeral(Cow::Borrowed(path.as_bytes()), Cow::Borrowed(code.as_bytes()));
    let program = parse_file(&arena, &file);
    assert!(program.errors.is_empty(), "test source did not parse: {:#?}", program.errors);

    let names = NameResolver::new(&arena).resolve(program);

    let issues: Vec<Issue> =
        SemanticsChecker::new(PHPVersion::new(8, 4, 0)).check(&file, program, &names).into_iter().collect();
    if Path::new(path).extension().is_some_and(|extension| extension == "sharp") {
        let leaks: Vec<String> = issues
            .iter()
            .filter_map(|issue| {
                let (text, word) = sharp_words::banned_word(issue)?;
                Some(format!("{path}: `{}` says {text:?}, which names {word}", issue.code.as_deref().unwrap_or("none")))
            })
            .collect();
        assert!(leaks.is_empty(), "A `.sharp` message speaks PHP#.\n{}", leaks.join("\n"));
    }

    issues
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

/// The plain PHP twin of `method`, whose body reads the parameter as `$extra`.
fn php_method(body: &str) -> String {
    format!("<?php\n\nclass Report\n{{\n    public function run(int $extra): int\n    {{\n{body}    }}\n}}\n")
}

fn leak(code: String) -> &'static str {
    Box::leak(code.into_boxed_str())
}

/// Two files that use every construct `check_slice` accepts: the slice, and the constructs only the standard library
/// declares, under the namespace `Sharp`. The engine's bridge lowers the same files.
#[test]
fn the_slice_fixtures_have_no_semantic_issues() {
    assert_eq!(issues(include_str!("fixtures/slice.sharp")), Vec::<String>::new());
    assert_eq!(issues(include_str!("fixtures/library.sharp")), Vec::<String>::new());
}

/// `extern` declares the effects of plain PHP at file level, spec section 29, and nowhere else.
#[test]
fn extern_is_a_declaration_of_the_file_and_of_no_method_body() {
    let code = "namespace App.Stubs;\n\nimport Stripe.StripeClient;\n\nextern StripeClient uses Http;\nextern Carbon.now uses Clock, Random;\nextern trim;\n\nclass Report\n{\n    public int run(int extra)\n    {\n        extern trim;\n        return extra;\n    }\n}\n";

    assert_eq!(issues(code), ["13:9 This statement is not supported yet in PHP#."]);
}

const NOT_AN_EFFECT: &str = "is not an effect: `uses` takes Database, Http, Files, Console, Process, Clock, Random, Cache, Mail, Environment or Events.";

/// Spec section 29: a function type's `uses` names effects, so a name that is no effect is refused, whether it names
/// nothing or a class.
#[test]
fn a_function_type_uses_only_effects() {
    let typo = "namespace App.Shop;\n\nclass Checkout\n{\n    Function<Charge(Cart) uses Htp>? charge = null;\n}\n";
    let class = "namespace App.Shop;\n\nimport App.Orders.Order;\n\nclass Checkout\n{\n    Function<Charge(Cart) uses Order>? charge = null;\n}\n";

    assert_eq!(issues(typo), [format!("5:32 `Htp` {NOT_AN_EFFECT}")]);
    assert_eq!(issues(class), [format!("7:32 `Order` {NOT_AN_EFFECT}")]);
}

/// An `extern`'s `uses` takes the same effects as a function type's.
#[test]
fn an_extern_uses_only_effects() {
    let typo = "namespace App.Stubs;\n\nimport Lib.Foo;\n\nextern Foo.bar uses Htp;\n";
    let class = "namespace App.Stubs;\n\nimport App.Orders.Order;\nimport Lib.Foo;\n\nextern Foo.baz uses Order;\n";

    assert_eq!(issues(typo), [format!("5:21 `Htp` {NOT_AN_EFFECT}")]);
    assert_eq!(issues(class), [format!("6:21 `Order` {NOT_AN_EFFECT}")]);
}

/// `extern event` marks a plain PHP class as an event, spec section 15, so it names a class, with or without the
/// effects its `uses` lists.
#[test]
fn an_extern_event_names_a_class() {
    let code = "namespace App.Stubs;\n\nimport Carbon.Carbon;\nimport Illuminate.Queue.Events.JobFailed;\nimport Illuminate.Queue.Events.JobQueued;\n\nextern event JobFailed;\nextern event JobQueued uses Database;\nextern event Carbon.now;\n";

    assert_eq!(issues(code), ["9:14 An `extern event` names a class, and `Carbon.now` names a member."]);
}

/// Spec section 15: an event runs as a final class whose parameters are its public get-only properties. It takes only
/// `public`, and each parameter is a type, a name and an optional default. An event is a class, so any capitalized word
/// but a reserved type name names one, as `Print` and `Exit` do, and `Self` is refused.
#[test]
fn an_event_takes_only_public_and_its_parameters_are_its_properties() {
    let code = "namespace App.Shop;\n\nprivate event Hidden(int id);\nstatic event Shared(int id);\npublic event Typed(public int id, int ...rest);\npublic event Tagged(Set<string> tags);\n[Logged] public event Noted(int id);\npublic event Tracked(int id { get; });\npublic event Maybe(int id, Set<int>? tags);\npublic event Paid(int orderId, string note = \"\") : Placed;\nevent Moved(int id);\ninterface Placed { }\npublic event Empty();\npublic event Print(int id);\npublic event Exit(int code);\npublic event Frozen(readonly int id);\npublic event Scoped(internal int id);\npublic event Self(int id);\n";
    let fixed = "namespace App.Shop;\n\npublic event Moved(int id);\n\nevent Emptied();\n";

    assert_eq!(
        issues(code),
        [
            "3:1 An event takes only `public`.",
            "4:1 An event takes only `public`.",
            "5:20 An event's parameter takes no modifier: it is a public get-only property of the event.",
            "5:39 An event's parameter cannot be variadic: it is one property of the event.",
            "6:21 A `Set` parameter of an event is not supported yet in PHP#.",
            "7:1 This construct is not supported yet in PHP#.",
            "8:29 An event's parameter takes no accessors: it is a get-only property of the event.",
            "9:28 A `Set` parameter of an event is not supported yet in PHP#.",
            "16:21 An event's parameter takes no modifier: it is a public get-only property of the event.",
            "17:21 An event's parameter takes no modifier: it is a public get-only property of the event.",
            "18:14 Cannot use `Self` as a class name: it is reserved."
        ]
    );
    assert_eq!(issues(fixed), Vec::<String>::new());
    let construct_notes: Vec<Vec<String>> = check("src/Report.sharp", code)
        .into_iter()
        .filter(|issue| issue.message == "This construct is not supported yet in PHP#.")
        .map(|issue| issue.notes)
        .collect();
    assert_eq!(
        construct_notes,
        [[
            "A PHP# event has an optional `public`, a name, parameters, each a type and a name with an optional default, and an optional `: Interface` header, as in `public event OrderPaid(int orderId) : OrderEvent;`."
        ]]
    );
}

/// An import of a class named like an effect shadows the effect, spec section 23, so the refusal names the imported
/// class and the import that frees the name. A class the file declares under that name keeps the plain refusal.
#[test]
fn an_effect_name_an_import_shadows_is_refused_with_the_imported_class() {
    let extern_ = "namespace App.Stubs;\n\nimport Lib.Clock;\n\nextern Clock.now uses Clock;\n";
    let function_type = "namespace App.Shop;\n\nimport Lib.Http;\n\nclass Checkout\n{\n    Function<Charge(Cart) uses Http>? charge = null;\n}\n";
    let declared = "namespace App.Shop;\n\nclass Clock\n{\n    Function<int() uses Clock>? tick = null;\n}\n";

    assert_eq!(
        issues(extern_),
        [
            "5:23 `Clock` here is the imported `Lib.Clock`, not the effect `Clock`. Import it under another name, as in `import Lib.Clock as SystemClock;`."
        ]
    );
    assert_eq!(
        issues(function_type),
        [
            "7:32 `Http` here is the imported `Lib.Http`, not the effect `Http`. Import it under another name, as in `import Lib.Http as SystemHttp;`."
        ]
    );
    assert_eq!(issues(declared), [format!("5:25 `Clock` {NOT_AN_EFFECT}")]);
}

/// Every standard effect passes a `uses`, `Events` too, written bare or through an import of its class.
#[test]
fn a_uses_takes_every_standard_effect_bare_or_imported() {
    let function_type = "namespace App.Shop;\n\nclass Checkout\n{\n    Function<Charge(Cart) uses Http, Mail, Events>? charge = null;\n    Function<int() uses Database, Files, Console, Process, Clock, Random, Cache, Environment>? all = null;\n}\n";
    let extern_ = "namespace App.Stubs;\n\nimport Lib.Foo;\n\nextern Foo.bar uses Http, Events;\n";
    let imported = "namespace App.Stubs;\n\nimport Lib.Foo;\nimport Sharp.Http;\n\nextern Foo.bar uses Http;\n";

    assert_eq!(issues(function_type), Vec::<String>::new());
    assert_eq!(issues(extern_), Vec::<String>::new());
    assert_eq!(issues(imported), Vec::<String>::new());
}

/// Spec section 28: a law states a fact about the values of a class or an enum, and an interface has no values.
#[test]
fn a_law_is_a_member_of_a_class_or_an_enum_and_of_no_interface() {
    let code = "namespace App.Shared;\n\npublic class Money\n{\n    law sameAmount(int a) => a == a;\n}\n\npublic enum Status\n{\n    case Open;\n\n    law openIsOpen(Status s) => Status.Open == Status.Open;\n}\n\npublic interface Priced\n{\n    law positive(int a) => a >= 0;\n}\n";

    assert_eq!(issues(code), ["17:5 This class member is not supported yet in PHP#."]);
}

/// A law's parameters range over every value, so none has a default and none collects the rest of the arguments, in a
/// class and in a module alike.
#[test]
fn a_law_parameter_with_a_default_or_a_spread_is_refused() {
    let class = "namespace App.Shared;\n\npublic class Money\n{\n    law withDefault(int a = 1) => a == a;\n    law withRest(int ...rest) => true;\n}\n";
    let module = "namespace App.Shared;\n\nmodule\n{\n    law withDefault(int a = 1) => a == a;\n    law withRest(int ...rest) => true;\n}\n";

    for (path, code) in [("src/Report.sharp", class), ("app/Shared/Module.sharp", module)] {
        assert_eq!(
            issues_in(path, code),
            [
                "5:27 A law's parameters range over every value, so `a` cannot have a default.",
                "6:22 A law's parameters range over every value, so `rest` cannot be variadic.",
            ],
            "{path}"
        );
        assert_eq!(
            check(path, code).into_iter().map(|issue| issue.notes).collect::<Vec<_>>(),
            [
                ["A law states a fact about every value of its parameters."],
                ["A law states a fact about every value of its parameters."],
            ],
            "{path}"
        );
    }
}

/// Spec section 32: a namespace's module is declared in its folder's `Module.sharp`. An environment module, in
/// `Module.<Environment>.sharp`, waits for the container.
#[test]
fn a_module_is_declared_only_in_its_folders_module_sharp() {
    let code = "namespace App.Shop;\n\nmodule\n{\n    law same(int a) => a == a;\n}\n";

    for (path, expected) in [
        ("app/Shop/Module.sharp", None),
        ("app/Shop/Checkout.sharp", Some("3:1 A module is declared in its folder's Module.sharp.")),
        ("app/Shop/Module.Production.sharp", Some("3:1 An environment module is not supported yet in PHP#.")),
    ] {
        assert_eq!(issues_in(path, code), Vec::from_iter(expected), "{path}");
    }
}

/// Spec section 32: `Module.sharp` holds its namespace, its imports and one module, and nothing else.
#[test]
fn module_sharp_holds_only_its_folders_module() {
    let module = "module\n{\n    law same(int a) => a == a;\n}\n";

    for declaration in [
        "public class Module\n{\n}\n",
        "public interface Priced\n{\n}\n",
        "public enum Status\n{\n    case Open;\n}\n",
        "extern trim;\n",
        "module\n{\n    law other(int a) => a == a;\n}\n",
    ] {
        let code = leak(format!("namespace App.Shop;\n\nimport App.Mail.Mailer;\n\n{module}\n{declaration}"));

        assert_eq!(
            issues_in("app/Shop/Module.sharp", code),
            ["10:1 Module.sharp holds only its folder's module."],
            "{declaration}"
        );
    }
}

/// A module's laws are named by the module and their own names, compared ignoring case, so each name is stated once.
#[test]
fn a_module_states_each_law_once() {
    let code = "namespace App.Shop;\n\nmodule\n{\n    law same(int a) => a == a;\n    law Same(int b) => b == b;\n}\n";

    assert_eq!(issues_in("app/Shop/Module.sharp", code), ["6:9 The module already states a law named `same`."]);
}

/// A law has no `this`, so a bare member in it is written through the class name, as in a static method.
#[test]
fn a_bare_member_in_a_law_is_written_through_the_class_name() {
    let code = "namespace App.Shared;\n\npublic class Money\n{\n    private int cents = 0;\n\n    law positive(Money a) => cents > 0;\n}\n";

    assert_eq!(
        issues(code),
        ["7:30 Write `Money.cents`: a static method reaches the members of its class through the class name."]
    );
}

/// A law and a method share their class's member names, so a name is declared once.
#[test]
fn a_law_and_a_method_named_alike_are_a_duplicate_member() {
    let code = "namespace App.Shared;\n\npublic class Money\n{\n    public bool positive(int a) => a > 0;\n\n    law positive(int a) => a > 0 || a <= 0;\n}\n";

    assert_eq!(issues(code), ["7:9 class method `Money.positive` has already been defined"]);
}

#[test]
fn every_construct_outside_the_slice_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\ninterface Shape\n{\n}\n\ntrait Named\n{\n}\n\nclass Report\n{\n    public int run(int extra)\n    {\n        switch (extra) {\n            default: return 1;\n        }\n        const made = new Report;\n        const partial = this.run(...);\n        const text = <<<TEXT\ntotal\nTEXT;\n        return extra;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "7:1 This statement is not supported yet in PHP#.",
            "15:9 This statement is not supported yet in PHP#.",
            "18:22 This `new` without arguments is not supported yet in PHP#.",
            "19:25 This expression is not supported yet in PHP#.",
            "20:22 This expression is not supported yet in PHP#.",
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

/// The body of `if`, `else` and each loop is a block in braces, a guard clause's `return` or `throw` too, and PHP's
/// colon-delimited bodies are never valid. The error points at the body, from a colon body's `:` to its `endif;`,
/// `endwhile;` or `endfor;`, and its help's braced body compiles in its place.
#[test]
fn every_body_is_written_in_braces() {
    let refused = [
        ("        if (extra > 1) return 1;\n", "7:24", "{ return 1; }"),
        ("        if (extra > 2) throw new Failure(extra);\n", "7:24", "{ throw new Failure(extra); }"),
        ("        if (extra > 1) return 1; else {\n            return 2;\n        }\n", "7:24", "{ return 1; }"),
        ("        if (extra > 1) {\n            return 1;\n        } else return 2;\n", "9:16", "{ return 2; }"),
        (
            "        if (extra > 1) {\n            return 1;\n        } else if (extra > 2) return 2;\n",
            "9:31",
            "{ return 2; }",
        ),
        ("        if (extra > 1) extra = 1;\n", "7:24", "{ extra = 1; }"),
        ("        if (extra > 1) this.run(extra);\n", "7:24", "{ this.run(extra); }"),
        ("        if (extra > 10);\n", "7:24", "{ }"),
        ("        while (extra > 1) return 1;\n", "7:27", "{ return 1; }"),
        ("        while (extra > 1) {\n            if (extra > 2) break;\n        }\n", "8:28", "{ break; }"),
        ("        while (extra > 1) {\n            if (extra > 2) continue;\n        }\n", "8:28", "{ continue; }"),
        ("        do extra += 1; while (extra < 3);\n", "7:12", "{ extra += 1; }"),
        ("        for (let step = 0; step < 3; step++) extra++;\n", "7:46", "{ extra++; }"),
        ("        for (const value of Store.values()) extra += value;\n", "7:45", "{ extra += value; }"),
        ("        while (extra > 1):\n            extra -= 1;\n        endwhile;\n", "7:26", "{ extra -= 1; }"),
        ("        for (;;):\n            break;\n        endfor;\n", "7:17", "{ break; }"),
        (
            "        if (extra > 3):\n            extra = 3;\n        elseif (extra > 2):\n            extra = 2;\n        else:\n            extra = 1;\n        endif;\n",
            "7:23",
            "{ extra = 3; } else if (extra > 2) { extra = 2; } else { extra = 1; }",
        ),
        (
            "        if (extra > 1) {\n            return 1;\n        } else if (extra > 2):\n            return 2;\n        endif;\n",
            "9:30",
            "{ return 2; }",
        ),
    ];

    for (body, at, fix) in refused {
        let code = leak(method(&format!("{body}        return extra;\n")));
        let issue = check("src/Report.sharp", code).remove(0);
        let span = issue.primary_span().expect("a primary span");
        let fixed = leak(format!("{}{fix}{}", &code[..span.start.offset as usize], &code[span.end.offset as usize..]));

        assert_eq!(
            issues(code),
            [format!("{at} PHP# writes the body of `if`, `else` and each loop in braces.")],
            "{body}"
        );
        assert_eq!(issue.help, Some(format!("Write `{fix}`.")), "{body}");
        assert_eq!(issues(fixed), Vec::<String>::new(), "{fixed}");
    }
}

#[test]
fn elseif_is_not_supported_yet() {
    let code = leak(method(
        "        if (extra > 1) {\n            return 1;\n        } elseif (extra > 2) {\n            return 2;\n        }\n        return extra;\n",
    ));

    assert_eq!(issues(code), ["9:11 This `elseif` is not supported yet in PHP#."]);
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

/// A function declared in a method is one PHP declares globally when the method runs, so it is the same error, once.
#[test]
fn a_top_level_function_is_an_error() {
    let message = "PHP# has no top-level functions: move `total` into a class as a static method.";
    for (code, at) in [
        ("namespace App.Tenant;\n\nfunction total(int extra) { return extra; }\n", "3:10"),
        ("namespace App.Tenant;\n\nFUNCTION total(int extra) { return extra; }\n", "3:10"),
        (leak(method("        function total() { }\n        return extra;\n")), "7:18"),
        (leak(method("        FUNCTION total() { }\n        return extra;\n")), "7:18"),
    ] {
        assert_eq!(issues(code), [format!("{at} {message}")], "{code}");
    }
}

/// Spec section 27: a `.sharp` file declares one type, so a statement at file level is never valid, whatever it is.
#[test]
fn a_statement_at_file_level_is_an_error() {
    for statement in [
        "let total = 1;",
        "strlen(\"x\");",
        "die;",
        "unset(here);",
        "isset(here);",
        "if (true) { }",
        "while (false) { }",
        "do { } while (false);",
        "for (const n of [1]) { }",
        "for (;;) { }",
        "try { } catch (Exception e) { }",
        "match (1) { _ => { } }",
        "break;",
        "continue;",
        "switch (1) { }",
        "global $g;",
        "static $s = 1;",
        "return;",
        "{ }",
        "echo 1;",
        "emit new OrderPaid(1);",
        ";",
    ] {
        let code = leak(format!("namespace App.Tenant;\n\n{statement}\n\nclass Report\n{{\n}}\n"));

        assert_eq!(issues(code), ["3:1 A statement cannot sit at file level in PHP#."], "{statement}");
    }

    let jump = "namespace App.Tenant;\n\ngoto end;\nend:\n\nclass Report\n{\n}\n";

    assert_eq!(
        issues(jump),
        ["3:1 A statement cannot sit at file level in PHP#.", "4:1 A statement cannot sit at file level in PHP#."]
    );
}

/// Every `.sharp` file runs with `strict_types=1`, so PHP's `declare` line is one error wherever it is written, and the
/// file compiles without it.
#[test]
fn declare_strict_types_is_one_error_wherever_it_is_written() {
    let message = "PHP# is always strict: remove `declare(strict_types=1);`.";
    let before = "declare(strict_types=1);\n\nnamespace App.Tenant;\n\nclass Report\n{\n}\n";
    let after = "namespace App.Tenant;\n\ndeclare(strict_types=1);\n\nclass Report\n{\n}\n";
    let inside = leak(method("        declare(strict_types=1);\n        return extra;\n"));
    let after_a_class = "class Report\n{\n}\n\ndeclare(strict_types=1);\n";
    let removed = leak(method("        return extra;\n"));

    assert_eq!(issues(before), [format!("1:1 {message}")]);
    assert_eq!(issues(after), [format!("3:1 {message}")]);
    assert_eq!(issues(inside), [format!("7:9 {message}")]);
    assert_eq!(issues(after_a_class), [format!("5:1 {message}")]);
    assert_eq!(issues(removed), Vec::<String>::new());
    // PHP keeps its own two checks of `declare`.
    assert_eq!(
        issues_in("src/Report.php", "<?php\n\necho 1;\n\ndeclare(strict_types=2);\n"),
        [
            "5:1 Strict type declaration must be the first statement in the file.",
            "5:22 The `strict_types` directive must be set to either `0` or `1`."
        ]
    );
}

/// PHP#'s `true`, `false` and `null` are lowercase, so a value written in another case is one error that names it.
#[test]
fn true_false_or_null_written_in_another_case_is_one_error() {
    for (value, literal) in [("TRUE", "true"), ("False", "false"), ("NULL", "null"), ("Null", "null")] {
        let code = leak(method(&format!("        let x = {value};\n        return extra;\n")));

        assert_eq!(issues(code), [format!("7:17 Write `{literal}` in lowercase.")], "{code}");
    }
}

/// PHP reserves `X::class` for a class's name, so a constant named `class` in any case fails when PHP loads the file.
#[test]
fn a_constant_named_class_is_an_error() {
    for (code, at, name, class) in [
        ("class Report\n{\n    public const int class = 1;\n}\n", "3:22", "class", "Report"),
        ("class Report\n{\n    public const int CLASS = 1;\n}\n", "3:22", "CLASS", "Report"),
        ("enum Suit\n{\n    case Hearts;\n\n    public const int class = 1;\n}\n", "5:22", "class", "Suit"),
    ] {
        assert_eq!(
            issues(code),
            [format!("{at} `{name}` can't name a constant: `{class}.class` already names the class.")],
            "{code}"
        );
    }
}

/// PHP's other `declare` settings, `ticks` and `encoding`, set what PHP# leaves at PHP's defaults, so each is one error
/// that names it, and the help removes it or the `declare` around its body.
#[test]
fn declare_of_another_setting_is_one_error_that_names_it() {
    for (body, head, help) in [
        ("        declare(ticks=1);\n", "declare(ticks=1)", "Delete it."),
        ("        declare(encoding=\"UTF-8\");\n", "declare(encoding=\"UTF-8\")", "Delete it."),
        (
            "        declare(ticks=1) { extra = 2; }\n",
            "declare(ticks=1)",
            "Write its body's code without the `declare` around it.",
        ),
        ("        declare(strict_types=1, ticks=1);\n", "declare(strict_types=1, ticks=1)", "Delete it."),
    ] {
        let code = leak(method(&format!("{body}        return extra;\n")));
        let issues: Vec<(String, Option<String>)> =
            check("src/Report.sharp", code).into_iter().map(|issue| (issue.message, issue.help)).collect();

        assert_eq!(
            issues,
            [(
                format!("PHP# has no `{head}`: a `.sharp` file runs with PHP's default settings."),
                Some(help.to_owned())
            )],
            "{code}"
        );
    }
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
        "        try {\n        } catch (Missing failure) {\n            extra = extra <> 1;\n        }\n        return count(this.run(...));\n",
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

/// Spec section 8 removes `echo` and `print`: output goes through `printf` or `fwrite`. Spec section 2 makes `echo` a
/// plain name, so `echo` written as a call is a call of a function no code declares, refused the same way.
#[test]
fn echo_is_an_error_that_names_printf_and_fwrite() {
    let code = leak(method("        echo \"total\", extra;\n        echo(\"total\");\n        return extra;\n"));
    let message = "PHP# has no `echo`: write `printf` or `fwrite`.";

    assert_eq!(issues(code), [format!("7:9 {message}"), format!("8:9 {message}")]);
}

#[test]
fn print_is_an_error_that_names_printf_and_fwrite() {
    let code =
        leak(method("        const shown = print \"total\";\n        print(\"total\");\n        return extra;\n"));
    let message = "PHP# has no `print`: write `printf` or `fwrite`.";

    assert_eq!(issues(code), [format!("7:23 {message}"), format!("8:9 {message}")]);
}

/// Spec section 2 makes `isset`, `empty`, `unset`, `array` and `list` plain names, so each written as PHP calls it is a
/// call of a function no code declares, refused with the PHP# form. A local of the name is called as any local that
/// holds a lambda is.
#[test]
fn isset_empty_unset_array_and_list_written_as_calls_name_the_php_sharp_form() {
    let code = leak(method(
        "        const set = isset(extra);\n        const blank = empty(extra);\n        unset(extra);\n        const items = array(1, 2);\n        list(first) = items;\n        return extra;\n",
    ));
    let local = leak(method("        const isset = (int value) => value > 0;\n        return isset(extra) ? 1 : 0;\n"));

    assert_eq!(
        issues(code),
        [
            "7:21 PHP# has no `isset`: test it against null, as in `value != null`.",
            "8:23 PHP# has no `empty`: compare the value itself, as in `items.count() == 0`.",
            "9:9 PHP# has no `unset`: a local ends at the `}` that closes its block, and `map.remove(key)` removes a Map's entry.",
            "10:23 PHP# has no `array`: write a list literal, as in `[1, 2]`.",
            "11:9 PHP# has no `list`: read each element into its own local, as in `const first = pair[0];`.",
        ]
    );
    assert_eq!(issues(local), Vec::<String>::new());
}

/// Spec section 2: a PHP keyword PHP# doesn't use is a plain name, and a keyword PHP# uses stays reserved, as the
/// spec's `keyword-names` example writes them.
#[test]
fn the_keyword_names_example_reserves_only_the_keywords_php_sharp_uses() {
    let code = leak(method(
        "        const list = [1, 2, 3];\n        let empty = true;\n        const print = \"draft\";\n        const match = 1;\n        return extra;\n",
    ));

    assert_eq!(issues(code), ["10:15 `match` is a PHP# keyword, so it can't name a local."]);
}

/// Spec section 2, after C#'s keywords: every word that starts or structures PHP# syntax is reserved, so it can't name
/// a `const` or `let` local, a parameter, a catch variable or a pattern variable. `this` keeps its own message. The
/// name the help suggests compiles.
#[test]
fn every_keyword_php_sharp_uses_is_refused_as_a_local_or_parameter_name() {
    const RESERVED: [&str; 52] = [
        "let",
        "const",
        "emit",
        "match",
        "new",
        "return",
        "throw",
        "if",
        "else",
        "for",
        "while",
        "do",
        "break",
        "continue",
        "try",
        "catch",
        "finally",
        "yield",
        "exit",
        "typeof",
        "is",
        "as",
        "import",
        "namespace",
        "class",
        "interface",
        "enum",
        "case",
        "event",
        "extern",
        "foreign",
        "module",
        "struct",
        "static",
        "public",
        "protected",
        "private",
        "internal",
        "abstract",
        "final",
        "sealed",
        "override",
        "virtual",
        "required",
        "readonly",
        "lazy",
        "true",
        "false",
        "null",
        "this",
        "super",
        "Self",
    ];

    for word in RESERVED {
        let renamed = format!("{}Value", word.to_lowercase());
        let rows = [
            (leak(method(&format!("        const {word} = 1;\n        return extra;\n"))), "7:15", "local"),
            (leak(method(&format!("        let {word} = 1;\n        return extra;\n"))), "7:13", "local"),
            (
                leak(format!("namespace App.Tenant;\n\nclass Report\n{{\n    public int run(int {word}) => 1;\n}}\n")),
                "5:24",
                "parameter",
            ),
            (
                leak(method(&format!(
                    "        try {{ return 1; }} catch (Exception {word}) {{ return 2; }}\n        return extra;\n"
                ))),
                "7:44",
                "catch variable",
            ),
            (
                leak(method(&format!("        if (extra is int {word}) {{ return 1; }}\n        return extra;\n"))),
                "7:26",
                "pattern variable",
            ),
        ];

        for (code, at, kind) in rows {
            let (message, help) = if word == "this" {
                (format!("Cannot name a {kind} `this`: `this` is the object the method runs on."), None)
            } else {
                (
                    format!("`{word}` is a PHP# keyword, so it can't name a {kind}."),
                    Some(format!("Rename it, as in `{renamed}`.")),
                )
            };
            let refused: Vec<(String, Option<String>)> =
                check("src/Report.sharp", code).into_iter().map(|issue| (issue.message, issue.help)).collect();

            assert_eq!(refused, [(message, help)], "{code}");
            assert_eq!(issues(code)[0].split_once(' ').map(|(location, _)| location), Some(at), "{code}");
        }

        let fixed = leak(method(&format!("        const {renamed} = 1;\n        return {renamed};\n")));
        assert_eq!(issues(fixed), Vec::<String>::new(), "{fixed}");
    }
}

/// `die("…")` prints its message and exits with status 0, which reports success, so spec section 8 removes `die`.
#[test]
fn die_in_every_form_is_an_error_that_names_stderr_and_exit() {
    let code = leak(method(
        "        die;\n        die();\n        die(\"failed\");\n        die(1);\n        return extra;\n",
    ));
    let message = "PHP# has no `die`: write the message to STDERR, then `exit(1)`.";

    assert_eq!(
        issues(code),
        [format!("7:9 {message}"), format!("8:9 {message}"), format!("9:9 {message}"), format!("10:9 {message}")]
    );
}

/// `exit("…")` prints its message and exits with status 0, as `die("…")` does. The engine runs a `.sharp` file
/// without the analyzer, which refuses an argument that is not an `int`, so `check_slice` refuses a string literal or
/// a template, in parentheses or not.
#[test]
fn exit_with_a_string_literal_is_an_error_as_die_is() {
    let code = leak(method(
        "        exit(\"failed\");\n        exit(status: \"failed\");\n        exit(`failed`);\n        exit((\"failed\"));\n        exit(((\"failed\")));\n        return extra;\n",
    ));
    let message = "PHP# has no `die`: write the message to STDERR, then `exit(1)`.";

    assert_eq!(
        issues(code),
        [
            format!("7:9 {message}"),
            format!("8:9 {message}"),
            format!("9:9 {message}"),
            format!("10:9 {message}"),
            format!("11:9 {message}"),
        ]
    );
}

#[test]
fn exit_without_parentheses_is_an_error_that_names_the_call() {
    let code = leak(method("        exit;\n        return extra;\n"));

    assert_eq!(issues(code), ["7:9 PHP# calls `exit` as a function: write `exit(0)`."]);
}

/// Spec section 8 keeps `exit(code)` as PHP 8.4's built-in function.
#[test]
fn exit_with_a_code_or_without_arguments_is_in_the_slice() {
    let code = leak(
        method("        exit(1);\n        exit(code);\n        exit(status: code);\n        exit();\n        return code;\n")
            .replace("int extra", "int code"),
    );

    assert_eq!(issues(code), Vec::<String>::new());
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

/// Spec section 9.1: the main constructor passes control to its base class with `: super(…)`, whose arguments are a
/// method body's expressions without `this`. Only a named constructor calls `: this(…)`.
#[test]
fn the_main_constructor_passes_control_with_super_and_its_arguments_cannot_use_this() {
    let code = "namespace App.Tenant;\n\npublic class Order : Entity\n{\n    public Order(Row row, int count) : super(row, count + 1) { }\n}\n\npublic class Copy : Entity\n{\n    public Copy(Row row) : this(row) { }\n}\n\npublic class Early : Entity\n{\n    public Early(Row row) : super(this, this.row) { }\n}\n\npublic class Late : Entity\n{\n    public Late(Row row) : super(super.row) { }\n}\n\npublic class Lambda : Entity\n{\n    public Lambda(Row row) : super(Rows.call(() => this.row), Rows.call(() => { return this.row; })) { }\n}\n\npublic class Named : Entity\n{\n    public Named(Row row) : super(row: this.row) { }\n}\n";

    assert_eq!(
        issues(code),
        [
            "10:28 The main constructor passes control to its base class with `: super(…)`. Only a named constructor calls `: this(…)`.",
            "15:35 `this` is not available before the constructor runs: pass a parameter instead.",
            "15:41 `this` is not available before the constructor runs: pass a parameter instead.",
            "20:34 This expression is not supported yet in PHP#.",
            "25:52 `this` is not available before the constructor runs: pass a parameter instead.",
            "25:88 `this` is not available before the constructor runs: pass a parameter instead.",
            "30:40 `this` is not available before the constructor runs: pass a parameter instead.",
        ]
    );
}

/// Spec section 9.1: a named constructor passes control to the main constructor with `: this(…)`, never `: super(…)`,
/// and the arguments of `: this(…)` cannot use `this`, which does not exist before the main constructor runs.
#[test]
fn a_named_constructor_passes_control_with_this_and_its_arguments_cannot_use_this() {
    let code = "namespace App.Tenant;\n\npublic class Order\n{\n    public Order(int count) { }\n\n    public Order.single() : this(1) { }\n    public Order.none() { }\n    public Order.base() : super() { }\n    public Order.early() : this(this.count) { }\n}\n";

    assert_eq!(
        issues(code),
        [
            "8:18 A named constructor passes control to the main constructor with `: this(…)`.",
            "9:27 A named constructor passes control to the main constructor with `: this(…)`.",
            "10:33 `this` is not available before the constructor runs: pass a parameter instead.",
        ]
    );
}

/// Spec section 9.1: a named constructor is declared on its own class's name with an access modifier and nothing else,
/// a name that is neither its class's nor one PHP reserves, parameters that declare no property, and a body. An
/// abstract class has no objects of its own, and named constructors are not inherited, so it declares none.
#[test]
fn a_named_constructor_is_declared_on_its_class_with_an_access_modifier_a_name_and_a_body() {
    let code = "namespace App.Tenant;\n\npublic class Order\n{\n    public Order(int count) { }\n\n    public Other.copy() : this(1) { }\n    public Order.order() : this(1) { }\n    public Order.__make() : this(1) { }\n    public static Order.fixed() : this(1) { }\n    Order.bare() : this(1) { }\n    public Order.kept(public int count { get; }) : this(count) { }\n    public Order.empty() : this(1);\n}\n\npublic abstract class Shape\n{\n    public Shape(int sides) { }\n\n    public Shape.triangle() : this(3) { }\n}\n";

    assert_eq!(
        issues(code),
        [
            "7:12 A named constructor is declared on its own class, as in `Order.copy`.",
            "8:18 A named constructor cannot have its class's name: `new Order(…)` calls the main constructor.",
            "9:18 A named constructor's name cannot start with `__`: PHP reserves those names for its magic methods.",
            "10:12 A named constructor takes only `public`, `protected` or `private`: remove `static`.",
            "11:11 A named constructor without `public`, `protected` or `private` is not supported yet in PHP#.",
            "12:23 A named constructor's parameter declares no property: only the main constructor's parameters do.",
            "13:35 A named constructor needs a body, even an empty `{ }`.",
            "20:18 An abstract class has no objects of its own, so it declares no named constructor.",
        ]
    );
}

/// Spec section 9.1: a named constructor runs as the static method of its name, so it shares no name with a method of
/// its class, static or not. The clash has the code and the words the analyzer reports it with across a class and its
/// parent. A law and a named constructor of one name are a duplicate member, as a law and a method are.
#[test]
fn a_named_constructor_shares_no_name_with_a_method() {
    let after = "namespace App.Tenant;\n\npublic class Order\n{\n    public Order(int count) { }\n\n    public Order.copy() : this(1) { }\n    public int copy() => 1;\n}\n";
    let before = "namespace App.Tenant;\n\npublic class Order\n{\n    public Order(int count) { }\n\n    public int Copy() => 1;\n    public Order.copy() : this(1) { }\n}\n";
    let static_method = "namespace App.Tenant;\n\npublic class Clash\n{\n    public Clash() { }\n\n    public Clash.of() : this() { }\n    public static Clash of() => new Clash();\n}\n";
    let law = "namespace App.Tenant;\n\npublic class Order\n{\n    public Order(int count) { }\n\n    law copy(int a) => a == a;\n    public Order.copy() : this(1) { }\n}\n";
    let coded = |code: &'static str| -> Vec<(String, Option<String>)> {
        check("src/Report.sharp", code).into_iter().map(|issue| (issue.message, issue.code)).collect()
    };
    let clash = |message: &str| vec![(message.to_owned(), Some("named-constructor-clash".to_owned()))];

    assert_eq!(coded(after), clash("`Order.copy` cannot share its name with `Order`'s named constructor `copy`."));
    assert_eq!(
        coded(before),
        clash("The named constructor `Order.copy` cannot share its name with the method `Order.Copy`.")
    );
    assert_eq!(coded(static_method), clash("`Clash.of` cannot share its name with `Clash`'s named constructor `of`."));
    assert_eq!(issues(after), ["8:16 `Order.copy` cannot share its name with `Order`'s named constructor `copy`."]);
    assert_eq!(
        issues(before),
        ["8:18 The named constructor `Order.copy` cannot share its name with the method `Order.Copy`."]
    );
    assert_eq!(issues(law), ["8:18 class method `Order.copy` has already been defined"]);
}

/// Spec section 9.1: only a class has constructors, so an interface, an enum and a static class declare no named one.
#[test]
fn only_a_class_with_instances_declares_a_named_constructor() {
    let interface = "namespace App.Tenant;\n\npublic interface Priced\n{\n    Priced.free() : this() { }\n}\n";
    let r#enum = "namespace App.Tenant;\n\nenum Suit\n{\n    case Hearts;\n\n    public Suit.red() : this() { }\n}\n";
    let r#static = "namespace App.Tenant;\n\npublic static class Text\n{\n    public Text.empty() : this() { }\n}\n";

    assert_eq!(
        issues(interface),
        ["5:12 An interface has no constructor: a named constructor creates an object of its own class."]
    );
    assert_eq!(issues(r#enum), ["7:17 An enum has no constructor: its cases are its only values."]);
    assert_eq!(issues(r#static), ["5:17 A static class has no constructor."]);
}

/// Spec section 9.1: a constructor runs once, when `new` creates the object, so no code calls a member named
/// `__construct`. The base class's arguments go in `: super(…)`.
#[test]
fn a_constructor_runs_once_so_no_code_calls_it_as_a_member() {
    let code = "namespace App.Tenant;\n\npublic class Base\n{\n    public Base(public int id { get; }) { }\n}\n\npublic class Once : Base\n{\n    public Once(int id) : super(id) { }\n}\n\npublic class Twice : Base\n{\n    public Twice(int id) : super(id)\n    {\n        super.__construct(id + 1);\n    }\n}\n\npublic class Again : Base\n{\n    public Again(int id) : super(id) { }\n\n    public void reset(Base other, Base? maybe)\n    {\n        super.__construct(2);\n        this.__construct(3);\n        other.__construct(4);\n        maybe?.__CONSTRUCT(5);\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "17:15 A constructor runs once, when `new` creates the object: pass the base class's arguments with `: super(…)`.",
            "27:15 A constructor runs once, when `new` creates the object: pass the base class's arguments with `: super(…)`.",
            "28:14 A constructor runs once, when `new` creates the object: pass the base class's arguments with `: super(…)`.",
            "29:15 A constructor runs once, when `new` creates the object: pass the base class's arguments with `: super(…)`.",
            "30:16 A constructor runs once, when `new` creates the object: pass the base class's arguments with `: super(…)`.",
        ]
    );
}

/// Spec section 25's example: `Self` is the class a static method is called on, and `new Self(…)` needs a `required`
/// constructor, which a subclass's constructor calls with `: super(…)`.
#[test]
fn self_is_a_return_type_creates_with_a_required_constructor_and_calls_static_methods() {
    let code = "namespace App.Tenant;\n\nimport Lib.Row;\nimport Lib.Clock;\n\npublic abstract class DatabaseEntity\n{\n    public required DatabaseEntity(Row row)\n    {\n    }\n\n    public static Self fromSchema(Row row)\n    {\n        return new Self(row);\n    }\n\n    public static Self? find(Row row) => Self.fromSchema(row);\n\n    public static Self|int counted(Row row) => Self.fromSchema(row);\n\n    public static (Self|int)? either(Row row) => null;\n}\n\npublic class Order : DatabaseEntity\n{\n    public Order(Row row, Clock? clock = null) : super(row)\n    {\n    }\n}\n\npublic interface Copyable\n{\n    Self copy();\n}\n";

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

/// Spec section 9.1: a named constructor belongs to the class that declares it and is not inherited, so `Self`, which
/// can be any subclass, cannot name one. The help names the class, and that fix compiles.
#[test]
fn new_self_cannot_name_a_named_constructor() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(int count)\n    {\n    }\n\n    public Report.of(int count) : this(count)\n    {\n    }\n\n    public static Report make() => new Self.of(1);\n}\n";
    let fixed = code.replace("new Self.of(1)", "new Report.of(1)");

    let issue = check("src/Report.sharp", code).remove(0);
    assert_eq!(
        issue.message,
        "`Self` cannot name a named constructor: a named constructor is not inherited, and `Self` can be any subclass."
    );
    assert_eq!(issue.help.as_deref(), Some("Name its class: write `new Report.of(1)`."));
    assert_eq!(
        issues(code),
        [
            "13:36 `Self` cannot name a named constructor: a named constructor is not inherited, and `Self` can be any subclass."
        ]
    );
    assert_eq!(issues(leak(fixed)), Vec::<String>::new());

    // A class that inherits `of` shows no declaring class here, so the analyzer reports it with that class's name.
    let inherited = "namespace App.Tenant;\n\nclass Summary : Report\n{\n    public Summary(int count) : super(count)\n    {\n    }\n\n    public static Report make() => new Self.of(1);\n}\n";
    assert_eq!(issues(inherited), Vec::<String>::new());
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
            format!("16:25 {without_self}"),
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
            format!("15:27 {message}"),
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

/// An interface member is public, written bare, so an access modifier on a property or a method is one error. A
/// `.php` interface keeps PHP's own checks.
#[test]
fn an_interface_member_is_public_written_bare() {
    let bare =
        "namespace App.Tenant;\n\ninterface Named\n{\n    string name { get; }\n\n    string label(int digits);\n}\n";
    assert_eq!(issues(bare), Vec::<String>::new());

    let message = "An interface member is always public, so it takes no access modifier.";
    for modifier in ["public", "protected", "private", "internal"] {
        let code = leak(bare.replace("    string", &format!("    {modifier} string")));

        assert_eq!(issues(code), [format!("5:5 {message}"), format!("7:5 {message}")], "{modifier}");
    }

    let php = "<?php\n\nnamespace App\\Tenant;\n\ninterface Named\n{\n    protected string $name { get; }\n\n    private function label(int $digits): string;\n}\n";
    assert_eq!(
        issues_in("src/Named.php", php),
        [
            "7:5 Interface virtual property `Named::$name` cannot have `protected` modifier.",
            "7:5 Interface virtual property `Named::$name` must be declared public.",
            "9:5 Interface method `Named::label` cannot have `private` modifier."
        ]
    );
}

/// Spec section 5's `internal` is not supported yet: on a member, a parameter or a declaration it is one error at the
/// modifier, and the member is not one written without an access modifier.
#[test]
fn internal_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\npublic class Report\n{\n    internal const int MAX = 1;\n    internal int count = 0;\n    internal int views { get; set; } = 0;\n    internal int total => 7;\n    public string name { get; internal set; } = \"\";\n\n    internal Report(internal int size { get; })\n    {\n    }\n\n    internal Report.empty() : this(0)\n    {\n    }\n\n    internal void run()\n    {\n    }\n}\n";
    let message = "This modifier is not supported yet in PHP#.";

    assert_eq!(
        issues(code),
        [
            format!("5:5 {message}"),
            format!("6:5 {message}"),
            format!("7:5 {message}"),
            format!("8:5 {message}"),
            "9:31 This accessor is not supported yet in PHP#.".to_owned(),
            format!("11:5 {message}"),
            format!("11:21 {message}"),
            format!("15:5 {message}"),
            format!("19:5 {message}"),
        ]
    );

    for declaration in [
        "internal class Ledger\n{\n}\n",
        "internal interface Priced\n{\n}\n",
        "internal enum Suit\n{\n    case Hearts;\n}\n",
        "internal event Paid(int orderId);\n",
    ] {
        let code = leak(format!("namespace App.Tenant;\n\n{declaration}"));

        assert_eq!(issues(code), [format!("3:1 {message}")], "{declaration}");
    }

    // An anonymous class is not supported yet, and `internal` among its modifiers is no other error.
    let anonymous = leak(method("        const other = new readonly internal class {};\n        return 1;\n"));
    assert_eq!(issues(anonymous), ["7:23 This expression is not supported yet in PHP#."]);
}

#[test]
fn another_modifier_on_an_interface_method_is_an_error() {
    let code = "namespace App.Tenant;\n\ninterface Measured\n{\n    static float unit();\n}\n";

    assert_eq!(issues(code), ["5:5 An interface method takes no modifier: every interface method is public."]);
}

#[test]
fn an_interface_constant_or_another_interface_modifier_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nabstract interface Measured\n{\n    const int SIDES = 4;\n}\n";

    assert_eq!(
        issues(code),
        ["3:1 This modifier is not supported yet in PHP#.", "5:5 This class member is not supported yet in PHP#."]
    );
}

/// An interface states a get-only property its classes give, as `OrderEvent` states the `orderId` its events give,
/// spec section 15. A `set` accessor or an accessor body is not supported yet.
#[test]
fn an_interface_property_is_get_only() {
    let code = "namespace App.Tenant;\n\ninterface Named\n{\n    string name { get; }\n    string label { get => \"label\"; }\n    string title { get; set; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "6:24 Interface virtual property `Named.label` must be abstract.",
            "6:5 This class member is not supported yet in PHP#.",
            "7:5 This class member is not supported yet in PHP#."
        ]
    );

    let php = "<?php\n\nnamespace App\\Tenant;\n\ninterface Named\n{\n    public string $name { get; }\n    public string $label { get => \"label\"; }\n}\n";
    assert_eq!(issues_in("src/Report.php", php), ["8:32 Interface virtual property `Named::$label` must be abstract."]);
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

/// Spec sections 2 and 24: a type takes any capitalized word but the type names the engine reserves. PHP#'s keywords
/// are lowercase and case-sensitive, as C#'s are, so `Match` and `Internal` name a class, as every PHP keyword PHP#
/// doesn't use does, capitalized. `Self` is the one capitalized keyword, and the engine reserves it with `True`,
/// `False`, `Null`, `Static`, `Parent`, `Array` and `Callable`. A lowercase keyword gets spec section 24's error.
#[test]
fn a_type_takes_any_capitalized_word_but_a_reserved_type_name() {
    const PHP_ONLY: [&str; 42] = [
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
    ];
    const RESERVED: [&str; 8] = ["Self", "True", "False", "Null", "Static", "Parent", "Array", "Callable"];

    for word in PHP_ONLY.iter().chain(&SHARP_KEYWORDS) {
        let name = format!("{}{}", word[..1].to_uppercase(), &word[1..]);
        let declarations = [
            (format!("class {name}\n{{\n}}\n"), "3:7"),
            (format!("interface {name}\n{{\n}}\n"), "3:11"),
            (format!("enum {name}\n{{\n    case One;\n}}\n"), "3:6"),
            (format!("event {name}(int id);\n"), "3:7"),
        ];

        for (declaration, at) in declarations {
            let code = leak(format!("namespace App.Tenant;\n\n{declaration}"));
            let expected = if RESERVED.contains(&name.as_str()) {
                vec![format!("{at} Cannot use `{name}` as a class name: it is reserved.")]
            } else {
                vec![]
            };

            assert_eq!(issues(code), expected, "{code}");
        }

        if !RESERVED.contains(&name.as_str()) {
            for header in [
                format!("class Shop : {name}"),
                format!("class Shop : Base, {name}"),
                format!("interface Shop : {name}"),
            ] {
                let code = leak(format!("namespace App.Tenant;\n\n{header}\n{{\n}}\n"));

                assert_eq!(issues(code), Vec::<String>::new(), "{code}");
            }
        }
    }

    // PHP keeps its refusal of a reserved word as a type's name or base type.
    assert_eq!(
        issues_in(
            "src/Report.php",
            "<?php\n\nclass Report extends parent\n{\n}\n\nclass Total implements readonly\n{\n}\n\ntrait readonly\n{\n}\n\ninterface readonly\n{\n}\n\nenum readonly\n{\n}\n"
        ),
        [
            "3:22 class `Report` cannot extend reserved keyword `parent`.",
            "7:24 class `Total` cannot implement reserved keyword `readonly`.",
            "11:7 Trait `readonly` name cannot be a reserved keyword.",
            "15:11 Interface `readonly` name cannot be a reserved keyword.",
            "19:6 Enum `readonly` name cannot be a reserved keyword."
        ]
    );

    let lowercase = "namespace App.Tenant;\n\nclass internal\n{\n}\n";

    assert_eq!(
        issues(lowercase),
        [
            "3:7 `internal` must start with a capital letter: PHP# capitalizes every namespace and every type except the built-in ones."
        ]
    );
}

#[test]
fn any_is_a_reserved_class_name() {
    let code = "namespace App.Tenant;\n\nclass Any\n{\n}\n";

    assert_eq!(issues(code), ["3:7 Cannot use `Any` as a class name: it is reserved."]);
}

/// The standard library declares `Sharp.Int`, `Sharp.Float` and `Sharp.Bool`, spec section 24. The engine compiles
/// the library knowing only the file, so it lets any `.sharp` file whose namespace is exactly `Sharp` declare them.
#[test]
fn the_sharp_namespace_declares_int_float_and_bool() {
    for name in ["Int", "Float", "Bool"] {
        let code = leak(format!(
            "namespace Sharp;\n\npublic static class {name}\n{{\n    public static int one() => 1;\n}}\n"
        ));

        assert_eq!(issues(code), Vec::<String>::new(), "{name}");
    }
}

/// The engine allows the three names in the namespace `Sharp` only, compared exactly, so every other spelling and
/// namespace keeps the reserved-name error.
#[test]
fn int_float_and_bool_outside_the_sharp_namespace_are_reserved() {
    for (namespace, name) in [
        ("App", "Int"),
        ("App", "Bool"),
        ("Sharp.Text", "Float"),
        ("SHARP", "Int"),
        ("Sharp", "INT"),
        ("Sharp", "Mixed"),
    ] {
        let code = leak(format!(
            "namespace {namespace};\n\npublic static class {name}\n{{\n    public static int one() => 1;\n}}\n"
        ));

        assert_eq!(
            issues(code),
            [format!("3:21 Cannot use `{name}` as a class name: it is reserved.")],
            "{namespace}.{name}"
        );
    }

    assert_eq!(
        issues("namespace sharp;\n\npublic static class Int\n{\n    public static int one() => 1;\n}\n"),
        [
            "1:11 `sharp` must start with a capital letter: PHP# capitalizes every namespace and every type except the built-in ones.",
            "3:21 Cannot use `Int` as a class name: it is reserved.",
        ]
    );
}

#[test]
fn methods_whose_names_differ_only_in_case_are_an_error() {
    let code = "class Report\n{\n    public int run() { return 1; }\n\n    public int Run() { return 2; }\n}\n";

    // The PHP checks already reject this, so PHP# adds no second error.
    assert_eq!(issues(code), ["5:16 class method `Report.Run` has already been defined"]);

    let php = "<?php\n\nclass Report\n{\n    public function run(): int { return 1; }\n\n    public function Run(): int { return 2; }\n}\n";
    assert_eq!(issues_in("src/Report.php", php), ["7:21 class method `Report::Run` has already been defined"]);
}

#[test]
fn an_import_whose_short_name_is_reserved_is_an_error() {
    let code = "namespace App.Tenant;\n\nimport Lib.Int;\nimport Lib.Mixed;\nimport Lib.Any;\n\nclass Report\n{\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:8 Cannot import `Lib.Int` as `Int`: PHP# reserves `Int` for a type.",
            "4:8 Cannot import `Lib.Mixed` as `Mixed`: PHP# reserves `Mixed` for a type.",
            "5:8 Cannot import `Lib.Any` as `Any`: PHP# reserves `Any` for a type.",
        ]
    );
}

/// Importing a standard library type class names the class a bare name already names, spec section 23.
#[test]
fn an_import_of_a_standard_library_type_class_is_allowed() {
    let code =
        "namespace App.Tenant;\n\nimport Sharp.Int;\nimport Sharp.Float;\nimport Sharp.Bool;\n\nclass Report\n{\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
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

/// Spec section 23: a renamed import is used by its new name, so two imports of one name clash whether either is
/// renamed or not.
#[test]
fn an_import_renamed_to_the_name_of_another_import_is_an_error() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc as Tool;\nimport Other.Tool;\nimport Third.Thing as TOOL;\n\nclass Report\n{\n}\n";

    assert_eq!(
        issues(code),
        [
            "4:8 Cannot import `Other.Tool` as `Tool`: `Lib.Calc` is already imported as `Tool`.",
            "5:8 Cannot import `Third.Thing` as `TOOL`: `Lib.Calc` is already imported as `Tool`.",
        ]
    );
}

#[test]
fn an_import_renamed_like_a_class_of_the_same_file_or_a_reserved_name_is_an_error() {
    let code = "namespace App.Tenant;\n\nimport App.Shared.Page as Report;\nimport Lib.Thing as Int;\nimport Sharp.Int as Float;\n\nclass Report\n{\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:8 Cannot import `App.Shared.Page` as `Report`: this file declares a class named `Report`.",
            "4:8 Cannot import `Lib.Thing` as `Int`: PHP# reserves `Int` for a type.",
            "5:8 Cannot import `Sharp.Int` as `Float`: PHP# reserves `Float` for a type.",
        ]
    );
}

/// Spec section 23: renaming one of two imports of one name, or an import named like a class the file declares, fixes
/// the clash. An import renamed to the class the file declares names that class, as a plain import of it does.
#[test]
fn renaming_an_import_away_from_a_taken_or_reserved_name_is_valid() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\nimport Other.Calc as OtherCalc;\nimport App.Shared.Report as SharedReport;\nimport Lib.Int as Number;\nimport App.Tenant.Report as Report;\n\nclass Report\n{\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

/// A clash names the code that fixes it: the import renamed with `as`, as rustc's E0252 suggests.
#[test]
fn a_clashing_import_is_helped_with_the_import_renamed() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\nimport Other.Calc;\nimport App.Shared.Report;\nimport Lib.Any;\n\nclass Report\n{\n}\n";

    let helps: Vec<Option<String>> = check("src/Report.sharp", code).into_iter().map(|issue| issue.help).collect();

    assert_eq!(
        helps,
        [
            Some("Rename the import with `as`, as in `import Other.Calc as OtherCalc;`.".to_owned()),
            Some("Rename the import with `as`, as in `import App.Shared.Report as OtherReport;`.".to_owned()),
            Some("Rename the import with `as`, as in `import Lib.Any as OtherAny;`.".to_owned()),
        ]
    );
}

/// Spec section 24 capitalizes every type but the built-in ones, and the parser reads a capitalized name before `<` as
/// a type with type arguments. So a class, an interface, an enum and an event start with a capital letter.
#[test]
fn a_class_interface_or_enum_name_starts_with_a_capital_letter() {
    let lowercase = "namespace App.Tenant;\n\nclass box\n{\n}\n\ninterface priced\n{\n}\n\nenum status\n{\n    case Open;\n}\n\nevent paid(int id);\n";
    let capitalized = "namespace App.Tenant;\n\nclass Box\n{\n}\n\ninterface Priced\n{\n}\n\nenum Status\n{\n    case Open;\n}\n\nevent Paid(int id);\n";
    let rule =
        "must start with a capital letter: PHP# capitalizes every namespace and every type except the built-in ones.";

    assert_eq!(
        issues(lowercase),
        [
            format!("3:7 `box` {rule}"),
            format!("7:11 `priced` {rule}"),
            format!("11:6 `status` {rule}"),
            format!("16:7 `paid` {rule}")
        ]
    );
    assert_eq!(issues(capitalized), Vec::<String>::new());
}

/// A name that starts with `_` is not capitalized. A `__Something__` name keeps its own error.
#[test]
fn a_type_name_that_starts_with_an_underscore_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass _Box\n{\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:7 `_Box` must start with a capital letter: PHP# capitalizes every namespace and every type except the built-in ones."
        ]
    );
}

/// An import's new name is a type name of the file, so it starts with a capital letter as a declared one does.
#[test]
fn an_import_alias_starts_with_a_capital_letter() {
    let lowercase = "namespace App.Tenant;\n\nimport Lib.Box as box;\n\nclass Report\n{\n}\n";
    let capitalized = "namespace App.Tenant;\n\nimport Lib.Box as Crate;\n\nclass Report\n{\n}\n";

    assert_eq!(
        issues(lowercase),
        [
            "3:19 `box` must start with a capital letter: PHP# capitalizes every namespace and every type except the built-in ones."
        ]
    );
    assert_eq!(issues(capitalized), Vec::<String>::new());
}

/// PHP# capitalizes every namespace as it does every type, so each part of a `namespace` line starts with a capital
/// letter, and the error names the part.
#[test]
fn a_namespace_part_starts_with_a_capital_letter() {
    let lowercase = "namespace App.store;\n\nclass Report\n{\n}\n";
    let capitalized = "namespace App.Store.Orders;\n\nclass Report\n{\n}\n";

    assert_eq!(
        issues(lowercase),
        [
            "1:15 `store` must start with a capital letter: PHP# capitalizes every namespace and every type except the built-in ones."
        ]
    );
    assert_eq!(issues(capitalized), Vec::<String>::new());
}

/// The first part of a namespace is checked as every other part is, and so is a namespace of one part.
#[test]
fn a_lowercase_first_namespace_part_is_an_error() {
    assert_eq!(
        issues("namespace app.Store;\n\nclass Report\n{\n}\n"),
        [
            "1:11 `app` must start with a capital letter: PHP# capitalizes every namespace and every type except the built-in ones."
        ]
    );
    assert_eq!(
        issues("namespace app;\n\nclass Report\n{\n}\n"),
        [
            "1:11 `app` must start with a capital letter: PHP# capitalizes every namespace and every type except the built-in ones."
        ]
    );
}

/// Each lowercase part of a namespace is its own error, at that part.
#[test]
fn each_lowercase_namespace_part_is_its_own_error() {
    let rule =
        "must start with a capital letter: PHP# capitalizes every namespace and every type except the built-in ones.";

    assert_eq!(
        issues("namespace app.Store.orders;\n\nclass Report\n{\n}\n"),
        [format!("1:11 `app` {rule}"), format!("1:21 `orders` {rule}")]
    );
}

/// A namespace part that starts with `_` is not capitalized, as a type name that starts with `_` is not.
#[test]
fn a_namespace_part_that_starts_with_an_underscore_is_an_error() {
    assert_eq!(
        issues("namespace App._Store;\n\nclass Report\n{\n}\n"),
        [
            "1:15 `_Store` must start with a capital letter: PHP# capitalizes every namespace and every type except the built-in ones."
        ]
    );
}

/// An import names a namespace another file declares, and plain PHP vendor namespaces are often lowercase, so an
/// import's namespace parts keep their letters.
#[test]
fn an_import_keeps_its_lowercase_namespace_parts() {
    assert_eq!(issues("namespace App.Tenant;\n\nimport vendor.lib.Box;\n\nclass Report\n{\n}\n"), Vec::<String>::new());
}

/// An interface's name is checked as a class's is, so a reserved one is an error, as the engine refuses it.
#[test]
fn an_interface_named_like_a_reserved_class_name_is_an_error() {
    let code = "namespace App.Tenant;\n\ninterface Mixed\n{\n}\n";

    assert_eq!(issues(code), ["3:11 Cannot use `Mixed` as a class name: it is reserved."]);
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

/// Spec section 29 removes every superglobal, written with `$` or bare as in `_SERVER["HTTP_HOST"]`.
#[test]
fn a_superglobal_with_or_without_dollar_is_an_error_that_names_a_request() {
    let code = leak(method(
        "        Store.keep(GLOBALS[\"a\"], $GLOBALS);\n        Store.keep(_SERVER[\"a\"], $_SERVER);\n        Store.keep(_GET[\"a\"], $_GET);\n        Store.keep(_POST[\"a\"], $_POST);\n        Store.keep(_FILES[\"a\"], $_FILES);\n        Store.keep(_COOKIE[\"a\"], $_COOKIE);\n        Store.keep(_SESSION[\"a\"], $_SESSION);\n        Store.keep(_REQUEST[\"a\"], $_REQUEST);\n        Store.keep(_ENV[\"a\"], $_ENV);\n        return extra;\n",
    ));
    let message = "PHP# has no superglobals; take a Request";

    assert_eq!(
        issues(code),
        [
            format!("7:20 {message}"),
            format!("7:34 {message}"),
            format!("8:20 {message}"),
            format!("8:34 {message}"),
            format!("9:20 {message}"),
            format!("9:31 {message}"),
            format!("10:20 {message}"),
            format!("10:32 {message}"),
            format!("11:20 {message}"),
            format!("11:33 {message}"),
            format!("12:20 {message}"),
            format!("12:34 {message}"),
            format!("13:20 {message}"),
            format!("13:35 {message}"),
            format!("14:20 {message}"),
            format!("14:35 {message}"),
            format!("15:20 {message}"),
            format!("15:31 {message}"),
        ]
    );
}

/// A write to a superglobal's element names the request as a read does, not the write target.
#[test]
fn a_write_to_a_superglobal_is_an_error_that_names_a_request() {
    let code = leak(method(
        "        _SESSION[\"user\"] = extra;\n        $_SESSION[\"user\"] = extra;\n        _GET[\"n\"]++;\n        return extra;\n",
    ));
    let message = "PHP# has no superglobals; take a Request";

    assert_eq!(issues(code), [format!("7:9 {message}"), format!("8:9 {message}"), format!("9:9 {message}")]);
}

/// A constant expression that reads a superglobal, or a member of one or of a `__Something__` name, names the
/// replacement, as a method body does: a parameter default, an attribute argument and a constant's value.
#[test]
fn a_superglobal_in_a_constant_expression_is_an_error_that_names_a_request() {
    let code = leak(
        method("        return 1;\n")
            .replace("{\n    public int run", "{\n    public const int A = __Foo__.bar;\n    [Field(_SERVER.x)]\n    public int run")
            .replace(
                "int extra",
                "string host = _SERVER[\"HTTP_HOST\"], string other = $_SERVER[\"HTTP_HOST\"], string server = _SERVER.HTTP_HOST, int line = __Foo__.bar",
            ),
    );
    let message = "PHP# has no superglobals; take a Request";
    let magic = "PHP# has no `__Foo__`: `Position.current()` gives the file, directory, line, column and function.";

    assert_eq!(
        issues(code),
        [
            format!("5:26 {magic}"),
            format!("6:12 {message}"),
            format!("7:34 {message}"),
            format!("7:71 {message}"),
            format!("7:110 {message}"),
            format!("7:140 {magic}"),
        ]
    );
}

/// A superglobal or a `__Something__` name before `.` or `?.` binds as a class, and is refused as its bare form is. A
/// class whose name only starts with `_` is read as a class.
#[test]
fn a_superglobal_or_a_double_underscore_name_before_a_member_is_an_error() {
    let code = leak(method(
        "        Store.keep(_SERVER.x);\n        Store.keep(GLOBALS.x);\n        _SERVER.read();\n        _SERVER.x = 1;\n        Store.keep(__Foo__.bar);\n        __Foo__.bar();\n        Store.keep(_SERVER?.x);\n        Store.keep(_Helper.x);\n        return extra;\n",
    ));
    let superglobal = "PHP# has no superglobals; take a Request";
    let magic = "PHP# has no `__Foo__`: `Position.current()` gives the file, directory, line, column and function.";

    assert_eq!(
        issues(code),
        [
            format!("7:20 {superglobal}"),
            format!("8:20 {superglobal}"),
            format!("9:9 {superglobal}"),
            format!("10:9 {superglobal}"),
            format!("11:20 {magic}"),
            format!("12:9 {magic}"),
            format!("13:20 {superglobal}"),
        ]
    );
}

/// Spec section 27 removes PHP's magic constants and every other `__Something__` name, in a body and in a constant
/// expression, and `Position` says where code sits. Each error names the magic constant as written.
#[test]
fn a_magic_constant_or_another_double_underscore_name_is_an_error_that_names_position() {
    let code = leak(
        method(
            "        Store.keep(__DIR__);\n        Store.keep(__FILE__);\n        Store.keep(__LINE__);\n        Store.keep(__FUNCTION__);\n        Store.keep(__METHOD__);\n        Store.keep(__CLASS__);\n        Store.keep(__NAMESPACE__);\n        Store.keep(__TRAIT__);\n        Store.keep(__PROPERTY__);\n        Store.keep(__COMPILER_HALT_OFFSET__);\n        Store.keep(__dir__);\n        Store.keep(____, __Version);\n        return extra;\n",
        )
        .replace("int extra", "int extra = __LINE__"),
    );
    let position = "`Position.current()` gives the file, directory, line, column and function.";

    assert_eq!(
        issues(code),
        [
            "5:32 PHP# has no `__LINE__`: write `Position.current().line`.".to_owned(),
            "7:20 PHP# has no `__DIR__`: write `Position.current().directory`.".to_owned(),
            "8:20 PHP# has no `__FILE__`: write `Position.current().file`.".to_owned(),
            "9:20 PHP# has no `__LINE__`: write `Position.current().line`.".to_owned(),
            "10:20 PHP# has no `__FUNCTION__`: write `Position.current().function`.".to_owned(),
            "11:20 PHP# has no `__METHOD__`: write `Position.current().function`.".to_owned(),
            "12:20 PHP# has no `__CLASS__`: write `typeof(Class)` with the class's name.".to_owned(),
            "13:20 PHP# has no `__NAMESPACE__`: write `Position.current().function`, which starts with the namespace."
                .to_owned(),
            format!("14:20 PHP# has no `__TRAIT__`: {position}"),
            format!("15:20 PHP# has no `__PROPERTY__`: {position}"),
            format!("16:20 PHP# has no `__COMPILER_HALT_OFFSET__`: {position}"),
            "17:20 PHP# has no `__dir__`: write `Position.current().directory`.".to_owned(),
        ]
    );
}

/// A declared `__Something__` name is refused as a read of one is, in every kind of declaration. A method named as
/// PHP's magic methods are, starting but not ending with `__`, keeps its own error.
#[test]
fn a_declared_double_underscore_name_is_an_error_that_names_position() {
    let code = "namespace App.Tenant;\n\nclass __Shape__\n{\n}\n\ninterface __Named__\n{\n    int __area__(int __side__);\n}\n\nenum __Suit__\n{\n    case __Hearts__;\n}\n\nclass Report\n{\n    private int __count__ = 0;\n    public int __views__ { get; set; }\n    public string __slug__ => \"a\";\n    public const int __MAX__ = 1;\n\n    public Report(private int __kept__)\n    {\n    }\n\n    public int __run__(int __extra__)\n    {\n        let __a__ = 1;\n        const __b__ = 2;\n        int __c__ = 3;\n        for (let __i__ = 0; __i__ < 1; __i__++) {\n        }\n        for (const __v__ of Store.values()) {\n        }\n        try {\n        } catch (Missing __e__) {\n        }\n        const f = (int __x__) => __x__;\n        return 1;\n    }\n\n    public int __get(string name) => 1;\n}\n";
    let position = "`Position.current()` gives the file, directory, line, column and function.";

    assert_eq!(
        issues(code),
        [
            format!("3:7 PHP# has no `__Shape__`: {position}"),
            format!("7:11 PHP# has no `__Named__`: {position}"),
            format!("9:9 PHP# has no `__area__`: {position}"),
            format!("9:22 PHP# has no `__side__`: {position}"),
            format!("12:6 PHP# has no `__Suit__`: {position}"),
            format!("14:10 PHP# has no `__Hearts__`: {position}"),
            format!("19:17 PHP# has no `__count__`: {position}"),
            format!("20:16 PHP# has no `__views__`: {position}"),
            format!("21:19 PHP# has no `__slug__`: {position}"),
            format!("22:22 PHP# has no `__MAX__`: {position}"),
            format!("24:31 PHP# has no `__kept__`: {position}"),
            format!("28:16 PHP# has no `__run__`: {position}"),
            format!("28:28 PHP# has no `__extra__`: {position}"),
            format!("30:13 PHP# has no `__a__`: {position}"),
            format!("31:15 PHP# has no `__b__`: {position}"),
            format!("32:13 PHP# has no `__c__`: {position}"),
            format!("33:18 PHP# has no `__i__`: {position}"),
            format!("35:20 PHP# has no `__v__`: {position}"),
            format!("38:26 PHP# has no `__e__`: {position}"),
            format!("40:24 PHP# has no `__x__`: {position}"),
            "44:16 This method name is not supported yet in PHP#.".to_owned(),
        ]
    );
}

/// A constructor carries its class's name, so a `__Something__` class reports its name once, at the class.
#[test]
fn a_double_underscore_class_with_a_constructor_reports_its_name_once() {
    let code = "namespace App.Tenant;\n\nclass __Shape__\n{\n    public __Shape__()\n    {\n    }\n}\n";

    assert_eq!(
        issues(code),
        ["3:7 PHP# has no `__Shape__`: `Position.current()` gives the file, directory, line, column and function."
            .to_owned()]
    );
}

/// A refusal skips a node's children only when its span covers them, so a method refused at its name still has its
/// body checked.
#[test]
fn a_method_refused_at_its_name_has_its_body_checked() {
    let code = "namespace App.Tenant;\n\nclass Forms\n{\n    public void forms()\n    {\n        const host = _SERVER[\"x\"];\n        echo \"a\";\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:17 A method named after its class is not supported yet in PHP#.",
            "7:22 PHP# has no superglobals; take a Request",
            "8:9 PHP# has no `echo`: write `printf` or `fwrite`.",
        ]
    );
}

/// A member refused at its name, a modifier or a type keeps its parameters, initial values and body checked.
#[test]
fn a_member_refused_at_part_of_its_declaration_has_the_rest_checked() {
    let code = "namespace App.Tenant;\n\ninterface Named\n{\n    void __get(string name = _SERVER[\"x\"]);\n}\n\nenum Suit\n{\n    case Hearts;\n\n    public Suit()\n    {\n        echo \"a\";\n    }\n}\n\nclass Report\n{\n    int count = __LINE__;\n    const int MAX = __LINE__;\n\n    public Report(public int id = __LINE__, &total = __LINE__)\n    {\n    }\n\n    int run()\n    {\n        Map<float, callable> rows = [:];\n        echo \"b\";\n        return 1;\n    }\n}\n";
    let line = "write `Position.current().line`.";

    assert_eq!(
        issues(code),
        [
            "5:10 This method name is not supported yet in PHP#.".to_owned(),
            "5:30 PHP# has no superglobals; take a Request".to_owned(),
            "12:12 An enum has no constructor: its cases are its only values.".to_owned(),
            "14:9 PHP# has no `echo`: write `printf` or `fwrite`.".to_owned(),
            format!("20:17 PHP# has no `__LINE__`: {line}"),
            "21:15 A constant without `public`, `protected` or `private` is not supported yet in PHP#.".to_owned(),
            format!("21:21 PHP# has no `__LINE__`: {line}"),
            "23:19 A `public` constructor parameter needs accessors: a public member is a property, as in `public int id { get; }`.".to_owned(),
            format!("23:35 PHP# has no `__LINE__`: {line}"),
            "23:45 A by-reference parameter is not supported yet in PHP#.".to_owned(),
            format!("23:54 PHP# has no `__LINE__`: {line}"),
            "27:9 A method without `public`, `protected` or `private` is not supported yet in PHP#.".to_owned(),
            "29:13 A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value.".to_owned(),
            "29:20 This type is not supported yet in PHP#.".to_owned(),
            "30:9 PHP# has no `echo`: write `printf` or `fwrite`.".to_owned(),
        ]
    );
}

/// An expression refused at its operator, keyword or target keeps its operands and arguments checked.
#[test]
fn an_expression_refused_at_part_of_it_has_the_rest_checked() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public void run()\n    {\n        const a = (array) _SERVER;\n        const b = _GET ?: __LINE__;\n        const c = self.make(__LINE__);\n        const d = new static(__LINE__);\n        const e = total(__LINE__);\n        const f = _SERVER[__LINE__];\n        PI = __LINE__;\n        _SERVER[\"x\"] = _GET;\n        const g = _GET ? 1 : 2 ? 3 : __LINE__;\n    }\n\n    public int total(int x) => x;\n}\n";
    let line = "write `Position.current().line`.";
    let request = "PHP# has no superglobals; take a Request";

    assert_eq!(
        issues(code),
        [
            "7:19 PHP# has no `(array)`: write a struct's `parse(value)` for an object, or `List.wrap(value)` for a value."
                .to_owned(),
            format!("7:27 {request}"),
            "8:24 PHP# has no `?:`: write `a ?? b` to replace null, or `c ? a : b` with a `bool` condition.".to_owned(),
            format!("8:19 {request}"),
            format!("8:27 PHP# has no `__LINE__`: {line}"),
            "9:19 PHP# has no `self`: write the class's own name, `Report`, for the declaring class, or `Self` for the class a static method is called on.".to_owned(),
            format!("9:29 PHP# has no `__LINE__`: {line}"),
            "10:23 PHP# writes `Self` for PHP's `static`.".to_owned(),
            format!("10:30 PHP# has no `__LINE__`: {line}"),
            format!("11:25 PHP# has no `__LINE__`: {line}"),
            format!("12:19 {request}"),
            format!("12:27 PHP# has no `__LINE__`: {line}"),
            "13:9 This write target is not supported yet in PHP#.".to_owned(),
            format!("13:14 PHP# has no `__LINE__`: {line}"),
            format!("14:9 {request}"),
            format!("14:24 {request}"),
            "15:19 Unparenthesized `a ? b : c ? d : e` is not supported. Use either `(a ? b : c) ? d : e` or `a ? b : (c ? d : e)`.".to_owned(),
            format!("15:38 PHP# has no `__LINE__`: {line}"),
        ]
    );
}

/// A top-level function and a namespace outside the slice are refused at their name or keyword, and the code inside
/// them is still checked.
#[test]
fn a_function_or_namespace_refused_at_its_name_has_its_code_checked() {
    let code = "namespace App.Tenant;\n\nfunction helper()\n{\n    echo \"a\";\n}\n\nnamespace App.Other;\n\nclass Report\n{\n    public void run()\n    {\n        echo \"b\";\n    }\n}\n";
    let braced = "namespace App.Tenant {\n    class Report\n    {\n        public void run()\n        {\n            echo \"a\";\n        }\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:10 PHP# has no top-level functions: move `helper` into a class as a static method.",
            "5:5 PHP# has no `echo`: write `printf` or `fwrite`.",
            "8:1 This namespace is not supported yet in PHP#.",
            "14:9 PHP# has no `echo`: write `printf` or `fwrite`.",
        ]
    );
    assert_eq!(
        issues(braced),
        ["1:1 This namespace is not supported yet in PHP#.", "6:13 PHP# has no `echo`: write `printf` or `fwrite`."]
    );
}

/// A class in a refused namespace is still the class its methods belong to, so its constructor is a constructor.
#[test]
fn a_constructor_in_a_refused_namespace_is_a_constructor() {
    let code = "namespace App.Tenant;\n\nnamespace App.Other;\n\nclass Report\n{\n    public Report()\n    {\n    }\n}\n\nenum Suit\n{\n    case Hearts;\n\n    public Suit()\n    {\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:1 This namespace is not supported yet in PHP#.",
            "16:12 An enum has no constructor: its cases are its only values.",
        ]
    );
}

/// A `Map` key whose type is outside the slice is refused once, as that type, and a key of a type the slice has but a
/// key cannot take is refused as a key.
#[test]
fn a_map_key_is_refused_once() {
    let code = leak(method(
        "        Map<callable, int> rows = [:];\n        Map<static, int> others = [:];\n        Map<float, int> prices = [:];\n        Map<callable?, int> maybe = [:];\n        Map<int?, int> counts = [:];\n        return 1;\n",
    ));
    let key = "A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value.";

    assert_eq!(
        issues(code),
        [
            "7:13 This type is not supported yet in PHP#.".to_owned(),
            "8:13 PHP# writes `Self` for PHP's `static`.".to_owned(),
            format!("9:13 {key}"),
            "10:13 This type is not supported yet in PHP#.".to_owned(),
            format!("11:13 {key}"),
        ]
    );
}

/// A `public` parameter declares a property only on a constructor. Elsewhere PHP's own error reports it alone.
#[test]
fn a_public_parameter_outside_a_constructor_reports_only_the_php_error() {
    let code = "namespace App.Tenant;\n\nfunction helper(public int id)\n{\n}\n\nclass Report\n{\n    public void run(public int id)\n    {\n    }\n}\n";
    let promoted = "Promoted properties are not allowed outside of constructors.";

    assert_eq!(
        issues(code),
        [
            format!("3:17 {promoted}"),
            "3:10 PHP# has no top-level functions: move `helper` into a class as a static method.".to_owned(),
            format!("9:21 {promoted}"),
        ]
    );
}

/// A bare call is the global function's, so only the analyzer, which knows the functions, reports one that names a
/// method instead.
#[test]
fn a_bare_member_name_is_an_error_that_names_this() {
    let code =
        "class Report\n{\n    public int count() { return 0; }\n\n    public int run() { return count + run(); }\n}\n";

    assert_eq!(
        issues(code),
        ["5:31 Write `this.count()`: members of the same object are always written with `this.`."]
    );
}

#[test]
fn a_bare_method_name_matches_ignoring_case_as_in_php() {
    let code = "class Report\n{\n    public int total() { return 0; }\n\n    public int run() { return Total; }\n}\n";

    assert_eq!(
        issues(code),
        ["5:31 Write `this.total()`: members of the same object are always written with `this.`."]
    );
}

#[test]
fn a_bare_member_in_a_static_method_names_the_class() {
    let code = "class Report\n{\n    public static int helper() { return 0; }\n\n    public static int run() { return helper; }\n}\n";

    assert_eq!(
        issues(code),
        ["5:38 Write `Report.helper()`: a static method reaches the members of its class through the class name."]
    );
}

#[test]
fn a_bare_call_named_like_a_method_of_its_class_compiles_as_the_global_function() {
    let code =
        "namespace Sharp.Math;\n\npublic static class Math\n{\n    public static float ceil(float x) => ceil(x);\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
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

    let php = "<?php\n\nnamespace App\\Tenant;\n\nclass Report\n{\n    #[Field(Report::class)]\n    #[Field(Mode::Write)]\n    #[Field([$this->run(1)])]\n    #[Field(label: new Report())]\n    public function run(int $extra): int\n    {\n        return $extra;\n    }\n}\n";
    assert_eq!(
        issues_in("src/Report.php", php),
        ["9:13 Attribute `Field` argument contains a non-constant expression."]
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

    let php = "<?php\n\nnamespace App\\Tenant;\n\nclass Report\n{\n    public function __construct(private readonly int $a, private static int $b) {}\n\n    public function run(private int $c): void {}\n}\n";
    assert_eq!(
        issues_in("src/Report.php", php),
        [
            "9:25 Promoted properties are not allowed outside of constructors.",
            "7:66 Parameter `$b` cannot have the `static` modifier.",
        ]
    );
}

/// Spec section 9: a parameter receives its value, and a lazy property computes its own, so no parameter is `lazy`,
/// whether it declares a member or not.
#[test]
fn a_parameter_cannot_be_lazy() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(public lazy int id { get; }, lazy int count) {}\n\n    public int run(lazy int extra) => extra;\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:26 A parameter cannot be `lazy`: a lazy property computes its own value, and a parameter receives one.",
            "5:48 A parameter cannot be `lazy`: a lazy property computes its own value, and a parameter receives one.",
            "7:20 A parameter cannot be `lazy`: a lazy property computes its own value, and a parameter receives one.",
        ]
    );
}

/// `lazy` is a modifier of spec section 6.5, which the engine cannot run yet on a property or anywhere else.
#[test]
fn lazy_off_a_parameter_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public lazy int total => 1;\n\n    public lazy int run() => 1;\n}\n";

    assert_eq!(
        issues(code),
        ["5:12 `lazy` is not supported yet in PHP#.", "7:12 `lazy` is not supported yet in PHP#."]
    );
}

/// Spec section 9.1: a class has one unnamed main constructor, and no overloading by argument types. The help's named
/// constructor compiles. A method named after its class has only its own error, before the constructor or after it.
#[test]
fn a_class_has_one_main_constructor() {
    let second = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(int id) {}\n    public Report(string name) {}\n}\n";
    let fixed = "namespace App.Tenant;\n\nclass Report\n{\n    public Report(int id) {}\n    public Report.fromName(string name) : this(strlen(name)) { }\n}\n";

    let issue = check("src/Report.sharp", second).remove(0);
    assert_eq!(issue.message, "A class has one main constructor: PHP# has no overloading by argument types.");
    assert_eq!(
        issue.help.as_deref(),
        Some(
            "Give it a name that passes control to the main constructor, as in `public Report.fromName(…) : this(…) { }`."
        )
    );
    assert_eq!(issues(fixed), Vec::<String>::new());

    for (code, expected) in [
        (
            "namespace App.Tenant;\n\nclass Report\n{\n    public int Report() => 1;\n    public Report(int id) {}\n}\n",
            "5:16 A method named after its class is not supported yet in PHP#.",
        ),
        (
            "namespace App.Tenant;\n\nclass Report\n{\n    public Report(int id) {}\n    public int Report() => 1;\n}\n",
            "6:16 A method named after its class is not supported yet in PHP#.",
        ),
    ] {
        assert_eq!(issues(code), [expected], "{code}");
    }
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

    assert_eq!(issues(code), ["5:21 Non-Abstract method `Report.run` must have a concrete body."]);

    let php = "<?php\n\nnamespace App\\Tenant;\n\nclass Report\n{\n    public function run(): int;\n}\n";
    assert_eq!(issues_in("src/Report.php", php), ["7:31 Non-Abstract method `Report::run` must have a concrete body."]);
}

#[test]
fn a_static_class_holds_constants_static_members_and_extern_methods() {
    let code = "namespace Sharp.Text;\n\npublic static class Text\n{\n    public const int LIMIT = 80;\n    private static int made = 0;\n\n    public static extern string slug(string title);\n\n    public static string plain(string title) => title;\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_member_of_a_static_class_that_is_not_static_is_an_error() {
    let code = "namespace App.Tenant;\n\npublic static class Text\n{\n    private int made = 0;\n    public int count { get; } = 0;\n\n    public string slug(string title) => title;\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:17 A static class holds only static members: make `made` static.",
            "6:16 A static class holds only static members: make `count` static.",
            "8:19 A static class holds only static members: make `slug` static.",
        ]
    );
}

/// A static class has no constructor, in its body or in an extension's.
#[test]
fn a_static_class_has_no_constructor() {
    let code = "namespace App.Tenant;\n\npublic static class Text\n{\n    public Text() {}\n}\n";
    let extension = "namespace App.Tenant;\n\npublic static class Text\n{\n    extension(string value)\n    {\n        public Text() {}\n    }\n}\n";

    assert_eq!(issues(code), ["5:12 A static class has no constructor."]);
    assert_eq!(issues(extension), ["7:16 A static class has no constructor."]);
}

/// Spec section 26: a static class declares extension blocks, each with one receiver and the methods and computed
/// properties it adds to the receiver's type. They are instance members of the receiver, so they are not `static`.
#[test]
fn an_extension_in_a_static_class_adds_methods_and_computed_properties() {
    let code = "namespace App.Shared.Text;\n\npublic static class TextExtensions\n{\n    extension(string value)\n    {\n        public string slug() => value;\n        public bool isBlank => value == \"\";\n        private string first(int count) => value;\n    }\n\n    extension(List<string> values)\n    {\n        public int size() => count(values);\n    }\n\n    public static string plain(string title) => title;\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

/// An extension adds members to a type from outside it, so only a static class declares one.
#[test]
fn an_extension_outside_a_static_class_is_an_error() {
    for (owner, code) in [
        (
            "class",
            "namespace App.Shared;\n\npublic class Text\n{\n    extension(string value)\n    {\n        public string slug() => value;\n    }\n}\n",
        ),
        (
            "enum",
            "namespace App.Shared;\n\npublic enum Size\n{\n    case Small;\n\n    extension(string value)\n    {\n        public string slug() => value;\n    }\n}\n",
        ),
        (
            "interface",
            "namespace App.Shared;\n\npublic interface Named\n{\n    extension(string value)\n    {\n        public string slug() => value;\n    }\n}\n",
        ),
    ] {
        let line = if owner == "enum" { 7 } else { 5 };

        assert_eq!(
            issues(code),
            [format!("{line}:5 An extension belongs in a static class, as in `public static class StringExtensions`.")],
            "{owner}"
        );
    }
}

/// An extension's receiver is the one value every call has, so it is one parameter, with no modifier, no default and
/// no `...`.
#[test]
fn an_extension_receiver_is_one_plain_parameter() {
    let refusal = "An extension's receiver is one parameter, with no modifier, default or `...`, as in `extension(string value)`.";
    for (receiver, column) in [
        ("", 14),
        ("string value, int count", 14),
        ("string value = \"\"", 15),
        ("string ...values", 15),
        ("public string value", 15),
    ] {
        let code = format!(
            "namespace App.Shared;\n\npublic static class TextExtensions\n{{\n    extension({receiver})\n    {{\n        public int size() => 0;\n    }}\n}}\n"
        );

        assert_eq!(issues(code.leak()), [format!("5:{column} {refusal}")], "`{receiver}`");
    }
}

/// An extension adds methods and computed properties to its receiver's type. A value of that type has no storage of
/// the extension's, and the static class holds its constants, operators, laws and constructors.
#[test]
fn an_extension_holds_only_methods_and_computed_properties() {
    let refusal =
        "An extension adds only methods and computed properties, as in `public bool isBlank => value == \"\";`.";
    for member in [
        "private int count = 0;",
        "public int size { get; } = 0;",
        "public const int LIMIT = 80;",
        "public static bool operator ==(string a, string b) => true;",
        "law sameSize(string a) => a == a;",
        "extension(int count) { }",
    ] {
        let code = format!(
            "namespace App.Shared;\n\npublic static class TextExtensions\n{{\n    extension(string value)\n    {{\n        {member}\n    }}\n}}\n"
        );

        assert_eq!(issues(code.leak()), [format!("7:9 {refusal}")], "`{member}`");
    }
}

/// An extension member is an instance member of its receiver: it takes only an access modifier.
#[test]
fn an_extension_member_takes_only_an_access_modifier() {
    for (modifier, column) in [("static", 16), ("abstract", 16), ("virtual", 16), ("override", 16)] {
        let code = format!(
            "namespace App.Shared;\n\npublic static class TextExtensions\n{{\n    extension(string value)\n    {{\n        public {modifier} string slug() => value;\n    }}\n}}\n"
        );

        assert_eq!(
            issues(code.leak()),
            [format!("7:{column} An extension member takes only an access modifier: remove `{modifier}`.")],
            "`{modifier}`"
        );
    }
}

/// An extension member runs as a static method that takes its receiver, so it names the receiver, never `this`.
#[test]
fn this_in_an_extension_member_is_an_error() {
    let code = "namespace App.Shared;\n\npublic static class TextExtensions\n{\n    extension(string value)\n    {\n        public string slug() => this.trim();\n        public bool isBlank => this == \"\";\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "7:33 An extension member names its receiver: write `value`, not `this`.",
            "8:32 An extension member names its receiver: write `value`, not `this`.",
        ]
    );
}

/// A static class runs each extension member as a static method of its name, so two members, in its extensions or
/// its own, cannot share a name.
#[test]
fn two_members_of_one_static_class_cannot_share_a_name_across_its_extensions() {
    let code = "namespace App.Shared;\n\npublic static class Sizes\n{\n    extension(string value)\n    {\n        public int size() => 0;\n        public bool blank => value == \"\";\n    }\n\n    extension(List<int> values)\n    {\n        public int size() => 0;\n        public bool blank() => false;\n    }\n\n    public static int size(int count) => count;\n}\n";

    assert_eq!(
        issues(code),
        [
            "13:20 class method `Sizes.size` has already been defined",
            "14:21 class method `Sizes.blank` has already been defined",
            "17:23 class method `Sizes.size` has already been defined",
        ]
    );
}

#[test]
fn a_static_class_cannot_extend_a_class_or_implement_an_interface() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\nimport Lib.Named;\n\npublic static class Text : Calc, Named\n{\n}\n";

    assert_eq!(issues(code), ["6:26 A static class cannot extend a class or implement an interface."]);
}

#[test]
fn a_static_class_takes_only_public_and_static() {
    let code = "namespace App.Tenant;\n\nabstract static class Text\n{\n}\n\nfinal static class Slug\n{\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:1 A static class takes only `public` and `static`: remove `abstract`.",
            "7:1 A static class takes only `public` and `static`: remove `final`.",
        ]
    );
    assert_eq!(
        issues_in("src/Text.php", "<?php\n\nstatic class Text\n{\n}\n"),
        ["3:1 Class `Text` cannot have the `static` modifier."]
    );
}

/// Only the analyzer knows which files are the standard library's, so it decides where an `extern` method goes.
#[test]
fn an_extern_method_outside_the_sharp_namespace_is_left_to_the_analyzer() {
    for namespace in ["App", "Sharpen.Text", "App.Sharp"] {
        let code = leak(format!(
            "namespace {namespace};\n\npublic static class Text\n{{\n    public static extern string slug(string title);\n}}\n"
        ));

        assert_eq!(issues(code), Vec::<String>::new(), "{namespace}");
    }
}

#[test]
fn an_extern_method_is_public_static_in_a_static_class_with_no_body_in_any_namespace() {
    for namespace in ["Sharp.Text", "App"] {
        let code = leak(format!(
            "namespace {namespace};\n\npublic class Plain\n{{\n    public static extern string slug(string title);\n}}\n\npublic static class Text\n{{\n    private static extern string trim(string title);\n    public static extern string pad(string title) => title;\n    public extern string cut(string title);\n}}\n"
        ));

        assert_eq!(
            issues(code),
            [
                "5:33 An `extern` method is `public static`, in a static class, with no body.",
                "10:34 An `extern` method is `public static`, in a static class, with no body.",
                "11:33 An `extern` method is `public static`, in a static class, with no body.",
                "12:26 An `extern` method is `public static`, in a static class, with no body.",
            ],
            "{namespace}"
        );
    }
}

/// An `extern` method is refused at its name, so a body written by mistake still has its own errors reported.
#[test]
fn an_extern_method_with_a_body_has_its_body_checked() {
    let code = "namespace Sharp.Text;\n\npublic static class Text\n{\n    public static extern string slug(string title)\n    {\n        echo title;\n        return title;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:33 An `extern` method is `public static`, in a static class, with no body.",
            "7:9 PHP# has no `echo`: write `printf` or `fwrite`.",
        ]
    );
}

#[test]
fn extern_on_a_field_is_not_supported_yet() {
    let code = "namespace Sharp.Text;\n\npublic static class Text\n{\n    private static extern int count = 0;\n}\n";

    assert_eq!(issues(code), ["5:20 This modifier is not supported yet in PHP#."]);
}

#[test]
fn the_error_control_operator_is_in_the_slice_under_the_sharp_namespace() {
    for namespace in ["Sharp", "Sharp.Text", "SHARP.Text"] {
        let code = leak(format!(
            "namespace {namespace};\n\npublic static class Text\n{{\n    public static string quiet(string title) => @trim(title);\n}}\n"
        ));

        assert_eq!(issues(code), Vec::<String>::new(), "{namespace}");
    }
}

#[test]
fn the_error_control_operator_outside_the_sharp_namespace_is_an_error() {
    for namespace in ["App", "Sharpen.Text", "App.Sharp"] {
        let code = leak(format!(
            "namespace {namespace};\n\npublic static class Text\n{{\n    public static string quiet(string title) => @trim(title);\n}}\n"
        ));

        assert_eq!(
            issues(code),
            [
                "5:49 `@` hides PHP's warnings, and only the standard library uses it: handle the failure where it happens."
            ],
            "{namespace}"
        );
    }
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

    let php = "<?php\n\nnamespace App\\Tenant;\n\nclass Report\n{\n    private int $made = 1;\n    private int $count = $this->made;\n}\n";
    assert_eq!(
        issues_in("src/Report.php", php),
        ["8:26 Property `Report::$count` value contains a non-constant expression."]
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

/// Spec sections 6 and 29 write these fields with no access modifier. A field with no modifier is `private`, as its
/// PHP twin declares it, while `public` stays refused: a field is storage.
#[test]
fn a_field_without_an_access_modifier_compiles() {
    let section_6 = "namespace App.Shop;\n\nclass Checkout\n{\n    Map<string, Plan> cache = [:];\n}\n";
    let section_29 = "namespace App.Shop;\n\nclass Checkout\n{\n    Function<Money(Offer)> priceOf;\n    Function<Charge(Cart) uses Http> charge;\n}\n";
    let function_types = "namespace App.Shop;\n\nclass Checkout\n{\n    Function<Charge(Cart) uses Http> charge;\n    Function<Charge(Cart) uses Http>? fallback;\n    Map<string, Function<Charge(Cart) uses Http, Mail>> handlers;\n    Function<int(Function<int(int) uses Http>)> apply;\n}\n";
    let public = "namespace App.Shop;\n\nclass Checkout\n{\n    public Map<string, Plan> cache = [:];\n}\n";

    assert_eq!(issues(section_6), Vec::<String>::new());
    assert_eq!(issues(section_29), Vec::<String>::new());
    assert_eq!(issues(function_types), Vec::<String>::new());
    assert_eq!(issues(public), ["5:5 A PHP# field cannot be `public`: a field is `private` or `protected`."]);
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
        ["6:13 Property `Report.plain` cannot have type `void`.", "5:13 Type `void` cannot be nullable."]
    );

    let php = "<?php\n\nnamespace App\\Tenant;\n\nclass Report\n{\n    private ?void $nothing;\n    private void $plain;\n}\n";
    assert_eq!(
        issues_in("src/Report.php", php),
        ["8:13 Property `Report::$plain` cannot have type `void`.", "7:14 Type `void` cannot be nullable."]
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
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    const A = 1;\n    public const B = 1, C = 2;\n    final public const D = 1;\n    public const E = 2 <> 3;\n    public const iterable F = [];\n}\n";

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
fn an_auto_property_has_get_an_optional_narrower_set_or_init_and_an_optional_constant_initial_value() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int views { get; private set; } = 0;\n    public string name { get; set; } = \"none\";\n    public float rate { get; protected set; }\n    protected int total { get; private set; }\n    public int id { get; }\n    private int hidden { get; set; }\n    public string title { get; init; } = \"\";\n    public string label { get; private init; } = \"\";\n    public string slug { get; init => field = strtolower(value); } = \"\";\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn an_auto_property_breaking_the_accessor_rules_is_an_error() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int a { get; public set; }\n    private int b { get; private set; }\n    protected int c { get; public set; }\n    public int d { set; }\n    public int e { get; get; }\n    protected int f { get; public init; }\n    public int g { get; set; init; }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:25 The `set` accessor of a PHP# property must be narrower than the property.",
            "6:26 The `set` accessor of a PHP# property must be narrower than the property.",
            "7:28 The `set` accessor of a PHP# property must be narrower than the property.",
            "8:16 A PHP# property needs a `get` accessor.",
            "9:25 A PHP# property declares each accessor once.",
            "10:28 The `init` accessor of a PHP# property must be narrower than the property.",
            "11:30 A PHP# property is written by `set` or by `init`, not both.",
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
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    int a { get; set; }\n    public static int b { get; }\n    public int c { private get; set; }\n    public abstract int d { get; }\n    public override int e { get => 1; }\n    public int g = 0 { get; }\n    public int h { get; set(int value); }\n}\n";

    assert_eq!(
        issues(code),
        [
            "8:12 Property `Report.d` cannot be declared abstract",
            "5:9 A property without `public`, `protected` or `private` is not supported yet in PHP#.",
            "6:23 A get-only static property is not supported yet in PHP#.",
            "7:20 This accessor is not supported yet in PHP#.",
            "8:12 This modifier is not supported yet in PHP#.",
            "9:25 Overriding a property is not supported yet in PHP#.",
            "10:16 An initial value before the accessors is not supported yet in PHP#.",
            "11:25 This accessor is not supported yet in PHP#.",
        ]
    );

    let php = "<?php\n\nnamespace App\\Tenant;\n\nclass Report\n{\n    abstract public int $d { get; }\n}\n";
    assert_eq!(issues_in("src/Report.php", php), ["7:5 Property `Report::$d` cannot be declared abstract"]);
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
/// none, as does `init`, which runs as the `set` hook. A lambda in an accessor returns for itself.
#[test]
fn a_get_returns_a_value_and_a_set_returns_none_as_php_hooks_do() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int a { get { return; } }\n    public int b { get => field; set { return value; } }\n    public int c { get => field; set { const run = () => { return 1; }; return; } }\n    public int d { get { const run = () => { return; }; return 1; } }\n    public int e { get => field; init { return value; } }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:26 A `get` accessor must return a value.",
            "6:47 A `set` accessor must not return a value.",
            "9:48 An `init` accessor must not return a value.",
        ]
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

/// A constant initial value is the property's default, but any other would run the `set` or `init` body in the
/// constructor.
#[test]
fn an_initial_value_that_is_not_constant_on_a_property_whose_set_has_a_body_is_not_supported_yet() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public int a { get; set => field = value; } = strlen(\"x\");\n    public int b { get => field; set; } = strlen(\"x\");\n    public int c { get; set => field = value; } = 1;\n    public int d { get; init => field = value; } = strlen(\"x\");\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:51 An initial value that is not constant, on a property whose `set` has a body, is not supported yet in PHP#.",
            "8:52 An initial value that is not constant, on a property whose `init` has a body, is not supported yet in PHP#.",
        ]
    );
}

#[test]
fn a_static_property_with_an_accessor_body_is_not_supported_yet() {
    let code =
        "namespace App.Tenant;\n\nclass Report\n{\n    public static int a { get; set => field = value; } = 0;\n}\n";

    assert_eq!(issues(code), ["5:12 A static property with an accessor body is not supported yet in PHP#."]);
}

/// Spec section 6.1: `init` sets a property while its object is created, and no object creates a static property.
/// The help's `set;` and `private set;` compile.
#[test]
fn a_static_property_has_no_init() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    public static int count { get; init; } = 0;\n}\n";
    let fixed = "namespace App.Tenant;\n\nclass Report\n{\n    public static int count { get; set; } = 0;\n    public static int total { get; private set; } = 0;\n}\n";

    assert_eq!(
        issues(code),
        ["5:36 A static property has no `init`: `init` sets a property while its object is created."]
    );
    assert_eq!(issues(fixed), Vec::<String>::new());
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

    let php = "<?php\n\nnamespace App\\Tenant;\n\nclass Report\n{\n    public function run(int ...$values, int $extra): int\n    {\n        return $extra;\n    }\n\n    public function sum(int ...$values = 1): int\n    {\n        return 0;\n    }\n}\n";
    assert_eq!(
        issues_in("src/Report.php", php),
        [
            "7:45 Invalid parameter order: parameter `$extra` is defined after variadic parameter `$values`.",
            "12:40 Invalid parameter definition: variadic parameter `$values` cannot have a default value.",
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

    let php = leak(
        php_method("        return 1;\n")
            .replace("int $extra", "?int|string $a, int|?string $b, int|null $c, null|Calc $d"),
    );
    assert_eq!(
        issues_in("src/Report.php", php),
        ["5:26 Type `int|string` cannot be nullable.", "5:45 Type `?string` cannot be part of a union."]
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
    assert_eq!(
        issues_in("src/Report.php", "<?php\n\nfunction f(?(int|string) $x) {}\n"),
        ["3:13 Type `(int|string)` cannot be nullable."]
    );
}

#[test]
fn a_single_type_in_parentheses_with_a_question_mark_after_it_reports_only_the_php_errors() {
    let code = leak(method("        return 1;\n").replace("int extra", "(Calc)? extra"));

    assert_eq!(issues(code), ["5:20 Type `(Calc)` cannot be nullable.", "5:21 Type `Calc` cannot be parenthesized."]);
}

#[test]
fn exponentiation_and_its_compound_assignment_are_in_the_slice() {
    let code = leak(method("        let a = extra ** 2 ** -1;\n        a **= 2;\n        return a;\n"));

    assert_eq!(issues(code), Vec::<String>::new());
}

/// Spec section 19 gives flags `|`, `&`, `^`, `~`, `<<`, `>>` and their compound forms, and lists `%=`. The analyzer
/// checks their operands. A parameter default, which is a constant expression, takes them too.
#[test]
fn bitwise_operators_their_compound_assignments_and_modulo_assignment_are_in_the_slice() {
    let body = leak(method(
        "        let a = extra & 1 | extra ^ 2;\n        a = extra << 1 >> 2;\n        a = ~extra;\n        a &= 1;\n        a |= 2;\n        a ^= 4;\n        a <<= 1;\n        a >>= 1;\n        a %= 3;\n        return a;\n",
    ));
    let default = "class Report\n{\n    public int run(int mask = ~0 & 1 | 2 ^ 4 << 1 >> 1)\n    {\n        return mask;\n    }\n}\n";

    assert_eq!(issues(body), Vec::<String>::new());
    assert_eq!(issues(default), Vec::<String>::new());
}

#[test]
fn operators_outside_the_slice_are_not_supported_yet() {
    let code = leak(method(
        "        let a = extra;\n        a = @extra;\n        a = extra xor true;\n        a = extra and true;\n        a = extra or true;\n        a = extra <> 1;\n        return a;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "8:13 `@` hides PHP's warnings, and only the standard library uses it: handle the failure where it happens.",
            "9:19 This operator is not supported yet in PHP#.",
            "10:19 This operator is not supported yet in PHP#.",
            "11:19 This operator is not supported yet in PHP#.",
            "12:19 This operator is not supported yet in PHP#.",
        ]
    );
}

/// `<=>` orders two values, as spec section 19 writes it for numbers, strings and instances whose class declares
/// `operator <=>`. The analyzer checks its operands.
#[test]
fn spaceship_is_in_the_slice() {
    let code = leak(method("        return extra <=> 1;\n"));

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn a_member_name_written_as_an_expression_is_not_supported_yet() {
    let code = leak(method("        return this.{\"run\"}(extra);\n"));

    assert_eq!(issues(code), ["7:21 This member name is not supported yet in PHP#."]);
}

#[test]
fn types_outside_the_slice_are_not_supported_yet() {
    let code = "class Report\n{\n    public object run(iterable? a, int|iterable b, iterable c, callable d, (Lib&Other)|null e)\n    {\n        return 1;\n    }\n\n    public self make(Lib f, float g, bool h, string i)\n    {\n        return this;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:12 This type is not supported yet in PHP#.",
            "3:23 This type is not supported yet in PHP#.",
            "3:40 This type is not supported yet in PHP#.",
            "3:52 This type is not supported yet in PHP#.",
            "3:64 This type is not supported yet in PHP#.",
            "3:76 This type is not supported yet in PHP#.",
            "3:88 This union that holds null is not supported yet in PHP#.",
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
            "9:9 PHP# has no `mixed`: write `Any?`, or `Any` for a value that is never null.",
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

/// A type argument is nullable when written with `?`, as every type is, in a field as in a nullable field.
#[test]
fn a_type_argument_takes_a_question_mark_in_a_field() {
    let code = "class Report\n{\n    private Map<string, Any?> saved = [:];\n    public List<int?> sizes { get; private set; } = [];\n    private Map<string, List<Line?>> lines = [:];\n    private Map<string, mixed> old = [:];\n    private Map<string, Any?>? maybe = null;\n    private Map<string, Function<Any?(Any)>> handlers = [:];\n}\n";

    assert_eq!(issues(code), ["6:25 PHP# has no `mixed`: write `Any?`, or `Any` for a value that is never null."]);
}

#[test]
fn a_type_argument_takes_a_question_mark_in_a_signature_and_a_local() {
    let code = "namespace App.Tenant;\n\nimport Lib.Calc;\n\nclass Report\n{\n    public List<Calc?> run(List<Order?> orders, Map<string, List<int?>> sizes, Any value)\n    {\n        List<Calc?> calcs = [];\n        Map<string, int?> prices = [:];\n        List<(int|string)?> keys = [];\n        const List<int?> counts = [];\n        Function<int?(List<string?>)> first = (List<string?> names) => null;\n        return calcs;\n    }\n}\n";

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
fn a_type_with_type_arguments_other_than_list_map_or_set_is_not_supported_yet() {
    let code = "class Report\n{\n    public List<void> run(Set<string, int> a, List<int, int> b, Map<string> c, Paged<Line> d, Map<string, void> e)\n    {\n        return [];\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:12 This type is not supported yet in PHP#.",
            "3:27 This type is not supported yet in PHP#.",
            "3:47 This type is not supported yet in PHP#.",
            "3:65 This type is not supported yet in PHP#.",
            "3:80 This type is not supported yet in PHP#.",
            "3:95 This type is not supported yet in PHP#.",
        ]
    );
}

/// The first `Set` slice holds `int`, `string` and backed enums, which run as the keys of a PHP array. A named type
/// may be a backed enum, which only the analyzer knows, so it passes here.
#[test]
fn a_set_of_an_element_other_than_int_string_or_a_named_type_is_not_supported_yet() {
    let code = "class Report\n{\n    public Set<string> run(Set<int> a, Set<Status> b, Set<float> c, Set<int?> d, Set<List<int>> e)\n    {\n        return [];\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "3:55 This type is not supported yet in PHP#.",
            "3:69 This type is not supported yet in PHP#.",
            "3:82 This type is not supported yet in PHP#.",
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

/// A member of `typeof(X)` or of a local holding a class value is a static member of the class it holds, as `X.y` is,
/// so the analyzer checks that the class has it. `Class`'s own members, such as `attributes`, come with the library.
#[test]
fn a_member_of_a_class_value_is_in_the_slice() {
    let code = leak(method(
        "        const type = typeof(Order);\n        const max = typeof(Order).MAX + type.MAX;\n        type.attributes();\n        return extra;\n",
    ));

    assert_eq!(issues(code), Vec::<String>::new());
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
            "8:19 PHP# has no `(array)`: write a struct's `parse(extra)` for an object, or `List.wrap(extra)` for a value.",
            "9:19 PHP# has no `(object)`: write a `Map` literal, or a struct.",
        ]
    );
}

/// Spec section 24 replaces `(array)` with a struct's `parse` for an object and `List.wrap` for a value, and names a
/// bare operand, as its `(array)row` does.
#[test]
fn an_array_cast_is_an_error_that_names_parse_and_list_wrap() {
    let code = leak(
        method(
            "        const data = (array)row;\n        const items = (array)this.list();\n        const server = (array)_SERVER;\n        const version = (array)__VERSION__;\n        return 1;\n",
        )
        .replace("int extra", "int row"),
    );
    let value = "write a struct's `parse(value)` for an object, or `List.wrap(value)` for a value.";

    assert_eq!(
        issues(code),
        [
            "7:22 PHP# has no `(array)`: write a struct's `parse(row)` for an object, or `List.wrap(row)` for a value."
                .to_owned(),
            format!("8:23 PHP# has no `(array)`: {value}"),
            format!("9:24 PHP# has no `(array)`: {value}"),
            "9:31 PHP# has no superglobals; take a Request".to_owned(),
            format!("10:25 PHP# has no `(array)`: {value}"),
            "10:32 PHP# has no `__VERSION__`: `Position.current()` gives the file, directory, line, column and function."
                .to_owned(),
        ]
    );
}

#[test]
fn an_object_cast_is_an_error_that_names_a_map_literal_and_a_struct() {
    let code = leak(method("        const row = (object)extra;\n        return 1;\n"));

    assert_eq!(issues(code), ["7:21 PHP# has no `(object)`: write a `Map` literal, or a struct."]);
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
fn any_and_nullable_any_are_types_of_the_slice() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private Any last = 0;\n\n    public Any payload { get; set; }\n\n    public Any keep(Any value, Any? maybe = null)\n    {\n        Any? held = maybe;\n        const Any kept = value;\n        for (Any? step = held; ; ) {\n        }\n        return kept;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

#[test]
fn mixed_does_not_exist_in_php_sharp() {
    let code = "namespace App.Tenant;\n\nclass Report\n{\n    private mixed last = null;\n\n    public mixed keep(mixed value)\n    {\n        mixed? held = value;\n        return held;\n    }\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:13 PHP# has no `mixed`: write `Any?`, or `Any` for a value that is never null.",
            "7:12 PHP# has no `mixed`: write `Any?`, or `Any` for a value that is never null.",
            "7:23 PHP# has no `mixed`: write `Any?`, or `Any` for a value that is never null.",
            "9:9 PHP# has no `mixed`: write `Any?`, or `Any` for a value that is never null.",
        ]
    );
}

#[test]
fn an_interface_method_and_a_class_constant_take_any_and_never_mixed() {
    let code = "namespace App.Tenant;\n\ninterface Source\n{\n    mixed read(mixed key);\n\n    Any? first(Any key);\n}\n\nclass Report\n{\n    public const mixed NONE = 1;\n\n    public const Any SOME = 1;\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:5 PHP# has no `mixed`: write `Any?`, or `Any` for a value that is never null.",
            "5:16 PHP# has no `mixed`: write `Any?`, or `Any` for a value that is never null.",
            "12:18 PHP# has no `mixed`: write `Any?`, or `Any` for a value that is never null.",
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

/// Spec section 27: as a parameter's default, `Position.current()` gives the caller's position, which waits for
/// typed compilation. The engine would run it as the parameter's own position, so the checker refuses it, and PHP
/// compares method names ignoring case.
#[test]
fn position_current_as_a_parameter_default_is_not_supported_yet() {
    let code = "namespace App;\n\nclass Reports\n{\n    public static void logSlow(string message, Position caller = Position.current())\n    {\n    }\n}\n";
    let shouted = leak(code.replace("Position.current()", "Position.CURRENT()"));
    let signature = "namespace App;\n\ninterface Logs\n{\n    void log(Position caller = Position.current());\n}\n";
    let lambda =
        leak(method("        const log = (Position caller = Position.current()) => caller;\n        return extra;\n"));
    let refusal = "A `Position.current()` default is not supported yet in PHP#.";

    assert_eq!(issues(code), [format!("5:66 {refusal}")]);
    assert_eq!(issues(shouted), [format!("5:66 {refusal}")]);
    assert_eq!(issues(signature), [format!("5:32 {refusal}")]);
    assert_eq!(issues(lambda), [format!("7:40 {refusal}")]);
    assert_eq!(
        check("src/Report.sharp", code).into_iter().map(|issue| issue.notes).collect::<Vec<_>>(),
        [[
            "As a parameter's default, `Position.current()` gives the caller's position, which waits for typed compilation."
        ]]
    );
}

/// In a body and in an initial value that runs in the constructor, `Position.current()` gives the position where it
/// is written, which the bridge lowers.
#[test]
fn position_current_in_a_body_or_an_initial_value_is_in_the_slice() {
    let code = "namespace App;\n\nclass Reports\n{\n    private Position created = Position.current();\n    public Position made { get; } = Position.current();\n\n    public string stubsFolder()\n    {\n        const stubs = Position.current().directory + \"/stubs\";\n        return stubs;\n    }\n}\n";

    assert_eq!(issues(code), Vec::<String>::new());
}

/// An imported or a declared `Position` is not the standard library's, so its `current()` default keeps the refusal
/// of any call in a constant expression.
#[test]
fn a_current_default_of_an_imported_or_declared_position_keeps_the_constant_expression_refusal() {
    let imported = "namespace App;\n\nimport App.Shared.Position;\n\nclass Reports\n{\n    public static void logSlow(Position caller = Position.current())\n    {\n    }\n}\n";
    let declared = "namespace App;\n\nclass Reports\n{\n    public static void logSlow(Position caller = Position.current())\n    {\n    }\n}\n\nclass Position\n{\n}\n";

    assert_eq!(issues(imported), ["7:50 This expression is not supported yet in PHP#."]);
    assert_eq!(issues(declared), ["5:50 This expression is not supported yet in PHP#."]);
}

/// A class constant's value and an enum case's value are constant expressions as a default is, and keep the refusal of
/// any call there. PHP's own check reports the constant's value too.
#[test]
fn position_current_as_a_constant_or_an_enum_case_value_keeps_the_constant_expression_refusal() {
    let code = "namespace App;\n\nclass Reports\n{\n    public const Position HERE = Position.current();\n}\n\nenum Spot : string\n{\n    case Here = Position.current();\n}\n";

    assert_eq!(
        issues(code),
        [
            "5:34 Constant `Reports.HERE` value contains a non-constant expression.",
            "5:34 This expression is not supported yet in PHP#.",
            "10:17 This expression is not supported yet in PHP#.",
        ]
    );

    let php = "<?php\n\nnamespace App;\n\nclass Reports\n{\n    public const Position HERE = Position::current();\n}\n\nenum Spot: string\n{\n    case Here = Position::current();\n}\n";
    assert_eq!(
        issues_in("src/Report.php", php),
        ["7:34 Constant `Reports::HERE` value contains a non-constant expression."]
    );
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
    let code = "namespace App.Tenant;\n\nenum Status\n{\n    case Active;\n\n    public bool active()\n    {\n        return this === Active && label != \"\";\n    }\n\n    public string label() => this.name;\n\n    public static Status first() => Active;\n}\n";

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

/// Spec section 19 groups `< <= > >= is as` in one row and `== != === <=>` in the next, and neither row chains.
#[test]
fn comparisons_and_equalities_do_not_chain_and_name_both_groupings() {
    let code = leak(method(
        "        const a = extra < 1 < 2;\n        const b = extra == 1 != 2;\n        const c = extra is int is bool;\n        const d = extra < 1 is bool;\n        const e = extra is > 1 < 2;\n        const f = extra < 1 == true;\n        const g = extra == 1 is bool;\n        const h = (extra < 1) < 2;\n        match (extra) {\n            > 1 < 2 => this.run(1),\n            default => this.run(2),\n        }\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:19 Comparisons do not chain: write `(extra < 1) < 2` or `extra < (1 < 2)`.",
            "8:19 Comparisons do not chain: write `(extra == 1) != 2` or `extra == (1 != 2)`.",
            "9:19 Comparisons do not chain: write `(extra is int) is bool`.",
            "10:19 Comparisons do not chain: write `(extra < 1) is bool` or `extra < (1 is bool)`.",
            "11:19 Comparisons do not chain: write `extra is > (1 < 2)`.",
            "16:13 Comparisons do not chain: write `> (1 < 2)`.",
        ]
    );
}

/// Decision 044: `is` binds with the comparisons, so `!entity is HasDesign` reads as `(!entity) is HasDesign`.
#[test]
fn not_before_is_is_an_error_that_writes_is_not() {
    let code = leak(method(
        "        if (!extra is int) {\n        }\n        if ((!extra) is bool) {\n        }\n        if (!(extra is int)) {\n        }\n        if (extra is not int) {\n        }\n        return extra;\n",
    ));

    assert_eq!(issues(code), ["7:13 Write `extra is not int`: `!` applies to `extra` before `is` tests it."]);
}

/// Decision 045: `not` beside `or` reads two ways, and `not` beside `and` reads the way it binds.
#[test]
fn not_beside_or_in_a_pattern_needs_parentheses_and_not_beside_and_does_not() {
    let code = leak(method(
        "        const a = extra is not Paid or Refunded;\n        const b = extra is Paid or not Refunded;\n        const c = match (extra) {\n            not Paid or Refunded => 1,\n            default => 0,\n        };\n        const d = extra is not (Paid or Refunded);\n        const e = extra is (not Paid) or Refunded;\n        const f = extra is not null and not \"\";\n        return extra;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "7:28 Write `not (Paid or Refunded)`, or `(not Paid) or Refunded`.",
            "8:28 Write `not (Paid or Refunded)`, or `Paid or (not Refunded)`.",
            "10:13 Write `not (Paid or Refunded)`, or `(not Paid) or Refunded`.",
        ]
    );
}

#[test]
fn a_binary_operation_as_a_statement_has_no_effect() {
    let code = leak(method(
        "        let flags = extra;\n        flags | 4;\n        extra + 1;\n        extra == 1;\n        1 - extra;\n        extra ?? 1;\n        this.run(1);\n        return flags;\n",
    ));

    assert_eq!(
        issues(code),
        [
            "8:9 This statement has no effect: write `flags |= 4` to keep its result.",
            "9:9 This statement has no effect: write `extra += 1` to keep its result.",
            "10:9 This statement has no effect: use the result of `extra == 1`, or remove the statement.",
            "11:9 This statement has no effect: use the result of `1 - extra`, or remove the statement.",
        ]
    );
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

/// Every semantic issue in the source, in the dialect its path names, as its message and its annotations' messages.
fn worded(path: &'static str, code: &'static str) -> Vec<String> {
    check(path, code)
        .into_iter()
        .map(|issue| {
            std::iter::once(issue.message)
                .chain(issue.annotations.into_iter().filter_map(|annotation| annotation.message))
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .collect()
}

/// A member defined twice is named as PHP# names a member, `Order.total`, and its class by the name the file writes,
/// `Order`. The PHP twin keeps Mago's `Order::total`, `Order::$count` and `Demo\Order`.
#[test]
fn a_member_defined_twice_and_its_class_are_named_as_sharp_writes_them() {
    let code = "namespace Demo;\n\npublic class Order\n{\n    private int count = 0;\n\n    private int count = 1;\n\n    public int total() => 1;\n\n    public int total() => 2;\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public int $count = 0;\n\n    public int $count = 1;\n\n    public function total(): int\n    {\n        return 1;\n    }\n\n    public function total(): int\n    {\n        return 2;\n    }\n}\n";

    assert_eq!(
        worded("src/Demo/Order.php", php),
        [
            "property `Order::$count` has already been defined | property `Order::$count` previously defined here. | class `Demo\\Order` defined here.",
            "class method `Order::total` has already been defined | previous definition | class `Demo\\Order` defined here.",
        ]
    );
    assert_eq!(
        worded("src/Demo/Order.sharp", code),
        [
            "property `Order.count` has already been defined | property `Order.count` previously defined here. | class `Order` defined here.",
            "class method `Order.total` has already been defined | previous definition | class `Order` defined here.",
        ]
    );
}

/// A construct outside the slice is refused with a note that lists what PHP# has where it is written, so the note of
/// a refused class member names a class's members and the note of a refused expression names a body's expressions.
#[test]
fn a_refusal_notes_what_php_sharp_has_where_it_is_written() {
    let class = "namespace App;\n\npublic class Order\n{\n    case Open;\n}\n";
    let body = leak(method("        const copy = clone this;\n        return extra;\n"));

    for (code, note) in [
        (
            class,
            "A PHP# class has attributes, an optional `public`, `abstract` or `final`, a name, an optional `: Base, Interface` header, constants, fields, properties, methods, named constructors, operators, laws and, in a static class, extensions, with no other modifiers, `extends` or `implements`.",
        ),
        (
            body,
            "In a method body, PHP# supports blocks, expression statements, `return`, `let` and `const` and typed locals, `if` with `else if` and `else`, `while`, `do … while`, `for`, `for … of`, `break` and `continue` without a level, and `try` with `catch` and `finally`, with literals, list and map literals, index reads, templates, parentheses, bare names, assignment, arithmetic, comparison and logical operators, `??`, the ternary `c ? a : b`, `(int)`, `(float)` and `(string)`, `++` and `--`, method calls and property reads written with `.` or `?.`, lambdas as in `x => x.id`, `new Class(...)`, `new Class.name(...)` and, with a `required` constructor, `new Self(...)`, calls of global functions and of a local that holds a lambda, `super.method(...)` and `Self.method(...)`, each with positional, named and spread arguments as in `max(...prices)`, `throw`, `exit(code)`, `typeof(Class)`, `is`, `as` and `match`.",
        ),
    ] {
        assert_eq!(
            check("src/Report.sharp", code).into_iter().flat_map(|issue| issue.notes).collect::<Vec<_>>(),
            [note],
            "{code}"
        );
    }
}

/// A class-level error names the class by the name the file writes: `Order` in a `.sharp` file, and its full name
/// `Demo\Order` in the PHP twin.
#[test]
fn a_class_error_names_the_class_as_sharp_writes_it() {
    let code = "namespace Demo;\n\npublic class Order\n{\n    case Open;\n\n    public abstract int total();\n}\n";
    let php =
        "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    case Open;\n\n    abstract public function total(): int;\n}\n";

    assert_eq!(
        worded("src/Demo/Order.php", php),
        [
            "Class `Order` cannot contain enum cases. | Enum case found in class. | Class `Demo\\Order` declared here.",
            "Class `Order` contains an abstract method `total`, so the class must be declared abstract. | Class is missing the `abstract` modifier. | Abstract method `Order::total` declared here.",
        ]
    );
    assert_eq!(
        worded("src/Demo/Order.sharp", code),
        [
            "Class `Order` cannot contain enum cases. | Enum case found in class. | Class `Order` declared here.",
            "Class `Order` contains an abstract method `total`, so the class must be declared abstract. | Class is missing the `abstract` modifier. | Abstract method `Order.total` declared here.",
            "This class member is not supported yet in PHP#. | Not supported yet.",
        ]
    );
}

/// Spec section 14 gives no form that calls a lambda where it is written, and no form that reads a member of a `new`
/// expression, so the errors for those PHP forms name no code to write in a `.sharp` file. The PHP twin keeps
/// upstream's examples.
#[test]
fn an_error_for_a_php_form_without_a_sharp_form_names_no_php_code() {
    let code = "namespace Demo;\n\nclass Order\n{\n    public int run()\n    {\n        const order = new Order[0]();\n        return () => { return 1; }();\n    }\n}\n";
    let php = "<?php\n\nnamespace Demo;\n\nclass Order\n{\n    public function run(): int\n    {\n        $order = new Order[0]();\n        return function () { return 1; }();\n    }\n}\n";
    let helps = |path, code| -> Vec<(String, Option<String>)> {
        check(path, code).into_iter().map(|issue| (issue.message, issue.help)).collect()
    };

    assert_eq!(
        helps("src/Demo/Order.php", php),
        [
            (
                "`[` cannot follow a class name in a `new` expression.".to_owned(),
                Some("Wrap the new expression in parentheses, e.g. `(new Foo())->bar()`.".to_owned())
            ),
            (
                "Immediately invoked closure must be wrapped in parentheses.".to_owned(),
                Some("Wrap the closure in parentheses before invoking it, e.g. `(function() { ... })()`.".to_owned())
            ),
        ]
    );
    assert_eq!(
        helps("src/Demo/Order.sharp", code),
        [
            ("This expression is not supported yet in PHP#.".to_owned(), None),
            ("`[` cannot follow a class name in a `new` expression.".to_owned(), None),
            ("This expression is not supported yet in PHP#.".to_owned(), None),
            ("Immediately invoked closure must be wrapped in parentheses.".to_owned(), None),
        ]
    );
}

/// A PHP# class `Money`, for `src/Money.sharp`, whose members start on line 5.
fn money(members: &str) -> &'static str {
    leak(format!("namespace App;\n\npublic class Money\n{{\n{members}}}\n"))
}

#[test]
fn a_class_declares_each_operator_public_static_with_its_own_class_as_a_parameter() {
    let code = money(
        "    [Pure] public static bool operator ==(Money a, Money? b) => true;\n    static public int operator <=>(Money a, Money b) => 0;\n    public static Money operator +(Money a, Money b) => a;\n    public static Money operator -(Money a, Money b) => a;\n    public static Money operator *(Money a, int factor) => a;\n    public static Money operator /(int a, Money b) => b;\n    public static Money operator %(Money a, int b) => a;\n    public static Money operator **(Money a, int b) => a;\n    public static Money operator -(Money a)\n    {\n        return a;\n    }\n",
    );

    assert_eq!(issues_in("src/Money.sharp", code), Vec::<String>::new());
}

#[test]
fn an_operator_a_class_cannot_declare_is_an_error_that_names_the_operators_it_derives_from() {
    let code = money(
        "    public static bool operator !=(Money a, Money b) => false;\n    public static bool operator <(Money a, Money b) => false;\n    public static bool operator >=(Money a, Money b) => false;\n    public static bool operator &&(Money a, Money b) => false;\n",
    );

    assert_eq!(
        issues_in("src/Money.sharp", code),
        [
            "5:33 `operator !=` cannot be declared: it is derived from `==`.",
            "6:33 `operator <` cannot be declared: it is derived from `<=>`.",
            "7:33 `operator >=` cannot be declared: it is derived from `<=>`.",
            "8:33 `operator &&` cannot be declared: only `+ - * / % **`, unary `-`, `==` and `<=>` can.",
        ]
    );
}

#[test]
fn an_operator_that_is_not_public_static_is_an_error() {
    let code = money(
        "    public Money operator +(Money a, Money b) => a;\n    private static Money operator -(Money a, Money b) => a;\n    static Money operator *(Money a, Money b) => a;\n",
    );

    let message = "An operator is `public static`, as in `public static Money operator +(Money a, Money b)`.";
    assert_eq!(
        issues_in("src/Money.sharp", code),
        [format!("5:18 {message}"), format!("6:26 {message}"), format!("7:18 {message}")]
    );
}

#[test]
fn an_operator_with_the_wrong_number_of_parameters_is_an_error() {
    let code = money(
        "    public static bool operator ==(Money a) => false;\n    public static Money operator +(Money a, Money b, Money c) => a;\n    public static Money operator -() => null;\n",
    );

    assert_eq!(
        issues_in("src/Money.sharp", code),
        [
            "5:35 `operator ==` takes two parameters.",
            "6:35 `operator +` takes two parameters.",
            "7:35 `operator -` takes one parameter, to negate, or two, to subtract.",
        ]
    );
}

#[test]
fn an_operator_without_its_class_as_a_parameter_is_an_error() {
    let code = money("    public static int operator +(int a, int b) => a + b;\n");

    assert_eq!(
        issues_in("src/Money.sharp", code),
        ["5:33 One parameter of `operator +` is `Money`, the class that declares it."]
    );
}

#[test]
fn equality_returns_bool_and_comparison_returns_int() {
    let code = money(
        "    public static int operator ==(Money a, Money b) => 1;\n    public static bool? operator <=>(Money a, Money b) => true;\n",
    );

    assert_eq!(
        issues_in("src/Money.sharp", code),
        ["5:19 `operator ==` returns `bool`.", "6:19 `operator <=>` returns `int`."]
    );
}

#[test]
fn an_operator_declared_twice_in_a_class_is_an_error() {
    let code = money(
        "    public static Money operator +(Money a, Money b) => a;\n    public static Money operator -(Money a) => a;\n    public static Money operator -(Money a, Money b) => a;\n    public static Money operator +(Money a, int b) => a;\n    public static Money operator -(Money a) => a;\n",
    );

    assert_eq!(
        issues_in("src/Money.sharp", code),
        ["8:25 `operator +` is declared twice in `Money`.", "9:25 Unary `operator -` is declared twice in `Money`."]
    );
}

#[test]
fn an_operator_body_is_a_method_body() {
    let code = money(
        "    public static Money operator +(Money a, Money b)\n    {\n        echo \"adding\";\n        return a;\n    }\n",
    );

    assert_eq!(issues_in("src/Money.sharp", code), ["7:9 PHP# has no `echo`: write `printf` or `fwrite`."]);
}

#[test]
fn an_interface_or_an_enum_declares_no_operator() {
    let interface =
        "namespace App;\n\npublic interface Priced\n{\n    static int operator +(Priced a, Priced b) => 0;\n}\n";
    let r#enum = "namespace App;\n\npublic enum Suit\n{\n    case Hearts;\n\n    public static int operator +(Suit a, Suit b) => 0;\n}\n";

    assert_eq!(issues_in("src/Priced.sharp", interface), ["5:5 This class member is not supported yet in PHP#."]);
    assert_eq!(issues_in("src/Suit.sharp", r#enum), ["7:5 This class member is not supported yet in PHP#."]);
}
