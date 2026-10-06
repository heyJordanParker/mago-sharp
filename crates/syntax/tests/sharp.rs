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

#[test]
fn dot_keeps_concatenating_in_php() {
    let arena = LocalArena::new();
    let program = parse(&arena, "src/Report.php", "<?php $a . $b;");

    let Some(Statement::Expression(statement)) = program.statements.get(1) else {
        panic!("expected an expression statement, got {:#?}", program.statements);
    };
    assert!(matches!(statement.expression, Expression::Binary(binary) if binary.operator.is_concatenation()));
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
