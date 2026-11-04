//! Raises one plain PHP class file into PHP#, the other way from `sharp-bridge`, which lowers PHP# into PHP.
//!
//! Each declaration's type is the one the project's analysis holds for it: its native declaration, its PHPDoc, an
//! extension host's refinement, or the type it inherits from its parent, and never a type the analysis inferred from a
//! body. A declaration with no type anywhere is `mixed`, which PHP# writes `Any?`. The output is edits on the file's own
//! text, so its comments and layout stay.
//!
//! A file converts whole or not at all. Each construct with no PHP# form yet, or none at all, is a [`Refusal`] that
//! names its place and the construct, and the file then stays plain PHP.

use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::ttype::resolution::TypeResolutionContext;
use mago_codex::ttype::template::variance::Variance;
use mago_codex::ttype::union::TUnion;
use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_names::scope::NamespaceScope;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::NamespaceBody;
use mago_syntax::cst::Program;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UseItem;
use mago_syntax::cst::UseItems;
use mago_syntax::token::is_sharp_keyword;

use crate::body::Locals;
use crate::edits::Edits;
use crate::types::Imports;
use crate::types::type_text;

mod body;
mod declaration;
mod edits;
mod library;

pub use library::Library;
mod types;

/// What the conversion of one file reads: the file and its syntax tree, and the project's analysis of it.
pub struct Input<'input, 'arena> {
    pub file: &'input File,
    pub program: &'input Program<'arena>,
    pub names: &'input ResolvedNames<'arena>,
    pub codebase: &'input CodebaseMetadata,
    pub artifacts: &'input AnalysisArtifacts,
    /// The standard library's members that run a PHP function, which a bare call of a built-in becomes.
    pub library: &'input Library,
}

/// A construct the converter does not convert, at `span`, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub span: Span,
    pub reason: String,
}

/// The PHP# text of `input`'s file, or every construct that keeps it plain PHP.
///
/// # Errors
///
/// Returns each [`Refusal`] the file holds.
pub fn convert(input: &Input<'_, '_>) -> Result<Vec<u8>, Vec<Refusal>> {
    let mut converter = Converter {
        input,
        source: &input.file.contents,
        edits: Edits::default(),
        imports: Imports::new(""),
        scope: NamespaceScope::global(),
        refusals: Vec::new(),
        imports_at: ImportsAt::FileStart,
        deleted_lines: None,
        class: None,
        method: None,
    };
    converter.program();
    if !converter.refusals.is_empty() {
        return Err(converter.refusals);
    }

    let added: Vec<String> = converter.imports.added().map(|import| format!("import {import};")).collect();
    if !added.is_empty() {
        let (at, text) = match converter.imports_at {
            ImportsAt::FileStart => (0, format!("{}\n\n", added.join("\n"))),
            ImportsAt::AfterNamespace(at) => (at, format!("\n\n{}", added.join("\n"))),
            ImportsAt::AfterImport(at) => (at, format!("\n{}", added.join("\n"))),
        };
        converter.edits.insert(at, text);
    }

    converter.edits.finish(converter.source).ok_or_else(|| {
        vec![Refusal { span: input.program.span(), reason: "two of the converter's rewrites overlap".to_owned() }]
    })
}

/// Where the `import` lines the conversion adds go.
#[derive(Debug, Clone, Copy)]
enum ImportsAt {
    /// At the start of a file with no namespace.
    FileStart,
    /// On their own lines after the `namespace` line, which ends at this offset.
    AfterNamespace(u32),
    /// Right after the last `import` line, which ends at this offset.
    AfterImport(u32),
}

/// The class whose members are converted.
struct Class<'input> {
    /// The class's name as the file declares it.
    name: String,
    metadata: &'input ClassLikeMetadata,
    is_final: bool,
    /// Each property, as `$name`, the class's constructor sets in a statement of its own body, which runs on every path.
    set_by_constructor: Vec<Vec<u8>>,
}

/// The method whose body is converted.
struct Method {
    /// The declared return, which a returned value lands in, or `None` in a constructor.
    returning: Option<TUnion>,
    /// The type parameters a `@var` tag in the body reads, the class's and the method's.
    resolution: TypeResolutionContext,
    /// The method's parameters, by their PHP names, which are never locals.
    parameters: Vec<Vec<u8>>,
    locals: Locals,
    /// The `parent::__construct(…)` statement a subclass's constructor starts with, which its `: super(…)` runs, so
    /// the body leaves it (spec section 9).
    super_call: Option<Span>,
}

struct Converter<'input, 'arena> {
    input: &'input Input<'input, 'arena>,
    source: &'input [u8],
    edits: Edits,
    imports: Imports,
    /// The file's namespace and `use` lines, which a `@var` tag's names resolve against, as the analysis reads them.
    scope: NamespaceScope,
    refusals: Vec<Refusal>,
    imports_at: ImportsAt,
    /// The start and end of the last run of whole lines [`Converter::delete_lines`] deleted.
    deleted_lines: Option<(u32, u32)>,
    class: Option<Class<'input>>,
    method: Option<Method>,
}

impl<'input> Converter<'input, '_> {
    fn refuse(&mut self, span: Span, reason: impl Into<String>) {
        self.refusals.push(Refusal { span, reason: reason.into() });
    }

    fn text(&self, span: Span) -> &'input [u8] {
        &self.source[span.start.offset as usize..span.end.offset as usize]
    }

    /// The offset of the first byte at or after `offset` that is no whitespace.
    fn skip_whitespace(&self, offset: u32) -> u32 {
        let mut end = offset as usize;
        while end < self.source.len() && self.source[end].is_ascii_whitespace() {
            end += 1;
        }

        end as u32
    }

    /// The whitespace that starts the line `offset` is on.
    fn indentation(&self, offset: u32) -> String {
        let start = self.source[..offset as usize].iter().rposition(|byte| *byte == b'\n').map_or(0, |at| at + 1);
        let width = self.source[start..].iter().take_while(|byte| matches!(byte, b' ' | b'\t')).count();

        String::from_utf8_lossy(&self.source[start..start + width]).into_owned()
    }

    /// The start and end of the whole lines `span` fills, its indentation and its line break included, so deleting
    /// them leaves the lines around it as they were. Only `span` when other code shares its lines.
    fn lines_of(&self, span: Span) -> (u32, u32) {
        let (mut start, mut end) = (span.start.offset as usize, span.end.offset as usize);
        while start > 0 && matches!(self.source[start - 1], b' ' | b'\t') {
            start -= 1;
        }
        while end < self.source.len() && matches!(self.source[end], b' ' | b'\t' | b'\r') {
            end += 1;
        }
        let whole =
            (start == 0 || self.source[start - 1] == b'\n') && self.source.get(end).is_none_or(|byte| *byte == b'\n');
        if !whole {
            return (span.start.offset, span.end.offset);
        }

        (start as u32, (end + 1).min(self.source.len()) as u32)
    }

    /// Deletes the whole lines `span` fills, and the blank line after them when a blank line comes before them too,
    /// so the file keeps one blank line where it had two around the statement. Lines deleted right after the last
    /// ones are one run, whose blank line before is the run's.
    fn delete_lines(&mut self, span: Span) {
        let (start, mut end) = self.lines_of(span);
        let run_start = match self.deleted_lines {
            Some((run_start, run_end)) if run_end == start => run_start,
            _ => start,
        };
        if self.source[..run_start as usize].ends_with(b"\n\n") && self.source.get(end as usize) == Some(&b'\n') {
            end += 1;
        }
        self.edits.delete(start, end);
        self.deleted_lines = Some((run_start, end));
    }

    /// `union` in PHP#, or a refusal at `span` with the type PHP# can't write.
    fn type_text(&mut self, union: &TUnion, span: Span) -> Option<String> {
        match type_text(union, self.input.codebase, &mut self.imports) {
            Ok(text) => Some(text),
            Err(reason) => {
                self.refuse(span, reason);
                None
            }
        }
    }

    /// The name this file writes for the class `identifier` names, as its resolved name gives it, or a refusal.
    fn class_name(&mut self, identifier: &Identifier<'_>) -> Option<String> {
        let resolved = self.input.names.get(identifier);
        let declared = self.input.codebase.get_class_like(resolved).map_or_else(
            || String::from_utf8_lossy(resolved).into_owned(),
            |class| class.original_name.as_str_lossy().into_owned(),
        );
        match self.imports.name_of(&declared) {
            Ok(name) => Some(name),
            Err(reason) => {
                self.refuse(identifier.span(), reason);
                None
            }
        }
    }

    /// Writes the class `identifier` names by the name this file reaches it by.
    fn rename_class(&mut self, identifier: &Identifier<'_>) {
        if let Some(name) = self.class_name(identifier) {
            self.edits.replace(identifier.span(), name);
        }
    }

    /// The PHP# name of a variable `$name`: the name alone, with `@` before a PHP# keyword (spec section 2). `$this`
    /// is the keyword `this`.
    fn variable_name(name: &[u8]) -> String {
        let bare = name.strip_prefix(b"$").unwrap_or(name);
        let bare = String::from_utf8_lossy(bare).into_owned();
        if bare != "this" && is_sharp_keyword(bare.as_bytes()) { format!("@{bare}") } else { bare }
    }

    fn program(&mut self) {
        let program = self.input.program;
        if let Some(error) = program.errors.first() {
            self.refuse(error.span(), "the file does not parse");
            return;
        }

        for statement in &program.statements {
            self.file_statement(statement);
        }
    }

    fn file_statement(&mut self, statement: &Statement<'_>) {
        match statement {
            Statement::OpeningTag(tag) => {
                let end = self.skip_whitespace(tag.span().end.offset);
                self.edits.delete(tag.span().start.offset, end);
            }
            // PHP# is always strict (spec section 27), so a file without `strict_types` takes strict calls too, which
            // the proof's analysis and the project's tests check.
            Statement::Declare(declare) => {
                if !declare.items.iter().all(|item| item.name.value.eq_ignore_ascii_case(b"strict_types")) {
                    self.refuse(declare.span(), "a `declare` other than `strict_types` has no PHP# form");
                    return;
                }
                let end = self.skip_whitespace(declare.span().end.offset);
                self.edits.delete(declare.span().start.offset, end);
            }
            Statement::Namespace(namespace) => {
                let NamespaceBody::Implicit(body) = &namespace.body else {
                    self.refuse(namespace.span(), "a namespace with braces has no PHP# form: a file is in one namespace");
                    return;
                };
                if let Some(name) = &namespace.name {
                    let written = String::from_utf8_lossy(name.value()).into_owned();
                    self.edits.replace(name.span(), written.replace('\\', "."));
                    self.imports = Imports::new(&written);
                    self.scope = NamespaceScope::for_namespace(name.value());
                }
                self.imports_at = ImportsAt::AfterNamespace(body.terminator.span().end.offset);
                for statement in &body.statements {
                    self.file_statement(statement);
                }
            }
            Statement::Use(r#use) => self.use_line(r#use),
            Statement::Class(class) => self.class(class),
            Statement::Interface(interface) => self.interface(interface),
            Statement::Enum(enumeration) => self.enumeration(enumeration),
            Statement::Trait(_) => {
                self.refuse(statement.span(), "a trait has no PHP# form: PHP# declares no traits (spec section 22)");
            }
            Statement::Noop(_) => {}
            _ => self.refuse(
                statement.span(),
                "a file-level statement or function has no PHP# form: a .sharp file declares one type (spec section 27)",
            ),
        }
    }

    /// A `use` line, as one `import` line per class it names (spec section 23). A function or a constant is reached
    /// through its namespace, which each use imports, and a root class of `Builtins.sharp` is named bare, so their
    /// lines go.
    fn use_line(&mut self, r#use: &mago_syntax::cst::Use<'_>) {
        self.scope.populate_from_use(r#use);
        let classes: Vec<(&[u8], &UseItem<'_>)> = match &r#use.items {
            UseItems::Sequence(sequence) => sequence.items.iter().map(|item| (b"".as_slice(), item)).collect(),
            UseItems::TypedSequence(_) | UseItems::TypedList(_) => Vec::new(),
            UseItems::MixedList(list) => list
                .items
                .iter()
                .filter(|item| item.r#type.is_none())
                .map(|item| (list.namespace.value(), &item.item))
                .collect(),
        };

        let mut lines = Vec::new();
        for (prefix, item) in classes {
            let written = [prefix, b"\\", item.name.value()].concat();
            let name = String::from_utf8_lossy(&written).trim_matches('\\').to_owned();
            let short = name.rsplit('\\').next().unwrap_or(&name).to_owned();
            let alias = item
                .alias
                .as_ref()
                .map_or_else(|| short.clone(), |alias| String::from_utf8_lossy(alias.identifier.value).into_owned());
            if Imports::is_default(&name) && alias == short {
                continue;
            }
            if !name.contains('\\') {
                self.refuse(
                    item.name.span(),
                    format!("`{name}` is a class of PHP's root namespace, which PHP# names only in the standard library (spec section 23)"),
                );
                continue;
            }
            self.imports.add_use(&alias, &name);
            // A name PHP# reserves for a type is imported under another (spec section 11).
            let alias = self.imports.name_of(&name).unwrap_or(alias);
            let dotted = name.replace('\\', ".");
            lines.push(if alias == short {
                format!("import {dotted};")
            } else {
                format!("import {dotted} as {alias};")
            });
        }

        let span = r#use.span();
        if lines.is_empty() {
            self.delete_lines(span);
            return;
        }
        let indentation = self.indentation(span.start.offset);
        self.edits.replace(span, lines.join(&format!("\n{indentation}")));
        self.imports_at = ImportsAt::AfterImport(span.end.offset);
    }

    /// The type parameter list of a class whose analysis gives it type parameters, from PHPDoc or a refinement.
    fn type_parameters(&mut self, class: &ClassLikeMetadata, span: Span) -> Option<String> {
        if class.template_types.is_empty() {
            return Some(String::new());
        }

        let mut written = Vec::new();
        for (index, (name, template)) in class.template_types.iter().enumerate() {
            let variance = match class.template_variance.get(index) {
                Some(Variance::Covariant) => "out ",
                Some(Variance::Contravariant) => "in ",
                _ => "",
            };
            if template.constraint.is_mixed() {
                written.push(format!("{variance}{name}"));
            } else {
                let bound = self.type_text(&template.constraint, span)?;
                written.push(format!("{variance}{name} : {bound}"));
            }
        }

        Some(format!("<{}>", written.join(", ")))
    }
}
