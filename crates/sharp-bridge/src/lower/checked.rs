use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_codex::metadata::CodebaseMetadata;
use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_syntax::cst::Program;
use mago_syntax::error::ParseError;

use super::inline::InlineForms;
use super::types::Types;

/// A program the checker accepted, with its resolved names and the types the analysis gave it.
///
/// It parsed without errors, and no check reported an error-level issue. Only [`check`] creates one, and the lowering
/// takes one, so the lowering never sees a construct the parser or the checks refuse.
///
/// Its fields are private to this module, so code must build one through [`check`]. `check` trusts its caller to pass
/// every issue the checks reported for the file. The orchestrator is that caller:
///
/// ```compile_fail
/// # mod inline {
/// #     pub struct InlineForm;
/// #     pub struct InlineForms;
/// #     impl InlineForms {
/// #         pub fn get(&self, _: &[u8]) -> Option<&InlineForm> { None }
/// #     }
/// #     pub fn key(_: &[u8], _: &[u8]) -> Vec<u8> { Vec::new() }
/// # }
/// # mod types {
/// #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lower/types.rs"));
/// # }
/// # mod checked {
/// #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lower/checked.rs"));
/// # }
/// # use mago_analyzer::artifacts::AnalysisArtifacts;
/// # use mago_codex::metadata::CodebaseMetadata;
/// # use mago_database::file::File;
/// # use mago_names::ResolvedNames;
/// # use mago_syntax::cst::Program;
/// fn skip_the_checks<'program>(
///     file: &'program File,
///     program: &'program Program<'program>,
///     names: ResolvedNames<'program>,
///     artifacts: &'program AnalysisArtifacts,
///     codebase: &'program CodebaseMetadata,
///     forms: &'program inline::InlineForms,
/// ) -> Option<checked::CheckedProgram<'program>> {
///     Some(checked::CheckedProgram { file, program, types: types::Types::new(names, artifacts, codebase, forms) })
/// }
/// # fn main() {}
/// ```
///
/// The same code compiles when the program goes through [`check`]:
///
/// ```
/// # mod inline {
/// #     pub struct InlineForm;
/// #     pub struct InlineForms;
/// #     impl InlineForms {
/// #         pub fn get(&self, _: &[u8]) -> Option<&InlineForm> { None }
/// #     }
/// #     pub fn key(_: &[u8], _: &[u8]) -> Vec<u8> { Vec::new() }
/// # }
/// # mod types {
/// #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lower/types.rs"));
/// # }
/// # mod checked {
/// #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lower/checked.rs"));
/// # }
/// # use mago_analyzer::artifacts::AnalysisArtifacts;
/// # use mago_codex::metadata::CodebaseMetadata;
/// # use mago_database::file::File;
/// # use mago_names::ResolvedNames;
/// # use mago_syntax::cst::Program;
/// fn skip_the_checks<'program>(
///     file: &'program File,
///     program: &'program Program<'program>,
///     names: ResolvedNames<'program>,
///     artifacts: &'program AnalysisArtifacts,
///     codebase: &'program CodebaseMetadata,
///     forms: &'program inline::InlineForms,
/// ) -> Option<checked::CheckedProgram<'program>> {
///     checked::check(file, program, names, artifacts, codebase, forms, &[]).ok()
/// }
/// # fn main() {}
/// ```
pub struct CheckedProgram<'program> {
    file: &'program File,
    program: &'program Program<'program>,
    types: Types<'program>,
}

impl<'program> CheckedProgram<'program> {
    /// `program` as if the checks passed, for the tests of what the lowering does with a construct they refuse.
    #[cfg(test)]
    pub(crate) fn unchecked(
        file: &'program File,
        program: &'program Program<'program>,
        names: ResolvedNames<'program>,
        artifacts: &'program AnalysisArtifacts,
        codebase: &'program CodebaseMetadata,
        inline_forms: &'program InlineForms,
    ) -> Self {
        Self { file, program, types: Types::new(names, artifacts, codebase, inline_forms) }
    }

    #[must_use]
    pub fn file(&self) -> &'program File {
        self.file
    }

    #[must_use]
    pub fn program(&self) -> &'program Program<'program> {
        self.program
    }

    #[must_use]
    pub fn names(&self) -> &ResolvedNames<'program> {
        self.types.names()
    }

    /// The checker's types, which each back end reads the program's types through.
    #[must_use]
    pub fn types(&self) -> &Types<'program> {
        &self.types
    }
}

/// Why [`check`] refused a program.
#[derive(Debug)]
pub enum Refusal<'program> {
    /// The parser's errors.
    Parse(&'program [ParseError]),
    /// The error-level issues the checks reported.
    Compile(Vec<Issue>),
}

/// Accepts `program` when it parsed without errors and the checks reported no error-level issue.
///
/// `program` is the orchestrator's parse of `file`, and `artifacts` and `codebase` are the file's analysis, which the
/// lowering reads its types from. `inline_forms` are the standard library's forms the lowering may inline, empty for
/// a standard library file. `issues` must be every issue the orchestrator reported for `file`. `check` runs no check
/// itself, so a caller that leaves an issue out owns the program it lowers.
///
/// # Errors
///
/// Returns the parser's errors, or else the error-level issues.
pub fn check<'program>(
    file: &'program File,
    program: &'program Program<'program>,
    names: ResolvedNames<'program>,
    artifacts: &'program AnalysisArtifacts,
    codebase: &'program CodebaseMetadata,
    inline_forms: &'program InlineForms,
    issues: &[Issue],
) -> Result<CheckedProgram<'program>, Refusal<'program>> {
    if !program.errors.is_empty() {
        return Err(Refusal::Parse(program.errors));
    }

    let errors: Vec<Issue> = issues.iter().filter(|issue| issue.level == Level::Error).cloned().collect();
    if !errors.is_empty() {
        return Err(Refusal::Compile(errors));
    }

    Ok(CheckedProgram { file, program, types: Types::new(names, artifacts, codebase, inline_forms) })
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use mago_allocator::LocalArena;
    use mago_names::resolver::NameResolver;
    use mago_syntax::dialect::Dialect;
    use mago_syntax::parser::parse_file_with_dialect;
    use mago_syntax::settings::ParserSettings;

    use super::*;

    fn file(code: &'static str) -> File {
        File::ephemeral(Cow::Borrowed(b"src/Report.sharp"), Cow::Borrowed(code.as_bytes()))
    }

    #[test]
    fn a_program_with_a_parse_error_is_not_checked() {
        let file = file("class Report\n{\n    public int run()\n    {\n        return this->run();\n    }\n}\n");
        let arena = LocalArena::new();
        let program = parse_file_with_dialect(&arena, &file, Dialect::Sharp, ParserSettings::default());
        let names = NameResolver::new(&arena).resolve(program);
        let (artifacts, codebase, forms) = (AnalysisArtifacts::new(), CodebaseMetadata::new(), InlineForms::default());

        let refusal = check(&file, program, names, &artifacts, &codebase, &forms, &[]);

        assert!(matches!(refusal, Err(Refusal::Parse(errors)) if errors.len() == 1));
    }

    #[test]
    fn a_program_with_an_error_level_issue_is_refused_with_only_its_errors() {
        let file = file("class Report\n{\n}\n");
        let arena = LocalArena::new();
        let program = parse_file_with_dialect(&arena, &file, Dialect::Sharp, ParserSettings::default());
        let names = NameResolver::new(&arena).resolve(program);
        let (artifacts, codebase, forms) = (AnalysisArtifacts::new(), CodebaseMetadata::new(), InlineForms::default());
        let issues = [Issue::warning("a warning"), Issue::error("an error"), Issue::help("a help")];

        let refusal = check(&file, program, names, &artifacts, &codebase, &forms, &issues);

        assert!(
            matches!(&refusal, Err(Refusal::Compile(errors)) if errors.len() == 1 && errors[0].message == "an error")
        );
    }
}
