#![allow(clippy::panic, clippy::expect_used, clippy::single_char_lifetime_names)]

use std::borrow::Cow;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_php_version::PHPVersion;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::*;
use mago_syntax::dialect::Dialect;
use mago_syntax::error::ParseError;
use mago_syntax::parser::parse_file;
use mago_syntax::parser::parse_file_with_dialect;
use mago_syntax::settings::ParserSettings;

fn parse<'arena>(arena: &'arena LocalArena, name: &'static str, code: &'static str) -> &'arena Program<'arena> {
    let file = File::ephemeral(Cow::Borrowed(name.as_bytes()), Cow::Borrowed(code.as_bytes()));

    parse_file(arena, &file)
}

#[test]
fn sharp_file_starts_in_code() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Demo/Report.sharp", "namespace Demo;\n");

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    assert_eq!(program.dialect, Dialect::Sharp);
    assert!(matches!(program.statements.first(), Some(Statement::Namespace(_))), "{:#?}", program.statements);
}

#[test]
fn a_caller_parses_a_file_in_the_dialect_it_names_whatever_the_file_name() {
    let arena = LocalArena::new();
    let sharp = File::ephemeral(Cow::Borrowed(b"Report.php"), Cow::Borrowed(b"namespace App.Tenant;\n"));
    let php = File::ephemeral(Cow::Borrowed(b"Report.sharp"), Cow::Borrowed(b"<?php namespace App\\Tenant;\n"));

    let sharp_program = parse_file_with_dialect(&arena, &sharp, Dialect::Sharp, ParserSettings::default());
    let php_program = parse_file_with_dialect(&arena, &php, Dialect::Php, ParserSettings::default());

    assert!(sharp_program.errors.is_empty(), "{:#?}", sharp_program.errors);
    assert_eq!(sharp_program.dialect, Dialect::Sharp);
    assert!(php_program.errors.is_empty(), "{:#?}", php_program.errors);
    assert_eq!(php_program.dialect, Dialect::Php);
}

fn source<'a>(code: &'a str, node: &impl HasSpan) -> &'a str {
    let span = node.span();

    &code[span.start.offset as usize..span.end.offset as usize]
}

#[test]
fn namespace_and_import_take_dotted_names() {
    const CODE: &str = "namespace App.Tenant.Store;\n\nimport App.Shared.Money;\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Store.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(Statement::Namespace(namespace)) = program.statements.first() else {
        panic!("expected a namespace, got {:#?}", program.statements);
    };
    let name = namespace.name.expect("a namespace name");
    assert!(matches!(name, Identifier::Dotted(_)), "{name:?}");
    assert_eq!(name.value(), b"App.Tenant.Store");
    assert_eq!(source(CODE, &name), "App.Tenant.Store");

    let Some(Statement::Use(import)) = namespace.statements().first() else {
        panic!("expected an import, got {:#?}", namespace.statements());
    };
    assert_eq!(import.r#use.value, b"import");
    let UseItems::Sequence(items) = &import.items else {
        panic!("expected a plain import, got {:#?}", import.items);
    };
    let imported = items.items.first().expect("one imported class").name;
    assert!(matches!(imported, Identifier::Dotted(_)), "{imported:?}");
    assert_eq!(imported.value(), b"App.Shared.Money");
    assert_eq!(source(CODE, &imported), "App.Shared.Money");
}

/// Asserts that `code`, the file at `path`, parses to one import of `written` renamed to `Rx`.
fn assert_renamed_import(path: &'static str, code: &'static str, written: &str) {
    let arena = LocalArena::new();
    let program = parse(&arena, path, code);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(Statement::Namespace(namespace)) =
        program.statements.iter().find(|statement| matches!(statement, Statement::Namespace(_)))
    else {
        panic!("expected a namespace, got {:#?}", program.statements);
    };
    let Some(Statement::Use(Use { items: UseItems::Sequence(items), .. })) = namespace.statements().first() else {
        panic!("expected one plain import, got {:#?}", namespace.statements());
    };
    let item = items.items.first().expect("one imported class");
    let alias = item.alias.as_ref().expect("an alias");

    assert_eq!(source(code, &item.name), written, "the imported name");
    assert_eq!(alias.r#as.value, b"as", "the keyword");
    assert_eq!(source(code, &alias.identifier), "Rx", "the alias");
}

/// Spec section 23 renames an import with `as`, into the alias node PHP's `use … as` has.
#[test]
fn an_import_renamed_with_as_holds_its_dotted_name_and_its_alias() {
    assert_renamed_import("src/App.sharp", "namespace App;\n\nimport Sharp.Text.Regex as Rx;\n", "Sharp.Text.Regex");
}

#[test]
fn a_php_use_renamed_with_as_holds_its_name_and_its_alias() {
    assert_renamed_import(
        "src/App.php",
        "<?php\n\nnamespace App;\n\nuse Sharp\\Text\\Regex as Rx;\n",
        "Sharp\\Text\\Regex",
    );
}

#[test]
fn dotted_name_with_a_space_is_a_parse_error() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Store.sharp", "namespace App. Tenant;\n");

    assert!(!program.errors.is_empty());
}

fn class_members<'arena>(program: &'arena Program<'arena>) -> &'arena Sequence<'arena, ClassLikeMember<'arena>> {
    let Some(Statement::Class(class)) = program.statements.first() else {
        panic!("expected a class, got {:#?}", program.statements);
    };

    &class.members
}

fn method_body<'arena>(program: &'arena Program<'arena>) -> &'arena [Statement<'arena>] {
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let MethodBody::Concrete(body) = &method.body else {
        panic!("expected a method body, got {:#?}", method.body);
    };

    body.statements.as_slice()
}

fn expression<'arena>(statement: &'arena Statement<'arena>) -> &'arena Expression<'arena> {
    match statement {
        Statement::Expression(statement) => statement.expression,
        Statement::Return(Return { value: Some(value), .. }) => value,
        _ => panic!("expected an expression, got {statement:#?}"),
    }
}

fn bare_name<'arena>(expression: &'arena Expression<'arena>) -> &'arena [u8] {
    let Expression::ConstantAccess(access) = expression else {
        panic!("expected a bare name, got {expression:#?}");
    };

    access.name.value()
}

#[test]
fn method_puts_its_return_type_first_without_a_function_keyword() {
    const CODE: &str =
        "class Report\n{\n    public static int total(int extra)\n    {\n        return extra;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    assert_eq!(method.function, None);
    assert_eq!(method.name.value, b"total");
    assert!(method.is_static());
    let return_type = method.return_type_hint.as_ref().expect("a return type");
    assert_eq!(return_type.colon, None);
    assert_eq!(source(CODE, return_type), "int");
    assert_eq!(source(CODE, method), &CODE[19..CODE.len() - 3]);

    let parameter = method.parameter_list.parameters.first().expect("one parameter");
    assert_eq!(parameter.variable.name, b"extra");
    assert_eq!(source(CODE, &parameter.variable), "extra");
    assert_eq!(source(CODE, parameter), "int extra");
}

#[test]
fn void_method_without_modifiers_starts_at_its_return_type() {
    const CODE: &str = "class Report\n{\n    void run() {}\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    assert_eq!(source(CODE, method), "void run() {}");
}

#[test]
fn a_constructor_is_a_method_named_after_its_class_without_a_return_type() {
    const CODE: &str = "class Report\n{\n    public Report(private int count, int extra) {}\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(constructor)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    assert_eq!(constructor.function, None);
    assert_eq!(constructor.return_type_hint, None);
    assert_eq!(source(CODE, &constructor.name), "Report");
    assert_eq!(source(CODE, constructor), "public Report(private int count, int extra) {}");
    let [count, extra] = constructor.parameter_list.parameters.as_slice() else {
        panic!("expected two parameters, got {:#?}", constructor.parameter_list);
    };
    assert_eq!(source(CODE, count), "private int count");
    assert_eq!(source(CODE, extra), "int extra");
}

#[test]
fn a_method_with_a_long_return_type_parses() {
    const CODE: &str = "class Report\n{\n    public static int|string|null total() { return 1; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    assert_eq!(source(CODE, method.return_type_hint.as_ref().expect("a return type")), "int|string|null");
}

#[test]
fn a_by_reference_parameter_is_a_parse_error() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", "class Report\n{\n    public void fill(int &count) {}\n}\n");

    assert!(!program.errors.is_empty());
}

#[test]
fn an_expression_bodied_method_is_its_arrow_its_expression_and_a_semicolon() {
    const CODE: &str = "class Report\n{\n    public int total() => this.count + 1;\n\n    public void touch() => this.save();\n\n    public Report(int count) => this.count = count;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let bodies: Vec<_> = class_members(program)
        .iter()
        .map(|member| {
            let ClassLikeMember::Method(method) = member else {
                panic!("expected a method, got {member:#?}");
            };
            let MethodBody::Expression(body) = &method.body else {
                panic!("expected an expression body, got {:#?}", method.body);
            };

            (source(CODE, method), source(CODE, &method.body), source(CODE, body.expression))
        })
        .collect();

    assert_eq!(
        bodies,
        [
            ("public int total() => this.count + 1;", "=> this.count + 1;", "this.count + 1"),
            ("public void touch() => this.save();", "=> this.save();", "this.save()"),
            ("public Report(int count) => this.count = count;", "=> this.count = count;", "this.count = count"),
        ]
    );
}

#[test]
fn a_computed_property_is_its_type_its_name_and_an_expression_body() {
    const CODE: &str = "class Report\n{\n    [Shown] public string slug => Str.slug(this.name);\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Property(Property::Computed(property))) = class_members(program).first() else {
        panic!("expected a computed property, got {:#?}", class_members(program));
    };
    assert_eq!(property.variable.name, b"slug");
    assert_eq!(source(CODE, property.hint.as_ref().expect("a type")), "string");
    assert_eq!(source(CODE, &property.body), "=> Str.slug(this.name);");
    assert_eq!(source(CODE, property.body.expression), "Str.slug(this.name)");
    assert_eq!(source(CODE, property), "[Shown] public string slug => Str.slug(this.name);");
}

#[test]
fn php_keeps_refusing_an_arrow_after_a_method_or_a_property() {
    for member in ["public function total(): int => 1;", "public int $total => 1;"] {
        let arena = LocalArena::new();
        let code: &'static str = Box::leak(format!("<?php class Report {{ {member} }}").into_boxed_str());
        let program = parse(&arena, "src/Report.php", code);

        assert!(!program.errors.is_empty(), "{member}");
    }
}

/// `required`, `via` and a named constructor are spec syntax outside the slice. Each one is a
/// single error where it starts, and the class around it still parses.
#[test]
fn spec_syntax_outside_the_slice_is_one_not_supported_error_where_it_starts() {
    for (member, message, start) in [
        ("public required int count { get; set; }", "`required` is not supported yet in PHP#.", "required"),
        ("public string name { get; set; } via Trimmed, Tracked;", "`via` is not supported yet in PHP#.", "via"),
        (
            "public Report(public int id { get; } via Tracked, int other) {}",
            "`via` is not supported yet in PHP#.",
            "via",
        ),
        (
            "public Report.fromJson(string json) : this(json.length) {}",
            "A named constructor is not supported yet in PHP#.",
            "Report.fromJson",
        ),
        (
            "public Report make() { return new Report.fromJson(\"{}\"); }",
            "A named constructor is not supported yet in PHP#.",
            "Report.fromJson",
        ),
    ] {
        let arena = LocalArena::new();
        let code: &'static str = Box::leak(
            format!("class Report\n{{\n    {member}\n\n    public int run() {{ return 1; }}\n}}\n").into_boxed_str(),
        );
        let program = parse(&arena, "src/Report.sharp", code);

        let [error] = program.errors else {
            panic!("expected one error for `{member}`, got {:#?}", program.errors);
        };
        assert_eq!(error.to_string(), message, "{member}");
        assert_eq!(source(code, error), start, "{member}");
        let Some(Statement::Class(class)) = program.statements.first() else {
            panic!("expected a class for `{member}`, got {:#?}", program.statements);
        };
        assert!(
            class
                .members
                .iter()
                .any(|member| matches!(member, ClassLikeMember::Method(run) if run.name.value == b"run")),
            "the class keeps parsing after `{member}`"
        );
    }
}

/// A case that carries data, spec section 20, is one error from its name to its closing parenthesis. The case stays
/// in the enum, and the members after it still parse. PHP keeps its own parse error at the parenthesis.
#[test]
fn a_case_that_carries_data_is_one_not_supported_error_and_the_enum_parses_on() {
    const CODE: &str = "enum PaymentResult\n{\n    case Open;\n    case Paid(string transactionId, int cents);\n\n    public int run() { return 1; }\n}\n";
    let arena = LocalArena::new();

    let php_code: &'static str = Box::leak(format!("<?php {CODE}").into_boxed_str());
    let php = parse(&arena, "src/PaymentResult.php", php_code);
    let Some(php_error) = php.errors.first() else {
        panic!("expected a PHP error, got none");
    };
    assert_eq!(source(php_code, php_error), "(");
    assert!(
        !php.errors.iter().any(|error| matches!(error, ParseError::NotSupportedYetInSharp(..))),
        "{:#?}",
        php.errors
    );

    let program = parse(&arena, "src/PaymentResult.sharp", CODE);
    let [error] = program.errors else {
        panic!("expected one error, got {:#?}", program.errors);
    };
    assert_eq!(error.to_string(), "A case that carries data is not supported yet in PHP#.");
    assert_eq!(source(CODE, error), "Paid(string transactionId, int cents)");
    let Some(Statement::Enum(payment_result)) = program.statements.first() else {
        panic!("expected an enum, got {:#?}", program.statements);
    };
    let [ClassLikeMember::EnumCase(open), ClassLikeMember::EnumCase(paid), ClassLikeMember::Method(run)] =
        payment_result.members.as_slice()
    else {
        panic!("expected the cases `Open` and `Paid` and the method `run`, got {:#?}", payment_result.members);
    };
    assert_eq!(open.item.name().value, b"Open");
    assert!(matches!(paid.item, EnumCaseItem::Unit(_)), "{:#?}", paid.item);
    assert_eq!(paid.item.name().value, b"Paid");
    assert_eq!(run.name.value, b"run");
}

/// A PHP#-only parse error is its own message, so `mago analyze` shows the rule as the issue's title.
#[test]
fn a_sharp_parse_error_shows_its_message_as_the_issue_title() {
    for (code, message) in [
        (
            "class Report\n{\n    public int run() { return this->total; }\n}\n",
            "`->` is PHP syntax: PHP# writes member access with `.`",
        ),
        (
            "class Report\n{\n    public int run() { return \\Lib\\Calc.make(); }\n}\n",
            "A `\\` name is PHP syntax: add `import Lib.Calc;` and write `Calc`",
        ),
        (
            "class Report\n{\n    public int run(extra) { return 1; }\n}\n",
            "A PHP# parameter needs a type, as in `int extra`.",
        ),
        ("class Report\n{\n    private count = 0;\n}\n", "A PHP# field needs a type, as in `private int count = 0;`."),
        (
            "class Report\n{\n    public required int count { get; set; }\n}\n",
            "`required` is not supported yet in PHP#.",
        ),
        (
            "class Report\n{\n    public void run() { for (const line in lines) {} }\n}\n",
            "PHP# loops over a collection with `of`, as in `for (const line of lines)`.",
        ),
        (
            "class Report\n{\n    public void run() { const check = handler ?? item => item.ready; }\n}\n",
            "A lambda after an operator needs parentheses, as in `handler ?? (item => item.ready)`.",
        ),
        (
            "class Report\n{\n    public void run() { const check = handler ?? (item) => { return item.ready; }; }\n}\n",
            "A lambda after an operator needs parentheses, as in `handler ?? (item => item.ready)`.",
        ),
        (
            "class Report\n{\n    public void run() { match (status) { Status.Open when strict ? forced : ready => {}, default => {}, } }\n}\n",
            "A `? :` in a `when` condition needs parentheses, as in `when (strict ? forced : ready) =>`.",
        ),
    ] {
        let arena = LocalArena::new();
        let program = parse(&arena, "src/Report.sharp", code);

        let [error] = program.errors else {
            panic!("expected one error for `{code}`, got {:#?}", program.errors);
        };
        assert_eq!(Issue::from(error).message, message, "{code}");
    }
}

#[test]
fn a_parameter_without_a_type_is_a_parse_error_that_names_the_rule() {
    for parameters in ["extra", "extra, int other", "extra = 1"] {
        let arena = LocalArena::new();
        let code: &'static str = Box::leak(
            format!("class Report\n{{\n    public int run({parameters}) {{ return 1; }}\n}}\n").into_boxed_str(),
        );
        let program = parse(&arena, "src/Report.sharp", code);

        let [error] = program.errors else {
            panic!("expected one error for `{parameters}`, got {:#?}", program.errors);
        };
        assert_eq!(error.to_string(), "A PHP# parameter needs a type, as in `int extra`.", "{parameters}");
        assert_eq!(source(code, error), "extra", "{parameters}");
    }
}

/// A field always has a type, an override of a plain PHP parent's property too, spec section 6.1. A bare name before
/// `=` or `;` is the field's name, and the class keeps parsing.
#[test]
fn a_field_without_a_type_is_a_parse_error_that_names_the_rule() {
    for (field, name) in [("protected override table = \"orders\";", "table"), ("private count;", "count")] {
        let arena = LocalArena::new();
        let code: &'static str = Box::leak(
            format!("class Order\n{{\n    {field}\n\n    public int run() {{ return 1; }}\n}}\n").into_boxed_str(),
        );
        let program = parse(&arena, "src/Order.sharp", code);

        let [error] = program.errors else {
            panic!("expected one error for `{field}`, got {:#?}", program.errors);
        };
        assert_eq!(error.to_string(), "A PHP# field needs a type, as in `private int count = 0;`.", "{field}");
        assert_eq!(source(code, error), name, "{field}");
        let [ClassLikeMember::Property(Property::Plain(untyped)), ClassLikeMember::Method(run)] =
            class_members(program).as_slice()
        else {
            panic!("expected a field and a method for `{field}`, got {:#?}", class_members(program));
        };
        assert!(untyped.hint.is_none(), "{field}");
        assert_eq!(run.name.value, b"run", "{field}");
    }
}

#[test]
fn a_parameter_with_a_dnf_type_parses() {
    const CODE: &str = "class Report\n{\n    public void fill((A&B)|null both) {}\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let [both] = method.parameter_list.parameters.as_slice() else {
        panic!("expected one parameter, got {:#?}", method.parameter_list);
    };
    assert_eq!(source(CODE, both.hint.as_ref().expect("a type")), "(A&B)|null");
}

#[test]
fn a_question_mark_after_a_type_makes_it_nullable() {
    const CODE: &str = "class Report\n{\n    public Calc? find(int? id, string? label = null) { return null; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let hints = std::iter::once(&method.return_type_hint.as_ref().expect("a return type").hint)
        .chain(method.parameter_list.parameters.iter().map(|parameter| parameter.hint.as_ref().expect("a type")));
    let nullable: Vec<(&str, &str, &str)> = hints
        .map(|hint| {
            let Hint::Nullable(nullable) = hint else {
                panic!("expected a nullable type, got {hint:#?}");
            };

            (source(CODE, hint), source(CODE, nullable.hint), source(CODE, &nullable.question_mark))
        })
        .collect();

    assert_eq!(nullable, [("Calc?", "Calc", "?"), ("int?", "int", "?"), ("string?", "string", "?")]);
}

/// The name and type arguments of a generic type, as written.
fn generic_type<'a>(code: &'a str, hint: &Hint) -> (&'a str, Vec<&'a str>) {
    let Hint::Generic(generic) = hint else {
        panic!("expected a generic type, got {hint:#?}");
    };

    (source(code, &generic.name), generic.arguments.iter().map(|argument| source(code, argument)).collect())
}

#[test]
fn a_collection_type_takes_its_type_arguments_in_angle_brackets() {
    const CODE: &str = "class Report\n{\n    public Map<string, List<Line>> group(List<Line> lines, Map<int, List<List<int>>>? depths) { return [:]; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let return_type = &method.return_type_hint.as_ref().expect("a return type").hint;
    assert_eq!(source(CODE, return_type), "Map<string, List<Line>>");
    assert_eq!(generic_type(CODE, return_type), ("Map", vec!["string", "List<Line>"]));

    let [lines, depths] = method.parameter_list.parameters.as_slice() else {
        panic!("expected two parameters, got {:#?}", method.parameter_list.parameters);
    };
    assert_eq!(generic_type(CODE, lines.hint.as_ref().expect("a type")), ("List", vec!["Line"]));
    let Some(Hint::Nullable(depths)) = &depths.hint else {
        panic!("expected a nullable type, got {:#?}", depths.hint);
    };
    assert_eq!(source(CODE, depths.hint), "Map<int, List<List<int>>>");
    assert_eq!(generic_type(CODE, depths.hint), ("Map", vec!["int", "List<List<int>>"]));
}

/// The return type and parameter types of a function type, as written.
fn function_type<'a>(code: &'a str, hint: &Hint) -> (&'a str, Vec<&'a str>) {
    let Hint::Function(function) = hint else {
        panic!("expected a function type, got {hint:#?}");
    };

    (source(code, function.return_type), function.parameters.iter().map(|parameter| source(code, parameter)).collect())
}

#[test]
fn a_function_type_writes_its_return_type_before_its_parameter_types() {
    const CODE: &str = "class Report\n{\n    private Function<Money?(Line, string)> priceOf;\n    private Function<void(Order)>? onPaid = null;\n    public Function<bool(Order)> eligibleFor(Map<string, Function<int()>> counters, Function<List<int>(List<Line>)> ids) { return o => true; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [
        ClassLikeMember::Property(Property::Plain(price_of)),
        ClassLikeMember::Property(Property::Plain(on_paid)),
        ClassLikeMember::Method(method),
    ] = class_members(program).as_slice()
    else {
        panic!("expected two fields and a method, got {:#?}", class_members(program));
    };

    let price_of = price_of.hint.as_ref().expect("a type");
    assert_eq!(source(CODE, price_of), "Function<Money?(Line, string)>");
    assert_eq!(function_type(CODE, price_of), ("Money?", vec!["Line", "string"]));
    let Some(Hint::Nullable(on_paid)) = &on_paid.hint else {
        panic!("expected a nullable type, got {:#?}", on_paid.hint);
    };
    assert_eq!(function_type(CODE, on_paid.hint), ("void", vec!["Order"]));
    let return_type = &method.return_type_hint.as_ref().expect("a return type").hint;
    assert_eq!(function_type(CODE, return_type), ("bool", vec!["Order"]));

    let [counters, ids] = method.parameter_list.parameters.as_slice() else {
        panic!("expected two parameters, got {:#?}", method.parameter_list.parameters);
    };
    let counters = counters.hint.as_ref().expect("a type");
    assert_eq!(generic_type(CODE, counters), ("Map", vec!["string", "Function<int()>"]));
    let Hint::Generic(counters) = counters else {
        panic!("expected a generic type, got {counters:#?}");
    };
    assert_eq!(function_type(CODE, &counters.arguments.as_slice()[1]), ("int", vec![]));
    assert_eq!(function_type(CODE, ids.hint.as_ref().expect("a type")), ("List<int>", vec!["List<Line>"]));
}

/// The lexer reads `(int)` as a cast, which in a function type is the parentheses around one parameter type.
#[test]
fn a_function_type_with_one_built_in_parameter_type_reads_the_cast_as_its_parentheses() {
    const CODE: &str =
        "class Report\n{\n    private Function<int(int)> twice;\n    private Function<bool( string )> blank;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let types: Vec<(&str, Vec<&str>, &str, &str)> = class_members(program)
        .iter()
        .map(|member| {
            let ClassLikeMember::Property(Property::Plain(field)) = member else {
                panic!("expected a field, got {member:#?}");
            };
            let Some(Hint::Function(function)) = &field.hint else {
                panic!("expected a function type, got {:#?}", field.hint);
            };
            let (_, parameters) = function_type(CODE, field.hint.as_ref().expect("a type"));

            (
                source(CODE, function.return_type),
                parameters,
                source(CODE, &function.left_parenthesis),
                source(CODE, &function.right_parenthesis),
            )
        })
        .collect();

    assert_eq!(types, [("int", vec!["int"], "(", ")"), ("bool", vec!["string"], "(", ")")]);
}

/// The effects a function type lists after its parameter types, as written.
fn function_type_effects<'a>(code: &'a str, hint: &Hint) -> Vec<&'a str> {
    let Hint::Function(function) = hint else {
        panic!("expected a function type, got {hint:#?}");
    };
    let mut effects = Vec::new();
    Node::FunctionHint(function).visit_children(|child| {
        if let Node::Uses(uses) = child {
            effects.extend(uses.names.iter().map(|name| source(code, name)));
        }
    });

    effects
}

/// The type of the one field of the one class in `code`.
fn field_type<'arena>(program: &'arena Program<'arena>) -> &'arena Hint<'arena> {
    let [ClassLikeMember::Property(Property::Plain(field))] = class_members(program).as_slice() else {
        panic!("expected one field, got {:#?}", class_members(program));
    };

    field.hint.as_ref().expect("a type")
}

/// Spec section 29: a function type lists its effects with `uses`, inside its brackets after its parameter types.
#[test]
fn a_function_type_lists_its_effects_after_its_parameter_types() {
    const CODE: &str = "class Checkout\n{\n    Function<Charge(Cart) uses Http> charge;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Checkout.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let charge = field_type(program);
    assert_eq!(source(CODE, charge), "Function<Charge(Cart) uses Http>");
    assert_eq!(function_type(CODE, charge), ("Charge", vec!["Cart"]));
    assert_eq!(function_type_effects(CODE, charge), ["Http"]);
}

#[test]
fn a_nullable_function_type_lists_its_effects_inside_the_question_mark() {
    const CODE: &str = "class Checkout\n{\n    Function<Charge(Cart) uses Http>? fallback;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Checkout.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Hint::Nullable(fallback) = field_type(program) else {
        panic!("expected a nullable type, got {:#?}", field_type(program));
    };
    assert_eq!(source(CODE, fallback.hint), "Function<Charge(Cart) uses Http>");
    assert_eq!(function_type_effects(CODE, fallback.hint), ["Http"]);
}

/// A comma after an effect names another effect, so the effects list ends at the function type's `>`, and a type
/// argument list's `>>` closes both.
#[test]
fn a_function_type_argument_takes_every_effect_before_its_closing_angle() {
    const CODE: &str = "class Checkout\n{\n    Map<string, Function<Charge(Cart) uses Http, Mail>> handlers;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Checkout.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let handlers = field_type(program);
    assert_eq!(generic_type(CODE, handlers), ("Map", vec!["string", "Function<Charge(Cart) uses Http, Mail>"]));
    let Hint::Generic(handlers) = handlers else {
        panic!("expected a generic type, got {handlers:#?}");
    };
    assert_eq!(function_type_effects(CODE, &handlers.arguments.as_slice()[1]), ["Http", "Mail"]);
}

#[test]
fn a_function_type_parameter_lists_its_own_effects() {
    const CODE: &str = "class Checkout\n{\n    Function<int(Function<int(int) uses Http>)> apply;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Checkout.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let apply = field_type(program);
    assert_eq!(function_type(CODE, apply), ("int", vec!["Function<int(int) uses Http>"]));
    assert_eq!(function_type_effects(CODE, apply), Vec::<&str>::new());
    let Hint::Function(apply) = apply else {
        panic!("expected a function type, got {apply:#?}");
    };
    assert_eq!(function_type_effects(CODE, &apply.parameters.as_slice()[0]), ["Http"]);
}

/// Effects written after a function type's `>` are refused with the form that writes them inside, and the parse goes
/// on after them, so a field, a nullable field, a parameter and a local each report that one error.
#[test]
fn effects_after_a_function_type_are_a_parse_error_that_names_the_written_form() {
    for code in [
        "class Checkout\n{\n    Function<Charge(Cart)> uses Http charge;\n}\n",
        "class Checkout\n{\n    Function<Charge(Cart)>? uses Http fallback;\n}\n",
        "class Checkout\n{\n    public void run(Function<Charge(Cart)> uses Http, Mail charge) {}\n}\n",
        "class Checkout\n{\n    public void run()\n    {\n        Function<Charge(Cart)> uses Http charge = cart => new Charge();\n    }\n}\n",
    ] {
        let arena = LocalArena::new();
        let program = parse(&arena, "src/Checkout.sharp", code);

        let [error] = program.errors else {
            panic!("expected one error for `{code}`, got {:#?}", program.errors);
        };
        assert_eq!(
            Issue::from(error).message,
            "A function type lists its effects inside its brackets, as in `Function<Charge(Cart) uses Http>`.",
            "{code}"
        );
        assert!(source(code, error).starts_with("uses Http"), "{code}: {}", source(code, error));
    }
}

/// `uses` names effects only right after a function type's parameter types, so it stays a name everywhere else, even
/// right after a function type.
#[test]
fn uses_after_a_function_type_stays_a_name() {
    const FIELD: &str = "class Checkout\n{\n    Function<int(int)> uses;\n}\n";
    const PARAMETERS: &str = "class Checkout\n{\n    public void run(Function<int(int)> uses, Function<int(int)> other, Function<int(int)> uses2) {}\n}\n";
    const LOCAL: &str =
        "class Checkout\n{\n    public void run()\n    {\n        Function<int(int)> uses = x;\n    }\n}\n";
    let arena = LocalArena::new();

    let program = parse(&arena, "src/Checkout.sharp", FIELD);
    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [ClassLikeMember::Property(Property::Plain(field))] = class_members(program).as_slice() else {
        panic!("expected one field, got {:#?}", class_members(program));
    };
    assert_eq!(field.hint.as_ref().map(|hint| source(FIELD, hint)), Some("Function<int(int)>"));
    assert_eq!(field.items.first().map(|item| source(FIELD, item)), Some("uses"));

    let program = parse(&arena, "src/Checkout.sharp", PARAMETERS);
    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let parameters: Vec<(&str, &str)> = method
        .parameter_list
        .parameters
        .iter()
        .map(|parameter| {
            (source(PARAMETERS, parameter.hint.as_ref().expect("a type")), source(PARAMETERS, &parameter.variable))
        })
        .collect();
    assert_eq!(
        parameters,
        [("Function<int(int)>", "uses"), ("Function<int(int)>", "other"), ("Function<int(int)>", "uses2")]
    );

    let program = parse(&arena, "src/Checkout.sharp", LOCAL);
    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::LocalDeclaration(local)] = method_body(program) else {
        panic!("expected one local, got {:#?}", method_body(program));
    };
    assert_eq!(local.hint.map(|hint| source(LOCAL, hint)), Some("Function<int(int)>"));
    assert_eq!(source(LOCAL, &local.name), "uses");
}

/// A `.sharp` file has no `<?php` or `?>`, so a nullable last type argument ends the type, as in `Map<string, Any?>`.
#[test]
fn a_nullable_type_argument_ends_a_collection_type() {
    const CODE: &str = "class Report\n{\n    public Map<string, Any?> group(List<int?> sizes, Map<int, List<string?>> names) { return [:]; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let return_type = &method.return_type_hint.as_ref().expect("a return type").hint;
    assert_eq!(generic_type(CODE, return_type), ("Map", vec!["string", "Any?"]));

    let [sizes, names] = method.parameter_list.parameters.as_slice() else {
        panic!("expected two parameters, got {:#?}", method.parameter_list.parameters);
    };
    assert_eq!(generic_type(CODE, sizes.hint.as_ref().expect("a type")), ("List", vec!["int?"]));
    assert_eq!(generic_type(CODE, names.hint.as_ref().expect("a type")), ("Map", vec!["int", "List<string?>"]));
}

/// A `.sharp` file has no `?>`, so a field written `= 0 ?><?php` is a parse error and never reaches the checker or
/// the lowering.
#[test]
fn a_closing_tag_does_not_end_a_field() {
    const CODE: &str = "namespace App.Tenant;\n\nclass Report\n{\n    private int count = 0 ?><?php\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(!program.errors.is_empty(), "expected a parse error, got {:#?}", program.statements);
}

/// A `.sharp` file has no `?>`, so a line comment that writes one runs to the end of its line.
#[test]
fn a_line_comment_runs_past_a_question_mark_and_greater_than() {
    const CODE: &str =
        "class Report\n{\n    // keeps a Map<string, Any?> of settings\n    public int run() { return 1; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    assert_eq!(source(CODE, &method.name), "run");
}

#[test]
fn a_local_and_a_field_can_have_a_collection_type_written() {
    const CODE: &str = "class Report\n{\n    private Map<string, int> counts = [:];\n    public void run()\n    {\n        List<Line> lines = [];\n        Map<string, List<int>>? groups = null;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [ClassLikeMember::Property(Property::Plain(field)), ClassLikeMember::Method(run)] =
        class_members(program).as_slice()
    else {
        panic!("expected a field and a method, got {:#?}", class_members(program));
    };
    assert_eq!(generic_type(CODE, field.hint.as_ref().expect("a type")), ("Map", vec!["string", "int"]));
    let MethodBody::Concrete(body) = &run.body else {
        panic!("expected a method body, got {:#?}", run.body);
    };
    let locals: Vec<(&str, &str)> = body
        .statements
        .iter()
        .map(|statement| {
            let Statement::LocalDeclaration(local) = statement else {
                panic!("expected a local, got {statement:#?}");
            };

            (source(CODE, local.hint.expect("a type")), source(CODE, &local.name))
        })
        .collect();
    assert_eq!(locals, [("List<Line>", "lines"), ("Map<string, List<int>>?", "groups")]);
}

#[test]
fn a_local_can_have_a_nullable_type_argument_written() {
    const CODE: &str = "class Report\n{\n    public void run()\n    {\n        List<Order?> orders = [];\n        Map<string, int?> prices = [:];\n        Map<string, List<int?>> sizes = [:];\n        List<(int|string)?> keys = [];\n        const List<Calc?> calcs = [];\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(run)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let MethodBody::Concrete(body) = &run.body else {
        panic!("expected a method body, got {:#?}", run.body);
    };
    let locals: Vec<(&str, Vec<&str>)> = body
        .statements
        .iter()
        .map(|statement| {
            let Statement::LocalDeclaration(local) = statement else {
                panic!("expected a local, got {statement:#?}");
            };

            generic_type(CODE, local.hint.expect("a type"))
        })
        .collect();
    assert_eq!(
        locals,
        [
            ("List", vec!["Order?"]),
            ("Map", vec!["string", "int?"]),
            ("Map", vec!["string", "List<int?>"]),
            ("List", vec!["(int|string)?"]),
            ("List", vec!["Calc?"]),
        ]
    );
}

#[test]
fn a_map_literal_writes_each_entry_as_key_colon_value() {
    const CODE: &str = "class Report\n{\n    public void run()\n    {\n        [line, other];\n        [\"pro\": pro, 2: team];\n        [:];\n        [];\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let arrays: Vec<&Array> = method_body(program)
        .iter()
        .map(|statement| {
            let Expression::Array(array) = expression(statement) else {
                panic!("expected a literal, got {statement:#?}");
            };

            array
        })
        .collect();
    let [list, map, empty_map, empty_list] = arrays.as_slice() else {
        panic!("expected four literals, got {arrays:#?}");
    };

    assert!(list.elements.iter().all(|element| matches!(element, ArrayElement::Value(_))), "{list:#?}");
    let entries: Vec<(&str, &str, &str)> = map
        .elements
        .iter()
        .map(|element| {
            let ArrayElement::KeyValue(entry) = element else {
                panic!("expected a map entry, got {element:#?}");
            };

            (source(CODE, entry.key), source(CODE, &entry.double_arrow), source(CODE, entry.value))
        })
        .collect();
    assert_eq!(entries, [("\"pro\"", ":", "pro"), ("2", ":", "team")]);
    assert_eq!(empty_map.colon.map(|colon| source(CODE, &colon)), Some(":"));
    assert_eq!(source(CODE, *empty_map), "[:]");
    assert!(empty_map.elements.is_empty());
    assert_eq!(empty_list.colon, None);
}

#[test]
fn a_php_double_arrow_in_a_literal_is_a_php_syntax_error() {
    const CODE: &str = "class Report\n{\n    public void run()\n    {\n        [\"pro\" => pro];\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(messages.len(), 1, "{messages:#?}");
    let Some(ParseError::PhpSyntaxInSharp(_, span)) = program.errors.first() else {
        panic!("expected a PHP-syntax error, got {:#?}", program.errors);
    };
    assert_eq!(source(CODE, span), "=>");
}

#[test]
fn any_is_the_type_php_writes_mixed_and_any_question_mark_is_it_nullable() {
    const CODE: &str = "class Report\n{\n    public Any find(Any? value) { return value; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(method)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let Hint::Mixed(any) = &method.return_type_hint.as_ref().expect("a return type").hint else {
        panic!("expected `Any`, got {:#?}", method.return_type_hint);
    };
    let Some(Hint::Nullable(NullableHint { hint: Hint::Mixed(nullable), .. })) =
        method.parameter_list.parameters.first().and_then(|parameter| parameter.hint.as_ref())
    else {
        panic!("expected `Any?`, got {:#?}", method.parameter_list.parameters);
    };

    assert_eq!((source(CODE, any), source(CODE, nullable)), ("Any", "Any"));
}

#[test]
fn any_in_a_php_file_is_a_class_name() {
    const CODE: &str = "<?php class Report { public function find(Any $value): Any { return $value; } }\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(Statement::Class(class)) =
        program.statements.iter().find(|statement| matches!(statement, Statement::Class(_)))
    else {
        panic!("expected a class, got {:#?}", program.statements);
    };
    let Some(ClassLikeMember::Method(method)) = class.members.first() else {
        panic!("expected a method, got {:#?}", class.members);
    };

    assert!(
        matches!(method.return_type_hint.as_ref().map(|hint| &hint.hint), Some(Hint::Identifier(_))),
        "{:#?}",
        method.return_type_hint
    );
}

#[test]
fn a_question_mark_before_a_type_is_a_php_syntax_error_that_names_the_suffix() {
    const CODE: &str = "class Report\n{\n    public ?int find(?Calc calc) { return null; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages,
        [
            "`?` before a type is PHP syntax: PHP# writes it after the type, as in `int?`",
            "`?` before a type is PHP syntax: PHP# writes it after the type, as in `int?`",
        ]
    );
    let spans: Vec<&str> = program.errors.iter().map(|error| source(CODE, error)).collect();
    assert_eq!(spans, ["?", "?"]);
}

#[test]
fn a_dollar_property_is_a_php_syntax_error() {
    const CODE: &str = "class Report\n{\n    private int $count = 0;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert_eq!(program.errors.len(), 1, "{:#?}", program.errors);
    let Some(ParseError::PhpSyntaxInSharp(_, span)) = program.errors.first() else {
        panic!("expected a PHP-syntax error, got {:#?}", program.errors);
    };
    assert_eq!(source(CODE, span), "$count");
}

#[test]
fn var_is_a_php_syntax_error() {
    const CODE: &str = "class Report\n{\n    var int count;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert_eq!(program.errors.len(), 1, "{:#?}", program.errors);
    let Some(ParseError::PhpSyntaxInSharp(_, span)) = program.errors.first() else {
        panic!("expected a PHP-syntax error, got {:#?}", program.errors);
    };
    assert_eq!(source(CODE, span), "var");
}

#[test]
fn a_field_keeps_its_type_and_bare_name() {
    const CODE: &str = "class Report\n{\n    private int count = 0;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Property(Property::Plain(field))) = class_members(program).first() else {
        panic!("expected a field, got {:#?}", class_members(program));
    };
    assert_eq!(source(CODE, field.hint.as_ref().expect("a type")), "int");
    let item = field.items.first().expect("one field");
    assert_eq!(source(CODE, item.variable()), "count");
    assert_eq!(source(CODE, field), "private int count = 0;");
}

#[test]
fn an_auto_property_takes_its_initial_value_after_its_accessors() {
    const CODE: &str =
        "class Report\n{\n    public int views { get; private set; } = 0;\n    public string name { get; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [ClassLikeMember::Property(Property::Hooked(views)), ClassLikeMember::Property(Property::Hooked(name))] =
        class_members(program).as_slice()
    else {
        panic!("expected two properties, got {:#?}", class_members(program));
    };
    assert_eq!(source(CODE, views), "public int views { get; private set; } = 0;");
    assert_eq!(source(CODE, &views.hook_list), "{ get; private set; }");
    let initial_value = views.initial_value.as_ref().expect("an initial value");
    assert_eq!(source(CODE, initial_value.value), "0");
    assert_eq!(source(CODE, name), "public string name { get; }");
    assert_eq!(name.initial_value, None);
}

#[test]
fn a_php_hooked_property_has_no_trailing_initial_value() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", "<?php class Report { public int $views { get; } = 0; }");

    assert!(!program.errors.is_empty());
}

#[test]
fn let_and_const_declare_locals_with_an_initializer() {
    const CODE: &str =
        "class Report\n{\n    void run()\n    {\n        let label = \"one\";\n        const base = 2;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::LocalDeclaration(label), Statement::LocalDeclaration(base)] = method_body(program) else {
        panic!("expected two local declarations, got {:#?}", method_body(program));
    };
    assert_eq!(label.keyword.expect("a keyword").value, b"let");
    assert!(label.hint.is_none());
    assert!(!label.is_const());
    assert_eq!(label.name.value, b"label");
    assert!(matches!(label.value, Expression::Literal(Literal::String(_))));
    assert_eq!(source(CODE, label), "let label = \"one\";");
    assert_eq!(base.keyword.expect("a keyword").value, b"const");
    assert!(base.is_const());
    assert_eq!(base.name.value, b"base");
}

#[test]
fn a_local_can_be_declared_with_its_type_written() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        Calc? found = null;\n        int total = 1;\n        const int base = 2;\n        const Calc? none = null;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let locals: Vec<(&str, &str, bool, &str)> = method_body(program)
        .iter()
        .map(|statement| {
            let Statement::LocalDeclaration(local) = statement else {
                panic!("expected a local declaration, got {statement:#?}");
            };

            (
                source(CODE, local),
                source(CODE, local.hint.as_ref().expect("a type")),
                local.is_const(),
                source(CODE, &local.name),
            )
        })
        .collect();

    assert_eq!(
        locals,
        [
            ("Calc? found = null;", "Calc?", false, "found"),
            ("int total = 1;", "int", false, "total"),
            ("const int base = 2;", "int", true, "base"),
            ("const Calc? none = null;", "Calc?", true, "none"),
        ]
    );
}

#[test]
fn a_local_can_be_declared_with_a_union_type_written() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        int|string key = 1;\n        const Calc|int?|float found = null;\n        for (int|bool step = 0; ; ) {\n        }\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let types: Vec<&str> = method_body(program)
        .iter()
        .map(|statement| {
            let local = match statement {
                Statement::LocalDeclaration(local) => local,
                Statement::For(r#for) => r#for.declaration.as_ref().expect("a declaration"),
                _ => panic!("expected a local declaration, got {statement:#?}"),
            };

            source(CODE, local.hint.as_ref().expect("a type"))
        })
        .collect();

    assert_eq!(types, ["int|string", "Calc|int?|float", "int|bool"]);
}

#[test]
fn a_local_union_type_may_hold_collection_and_function_types() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        List<int>|string items = [];\n        Function<int()>|int make = 1;\n        (List<int>|string)? maybe = null;\n        (Function<int()>|int)? later = null;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let types: Vec<&str> = method_body(program)
        .iter()
        .map(|statement| {
            let Statement::LocalDeclaration(local) = statement else {
                panic!("expected a local declaration, got {statement:#?}");
            };

            source(CODE, local.hint.as_ref().expect("a type"))
        })
        .collect();

    assert_eq!(types, ["List<int>|string", "Function<int()>|int", "(List<int>|string)?", "(Function<int()>|int)?"]);
}

#[test]
fn a_union_in_parentheses_with_a_question_mark_after_it_is_nullable_wherever_a_type_goes() {
    const CODE: &str = "class Report\n{\n    private (int|string)? key = null;\n    public (int|Calc)? id { get; }\n\n    public Report(private (bool|float)? flag)\n    {\n    }\n\n    public (Calc|string)? find((int|float)? extra)\n    {\n        (int|string)? found = null;\n        const (int|bool)? fixed = null;\n        for ((int|float)? step = null; ; ) {\n        }\n        return found;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [
        ClassLikeMember::Property(Property::Plain(field)),
        ClassLikeMember::Property(Property::Hooked(property)),
        ClassLikeMember::Method(constructor),
        ClassLikeMember::Method(find),
    ] = class_members(program).as_slice()
    else {
        panic!("expected a field, a property and two methods, got {:#?}", class_members(program));
    };
    let MethodBody::Concrete(body) = &find.body else {
        panic!("expected a method body, got {:#?}", find.body);
    };
    let locals = body.statements.iter().filter_map(|statement| match statement {
        Statement::LocalDeclaration(local) => local.hint,
        Statement::For(r#for) => r#for.declaration.as_ref().and_then(|local| local.hint),
        _ => None,
    });
    let hints = [field.hint.as_ref(), property.hint.as_ref()]
        .into_iter()
        .flatten()
        .chain(constructor.parameter_list.parameters.iter().filter_map(|parameter| parameter.hint.as_ref()))
        .chain(find.return_type_hint.iter().map(|return_type| &return_type.hint))
        .chain(find.parameter_list.parameters.iter().filter_map(|parameter| parameter.hint.as_ref()))
        .chain(locals);
    let unions: Vec<&str> = hints
        .map(|hint| {
            let Hint::Nullable(NullableHint { hint: Hint::Parenthesized(parenthesized), .. }) = hint else {
                panic!("expected a nullable type in parentheses, got {hint:#?}");
            };
            assert!(matches!(parenthesized.hint, Hint::Union(_)), "{hint:#?}");

            source(CODE, hint)
        })
        .collect();

    assert_eq!(
        unions,
        [
            "(int|string)?",
            "(int|Calc)?",
            "(bool|float)?",
            "(Calc|string)?",
            "(int|float)?",
            "(int|string)?",
            "(int|bool)?",
            "(int|float)?",
        ]
    );
}

/// The lexer reads `(int)` as a cast, as PHP does, so PHP# writes `int?`.
#[test]
fn a_built_in_type_in_parentheses_is_a_parse_error() {
    const CODE: &str = "class Report\n{\n    public int run((int)? extra) { return 1; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    let spans: Vec<&str> = program.errors.iter().map(|error| source(CODE, error)).collect();
    assert_eq!(spans.first(), Some(&"(int)"), "{:#?}", program.errors);
}

#[test]
fn a_statement_starting_with_parentheses_without_a_union_is_an_expression() {
    const CODE: &str =
        "class Report\n{\n    void run()\n    {\n        (found).run();\n        (found || other);\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [call, logical] = method_body(program) else {
        panic!("expected two statements, got {:#?}", method_body(program));
    };
    assert!(matches!(expression(call), Expression::Call(Call::Method(_))), "{call:#?}");
    assert!(matches!(expression(logical), Expression::Parenthesized(_)), "{logical:#?}");
}

#[test]
fn a_spaced_question_mark_after_a_name_is_not_a_typed_local() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        found ? total = 1 : 2;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [statement] = method_body(program) else {
        panic!("expected one statement, got {:#?}", method_body(program));
    };
    assert!(matches!(expression(statement), Expression::Conditional(_)), "{statement:#?}");
}

#[test]
fn a_statement_starting_with_a_name_and_a_bar_is_a_typed_local_only_when_a_name_follows_the_type() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        flags | MASK;\n        a | b == c;\n        (flags | 1);\n        int|string key = 1;\n        Calc|List<int>|Report? found = null;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let statements: Vec<String> = method_body(program)
        .iter()
        .map(|statement| match statement {
            Statement::LocalDeclaration(local) => {
                format!("local {}", source(CODE, local.hint.as_ref().expect("a type")))
            }
            Statement::Expression(statement) => format!("expression {}", grouping(CODE, statement.expression)),
            _ => panic!("expected a local or an expression statement, got {statement:#?}"),
        })
        .collect();
    assert_eq!(
        statements,
        [
            "expression (flags | MASK)",
            "expression ((a | b) == c)",
            "expression (flags | 1)",
            "local int|string",
            "local Calc|List<int>|Report?",
        ]
    );
}

#[test]
fn a_for_loop_declares_its_counter_with_let_or_const() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        for (let i = 0; i < 3; i++) {\n        }\n        for (const j = 0; ; ) {\n        }\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::For(counted), Statement::For(endless)] = method_body(program) else {
        panic!("expected two for loops, got {:#?}", method_body(program));
    };

    let declaration = counted.declaration.as_ref().expect("a declaration");
    assert_eq!(declaration.name.value, b"i");
    assert!(!declaration.is_const());
    assert_eq!(source(CODE, declaration), "let i = 0;");
    assert_eq!(source(CODE, &counted.initializations_semicolon), ";");
    assert!(counted.initializations.is_empty());
    assert_eq!(counted.conditions.len(), 1);
    assert_eq!(counted.increments.len(), 1);
    assert_eq!(source(CODE, counted), "for (let i = 0; i < 3; i++) {\n        }");

    assert!(endless.declaration.as_ref().is_some_and(LocalDeclaration::is_const));
    assert!(endless.conditions.is_empty());
}

#[test]
fn a_for_loop_declares_its_counter_with_its_type_written() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        for (int i = 0; i < 3; i++) {\n        }\n        for (const int? j = null; ; ) {\n        }\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::For(counted), Statement::For(endless)] = method_body(program) else {
        panic!("expected two for loops, got {:#?}", method_body(program));
    };

    let declaration = counted.declaration.as_ref().expect("a declaration");
    assert_eq!(source(CODE, declaration.hint.expect("a type")), "int");
    assert!(!declaration.is_const());
    assert_eq!(source(CODE, declaration), "int i = 0;");
    assert_eq!(counted.conditions.len(), 1);

    let declaration = endless.declaration.as_ref().expect("a declaration");
    assert_eq!(source(CODE, declaration.hint.expect("a type")), "int?");
    assert!(declaration.is_const());
}

#[test]
fn for_of_declares_its_loop_variable_or_key_and_value() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        for (const line of lines) {\n        }\n        for (let [key, plan] of this.plans()) {\n        }\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::ForOf(values), Statement::ForOf(entries)] = method_body(program) else {
        panic!("expected two for … of loops, got {:#?}", method_body(program));
    };

    assert!(values.is_const());
    let ForOfTarget::Value(line) = &values.target else {
        panic!("expected one loop variable, got {:#?}", values.target);
    };
    assert_eq!(line.name.value, b"line");
    assert!(line.hint.is_none());
    assert_eq!(bare_name(values.expression), b"lines");
    assert_eq!(source(CODE, &values.of), "of");
    assert_eq!(source(CODE, values), "for (const line of lines) {\n        }");

    assert!(!entries.is_const());
    let ForOfTarget::KeyValue(pair) = &entries.target else {
        panic!("expected a key and a value, got {:#?}", entries.target);
    };
    assert_eq!((pair.key.name.value, pair.value.name.value), (&b"key"[..], &b"plan"[..]));
    assert_eq!(source(CODE, &entries.target), "[key, plan]");
    assert!(matches!(entries.expression, Expression::Call(Call::Method(_))));
}

#[test]
fn a_const_loop_variable_can_have_its_type_written() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        for (const [Status status, int n] of counts) {\n        }\n        for (const [Status status, n] of counts) {\n        }\n        for (const Map<string, List<int>> group of groups) {\n        }\n        for (const (int|string)? id of ids) {\n        }\n        for (const List<int> counter = []; ; ) {\n        }\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [
        Statement::ForOf(typed),
        Statement::ForOf(key_typed),
        Statement::ForOf(group),
        Statement::ForOf(id),
        Statement::For(counted),
    ] = method_body(program)
    else {
        panic!("expected four for … of loops and a for loop, got {:#?}", method_body(program));
    };

    let ForOfTarget::KeyValue(pair) = &typed.target else {
        panic!("expected a key and a value, got {:#?}", typed.target);
    };
    assert_eq!(source(CODE, pair.key.hint.expect("a key type")), "Status");
    assert_eq!(source(CODE, &pair.key), "Status status");
    assert_eq!(source(CODE, pair.value.hint.expect("a value type")), "int");
    assert_eq!(pair.value.name.value, b"n");
    assert_eq!(source(CODE, &typed.target), "[Status status, int n]");

    let ForOfTarget::KeyValue(pair) = &key_typed.target else {
        panic!("expected a key and a value, got {:#?}", key_typed.target);
    };
    assert_eq!(source(CODE, pair.key.hint.expect("a key type")), "Status");
    assert!(pair.value.hint.is_none());

    let ForOfTarget::Value(group) = &group.target else {
        panic!("expected one loop variable, got {:#?}", group.target);
    };
    assert_eq!(source(CODE, group.hint.expect("a type")), "Map<string, List<int>>");
    assert_eq!(group.name.value, b"group");

    let ForOfTarget::Value(id) = &id.target else {
        panic!("expected one loop variable, got {:#?}", id.target);
    };
    assert_eq!(source(CODE, id.hint.expect("a type")), "(int|string)?");

    let declaration = counted.declaration.as_ref().expect("a declaration");
    assert_eq!(source(CODE, declaration.hint.expect("a type")), "List<int>");
}

/// A typed local never takes `let`, so neither does a typed loop variable.
#[test]
fn a_let_loop_variable_with_its_type_written_is_a_parse_error() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        for (let [Status status, int n] of counts) {\n        }\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    let first = program.errors.first().expect("a parse error");
    assert_eq!(source(CODE, first), "status");
}

#[test]
fn a_catch_clause_names_its_variable_without_a_dollar() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        try {\n        } catch (Missing | Broken failure) {\n        } catch (Throwable) {\n        } finally {\n        }\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::Try(r#try)] = method_body(program) else {
        panic!("expected a try, got {:#?}", method_body(program));
    };
    let [named, unnamed] = r#try.catch_clauses.as_slice() else {
        panic!("expected two catch clauses, got {:#?}", r#try.catch_clauses);
    };

    assert_eq!(source(CODE, &named.hint), "Missing | Broken");
    let variable = named.variable.as_ref().expect("a variable");
    assert_eq!(variable.name, b"failure");
    assert_eq!(source(CODE, variable), "failure");
    assert_eq!(source(CODE, &unnamed.hint), "Throwable");
    assert!(unnamed.variable.is_none());
    assert!(r#try.finally_clause.is_some());
}

#[test]
fn a_dollar_catch_variable_is_a_php_syntax_error() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        try {\n        } catch (Missing $failure) {\n        }\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    let spans: Vec<&str> = program.errors.iter().map(|error| source(CODE, error)).collect();
    assert_eq!(spans, ["$failure"]);
}

#[test]
fn for_in_is_a_parse_error_that_names_of() {
    let arena = LocalArena::new();
    let program = parse(
        &arena,
        "src/Report.sharp",
        "class Report\n{\n    void run()\n    {\n        for (const line in lines) {\n        }\n    }\n}\n",
    );

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(messages, ["PHP# loops over a collection with `of`, as in `for (const line of lines)`."]);
}

#[test]
fn foreach_is_a_php_syntax_error() {
    let arena = LocalArena::new();
    let program = parse(
        &arena,
        "src/Report.sharp",
        "class Report\n{\n    void run()\n    {\n        foreach (lines as line) {\n        }\n    }\n}\n",
    );

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(messages, ["`foreach` is PHP syntax: PHP# loops over a collection with `for … of`"]);
}

#[test]
fn a_php_for_loop_has_no_declaration() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", "<?php for ($i = 0; $i < 3; $i++) {}");

    let Some(Statement::For(r#for)) = program.statements.get(1) else {
        panic!("expected a for loop, got {:#?}", program.statements);
    };
    assert!(r#for.declaration.is_none());
    assert_eq!(r#for.initializations.len(), 1);
}

#[test]
fn dot_reads_as_member_access_whatever_the_name_before_it() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        calc.add(label, base);\n        Calc.make();\n        this.total;\n        total = total - 1;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [add, make, total, assignment] = method_body(program) else {
        panic!("expected four statements, got {:#?}", method_body(program));
    };

    let Expression::Call(Call::Method(add)) = expression(add) else {
        panic!("expected `calc.add(...)` to be a method call, got {add:#?}");
    };
    assert_eq!(bare_name(add.object), b"calc");
    assert_eq!(source(CODE, &add.arrow), ".");
    let arguments: Vec<_> = add.argument_list.arguments.iter().map(|argument| bare_name(argument.value())).collect();
    assert_eq!(arguments, [&b"label"[..], &b"base"[..]]);

    let Expression::Call(Call::Method(make)) = expression(make) else {
        panic!("expected `Calc.make()` to be a method call, got {make:#?}");
    };
    assert_eq!(bare_name(make.object), b"Calc");

    let Expression::Access(Access::Property(total)) = expression(total) else {
        panic!("expected `this.total` to be a property access, got {total:#?}");
    };
    assert_eq!(bare_name(total.object), b"this");

    let Expression::Assignment(assignment) = expression(assignment) else {
        panic!("expected an assignment, got {assignment:#?}");
    };
    assert_eq!(bare_name(assignment.lhs), b"total");
    assert!(matches!(assignment.rhs, Expression::Binary(Binary { operator: BinaryOperator::Subtraction(_), .. })));
}

#[test]
fn question_mark_dot_reads_as_null_safe_member_access() {
    const CODE: &str =
        "class Report\n{\n    void run()\n    {\n        calc?.add(1);\n        this?.total.next;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [add, next] = method_body(program) else {
        panic!("expected two statements, got {:#?}", method_body(program));
    };

    let Expression::Call(Call::NullSafeMethod(add)) = expression(add) else {
        panic!("expected `calc?.add(1)` to be a null-safe method call, got {add:#?}");
    };
    assert_eq!(bare_name(add.object), b"calc");
    assert_eq!(source(CODE, &add.question_mark_arrow), "?.");
    assert_eq!(source(CODE, add), "calc?.add(1)");

    let Expression::Access(Access::Property(next)) = expression(next) else {
        panic!("expected `this?.total.next` to read `next`, got {next:#?}");
    };
    let Expression::Access(Access::NullSafeProperty(total)) = next.object else {
        panic!("expected `this?.total` to be a null-safe property access, got {:#?}", next.object);
    };
    assert_eq!(bare_name(total.object), b"this");
    assert_eq!(source(CODE, total), "this?.total");
}

/// The expression the `n`th statement of the first method evaluates: a local's value, or the statement's expression.
fn statement_expression<'arena>(program: &'arena Program<'arena>, n: usize) -> &'arena Expression<'arena> {
    match &method_body(program)[n] {
        Statement::LocalDeclaration(local) => local.value,
        statement => expression(statement),
    }
}

#[test]
fn a_lambda_with_an_expression_body_is_an_arrow_function_without_fn() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        lines.filter(l => !l.refunded);\n        const add = (a, b) => a + b;\n        const check = (Order order, int minimum = 1) => order.total >= minimum;\n        const one = () => 1;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::Call(Call::Method(filter)) = statement_expression(program, 0) else {
        panic!("expected `lines.filter(…)`, got {:#?}", method_body(program));
    };
    let mut lambdas = vec![filter.argument_list.arguments.first().expect("one argument").value()];
    lambdas.extend((1..4).map(|n| statement_expression(program, n)));

    /// A parameter's name and type.
    type Parameter<'a> = (&'a str, Option<&'a str>);

    let lambdas: Vec<(&str, Vec<Parameter>, &str, &str)> = lambdas
        .into_iter()
        .map(|lambda| {
            let Expression::ArrowFunction(lambda) = lambda else {
                panic!("expected a lambda, got {lambda:#?}");
            };
            assert!(lambda.r#fn.is_none());
            let parameters = lambda
                .parameter_list
                .parameters
                .iter()
                .map(|parameter| {
                    (source(CODE, &parameter.variable), parameter.hint.as_ref().map(|hint| source(CODE, hint)))
                })
                .collect();

            (source(CODE, lambda), parameters, source(CODE, &lambda.arrow), source(CODE, lambda.expression))
        })
        .collect();

    assert_eq!(
        lambdas,
        [
            ("l => !l.refunded", vec![("l", None)], "=>", "!l.refunded"),
            ("(a, b) => a + b", vec![("a", None), ("b", None)], "=>", "a + b"),
            (
                "(Order order, int minimum = 1) => order.total >= minimum",
                vec![("order", Some("Order")), ("minimum", Some("int"))],
                "=>",
                "order.total >= minimum"
            ),
            ("() => 1", vec![], "=>", "1"),
        ]
    );
}

#[test]
fn a_lambda_with_a_block_body_is_a_closure_without_function_or_use() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        const increment = () => { count += 1; };\n        const price = (Line line, string currency) => {\n            return line.priceIn(currency);\n        };\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let lambdas: Vec<(&str, &str, &str, usize)> = (0..2)
        .map(|n| {
            let Expression::Closure(lambda) = statement_expression(program, n) else {
                panic!("expected a block lambda, got {:#?}", method_body(program));
            };
            assert!(lambda.function.is_none());
            assert!(lambda.use_clause.is_none());

            (
                source(CODE, lambda),
                source(CODE, &lambda.parameter_list),
                source(CODE, &lambda.arrow.expect("an arrow")),
                lambda.body.statements.len(),
            )
        })
        .collect();

    assert_eq!(
        lambdas,
        [
            ("() => { count += 1; }", "()", "=>", 1),
            (
                "(Line line, string currency) => {\n            return line.priceIn(currency);\n        }",
                "(Line line, string currency)",
                "=>",
                1
            ),
        ]
    );
}

#[test]
fn a_lambda_body_takes_the_whole_expression_and_stops_at_a_comma() {
    const CODE: &str =
        "class Report\n{\n    void run()\n    {\n        retry(3, () => client.send(a ?? b), 100);\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::Call(Call::Function(retry)) = statement_expression(program, 0) else {
        panic!("expected `retry(…)`, got {:#?}", method_body(program));
    };
    let arguments: Vec<&str> =
        retry.argument_list.arguments.iter().map(|argument| source(CODE, argument.value())).collect();
    assert_eq!(arguments, ["3", "() => client.send(a ?? b)", "100"]);
}

#[test]
fn a_parenthesized_expression_is_not_a_lambda() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        return (a + b) * (c);\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::Binary(product) = statement_expression(program, 0) else {
        panic!("expected a product, got {:#?}", method_body(program));
    };
    assert!(matches!(product.lhs, Expression::Parenthesized(_)), "{:#?}", product.lhs);
    assert!(matches!(product.rhs, Expression::Parenthesized(_)), "{:#?}", product.rhs);
}

#[test]
fn a_php_closure_or_arrow_function_is_php_syntax_that_names_the_lambda() {
    for (lambda, start) in [("fn (int x) => x", "fn"), ("function (int x) { return x; }", "function")] {
        let arena = LocalArena::new();
        let code: &'static str = Box::leak(
            format!("class Report\n{{\n    void run()\n    {{\n        const f = {lambda};\n    }}\n}}\n")
                .into_boxed_str(),
        );
        let program = parse(&arena, "src/Report.sharp", code);

        let [error] = program.errors else {
            panic!("expected one error for `{lambda}`, got {:#?}", program.errors);
        };
        assert_eq!(
            Issue::from(error).message,
            "PHP# writes a lambda as a bare arrow without `fn` or `function`, as in `x => x.id` or `(a, b) => { … }`",
            "{lambda}"
        );
        assert_eq!(source(code, error), start, "{lambda}");
    }
}

#[test]
fn php_keeps_its_arrow_functions_and_closures() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", "<?php $f = fn($x) => $x; $g = function () use ($f) { return 1; };");

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
}

#[test]
fn a_question_mark_apart_from_the_dot_is_not_null_safe_access() {
    let arena = LocalArena::new();
    let program =
        parse(&arena, "src/Report.sharp", "class Report\n{\n    void run()\n    {\n        calc? .add(1);\n    }\n}\n");

    assert!(!program.errors.is_empty());
}

#[test]
fn php_member_access_and_concatenating_assignment_are_parse_errors_that_name_the_dot() {
    for (code, message) in [
        ("calc->add()", "`->` is PHP syntax: PHP# writes member access with `.`"),
        ("calc?->add()", "`?->` is PHP syntax: PHP# writes null-safe member access with `?.`"),
        ("Calc::make()", "`::` is PHP syntax: PHP# writes static access with `.`"),
        ("label .= \"x\"", "`.=` is PHP syntax: in PHP# `.` is member access"),
    ] {
        let source = format!("class Report\n{{\n    void run()\n    {{\n        {code};\n    }}\n}}\n");
        let arena = LocalArena::new();
        let program = parse(&arena, "src/Report.sharp", Box::leak(source.into_boxed_str()));

        let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
        assert_eq!(messages, [message], "{code}");
    }
}

#[test]
fn a_php_operator_is_reported_once_where_it_is_consumed() {
    let arena = LocalArena::new();
    let program = parse(
        &arena,
        "src/Report.sharp",
        "class Report\n{\n    void run()\n    {\n        a = b + a .= \"x\";\n    }\n}\n",
    );

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(messages, ["`.=` is PHP syntax: in PHP# `.` is member access"]);
}

#[test]
fn the_use_keyword_is_a_parse_error_in_every_form() {
    for code in [
        "use Calc;\n",
        "use Lib\\Calc as Adder;\n",
        "use Lib\\{Calc, Money};\n",
        "use function Lib\\{make, total};\n",
        "class Report\n{\n    use Shared;\n}\n",
        "class Report\n{\n    use Lib\\Shared { Lib\\Shared::run as start; }\n}\n",
    ] {
        let arena = LocalArena::new();
        let program = parse(&arena, "src/Report.sharp", code);

        let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
        assert_eq!(messages, ["`use` is PHP syntax: PHP# imports a class with `import`"], "{code}");
    }
}

#[test]
fn a_php_closure_reports_its_function_and_its_use_clause() {
    let arena = LocalArena::new();
    let program = parse(
        &arena,
        "src/Report.sharp",
        "class Report\n{\n    void run()\n    {\n        const total = function () use ($count) { return 1; };\n    }\n}\n",
    );

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages,
        [
            "PHP# writes a lambda as a bare arrow without `fn` or `function`, as in `x => x.id` or `(a, b) => { … }`",
            "`use` is PHP syntax: PHP# imports a class with `import`",
        ]
    );
}

#[test]
fn a_qualified_name_is_a_parse_error_that_names_the_import() {
    for code in [
        "class Report\n{\n    int run()\n    {\n        return \\Lib\\Calc.make();\n    }\n}\n",
        "class Report\n{\n    int run()\n    {\n        return Lib\\Calc.make();\n    }\n}\n",
        "class Report\n{\n    void run(\\Lib\\Calc calc)\n    {\n    }\n}\n",
    ] {
        let arena = LocalArena::new();
        let program = parse(&arena, "src/Report.sharp", code);

        let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
        assert_eq!(messages, ["A `\\` name is PHP syntax: add `import Lib.Calc;` and write `Calc`"], "{code}");
    }
}

#[test]
fn a_qualified_null_true_or_false_is_a_parse_error_that_names_the_keyword() {
    for (code, keyword) in [
        ("class Report\n{\n    Any? run()\n    {\n        return \\null;\n    }\n}\n", "null"),
        ("class Report\n{\n    bool run()\n    {\n        return \\true;\n    }\n}\n", "true"),
        ("class Report\n{\n    bool run()\n    {\n        return \\false;\n    }\n}\n", "false"),
        ("class Report\n{\n    bool run()\n    {\n        return \\FALSE;\n    }\n}\n", "false"),
    ] {
        let arena = LocalArena::new();
        let program = parse(&arena, "src/Report.sharp", code);

        let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
        assert_eq!(messages, [format!("A `\\` name is PHP syntax: write `{keyword}`")], "{code}");
    }
}

#[test]
fn a_method_written_with_function_is_a_parse_error() {
    let arena = LocalArena::new();
    let program =
        parse(&arena, "src/Report.sharp", "class Report\n{\n    public function run(): int { return 1; }\n}\n");

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages.first().map(String::as_str),
        Some("`function` is PHP syntax: a PHP# method starts with its return type")
    );
}

fn returned_template<'arena>(program: &'arena Program<'arena>) -> &'arena InterpolatedString<'arena> {
    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::CompositeString(CompositeString::Interpolated(template)) = expression(&method_body(program)[0])
    else {
        panic!("expected a template, got {:#?}", method_body(program));
    };

    template
}

#[test]
fn a_template_is_an_interpolated_string_whose_dollar_braces_take_any_expression() {
    const CODE: &str = "class Report\n{\n    string run()\n    {\n        return `Order ${order.number}: ${count(items) + 1} items`;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);
    let template = returned_template(program);

    assert_eq!(source(CODE, template), "`Order ${order.number}: ${count(items) + 1} items`");
    let parts: Vec<&str> = template.parts.iter().map(|part| source(CODE, part)).collect();
    assert_eq!(parts, ["Order ", "${order.number}", ": ", "${count(items) + 1}", " items"]);
    let [_, StringPart::BracedExpression(number), _, StringPart::BracedExpression(count), _] =
        template.parts.as_slice()
    else {
        panic!("expected two interpolations, got {:#?}", template.parts);
    };
    assert!(matches!(number.expression, Expression::Access(Access::Property(_))), "{:#?}", number.expression);
    assert!(matches!(count.expression, Expression::Binary(_)), "{:#?}", count.expression);
}

#[test]
fn a_template_has_no_empty_text_around_its_interpolations() {
    const CODE: &str = "class Report\n{\n    string run()\n    {\n        return `${first}${last}`;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);
    let template = returned_template(program);

    let parts: Vec<&str> = template.parts.iter().map(|part| source(CODE, part)).collect();
    assert_eq!(parts, ["${first}", "${last}"]);
}

#[test]
fn a_template_reads_dollar_names_and_braces_as_text_and_decodes_javascript_escapes() {
    const CODE: &str = "class Report\n{\n    string run()\n    {\n        return `$price {$tax} \\${total} \\x41\\u{42} \\`a\\\nb`;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);
    let template = returned_template(program);

    let [StringPart::Literal(text)] = template.parts.as_slice() else {
        panic!("expected one literal part, got {:#?}", template.parts);
    };
    assert_eq!(text.value, Some(&b"$price {$tax} ${total} AB `ab"[..]));
}

#[test]
fn an_escape_javascript_refuses_is_a_parse_error_at_the_escape() {
    for escape in ["\\1", "\\01", "\\x4", "\\u12", "\\u{110000}"] {
        let code = format!("class Report\n{{\n    string run()\n    {{\n        return `a{escape}`;\n    }}\n}}\n");
        let code: &'static str = Box::leak(code.into_boxed_str());
        let arena = LocalArena::new();
        let program = parse(&arena, "src/Report.sharp", code);

        let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
        assert_eq!(
            messages,
            ["A template takes JavaScript's escapes, as in `\\n`, `\\x41` or `\\u{1F600}`."],
            "{escape}"
        );
        assert_eq!(source(code, &program.errors[0]), &escape[..2], "{escape}");
    }
}

#[test]
fn a_double_quoted_string_never_interpolates() {
    const CODE: &str =
        "class Report\n{\n    string run()\n    {\n        return \"$price {$tax} ${total}\";\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::Literal(Literal::String(string)) = expression(&method_body(program)[0]) else {
        panic!("expected a literal string, got {:#?}", method_body(program));
    };
    assert_eq!(string.value, Some(&b"$price {$tax} ${total}"[..]));
}

#[test]
fn backticks_keep_executing_a_shell_command_in_php() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", "<?php `ls $dir`;");

    let Some(Statement::Expression(statement)) = program.statements.get(1) else {
        panic!("expected an expression statement, got {:#?}", program.statements);
    };
    let Expression::CompositeString(CompositeString::ShellExecute(command)) = statement.expression else {
        panic!("expected a shell command, got {:#?}", statement.expression);
    };
    assert!(
        matches!(command.parts.as_slice(), [StringPart::Literal(_), StringPart::Expression(_)]),
        "{:#?}",
        command.parts
    );
}

/// A PHP# method returning a sum of `terms` terms.
fn sum(terms: usize) -> &'static str {
    let code = format!(
        "namespace App;\n\nclass Report\n{{\n    public int run(int extra)\n    {{\n        return {};\n    }}\n}}\n",
        vec!["extra"; terms].join(" + ")
    );

    Box::leak(code.into_boxed_str())
}

/// The statements of the file's namespace, which declares it.
fn namespace_statements<'program>(program: &'program Program<'program>) -> &'program [Statement<'program>] {
    let Some(Statement::Namespace(namespace)) = program.statements.first() else {
        panic!("expected a namespace, got {:#?}", program.statements);
    };

    namespace.statements().as_slice()
}

/// The namespace, the class and the `return` are three levels, so a sum of 509 terms nests its innermost term 512
/// levels deep, and a sum of 510 terms nests it 513 levels deep. The parser leaves out the class it refuses, at its
/// innermost term.
#[test]
fn nesting_deeper_than_512_levels_is_a_parse_error_that_leaves_out_its_statement() {
    let arena = LocalArena::new();
    let accepted = parse(&arena, "src/Report.sharp", sum(509));
    assert!(accepted.errors.is_empty(), "{:#?}", accepted.errors);

    let code = sum(510);
    let refused = parse(&arena, "src/Report.sharp", code);
    let [error @ ParseError::NestingTooDeepInSharp(span)] = refused.errors else {
        panic!("expected one nesting error, got {:#?}", refused.errors);
    };
    assert_eq!(error.to_string(), "PHP# nests statements, expressions and types at most 512 levels deep.");
    assert_eq!(&code[span.start.offset as usize..span.end.offset as usize], "extra");
    assert_eq!(span.start.offset as usize, code.find("extra + ").expect("the sum is written"));
    assert!(namespace_statements(refused).is_empty(), "{:#?}", refused.statements);
}

/// The parser refuses one statement of the namespace, and keeps the next.
#[test]
fn a_statement_nested_too_deep_leaves_the_next_statement_in_the_file() {
    let code = format!("{}\nclass Total\n{{\n}}\n", sum(510));
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", Box::leak(code.into_boxed_str()));

    assert!(matches!(program.errors, [ParseError::NestingTooDeepInSharp(_)]), "{:#?}", program.errors);
    let [Statement::Class(class)] = namespace_statements(program) else {
        panic!("expected the second class, got {:#?}", program.statements);
    };
    assert_eq!(class.name.value, b"Total");
}

/// A type counts as a level as a statement or expression does, so a union of 100,000 members is refused.
#[test]
fn a_union_of_100_000_types_is_a_parse_error() {
    let union = (0..100_000).map(|index| format!("A{index}")).collect::<Vec<_>>().join("|");
    let code = format!(
        "namespace App;\n\nclass Report\n{{\n    public {union} run()\n    {{\n        return null;\n    }}\n}}\n"
    );
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", Box::leak(code.into_boxed_str()));

    assert!(matches!(program.errors, [ParseError::NestingTooDeepInSharp(_)]), "{:#?}", program.errors);
    assert!(namespace_statements(program).is_empty(), "{:#?}", program.statements);
}

/// `??` nests to its right, so the parser's recursion reaches the limit every 512 terms of a long chain. The
/// statement still gets one error.
#[test]
fn a_coalescing_chain_of_100_000_terms_is_one_parse_error() {
    let code = sum(100_000).replace(" + ", " ?? ");
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", Box::leak(code.into_boxed_str()));

    assert!(
        matches!(
            program.errors.iter().filter(|error| matches!(error, ParseError::NestingTooDeepInSharp(_))).count(),
            1
        ),
        "{:#?}",
        program.errors
    );
    assert!(namespace_statements(program).is_empty(), "{:#?}", program.statements);
}

/// PHP itself compiles a sum of tens of thousands of terms, so a PHP file keeps every nesting the parser builds, and
/// a search of the whole tree reaches its innermost term.
#[test]
fn a_php_file_keeps_nesting_deeper_than_512_levels() {
    let code = format!(
        "<?php\nnamespace App;\n\nclass Report\n{{\n    public function run(int $extra): int\n    {{\n        return {};\n    }}\n}}\n",
        vec!["$extra"; 100_000].join(" + ")
    );
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", Box::leak(code.into_boxed_str()));
    assert!(program.errors.is_empty(), "{:#?}", program.errors);

    let terms = Node::Program(program).filter_map(|node| matches!(node, Node::DirectVariable(_)).then_some(()));
    assert_eq!(terms.len(), 100_001);
}

/// The span of a PHP sum or call chain of 10,000 terms reaches its first term, and whether it is constant reaches its
/// last, on a thread whose stack is far smaller than one frame per term, as a rayon worker's or a PHP fiber's stack is
/// for a deep enough chain.
#[test]
fn a_php_chain_of_10_000_terms_has_a_span_and_a_constness_on_a_small_stack() {
    let sum = vec!["1"; 10_000].join(" + ");
    let chain = format!("$this{}", "->run(1)".repeat(10_000));
    let code = format!(
        "<?php\nnamespace App;\n\nclass Report\n{{\n    public function run(int $extra): int\n    {{\n        return {sum};\n    }}\n\n    public function chain(): self\n    {{\n        return {chain};\n    }}\n}}\n"
    );
    let [sum_span, chain_span] = [&sum, &chain].map(|terms| {
        let start = code.find(terms.as_str()).expect("the chain is written");
        (start, start + terms.len())
    });

    let values = std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let arena = LocalArena::new();
            let program = parse(&arena, "src/Report.php", Box::leak(code.into_boxed_str()));
            assert!(program.errors.is_empty(), "{:#?}", program.errors);

            Node::Program(program).filter_map(|node| match node {
                Node::Return(statement) => statement.value.map(|value| {
                    let span = value.span();
                    (
                        (span.start.offset as usize, span.end.offset as usize),
                        value.is_constant(&PHPVersion::LATEST, false),
                    )
                }),
                _ => None,
            })
        })
        .expect("the thread starts")
        .join()
        .expect("the spans and constness are computed");

    assert_eq!(values, [(sum_span, true), (chain_span, false)]);
}

/// The source of every attribute list under `node`, in source order.
fn attribute_lists<'a>(code: &'a str, node: Node<'_, '_>) -> Vec<&'a str> {
    let mut lists = Vec::new();
    if let Node::AttributeList(list) = node {
        lists.push(source(code, list));
    }
    for child in node.children() {
        lists.extend(attribute_lists(code, child));
    }

    lists
}

#[test]
fn attributes_are_written_in_square_brackets_on_every_declaration() {
    const CODE: &str = "[Entity(label: \"Order Items\"), Searchable]\n[Table(\"orders\")]\nclass Order\n{\n    [Field] private int count = 0;\n    [Field(label: \"Name\")] public string name { get; set; }\n\n    public Order([Field(label: \"Tenant\")] public string tenant { get; }) {}\n\n    [Action(Mode.Write)]\n    [Retry(3, null)]\n    public void run([Field] int page = 1) {}\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Order.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    assert_eq!(
        attribute_lists(CODE, Node::Program(program)),
        [
            "[Entity(label: \"Order Items\"), Searchable]",
            "[Table(\"orders\")]",
            "[Field]",
            "[Field(label: \"Name\")]",
            "[Field(label: \"Tenant\")]",
            "[Action(Mode.Write)]",
            "[Retry(3, null)]",
            "[Field]",
        ]
    );

    let Some(Statement::Class(class)) = program.statements.first() else {
        panic!("expected a class, got {:#?}", program.statements);
    };
    let [entity, table] = class.attribute_lists.as_slice() else {
        panic!("expected two attribute lists on the class, got {:#?}", class.attribute_lists);
    };
    assert_eq!(source(CODE, &entity.hash_left_bracket), "[");
    let names: Vec<&[u8]> = entity.attributes.iter().map(|attribute| attribute.name.value()).collect();
    assert_eq!(names, [&b"Entity"[..], &b"Searchable"[..]]);
    let arguments = entity.attributes.first().and_then(|entity| entity.argument_list.as_ref()).expect("arguments");
    assert_eq!(source(CODE, arguments), "(label: \"Order Items\")");
    assert_eq!(table.attributes.len(), 1);
}

#[test]
fn a_statement_starting_with_a_list_is_not_an_attribute() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        [1, 2];\n        [a][0] = 1;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [list, write] = method_body(program) else {
        panic!("expected two statements, got {:#?}", method_body(program));
    };
    assert!(matches!(expression(list), Expression::Array(_)), "{list:#?}");
    assert!(matches!(expression(write), Expression::Assignment(_)), "{write:#?}");
}

#[test]
fn php_attribute_syntax_is_a_parse_error_that_names_the_brackets() {
    const CODE: &str = "#[Marker]\nclass Report\n{\n    #[Marker]\n    public void run(#[Marker] int page) {}\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    let messages: Vec<String> = program.errors.iter().map(ToString::to_string).collect();
    assert_eq!(messages, ["`#[` is PHP syntax: PHP# writes attributes in square brackets, as in `[Searchable]`"; 3]);
    let spans: Vec<&str> = program.errors.iter().map(|error| source(CODE, error)).collect();
    assert_eq!(spans, ["#["; 3]);
    assert_eq!(attribute_lists(CODE, Node::Program(program)), ["#[Marker]"; 3]);
}

#[test]
fn an_attribute_target_is_not_supported_yet() {
    const CODE: &str = "class Report\n{\n    [return: NotNull]\n    public string run() { return \"\"; }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    let [error] = program.errors else {
        panic!("expected one error, got {:#?}", program.errors);
    };
    assert_eq!(error.to_string(), "An attribute target is not supported yet in PHP#.");
    assert_eq!(source(CODE, error), "return:");
    let Some(ClassLikeMember::Method(run)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let names: Vec<&[u8]> = run
        .attribute_lists
        .iter()
        .flat_map(|list| list.attributes.iter())
        .map(|attribute| attribute.name.value())
        .collect();
    assert_eq!(names, [&b"NotNull"[..]]);
}

#[test]
fn typeof_names_a_class_by_its_short_name() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        Store.keep(typeof(Order));\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [statement] = method_body(program) else {
        panic!("expected one statement, got {:#?}", method_body(program));
    };
    let Expression::Call(Call::Method(keep)) = expression(statement) else {
        panic!("expected a method call, got {statement:#?}");
    };
    let Some(Expression::TypeOf(type_of)) = keep.argument_list.arguments.first().map(Argument::value) else {
        panic!("expected `typeof(Order)`, got {:#?}", keep.argument_list.arguments);
    };
    assert_eq!(type_of.class.value(), b"Order");
    assert_eq!(source(CODE, type_of), "typeof(Order)");
}

#[test]
fn a_class_or_an_interface_names_its_base_class_and_interfaces_after_a_colon() {
    const CODE: &str = "public class Page : Entity, Linkable\n{\n}\n\npublic interface Linkable : Named\n{\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Page.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::Class(class), Statement::Interface(interface)] = program.statements.as_slice() else {
        panic!("expected a class and an interface, got {:#?}", program.statements);
    };
    let headers: Vec<(&str, Vec<&[u8]>)> = [&class.inheritance, &interface.inheritance]
        .into_iter()
        .map(|inheritance| {
            let inheritance = inheritance.as_ref().expect("a header");
            (source(CODE, inheritance), inheritance.types.iter().map(Identifier::value).collect())
        })
        .collect();
    assert_eq!(headers, [(": Entity, Linkable", vec![&b"Entity"[..], b"Linkable"]), (": Named", vec![&b"Named"[..]])]);
    assert_eq!(source(CODE, class), "public class Page : Entity, Linkable\n{\n}");
}

/// An enum's header holds a backing type, `int` or `string`, first, and then its interfaces. After a backing type, the
/// `,` opens the interfaces as the `:` does without one, so each token belongs to one node.
#[test]
fn an_enum_names_its_backing_type_then_its_interfaces_after_a_colon() {
    const CODE: &str = "public enum Status : string, HasLabel, Sorted\n{\n}\n\nenum Suit : HasLabel\n{\n}\n\nenum Retry : string\n{\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Status.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let headers: Vec<_> = program
        .statements
        .iter()
        .map(|statement| {
            let Statement::Enum(r#enum) = statement else {
                panic!("expected an enum, got {statement:#?}");
            };

            (
                r#enum.backing_type_hint.as_ref().map(|backing_type| source(CODE, backing_type)),
                r#enum.inheritance.as_ref().map(|inheritance| source(CODE, inheritance)),
                r#enum
                    .inheritance
                    .iter()
                    .flat_map(|inheritance| inheritance.types.iter().map(Identifier::value))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        headers,
        [
            (Some(": string"), Some(", HasLabel, Sorted"), vec![&b"HasLabel"[..], b"Sorted"]),
            (None, Some(": HasLabel"), vec![&b"HasLabel"[..]]),
            (Some(": string"), None, vec![]),
        ]
    );
}

#[test]
fn a_php_enum_keeps_its_implements_clause_and_has_no_header() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Status.php", "<?php enum Status: string implements HasLabel {}");

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::OpeningTag(_), Statement::Enum(r#enum)] = program.statements.as_slice() else {
        panic!("expected an enum, got {:#?}", program.statements);
    };
    assert!(r#enum.backing_type_hint.is_some());
    assert!(r#enum.implements.is_some());
    assert!(r#enum.inheritance.is_none());
}

/// An enum or a trait takes `public` as a class does, so the checker refuses a trait where it starts.
#[test]
fn an_enum_or_a_trait_starts_with_its_modifiers() {
    const CODE: &str = "public enum Suit\n{\n}\n\npublic trait Tagged\n{\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Suit.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::Enum(r#enum), Statement::Trait(r#trait)] = program.statements.as_slice() else {
        panic!("expected an enum and a trait, got {:#?}", program.statements);
    };
    assert_eq!(source(CODE, r#enum), "public enum Suit\n{\n}");
    assert_eq!(source(CODE, r#trait), "public trait Tagged\n{\n}");
}

#[test]
fn virtual_and_override_are_method_modifiers_and_stay_names_elsewhere() {
    const CODE: &str = "class Shape\n{\n    public virtual string name()\n    {\n        return override(virtual);\n    }\n\n    protected override int size()\n    {\n        return 1;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Shape.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let modifiers: Vec<Vec<String>> = class_members(program)
        .iter()
        .map(|member| {
            let ClassLikeMember::Method(method) = member else {
                panic!("expected a method, got {member:#?}");
            };

            method.modifiers.iter().map(ToString::to_string).collect()
        })
        .collect();
    assert_eq!(modifiers, [vec!["Public", "Virtual"], vec!["Protected", "Override"]]);
}

#[test]
fn super_before_a_dot_is_the_parent_class() {
    const CODE: &str =
        "class Page\n{\n    public override string label()\n    {\n        return super.label();\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Page.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [statement] = method_body(program) else {
        panic!("expected one statement, got {:#?}", method_body(program));
    };
    let Expression::Call(Call::Method(label)) = expression(statement) else {
        panic!("expected a method call, got {statement:#?}");
    };
    let Expression::Parent(keyword) = label.object else {
        panic!("expected `super`, got {:#?}", label.object);
    };
    assert_eq!(source(CODE, keyword), "super");
    assert_eq!(source(CODE, label), "super.label()");
}

#[test]
fn super_without_a_dot_is_a_name() {
    const CODE: &str = "class Page\n{\n    public int label()\n    {\n        return super;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Page.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [statement] = method_body(program) else {
        panic!("expected one statement, got {:#?}", method_body(program));
    };
    assert_eq!(bare_name(expression(statement)), b"super");
}

/// `required` on a constructor is a modifier, spec section 25, and stays a name elsewhere. The parser keeps its own
/// "not supported yet" error for `required` on any other member.
#[test]
fn required_is_a_constructor_modifier_and_stays_a_name_elsewhere() {
    const CODE: &str = "class Entity\n{\n    public required Entity(Row row)\n    {\n        required(row);\n    }\n\n    required Entity(int count) {}\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Entity.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let modifiers: Vec<Vec<String>> = class_members(program)
        .iter()
        .map(|member| {
            let ClassLikeMember::Method(method) = member else {
                panic!("expected a method, got {member:#?}");
            };

            method.modifiers.iter().map(ToString::to_string).collect()
        })
        .collect();
    assert_eq!(modifiers, [vec!["Public", "Required"], vec!["Required"]]);
    let Some(ClassLikeMember::Method(constructor)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    assert_eq!(constructor.return_type_hint, None);
    assert_eq!(source(CODE, &constructor.modifiers.as_slice()[1]), "required");
}

#[test]
fn required_on_a_method_with_a_return_type_is_not_supported_yet() {
    for member in ["public required int run() { return 1; }", "public required static int make() { return 1; }"] {
        let arena = LocalArena::new();
        let code: &'static str = Box::leak(
            format!("class Report\n{{\n    {member}\n\n    public int other() {{ return 1; }}\n}}\n").into_boxed_str(),
        );
        let program = parse(&arena, "src/Report.sharp", code);

        let [error] = program.errors else {
            panic!("expected one error for `{member}`, got {:#?}", program.errors);
        };
        assert_eq!(error.to_string(), "`required` is not supported yet in PHP#.", "{member}");
        assert_eq!(source(code, error), "required", "{member}");
    }
}

/// A static class is `static` among a class's modifiers, and a method with a native body is `extern` among a member's
/// modifiers, with no body, spec section 29. `extern` stays a name elsewhere.
#[test]
fn static_starts_a_static_class_and_extern_a_method_without_a_body() {
    const CODE: &str = "public static class Text\n{\n    public static extern string slug(string title);\n\n    extern string plain(string title);\n\n    public static string trim(string text)\n    {\n        return extern(text);\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "library/Sharp/Text/Text.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(Statement::Class(class)) = program.statements.first() else {
        panic!("expected a class, got {:#?}", program.statements);
    };
    let class_modifiers: Vec<String> = class.modifiers.iter().map(ToString::to_string).collect();
    assert_eq!(class_modifiers, ["Public", "Static"]);
    let methods: Vec<&Method> = class_members(program)
        .iter()
        .map(|member| {
            let ClassLikeMember::Method(method) = member else {
                panic!("expected a method, got {member:#?}");
            };

            method
        })
        .collect();
    let modifiers: Vec<Vec<String>> =
        methods.iter().map(|method| method.modifiers.iter().map(ToString::to_string).collect()).collect();
    assert_eq!(modifiers, [vec!["Public", "Static", "Extern"], vec!["Extern"], vec!["Public", "Static"]]);
    assert!(matches!(methods[0].body, MethodBody::Abstract(_)), "{:#?}", methods[0].body);
    assert_eq!(source(CODE, methods[0]), "public static extern string slug(string title);");
    assert_eq!(source(CODE, &methods[1].modifiers.as_slice()[0]), "extern");
}

/// PHP has no `extern`, so a `.php` file keeps reading it as a name, here a property's class type.
#[test]
fn extern_in_a_php_file_is_a_name() {
    const CODE: &str = "<?php\n\nclass Text\n{\n    public extern $value;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Text.php", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::OpeningTag(_), Statement::Class(class)] = program.statements.as_slice() else {
        panic!("expected a class, got {:#?}", program.statements);
    };
    let Some(ClassLikeMember::Property(property)) = class.members.first() else {
        panic!("expected a property, got {:#?}", class.members);
    };
    let modifiers: Vec<String> = property.modifiers().iter().map(ToString::to_string).collect();
    assert_eq!(modifiers, ["Public"]);
    assert_eq!(property.hint().map(|hint| source(CODE, hint)), Some("extern"));
}

/// A file-level `extern` declares the effects of a plain PHP class, method or function, spec section 29. Its target is
/// a bare name or `Class.member`, and `uses` lists its effects.
#[test]
fn extern_declares_a_class_a_method_or_a_function_and_its_effects() {
    const CODE: &str = "namespace App.Stubs;\n\nimport Stripe.StripeClient;\nimport Carbon.Carbon;\n\nextern StripeClient uses Http;\nextern Carbon.now uses Clock;\nextern now uses Clock;\nextern BigDecimal;\nextern trim;\nextern Mailer uses Http, Mail;\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "app/Stubs/Stripe.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(Statement::Namespace(namespace)) = program.statements.first() else {
        panic!("expected a namespace, got {:#?}", program.statements);
    };
    let externs: Vec<(&str, bool, Vec<&str>, &str)> = namespace
        .statements()
        .iter()
        .filter_map(|statement| match statement {
            Statement::Extern(declaration) => Some(declaration),
            _ => None,
        })
        .map(|declaration| {
            (
                source(CODE, &declaration.target),
                declaration.target.is_dotted(),
                declaration.uses.iter().flat_map(|uses| uses.names.iter()).map(|name| source(CODE, name)).collect(),
                source(CODE, declaration),
            )
        })
        .collect();
    assert_eq!(
        externs,
        [
            ("StripeClient", false, vec!["Http"], "extern StripeClient uses Http;"),
            ("Carbon.now", true, vec!["Clock"], "extern Carbon.now uses Clock;"),
            ("now", false, vec!["Clock"], "extern now uses Clock;"),
            ("BigDecimal", false, vec![], "extern BigDecimal;"),
            ("trim", false, vec![], "extern trim;"),
            ("Mailer", false, vec!["Http", "Mail"], "extern Mailer uses Http, Mail;"),
        ]
    );
    let Some(Statement::Extern(stripe)) =
        namespace.statements().iter().find(|statement| matches!(statement, Statement::Extern(_)))
    else {
        panic!("expected an extern, got {:#?}", namespace.statements());
    };
    assert_eq!(stripe.r#extern.value, b"extern");
    assert_eq!(stripe.uses.as_ref().map(|uses| source(CODE, uses)), Some("uses Http"));
}

/// `extern` declares only when a name follows it, so `extern(text)` at the start of a PHP# statement stays a call.
#[test]
fn extern_before_a_parenthesis_stays_a_call_in_php_sharp() {
    const CODE: &str = "class Text\n{\n    public void run(string text)\n    {\n        extern(text);\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Text.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [statement] = method_body(program) else {
        panic!("expected one statement, got {:#?}", method_body(program));
    };
    let Expression::Call(Call::Function(call)) = expression(statement) else {
        panic!("expected a call, got {statement:#?}");
    };
    assert_eq!(source(CODE, call.function), "extern");
}

/// PHP has no `extern` declaration, so a `.php` statement that starts with `extern` stays a call or a constant.
#[test]
fn extern_starting_a_php_statement_stays_a_call_or_a_constant() {
    const CODE: &str = "<?php\n\nextern(text);\nextern;\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/text.php", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::OpeningTag(_), call, constant] = program.statements.as_slice() else {
        panic!("expected two expression statements, got {:#?}", program.statements);
    };
    assert!(matches!(expression(call), Expression::Call(Call::Function(_))), "{call:#?}");
    assert_eq!(bare_name(expression(constant)), b"extern");
}

/// Spec section 28: a law states a fact about a class's values, written as its name, its typed parameters and an
/// expression after `=>`.
#[test]
fn a_law_in_a_class_is_its_name_its_parameters_and_an_expression_body() {
    const CODE: &str = "public class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n    public Money add(Money other) => new Money(this.amount + other.amount, this.currency);\n\n    law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "app/Shared/Money.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [ClassLikeMember::Method(_), ClassLikeMember::Method(_), ClassLikeMember::Law(law)] =
        class_members(program).as_slice()
    else {
        panic!("expected two methods and a law, got {:#?}", class_members(program));
    };
    let parameters: Vec<&str> = law.parameter_list.parameters.iter().map(|parameter| source(CODE, parameter)).collect();
    assert_eq!(law.law.value, b"law");
    assert_eq!(source(CODE, &law.name), "addKeepsCurrency");
    assert_eq!(parameters, ["Money a", "Money b"]);
    assert_eq!(source(CODE, law.body.expression), "a.add(b).currency == a.currency");
    assert_eq!(source(CODE, law), "law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;");
}

/// Spec section 28 gives a state machine its laws on its status enum, beside the cases and the transition method.
#[test]
fn a_law_in_an_enum_follows_its_cases_and_methods() {
    const CODE: &str = "public enum Status : string\n{\n    case Open = \"open\";\n    case Refunded = \"refunded\";\n\n    public Status after(Payment e) => this;\n    law refundedIsFinal(Payment e) => Status.Refunded.after(e) == Status.Refunded;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "app/Billing/Status.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(Statement::Enum(status)) = program.statements.first() else {
        panic!("expected an enum, got {:#?}", program.statements);
    };
    let [
        ClassLikeMember::EnumCase(_),
        ClassLikeMember::EnumCase(_),
        ClassLikeMember::Method(_),
        ClassLikeMember::Law(law),
    ] = status.members.as_slice()
    else {
        panic!("expected two cases, a method and a law, got {:#?}", status.members);
    };
    assert_eq!(source(CODE, law), "law refundedIsFinal(Payment e) => Status.Refunded.after(e) == Status.Refunded;");
}

/// PHP has no laws, so a `.php` member that starts with `law` stays a typed property, and `law(…)` a call.
#[test]
fn law_in_a_php_file_is_a_name() {
    const CODE: &str = "<?php\n\nclass Rules\n{\n    law $law;\n\n    public function check(): bool\n    {\n        return law(1);\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Rules.php", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::OpeningTag(_), Statement::Class(class)] = program.statements.as_slice() else {
        panic!("expected a class, got {:#?}", program.statements);
    };
    let [ClassLikeMember::Property(property), ClassLikeMember::Method(check)] = class.members.as_slice() else {
        panic!("expected a property and a method, got {:#?}", class.members);
    };
    assert_eq!(property.hint().map(|hint| source(CODE, hint)), Some("law"));
    let MethodBody::Concrete(body) = &check.body else {
        panic!("expected a method body, got {:#?}", check.body);
    };
    let [statement] = body.statements.as_slice() else {
        panic!("expected one statement, got {:#?}", body.statements);
    };
    assert!(matches!(expression(statement), Expression::Call(Call::Function(_))), "{statement:#?}");
}

/// `law` starts a law only at the start of a member, before a name and `(`. After a modifier it is a return type, and
/// before `(` a call.
#[test]
fn law_after_a_modifier_or_before_a_parenthesis_stays_a_name_in_php_sharp() {
    const CODE: &str = "class Rules\n{\n    public law make(int x) => law(x);\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Rules.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [ClassLikeMember::Method(make)] = class_members(program).as_slice() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    assert_eq!(make.return_type_hint.as_ref().map(|hint| source(CODE, hint)), Some("law"));
    let MethodBody::Expression(body) = &make.body else {
        panic!("expected an expression body, got {:#?}", make.body);
    };
    let Expression::Call(Call::Function(call)) = body.expression else {
        panic!("expected a call, got {:#?}", body.expression);
    };
    assert_eq!(source(CODE, call.function), "law");
}

/// The lexer reads `Self` and `self` as one keyword. The checker tells them apart by how the keyword is written.
#[test]
fn self_is_a_return_type_an_instantiated_class_and_the_class_of_a_static_call() {
    const CODE: &str =
        "class Entity\n{\n    public static Self make()\n    {\n        return new Self(Self.count());\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Entity.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Method(make)) = class_members(program).first() else {
        panic!("expected a method, got {:#?}", class_members(program));
    };
    let Some(FunctionLikeReturnTypeHint { hint: Hint::Self_(keyword), .. }) = &make.return_type_hint else {
        panic!("expected a `Self` return type, got {:#?}", make.return_type_hint);
    };
    assert_eq!(keyword.value, b"Self");
    let [statement] = method_body(program) else {
        panic!("expected one statement, got {:#?}", method_body(program));
    };
    let Expression::Instantiation(Instantiation {
        class: Expression::Self_(class),
        argument_list: Some(arguments),
        ..
    }) = expression(statement)
    else {
        panic!("expected `new Self(…)`, got {statement:#?}");
    };
    assert_eq!(class.value, b"Self");
    let [Argument::Positional(argument)] = arguments.arguments.as_slice() else {
        panic!("expected one argument, got {arguments:#?}");
    };
    let Expression::Call(Call::Method(count)) = argument.value else {
        panic!("expected a method call, got {:#?}", argument.value);
    };
    assert!(matches!(count.object, Expression::Self_(keyword) if keyword.value == b"Self"), "{:#?}", count.object);
    assert_eq!(source(CODE, count), "Self.count()");
}

#[test]
fn typeof_in_php_is_a_function_call() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", "<?php typeof(Order);");

    let Some(Statement::Expression(statement)) = program.statements.get(1) else {
        panic!("expected an expression statement, got {:#?}", program.statements);
    };
    assert!(matches!(statement.expression, Expression::Call(Call::Function(_))), "{statement:#?}");
}

/// PHP# casts are lowercase only, spec section 24, so `(Int)`, `(Float)` and `(Bool)` are a class name in parentheses,
/// and the standard library's classes can be named there.
#[test]
fn a_capitalized_type_in_parentheses_is_a_class_name_and_not_a_cast() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        typeof(Int);\n        typeof(Float);\n        typeof(Bool);\n        (Int.parse(text));\n        (int)total;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [int, float, bool, parsed, cast] = method_body(program) else {
        panic!("expected five statements, got {:#?}", method_body(program));
    };
    for (statement, class) in [(int, "Int"), (float, "Float"), (bool, "Bool")] {
        let Expression::TypeOf(type_of) = expression(statement) else {
            panic!("expected `typeof({class})`, got {statement:#?}");
        };
        assert_eq!(type_of.class.value(), class.as_bytes());
    }
    let Expression::Parenthesized(parenthesized) = expression(parsed) else {
        panic!("expected `(Int.parse(text))`, got {parsed:#?}");
    };
    assert_eq!(source(CODE, parenthesized.expression), "Int.parse(text)");
    assert!(
        matches!(
            expression(cast),
            Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::IntCast(..), .. })
        ),
        "{cast:#?}"
    );
}

/// PHP lexes its casts ignoring case, so a PHP file keeps reading `(Int) $x` as an int cast.
#[test]
fn php_keeps_reading_a_capitalized_cast_as_a_cast() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", "<?php (Int) $x; (Float) $x; (Bool) $x;");

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let operators: Vec<&UnaryPrefixOperator> = program
        .statements
        .iter()
        .skip(1)
        .map(|statement| match statement {
            Statement::Expression(ExpressionStatement { expression: Expression::UnaryPrefix(prefix), .. }) => {
                &prefix.operator
            }
            _ => panic!("expected a cast, got {statement:#?}"),
        })
        .collect();
    assert!(
        matches!(
            operators.as_slice(),
            [UnaryPrefixOperator::IntCast(..), UnaryPrefixOperator::FloatCast(..), UnaryPrefixOperator::BoolCast(..)]
        ),
        "{operators:#?}"
    );
}

#[test]
fn dot_keeps_concatenating_in_php() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", "<?php $a . $b;");

    let Some(Statement::Expression(statement)) = program.statements.get(1) else {
        panic!("expected an expression statement, got {:#?}", program.statements);
    };
    assert!(matches!(statement.expression, Expression::Binary(binary) if binary.operator.is_concatenation()));
}

/// The expression's source with every binary operation, `is` and `as` wrapped in parentheses, and the value of a
/// comparison or value pattern grouped the same way, showing how the parser grouped it.
fn grouping(code: &str, expression: &Expression) -> String {
    match expression {
        Expression::Binary(binary) => format!(
            "({} {} {})",
            grouping(code, binary.lhs),
            source(code, &binary.operator),
            grouping(code, binary.rhs)
        ),
        Expression::Is(is) => {
            let pattern = match is.pattern {
                Pattern::Comparison(comparison) => {
                    format!("{} {}", source(code, &comparison.operator), grouping(code, comparison.value))
                }
                Pattern::Value(value) => grouping(code, value),
                pattern => source(code, pattern).to_owned(),
            };

            format!("({} is {pattern})", grouping(code, is.value))
        }
        Expression::As(r#as) => format!("({} as {})", grouping(code, r#as.value), source(code, r#as.hint)),
        _ => source(code, expression).to_owned(),
    }
}

#[test]
fn bitwise_operators_bind_tighter_than_comparisons() {
    const CODE: &str = "class Report\n{\n    bool run()\n    {\n        return permissions & WRITE != 0;\n        return a | b == c ^ d;\n        return a & b < c | d;\n        return a == b & c;\n        return a << 1 & b;\n        return a | b ^ c & d;\n        return a < b == c;\n        return a && b | c;\n        return a + b << c;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let groupings: Vec<String> =
        method_body(program).iter().map(|statement| grouping(CODE, expression(statement))).collect();
    assert_eq!(
        groupings,
        [
            "((permissions & WRITE) != 0)",
            "((a | b) == (c ^ d))",
            "((a & b) < (c | d))",
            "(a == (b & c))",
            "((a << 1) & b)",
            "(a | (b ^ (c & d)))",
            "((a < b) == c)",
            "(a && (b | c))",
            "((a + b) << c)",
        ]
    );
}

#[test]
fn php_keeps_bitwise_operators_looser_than_comparisons() {
    const CODE: &str = "<?php\n$permissions & $WRITE != 0;\n$a | $b == $c ^ $d;\n$a & $b < $c | $d;\n$a == $b & $c;\n$a << 1 & $b;\n$a | $b ^ $c & $d;\n$a < $b == $c;\n$a && $b | $c;\n$a + $b << $c;\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let groupings: Vec<String> =
        program.statements.iter().skip(1).map(|statement| grouping(CODE, expression(statement))).collect();
    assert_eq!(
        groupings,
        [
            "($permissions & ($WRITE != 0))",
            "($a | (($b == $c) ^ $d))",
            "(($a & ($b < $c)) | $d)",
            "(($a == $b) & $c)",
            "(($a << 1) & $b)",
            "($a | ($b ^ ($c & $d)))",
            "(($a < $b) == $c)",
            "($a && ($b | $c))",
            "(($a + $b) << $c)",
        ]
    );
}

#[test]
fn is_and_as_bind_with_the_comparisons_below_the_bitwise_operators() {
    const CODE: &str = "class Report\n{\n    bool run()\n    {\n        return flags is > 1 | 2;\n        return flags is 1 | 2;\n        return a & b is int;\n        return a | b as Calc;\n        return a == b is Calc;\n        return a < b is Calc;\n        return a is Calc is Report;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let groupings: Vec<String> =
        method_body(program).iter().map(|statement| grouping(CODE, expression(statement))).collect();
    assert_eq!(
        groupings,
        [
            "(flags is > (1 | 2))",
            "(flags is (1 | 2))",
            "((a & b) is int)",
            "((a | b) as Calc)",
            "(a == (b is Calc))",
            "((a < b) is Calc)",
            "((a is Calc) is Report)",
        ]
    );
}

#[test]
fn php_file_starts_in_inline_text() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Demo/Report.php", "namespace Demo;\n");

    assert_eq!(program.dialect, Dialect::Php);
    assert!(matches!(program.statements.first(), Some(Statement::Inline(_))), "{:#?}", program.statements);
}

#[test]
fn is_tests_a_value_against_a_pattern_as_tightly_as_a_comparison() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        entity is HasDesign && entity is not int count;\n        result as Paid ?? other;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::Binary(and) = expression(&method_body(program)[0]) else {
        panic!("expected `&&`, got {:#?}", method_body(program)[0]);
    };
    let (Expression::Is(design), Expression::Is(count)) = (and.lhs, and.rhs) else {
        panic!("expected two `is`, got {and:#?}");
    };
    assert_eq!(source(CODE, design), "entity is HasDesign");
    assert!(matches!(design.pattern, Pattern::Type(TypePattern { variable: None, .. })), "{design:#?}");
    let Pattern::Not(not) = count.pattern else {
        panic!("expected `not`, got {count:#?}");
    };
    let Pattern::Type(int) = not.pattern else {
        panic!("expected a type pattern, got {not:#?}");
    };
    assert_eq!(source(CODE, &int.hint), "int");
    assert_eq!(int.variable.map(|variable| variable.value), Some(&b"count"[..]));

    let Expression::Binary(coalesce) = expression(&method_body(program)[1]) else {
        panic!("expected `??`, got {:#?}", method_body(program)[1]);
    };
    let Expression::As(r#as) = coalesce.lhs else {
        panic!("expected `as` before `??`, got {coalesce:#?}");
    };
    assert_eq!(source(CODE, r#as), "result as Paid");
}

/// The condition, the value when it holds and the value when it fails of the `? :` that `run` returns, after checking
/// that the file parses whole and that the condition is an `is` whose type pattern declares no name.
fn conditional_after_is(code: &'static str) -> [&'static str; 3] {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", code);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::Conditional(conditional) = expression(&method_body(program)[0]) else {
        panic!("expected `? :`, got {:#?}", method_body(program)[0]);
    };
    let Expression::Is(Is { pattern: Pattern::Type(TypePattern { variable: None, .. }), .. }) = conditional.condition
    else {
        panic!("expected `is` and a type as the condition, got {conditional:#?}");
    };
    let then = conditional.then.expect("a value when the condition holds");

    [source(code, conditional.condition), source(code, then), source(code, conditional.r#else)]
}

#[test]
fn a_question_mark_after_the_type_that_is_tests_starts_a_conditional_as_in_csharp() {
    assert_eq!(
        conditional_after_is(
            "class Report\n{\n    string run()\n    {\n        return value is string ? value : \"\";\n    }\n}\n"
        ),
        ["value is string", "value", "\"\""]
    );
}

#[test]
fn a_question_mark_after_a_class_that_is_tests_starts_a_conditional() {
    assert_eq!(
        conditional_after_is(
            "class Report\n{\n    string run()\n    {\n        return entity is Address ? entity.city : \"\";\n    }\n}\n"
        ),
        ["entity is Address", "entity.city", "\"\""]
    );
}

#[test]
fn a_conditional_after_is_takes_a_nested_conditional_in_parentheses() {
    assert_eq!(
        conditional_after_is(
            "class Report\n{\n    string run()\n    {\n        return value is string ? (strict ? value : fallback) : \"\";\n    }\n}\n"
        ),
        ["value is string", "(strict ? value : fallback)", "\"\""]
    );
}

#[test]
fn a_question_mark_after_the_type_that_as_converts_to_starts_a_conditional() {
    const CODE: &str = "class Report\n{\n    int run()\n    {\n        return value as bool ? 1 : 0;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::Conditional(conditional) = expression(&method_body(program)[0]) else {
        panic!("expected `? :`, got {:#?}", method_body(program)[0]);
    };
    assert!(matches!(conditional.condition, Expression::As(_)), "{conditional:#?}");
    assert_eq!(source(CODE, conditional.condition), "value as bool");
}

#[test]
fn a_question_mark_before_the_name_an_is_pattern_declares_makes_the_type_nullable() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        if (value is string? text) {\n        }\n        value is string? text;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::If(r#if), statement] = method_body(program) else {
        panic!("expected an if and an expression, got {:#?}", method_body(program));
    };
    for is in [r#if.condition, expression(statement)] {
        let Expression::Is(Is {
            pattern: Pattern::Type(TypePattern { hint: hint @ Hint::Nullable(_), variable }), ..
        }) = is
        else {
            panic!("expected `is` and a nullable type, got {is:#?}");
        };
        assert_eq!(source(CODE, hint), "string?");
        assert_eq!(variable.map(|variable| variable.value), Some(&b"text"[..]));
    }
}

#[test]
fn a_question_mark_that_ends_an_is_pattern_makes_the_type_nullable() {
    const CODE: &str = "class Report\n{\n    bool run()\n    {\n        if (value is string?) {\n        }\n        return value is string?;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::If(r#if), returned] = method_body(program) else {
        panic!("expected an if and a return, got {:#?}", method_body(program));
    };
    for is in [r#if.condition, expression(returned)] {
        let Expression::Is(Is {
            pattern: Pattern::Type(TypePattern { hint: Hint::Nullable(_), variable: None }), ..
        }) = is
        else {
            panic!("expected `is` and a nullable type, got {is:#?}");
        };
        assert_eq!(source(CODE, is), "value is string?");
    }
}

#[test]
fn a_question_mark_before_the_end_of_a_pattern_makes_the_type_nullable() {
    const CODE: &str = "class Report\n{\n    int run()\n    {\n        return match (value) {\n            int? n => 1,\n            string? when strict => 2,\n            float? f when f > 0.0 => 3,\n            { name: string? n, age: int? } => 4,\n            bool? b or null => 5,\n            default => 0,\n        };\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::PatternMatch(r#match) = expression(&method_body(program)[0]) else {
        panic!("expected a match, got {:#?}", method_body(program)[0]);
    };
    let patterns: Vec<&str> = r#match
        .arms
        .iter()
        .filter_map(|arm| match arm {
            PatternMatchArm::Pattern(arm) => Some(source(CODE, arm.pattern)),
            PatternMatchArm::Default(_) => None,
        })
        .collect();
    assert_eq!(patterns, ["int? n", "string?", "float? f", "{ name: string? n, age: int? }", "bool? b or null"]);
    let Some(PatternMatchArm::Pattern(first)) = r#match.arms.first() else {
        panic!("expected a pattern arm, got {:#?}", r#match.arms);
    };
    assert!(matches!(first.pattern, Pattern::Type(TypePattern { hint: Hint::Nullable(_), .. })), "{first:#?}");
}

#[test]
fn patterns_join_with_and_or_and_not_as_in_csharp() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        total is >= 1000 and < 10000 or 0 or not (int or null);\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::Is(is) = expression(&method_body(program)[0]) else {
        panic!("expected `is`, got {:#?}", method_body(program)[0]);
    };
    let Pattern::Binary(or) = is.pattern else {
        panic!("expected `or`, got {is:#?}");
    };
    assert!(!or.is_and());
    assert_eq!(source(CODE, or.left), ">= 1000 and < 10000 or 0");
    assert!(matches!(or.right, Pattern::Not(NotPattern { pattern: Pattern::Parenthesized(_), .. })), "{or:#?}");
    let Pattern::Binary(first) = or.left else {
        panic!("expected `or`, got {or:#?}");
    };
    let Pattern::Binary(range) = first.left else {
        panic!("expected `and`, got {first:#?}");
    };
    assert!(range.is_and());
    assert!(matches!(range.left, Pattern::Comparison(_)), "{range:#?}");
    assert!(matches!(first.right, Pattern::Value(Expression::Literal(_))), "{first:#?}");
}

#[test]
fn equals_before_a_constant_is_a_comparison_pattern() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        status is == LIMIT;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::Is(Is { pattern: Pattern::Comparison(comparison), .. }) = expression(&method_body(program)[0])
    else {
        panic!("expected a comparison pattern, got {:#?}", method_body(program)[0]);
    };
    assert!(matches!(comparison.operator, BinaryOperator::Equal(_)), "{comparison:#?}");
    assert_eq!(source(CODE, comparison.value), "LIMIT");
}

#[test]
fn a_properties_pattern_tests_each_property_against_a_pattern() {
    const CODE: &str =
        "class Report\n{\n    void run()\n    {\n        response is { status: 200, body: string body };\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::Is(Is { pattern: Pattern::Properties(properties), .. }) = expression(&method_body(program)[0])
    else {
        panic!("expected a properties pattern, got {:#?}", method_body(program)[0]);
    };
    let written: Vec<&str> = properties.properties.iter().map(|property| source(CODE, property)).collect();
    assert_eq!(written, ["status: 200", "body: string body"]);
}

#[test]
fn list_and_enum_case_patterns_are_not_supported_yet() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        lines is [Line first, ...List<Line> rest];\n        result is PaymentResult.Paid(string id);\n        result is PaymentResult.Declined declined;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    let errors: Vec<(String, &str)> =
        program.errors.iter().map(|error| (error.to_string(), source(CODE, error))).collect();
    assert_eq!(
        errors,
        [
            ("A list pattern is not supported yet in PHP#.".to_owned(), "[Line first, ...List<Line> rest]"),
            ("An enum case pattern is not supported yet in PHP#.".to_owned(), "PaymentResult.Paid(string id)"),
            ("An enum case pattern is not supported yet in PHP#.".to_owned(), "PaymentResult.Declined declined"),
        ]
    );
}

#[test]
fn a_case_without_fields_or_a_name_is_a_value_pattern() {
    const CODE: &str =
        "class Report\n{\n    void run()\n    {\n        status is Status.Open or Status.Closed;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::Is(Is { pattern: Pattern::Binary(or), .. }) = expression(&method_body(program)[0]) else {
        panic!("expected an `or` pattern, got {:#?}", method_body(program)[0]);
    };
    let (Pattern::Value(open), Pattern::Value(closed)) = (or.left, or.right) else {
        panic!("expected two value patterns, got {or:#?}");
    };
    assert!(matches!(open, Expression::Access(Access::Property(_))), "{open:#?}");
    assert_eq!([source(CODE, *open), source(CODE, *closed)], ["Status.Open", "Status.Closed"]);
}

#[test]
fn match_takes_patterns_when_conditions_and_a_default_arm() {
    const CODE: &str = "class Report\n{\n    string run()\n    {\n        return match (count) {\n            0 => \"none\",\n            int n when n > 100 => \"many\",\n            default => \"some\",\n        };\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::PatternMatch(r#match) = expression(&method_body(program)[0]) else {
        panic!("expected a match, got {:#?}", method_body(program)[0]);
    };
    let arms: Vec<&str> = r#match.arms.iter().map(|arm| source(CODE, arm)).collect();
    assert_eq!(arms, ["0 => \"none\"", "int n when n > 100 => \"many\"", "default => \"some\""]);
    let Some(PatternMatchArm::Pattern(many)) = r#match.arms.get(1) else {
        panic!("expected a pattern arm, got {:#?}", r#match.arms);
    };
    assert_eq!(source(CODE, many.guard.as_ref().expect("a when condition")), "when n > 100");
    assert!(r#match.arms.get(2).is_some_and(PatternMatchArm::is_default));
}

/// The `when` conditions of a `match` that starts a statement and a `match` that gives a value, each with the arms
/// `Status.Open when {condition}` and `default`, after checking that both parse whole.
fn guard_conditions<'arena>(arena: &'arena LocalArena, condition: &str) -> [&'arena Expression<'arena>; 2] {
    let code: &'static str = Box::leak(
        format!(
            "class Report\n{{\n    int run()\n    {{\n        match (status) {{\n            Status.Open when {condition} => {{}},\n            default => {{}},\n        }}\n        return match (status) {{\n            Status.Open when {condition} => 1,\n            default => 0,\n        }};\n    }}\n}}\n"
        )
        .into_boxed_str(),
    );
    let program = parse(arena, "src/Report.sharp", code);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::PatternMatch(statement), returned] = method_body(program) else {
        panic!("expected a match statement and a return, got {:#?}", method_body(program));
    };
    let Expression::PatternMatch(returned) = expression(returned) else {
        panic!("expected a returned match, got {returned:#?}");
    };

    [statement, returned].map(|r#match| {
        let [PatternMatchArm::Pattern(open), PatternMatchArm::Default(_)] = r#match.arms.as_slice() else {
            panic!("expected a pattern arm and a default arm, got {:#?}", r#match.arms);
        };
        let guard = open.guard.as_ref().expect("a when condition");
        assert_eq!(source(code, guard), format!("when {condition}"), "the guard ends at the arm's `=>`");

        guard.condition
    })
}

#[test]
fn a_guard_can_be_a_bare_name() {
    let arena = LocalArena::new();

    for condition in guard_conditions(&arena, "forced") {
        assert_eq!(bare_name(condition), b"forced");
    }
}

#[test]
fn a_guard_can_negate_a_name() {
    let arena = LocalArena::new();

    for condition in guard_conditions(&arena, "!forced") {
        let Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::Not(_), operand }) = condition else {
            panic!("expected `!forced`, got {condition:#?}");
        };
        assert_eq!(bare_name(operand), b"forced");
    }
}

#[test]
fn a_guard_can_compare_with_a_name() {
    let arena = LocalArena::new();

    for condition in guard_conditions(&arena, "count > limit") {
        let Expression::Binary(Binary { lhs, operator: BinaryOperator::GreaterThan(_), rhs }) = condition else {
            panic!("expected `count > limit`, got {condition:#?}");
        };
        assert_eq!([bare_name(lhs), bare_name(rhs)], [b"count".as_slice(), b"limit"]);
    }
}

#[test]
fn a_guard_can_call_a_method() {
    let arena = LocalArena::new();

    for condition in guard_conditions(&arena, "this.ready()") {
        assert!(matches!(condition, Expression::Call(Call::Method(_))), "{condition:#?}");
    }
}

/// A lambda inside a guard is in parentheses, as an argument or as a value of its own, so the guard's `=>` is never
/// the lambda's.
#[test]
fn a_lambda_in_parentheses_inside_a_guard_stays_a_lambda() {
    let arena = LocalArena::new();

    for condition in guard_conditions(&arena, "items.any(item => item.ready) && (() => forced)()") {
        let Expression::Binary(Binary { lhs: Expression::Call(Call::Method(any)), rhs, .. }) = condition else {
            panic!("expected `items.any(…) && …`, got {condition:#?}");
        };
        let argument = any.argument_list.arguments.first().expect("one argument").value();
        assert!(matches!(argument, Expression::ArrowFunction(_)), "{argument:#?}");
        let Expression::Call(Call::Function(FunctionCall { function: Expression::Parenthesized(called), .. })) = rhs
        else {
            panic!("expected `(() => forced)()`, got {rhs:#?}");
        };
        assert!(matches!(called.expression, Expression::ArrowFunction(_)), "{called:#?}");
    }
}

/// A lambda binds as loosely as assignment, as in C#: it is a whole value, the value of an assignment, or a branch of
/// `? :`. After any other operator it needs parentheses.
#[test]
fn a_lambda_after_an_operator_needs_parentheses_as_in_csharp() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        const first = handler ?? (item => item.ready);\n        const second = strict ? handler : item => item.ready;\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Expression::Binary(Binary { rhs: Expression::Parenthesized(fallback), .. }) = statement_expression(program, 0)
    else {
        panic!("expected `handler ?? (…)`, got {:#?}", method_body(program));
    };
    assert!(matches!(fallback.expression, Expression::ArrowFunction(_)), "{fallback:#?}");
    let Expression::Conditional(Conditional { r#else: Expression::ArrowFunction(_), .. }) =
        statement_expression(program, 1)
    else {
        panic!("expected `strict ? handler : item => item.ready`, got {:#?}", method_body(program));
    };

    const UNPARENTHESIZED: &str = "class Report\n{\n    void run()\n    {\n        const first = handler ?? item => item.ready;\n        lines->count();\n    }\n}\n";
    let unparenthesized = parse(&arena, "src/Report.sharp", UNPARENTHESIZED);
    let errors: Vec<(String, &str)> =
        unparenthesized.errors.iter().map(|error| (error.to_string(), source(UNPARENTHESIZED, error))).collect();
    assert_eq!(
        errors,
        [
            (
                "A lambda after an operator needs parentheses, as in `handler ?? (item => item.ready)`.".to_owned(),
                "item =>"
            ),
            ("`->` is PHP syntax: PHP# writes member access with `.`".to_owned(), "->"),
        ]
    );
}

/// A `? :` binds more loosely than a `when` condition reads, as in C#, so the arm's `=>` would end its `else` value.
/// The parser reports it once, reads both branches, and parses on.
#[test]
fn a_conditional_at_the_top_of_a_guard_needs_parentheses_as_in_csharp() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        match (status) {\n            Status.Open when strict ? forced : ready => {},\n            default => {},\n        }\n        lines->count();\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    let errors: Vec<(String, &str)> =
        program.errors.iter().map(|error| (error.to_string(), source(CODE, error))).collect();
    assert_eq!(
        errors,
        [
            (
                "A `? :` in a `when` condition needs parentheses, as in `when (strict ? forced : ready) =>`."
                    .to_owned(),
                "strict ? forced : ready"
            ),
            ("`->` is PHP syntax: PHP# writes member access with `.`".to_owned(), "->"),
        ]
    );
}

#[test]
fn a_conditional_in_parentheses_is_a_guard() {
    let arena = LocalArena::new();

    for condition in guard_conditions(&arena, "(strict ? forced : ready)") {
        let Expression::Parenthesized(Parenthesized { expression: Expression::Conditional(_), .. }) = condition else {
            panic!("expected `(strict ? forced : ready)`, got {condition:#?}");
        };
    }
}

#[test]
fn a_match_that_starts_a_statement_takes_block_arms_and_no_semicolon() {
    const CODE: &str = "class Report\n{\n    void run()\n    {\n        match (shape) {\n            Circle circle => {\n                draw(circle);\n            },\n            default => {},\n        }\n        done();\n    }\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [Statement::PatternMatch(r#match), Statement::Expression(_)] = method_body(program) else {
        panic!("expected a match statement and a call, got {:#?}", method_body(program));
    };
    assert!(r#match.arms.iter().all(|arm| matches!(arm.body(), PatternMatchArmBody::Block(_))), "{:#?}", r#match.arms);
}

#[test]
fn php_keeps_its_match_of_conditions() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", "<?php match ($a) { 1 => 2, default => 3 };");

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(Statement::Expression(statement)) = program.statements.get(1) else {
        panic!("expected an expression statement, got {:#?}", program.statements);
    };
    assert!(matches!(statement.expression, Expression::Match(_)), "{statement:#?}");
}

#[test]
fn an_operator_is_its_return_type_operator_its_symbol_its_parameters_and_its_body() {
    const CODE: &str = "class Money\n{\n    public static bool operator ==(Money a, Money b) => a.cents == b.cents;\n\n    public static int operator <=>(Money a, Money b)\n    {\n        return a.cents - b.cents;\n    }\n\n    public static Money operator +(Money a, Money b) => a;\n\n    public static Money operator -(Money a, Money b) => a;\n\n    public static Money operator *(Money a, int b) => a;\n\n    public static Money operator /(Money a, int b) => a;\n\n    public static Money operator %(Money a, int b) => a;\n\n    public static Money operator **(Money a, int b) => a;\n\n    public static Money operator -(Money a) => a;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Money.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let operators: Vec<_> = class_members(program)
        .iter()
        .map(|member| {
            let ClassLikeMember::Operator(operator) = member else {
                panic!("expected an operator, got {member:#?}");
            };
            assert!(operator.return_type_hint.colon.is_none());
            assert_eq!(operator.operator.value, b"operator");

            (
                source(CODE, &operator.return_type_hint),
                source(CODE, &operator.symbol),
                operator.parameter_list.parameters.len(),
                matches!(operator.body, MethodBody::Expression(_)),
            )
        })
        .collect();

    assert_eq!(
        operators,
        [
            ("bool", "==", 2, true),
            ("int", "<=>", 2, false),
            ("Money", "+", 2, true),
            ("Money", "-", 2, true),
            ("Money", "*", 2, true),
            ("Money", "/", 2, true),
            ("Money", "%", 2, true),
            ("Money", "**", 2, true),
            ("Money", "-", 1, true),
        ]
    );
    let Some(ClassLikeMember::Operator(equality)) = class_members(program).first() else {
        panic!("expected an operator, got {:#?}", class_members(program));
    };
    assert!(matches!(equality.symbol, BinaryOperator::Equal(_)), "{:#?}", equality.symbol);
    assert_eq!(source(CODE, equality), "public static bool operator ==(Money a, Money b) => a.cents == b.cents;");
}

#[test]
fn an_operator_starts_at_its_attributes() {
    const CODE: &str = "class Money\n{\n    [Pure] public static Money operator +(Money a, Money b) => a;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Money.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(ClassLikeMember::Operator(operator)) = class_members(program).first() else {
        panic!("expected an operator, got {:#?}", class_members(program));
    };
    assert_eq!(source(CODE, operator), "[Pure] public static Money operator +(Money a, Money b) => a;");
}

#[test]
fn a_binary_operator_a_class_cannot_declare_parses_for_semantics_to_refuse() {
    const CODE: &str = "class Money\n{\n    public static bool operator !=(Money a, Money b) => false;\n\n    public static bool operator <(Money a, Money b) => false;\n\n    public static bool operator &&(Money a, Money b) => false;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Money.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let symbols: Vec<_> = class_members(program)
        .iter()
        .map(|member| {
            let ClassLikeMember::Operator(operator) = member else {
                panic!("expected an operator, got {member:#?}");
            };

            source(CODE, &operator.symbol)
        })
        .collect();
    assert_eq!(symbols, ["!=", "<", "&&"]);
}

#[test]
fn a_symbol_that_is_no_binary_operator_is_a_parse_error_that_names_the_declared_operators() {
    const CODE: &str =
        "class Money\n{\n    public static bool operator !(Money a) => false;\n\n    public int cents() => 1;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Money.sharp", CODE);

    let errors: Vec<_> = program.errors.iter().map(|error| (error.to_string(), source(CODE, error))).collect();
    assert_eq!(
        errors,
        [("`operator !` cannot be declared: only `+ - * / % **`, unary `-`, `==` and `<=>` can.".to_owned(), "!")]
    );
    let [ClassLikeMember::Method(cents)] = class_members(program).as_slice() else {
        panic!("expected the method after the operator, got {:#?}", class_members(program));
    };
    assert_eq!(cents.name.value, b"cents");
}

#[test]
fn an_operator_without_a_body_is_a_parse_error() {
    let arena = LocalArena::new();
    let program =
        parse(&arena, "src/Money.sharp", "class Money\n{\n    public static bool operator ==(Money a, Money b);\n}\n");

    assert_eq!(
        program.errors.first().map(ToString::to_string).as_deref(),
        Some("Expected one of `LeftBrace`, found `Semicolon`")
    );
}

#[test]
fn a_member_named_operator_is_a_method_or_a_field() {
    const CODE: &str = "class Calc\n{\n    public int operator() => 1;\n\n    private int operator = 1;\n}\n";
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Calc.sharp", CODE);

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let [ClassLikeMember::Method(method), ClassLikeMember::Property(field)] = class_members(program).as_slice() else {
        panic!("expected a method and a field, got {:#?}", class_members(program));
    };
    assert_eq!(method.name.value, b"operator");
    assert_eq!(field.first_variable().name, b"operator");
}

#[test]
fn php_keeps_reading_operator_as_a_name() {
    let arena = LocalArena::new();
    let program = parse(
        &arena,
        "src/Calc.php",
        "<?php class Calc { public function operator() {} public operator $value; const operator = 1; }",
    );

    assert!(program.errors.is_empty(), "{:#?}", program.errors);
    let Some(Statement::Class(class)) = program.statements.get(1) else {
        panic!("expected a class, got {:#?}", program.statements);
    };
    let [ClassLikeMember::Method(method), ClassLikeMember::Property(property), ClassLikeMember::Constant(constant)] =
        class.members.as_slice()
    else {
        panic!("expected a method, a property and a constant, got {:#?}", class.members);
    };
    assert_eq!(method.name.value, b"operator");
    assert!(matches!(property.hint(), Some(Hint::Identifier(name)) if name.value() == b"operator"), "{property:#?}");
    assert_eq!(constant.first_item().name.value, b"operator");
}
