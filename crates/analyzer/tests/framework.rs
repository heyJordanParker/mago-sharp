#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::LazyLock;

use mago_allocator::LocalArena;
use mago_analyzer::Analyzer;
use mago_analyzer::analysis_result::AnalysisResult;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::scanner::scan_program;
use mago_database::DatabaseReader;
use mago_database::file::File;
use mago_names::resolver::NameResolver;
use mago_prelude::Prelude;
use mago_syntax::parser::parse_file;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::PRELUDE;
use crate::prelude_population::populate_on_prelude;

static PLUGIN_REGISTRY: LazyLock<PluginRegistry> = LazyLock::new(PluginRegistry::with_library_providers);

#[derive(Debug, Clone)]
pub struct TestCase<'src> {
    name: &'src str,
    content: &'src [u8],
    settings: Option<Settings>,
    expected_property_reads: Vec<(&'src str, &'src str, usize)>,
    expected_symbol_references: Vec<(&'src str, &'src str, usize)>,
}

impl<'src> TestCase<'src> {
    #[must_use]
    pub fn new(name: &'src str, content: &'src [u8]) -> Self {
        Self {
            name,
            content,
            settings: None,
            expected_property_reads: Vec::new(),
            expected_symbol_references: Vec::new(),
        }
    }

    #[must_use]
    pub fn settings(mut self, settings: Settings) -> Self {
        self.settings = Some(settings);
        self
    }

    #[must_use]
    pub fn expect_property_reads(mut self, class: &'src str, property: &'src str, count: usize) -> Self {
        self.expected_property_reads.push((class, property, count));
        self
    }

    #[must_use]
    pub fn expect_symbol_reference_count(mut self, class: &'src str, member: &'src str, count: usize) -> Self {
        self.expected_symbol_references.push((class, member, count));
        self
    }

    pub fn run(self) {
        run_test_case_inner(self);
    }
}

#[must_use]
pub fn default_test_settings() -> Settings {
    Settings {
        find_unused_expressions: true,
        find_unused_definitions: true,
        check_throws: true,
        allow_possibly_undefined_array_keys: false,
        strict_list_index_checks: true,
        check_property_initialization: true,
        ..Default::default()
    }
}

#[must_use]
pub fn infection_like_settings() -> Settings {
    Settings {
        find_unused_expressions: true,
        find_unused_definitions: true,
        find_unused_parameters: true,
        check_throws: true,
        analyze_dead_code: true,
        memoize_properties: true,
        allow_possibly_undefined_array_keys: true,
        ..Default::default()
    }
}

#[must_use]
pub fn no_boolean_literal_comparison_settings() -> Settings {
    Settings { no_boolean_literal_comparison: true, ..Default::default() }
}

#[must_use]
pub fn php_90_settings() -> Settings {
    Settings::new(mago_php_version::PHPVersion::new(9, 0, 0))
}

#[must_use]
pub fn check_name_casing_settings() -> Settings {
    Settings { check_name_casing: true, ..Default::default() }
}

#[must_use]
pub fn check_missing_type_hints_settings() -> Settings {
    Settings { check_missing_type_hints: true, ..Default::default() }
}

fn run_test_case_inner(config: TestCase) {
    let Prelude { mut database, mut metadata, mut symbol_references } = PRELUDE.clone();

    let file = File::ephemeral(Cow::Owned(config.name.as_bytes().to_vec()), Cow::Owned(config.content.to_vec()));
    let file_id = database.add(file);
    let source_file = database.get_ref(&file_id).expect("File just added should exist");

    let arena = LocalArena::new();
    let program = parse_file(&arena, source_file);
    assert!(!program.has_errors(), "Parse failed: {:?}", program.errors);

    let resolver = NameResolver::new(&arena);
    let resolved_names = resolver.resolve(program);

    let settings = config.settings.unwrap_or_else(default_test_settings);

    metadata.extend(scan_program(&arena, source_file, program, &resolved_names, settings.version));
    populate_on_prelude(&mut metadata, &mut symbol_references);

    let mut analysis_result = AnalysisResult::new(symbol_references);
    let analyzer = Analyzer::new(&arena, source_file, &resolved_names, &metadata, &PLUGIN_REGISTRY, settings);

    let analysis_run_result = analyzer.analyze(program, &mut analysis_result);

    if let Err(err) = analysis_run_result {
        panic!("Test '{}': Expected analysis to succeed, but it failed with an error: {}", config.name, err);
    }

    for (class, property, expected) in config.expected_property_reads {
        let symbol = (ascii_lowercase_word(class.as_bytes()), word(property.as_bytes()));
        let actual = analysis_result.symbol_references.count_property_reads(&symbol);
        assert_eq!(
            expected, actual,
            "Test '{}': expected {expected} read reference(s) to {class}::{property}, found {actual}",
            config.name,
        );
    }

    for (class, member, expected) in config.expected_symbol_references {
        let symbol = (ascii_lowercase_word(class.as_bytes()), word(member.as_bytes()));
        let actual = analysis_result.symbol_references.count_referencing_symbols(&symbol, false)
            + analysis_result.symbol_references.count_referencing_symbols(&symbol, true);
        assert_eq!(
            expected, actual,
            "Test '{}': expected {expected} reference(s) to {class}::{member}, found {actual}",
            config.name,
        );
    }

    verify_reported_issues(config.name, config.content, analysis_result, metadata);
}

/// The issues a case expects, by code. Each comment line that starts with `@mago-expect analysis:<code>` names one,
/// and `<code>(N)` names N, as in `@mago-expect analysis:mixed-operand(2), unused-variable`. The case writes them in
/// its comments, and only this harness reads them: Mago reads no comment of the code it analyzes.
fn expected_issue_counts(content: &[u8]) -> BTreeMap<String, usize> {
    let mut expected = BTreeMap::new();
    for line in String::from_utf8_lossy(content).lines() {
        let Some((before, written)) = line.split_once("@mago-expect") else {
            continue;
        };
        // The comment's own text starts with the expectation: after `//`, `#` or `/*` on the line, or after the `*` of
        // a docblock's line, so a commented-out comment names nothing.
        let opener =
            ["//", "/*", "#"].into_iter().filter_map(|opener| Some((before.find(opener)?, opener.len()))).min();
        let comment = match opener {
            Some((start, length)) => &before[start + length..],
            None => match before.trim_start().strip_prefix('*') {
                Some(comment) => comment,
                None => continue,
            },
        };
        if !comment.trim_start_matches('*').trim().is_empty() {
            continue;
        }
        let Some((category, codes)) = written.trim_start().split_once(':') else {
            continue;
        };
        if !matches!(category, "analysis" | "analyzer" | "analyser") {
            continue;
        }

        for code in expected_codes(codes.trim_start()) {
            let code = code.split_once(':').map_or(code, |(_, code)| code.trim_start());
            let (code, count) = match code.strip_suffix(')').and_then(|code| code.rsplit_once('(')) {
                Some((code, count)) => (code.trim_end(), count.trim().parse().expect("a count is a number")),
                None => (code, 1),
            };

            *expected.entry(code.to_owned()).or_insert(0) += count;
        }
    }

    expected
}

/// The codes of an `@mago-expect` line, the comma-separated list before its description: whitespace ends the list
/// unless a comma or a category's `:` stands beside it.
fn expected_codes(written: &str) -> impl Iterator<Item = &str> {
    let mut end = written.len();
    let mut rest = written;
    while let Some(space) = rest.find(char::is_whitespace) {
        let before = written.len() - rest.len() + space;
        let after = rest[space..].trim_start();
        if written[..before].ends_with([',', ':']) || after.starts_with(',') {
            rest = after;
            continue;
        }

        end = before;
        break;
    }

    written[..end].split(',').map(str::trim).filter(|code| !code.is_empty())
}

fn verify_reported_issues(
    test_name: &str,
    content: &[u8],
    mut analysis_result: AnalysisResult,
    mut codebase: CodebaseMetadata,
) {
    let expected_issue_counts = expected_issue_counts(content);
    let mut actual_issues_collected = std::mem::take(&mut analysis_result.issues);

    actual_issues_collected.extend(codebase.take_issues(true));

    let mut actual_issue_counts: BTreeMap<String, usize> = BTreeMap::new();
    for actual_issue in &actual_issues_collected {
        let Some(issue_code) = actual_issue.code.clone() else {
            panic!("Analyzer returned an issue with no code: {actual_issue:?}");
        };

        *actual_issue_counts.entry(issue_code).or_insert(0) += 1;
    }

    if actual_issue_counts != expected_issue_counts {
        let mut panic_message = format!("Test '{test_name}' failed with issue discrepancies:\n");
        let mut codes: Vec<&String> = actual_issue_counts.keys().chain(expected_issue_counts.keys()).collect();
        codes.sort_unstable();
        codes.dedup();
        for code in codes {
            let actual = actual_issue_counts.get(code).copied().unwrap_or(0);
            let expected = expected_issue_counts.get(code).copied().unwrap_or(0);
            if actual != expected {
                let lines: Vec<String> = actual_issues_collected
                    .iter()
                    .filter(|issue| issue.code.as_deref() == Some(code.as_str()))
                    .filter_map(|issue| issue.primary_span())
                    .map(|span| content[..span.start.offset as usize].split(|byte| *byte == b'\n').count().to_string())
                    .collect();
                let _ = writeln!(
                    panic_message,
                    "  - `{code}`: expected {expected}, found {actual}, on lines [{}].",
                    lines.join(", ")
                );
            }
        }

        panic!("{}", panic_message);
    }
}
