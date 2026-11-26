#![allow(clippy::expect_used, clippy::missing_panics_doc, clippy::unwrap_used)]

use std::borrow::Cow;
use std::path::Path;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_linter::Linter;
use mago_linter::external::ExternalLinter;
use mago_linter::settings::Settings;
use mago_names::resolver::NameResolver;
use mago_php_version::PHPVersion;
use mago_syntax::parser::parse_file;
use mago_text_edit::Safety;

use crate::common;

#[test]
fn external_linter_issue_preserves_suggested_edits() -> Result<(), Box<dyn std::error::Error>> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the external linter SDK test") {
        return Ok(());
    }

    let host =
        common::host(&repository.join("composer/tests/Sdk/worker.php"), common::NO_DEADLINE, common::NO_DEADLINE, &[]);
    let external = ExternalLinter::initialize([host], PHPVersion::PHP85)?;

    let source = b"<?php\n\nPsl\\Iter\\any([1], static fn(int $value): bool => $value > 0);\n";
    let file = File::ephemeral(Cow::Borrowed(b"src/example.php"), Cow::Borrowed(source));
    let arena = LocalArena::new();
    let program = parse_file(&arena, &file);
    let resolved_names = NameResolver::new(&arena).resolve(program);
    let settings = Settings { php_version: PHPVersion::PHP85, ..Settings::default() };
    let only = vec!["mago-sdk-test/prefer-array-any".to_owned()];
    let linter = Linter::new(&arena, &settings, Some(&only), false);

    let issues = linter.lint_with_external(&file, program, &resolved_names, &external)?;
    let issue = issues
        .iter()
        .find(|issue| issue.code.as_deref() == Some("mago-sdk-test/prefer-array-any"))
        .expect("the PHP rule should report its issue");
    let edits = issue.edits.get(&file.id).expect("the PHP rule should attach one edit batch");
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, b"array_any");
    assert_eq!(edits[0].safety, Safety::Safe);
    assert_eq!(&source[edits[0].range.start as usize..edits[0].range.end as usize], b"Psl\\Iter\\any");

    Ok(())
}
