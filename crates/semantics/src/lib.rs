#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use std::collections::HashSet;

use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_php_version::PHPVersion;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_reporting::Level;
use mago_syntax::cst::Program;
use mago_syntax::dialect::Dialect;
use mago_syntax::error::ParseError;
use mago_syntax::walker::Walker;

use crate::internal::CheckingWalker;
use crate::internal::context::Context;

pub use crate::internal::checker::class_like::is_php_keyword;

mod internal;

/// The main entry point for performing semantic analysis on a PHP program's CST.
///
/// This checker is responsible for traversing the Concrete Syntax Tree (CST)
/// and validating the code against a set of semantic rules, such as type correctness,
/// variable usage, and adherence to language features for a specific PHP version.
#[derive(Debug, Clone, Copy)]
pub struct SemanticsChecker {
    version: PHPVersion,
}

impl SemanticsChecker {
    /// Creates a new `SemanticsChecker`.
    ///
    /// # Arguments
    ///
    /// - `php_version`: The target PHP version to check against.
    ///
    #[must_use]
    pub fn new(php_version: PHPVersion) -> Self {
        Self { version: php_version }
    }

    /// Analyzes the given program CST for semantic issues.
    ///
    /// This method walks the entire CST, applying semantic rules and collecting any
    /// violations it finds.
    ///
    /// # Arguments
    ///
    /// - `source`: The source file being analyzed.
    /// - `program`: The root of the CST for the program.
    /// - `names`: The resolved names (e.g., fully qualified class names) from the name resolution pass.
    ///
    /// # Returns
    ///
    /// An `IssueCollection` containing all semantic issues discovered during the check.
    #[must_use]
    pub fn check<'ast, 'arena>(
        &self,
        file: &File,
        program: &'ast Program<'arena>,
        names: &'ast ResolvedNames<'arena>,
    ) -> IssueCollection {
        // A PHP version gates PHP's syntax, never PHP#'s, so a `.sharp` file is checked as the engine's PHP that PHP#
        // lowers to, whatever version the project names, and gets none of the version checks.
        let version = if program.dialect.is_sharp() { PHPVersion::ENGINE } else { self.version };
        let mut context = Context::new(version, program, names, file);

        CheckingWalker.walk_program(program, &mut context);

        context.finalize()
    }
}

/// Returns the analyzer issues of one file to report beside its parse errors and semantic issues.
///
/// In a PHP# file, an analyzer issue on a line that holds a parse error or a semantic error, or whose span overlaps
/// one, as `exit(` does with a refused argument on the next line, is dropped as a follow-on of that refusal, as a
/// missing `: super(…)` is beside the parse error of `: base(row)`. The file cannot run until the refusal is fixed, so an issue
/// dropped there shows up one fix later at worst. A PHP file keeps every analyzer issue.
#[must_use]
pub fn drop_follow_on_issues(
    file: &File,
    parse_errors: &[ParseError],
    semantic_issues: &IssueCollection,
    analyzer_issues: IssueCollection,
) -> IssueCollection {
    if !Dialect::of(file).is_sharp() {
        return analyzer_issues;
    }

    let refusals: Vec<_> = semantic_issues
        .iter()
        .filter(|issue| issue.level == Level::Error)
        .filter_map(|issue| issue.primary_span())
        .filter(|span| span.file_id == file.id)
        .chain(parse_errors.iter().filter_map(|error| Issue::from(error).primary_span()))
        .collect();
    let refused_lines: HashSet<u32> = refusals.iter().map(|span| file.line_number(span.start.offset)).collect();

    analyzer_issues
        .into_iter()
        .filter(|issue| {
            issue.primary_span().is_none_or(|span| {
                span.file_id != file.id
                    || !(refused_lines.contains(&file.line_number(span.start.offset))
                        || refusals.iter().any(|refusal| !refusal.is_before(&span) && !span.is_before(refusal)))
            })
        })
        .collect()
}
