use std::borrow::Cow;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_names::resolver::NameResolver;
use mago_php_version::PHPVersion;
use mago_semantics::SemanticsChecker;
use mago_syntax::parser::parse_file;

/// Every issue as `line message (previous line)`, where the previous line is the first secondary annotation's line.
fn issues(code: &str) -> Vec<String> {
    let arena = LocalArena::new();
    let file = File::ephemeral(Cow::Borrowed(b"test.php"), Cow::Owned(code.as_bytes().to_vec()));
    let program = parse_file(&arena, &file);
    assert!(program.errors.is_empty(), "test source did not parse: {:?}", program.errors);

    let names = NameResolver::new(&arena).resolve(program);
    let line = |offset: u32| file.line_number(offset) + 1;

    SemanticsChecker::new(PHPVersion::PHP85)
        .check(&file, program, &names)
        .iter()
        .map(|issue| {
            let primary = issue.primary_span().map_or(0, |span| line(span.start.offset));
            let previous = issue.annotations.iter().find(|annotation| !annotation.is_primary());
            let previous = previous.map_or(0, |annotation| line(annotation.span.start.offset));

            format!("{primary} {} ({previous})", issue.message)
        })
        .collect()
}

#[test]
fn a_method_defined_again_in_any_case_points_at_its_first_definition() {
    let code = "<?php
class Report {
    public function run(): void {}
    public function other(): void {}
    public function RUN(): void {}
    public function Run(): void {}
}
";

    assert_eq!(
        issues(code),
        [
            "5 class method `Report::RUN` has already been defined (3)",
            "6 class method `Report::Run` has already been defined (3)",
        ]
    );
}

#[test]
fn a_property_defined_again_points_at_its_first_definition() {
    let code = "<?php
class Report {
    public int $total;
    public int $count;
    public int $total;
    public int $count { get => 1; }
    public function __construct(public int $count) {}
}
";

    assert_eq!(
        issues(code),
        [
            "5 property `Report::$total` has already been defined (3)",
            "6 property `Report::$count` has already been defined (4)",
            "7 promoted property `Report::$count` has already been defined as a property (4)",
        ]
    );
}

#[test]
fn a_property_defined_after_a_promoted_property_points_at_the_parameter() {
    let code = "<?php
class Report {
    public function __construct(public int $total) {}
    public int $total;
}
";

    assert_eq!(issues(code), ["4 property `Report::$total` has already been defined as a promoted property (3)"]);
}

#[test]
fn a_constant_or_case_defined_again_points_at_its_first_definition() {
    let code = "<?php
enum Status {
    case Open;
    const LIMIT = 1;
    const LIMIT = 2;
    const Open = 3;
    case LIMIT;
    case Open;
}
";

    assert_eq!(
        issues(code),
        [
            "5 enum constant `Status::LIMIT` has already been defined (4)",
            "6 enum case `Status::Open` and constant `Status::Open` cannot have the same name (3)",
            "7 enum case `Status::LIMIT` and constant `Status::LIMIT` cannot have the same name (4)",
            "8 enum case `Status::Open` has already been defined (3)",
        ]
    );
}
