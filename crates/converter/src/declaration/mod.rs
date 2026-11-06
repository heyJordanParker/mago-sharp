//! Classes and their members: the header, properties and fields, methods and constructors, and their parameters. This
//! module holds what every declaration shares, its attributes, docblock and visibility, and each family has a module.
//!
//! PHP# defaults a class to `internal` and a member to `private` (spec section 5), so the converter writes `public`
//! wherever PHP leaves the visibility out. PHP's methods are open unless `final`, and PHP#'s are closed unless
//! `virtual` (spec section 22), so an open PHP method is `virtual`, and one that replaces its parent's is `override`.

mod class;
mod constructor;
mod members;

use mago_codex::metadata::ttype::TypeMetadata;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::Sequence;

use crate::Converter;
use crate::types::narrowing;
use crate::types::type_text;

/// The PHPDoc tags whose types the converter writes in the declaration, which leave its docblock.
const WRITTEN_TAGS: [&str; 16] = [
    "@var",
    "@param",
    "@return",
    "@template",
    "@template-covariant",
    "@template-contravariant",
    "@extends",
    "@implements",
    "@use",
    "@template-extends",
    "@template-implements",
    "@template-use",
    "@psalm-template",
    "@psalm-var",
    "@psalm-param",
    "@psalm-return",
];

/// The accessors PHP# writes after a property's name: none for a field, which is storage, `get` alone for a
/// `readonly` one, and a `set` with the narrower visibility PHP's `private(set)` or `protected(set)` gives (spec
/// sections 6 and 13).
fn accessors(is_public: bool, is_readonly: bool, modifiers: &Sequence<'_, Modifier<'_>>) -> &'static str {
    let set = modifiers.iter().find(|modifier| modifier.is_write_visibility());
    match (is_public, is_readonly, set) {
        (_, true, _) => " { get; }",
        (_, false, Some(Modifier::PrivateSet(_))) => " { get; private set; }",
        (_, false, Some(Modifier::ProtectedSet(_))) => " { get; protected set; }",
        (true, false, _) => " { get; set; }",
        (false, false, _) => "",
    }
}

/// The case spec section 24 names a kind of declaration in, as TypeScript does.
#[derive(Debug, Clone, Copy)]
pub(crate) enum NameCase {
    /// Types and enum cases.
    Pascal,
    /// Methods, properties, fields, parameters and locals.
    Camel,
    /// Constants.
    UpperSnake,
}

fn has(modifiers: &Sequence<'_, Modifier<'_>>, matches: impl Fn(&Modifier<'_>) -> bool) -> Option<Span> {
    modifiers.iter().find(|modifier| matches(modifier)).map(HasSpan::span)
}

impl Converter<'_, '_> {
    /// PHP's `#[…]` attributes as PHP#'s `[…]` (spec section 1). Each stays, with its arguments.
    pub(crate) fn attributes(&mut self, lists: &Sequence<'_, AttributeList<'_>>) {
        for list in lists {
            self.edits.replace(list.hash_left_bracket, "[");
            for attribute in &list.attributes {
                self.rename_class(&attribute.name);
                let Some(arguments) = &attribute.argument_list else {
                    continue;
                };
                for argument in &arguments.arguments {
                    match argument {
                        PartialArgument::Positional(argument) if argument.ellipsis.is_none() => {
                            self.expression(argument.value, None);
                        }
                        PartialArgument::Named(argument) => self.expression(argument.value, None),
                        _ => self.refuse(
                            argument.span(),
                            "a spread or placeholder argument of an attribute has no PHP# form",
                        ),
                    }
                }
            }
        }
    }

    /// Takes each tag whose type the declaration now writes out of the docblock right before `declaration`, and the
    /// whole docblock when nothing else is left in it. Its prose and every other tag stay.
    pub(crate) fn docblock(&mut self, declaration: Span) {
        let Some(docblock) = self.docblock_before(declaration) else {
            return;
        };

        let text = String::from_utf8_lossy(docblock.1).into_owned();
        let is_written = |line: &str| {
            let content = line.trim().trim_start_matches("/**").trim_start_matches('*').trim();
            content.split_whitespace().next().is_some_and(|tag| WRITTEN_TAGS.contains(&tag))
        };
        let lines: Vec<&str> = text.lines().collect();
        let mut kept: Vec<&str> = Vec::new();
        let mut index = 0;
        while index < lines.len() {
            if is_written(lines[index]) {
                // A tag's text runs on over the lines up to the next tag or the end, as a long `@param` does.
                index += 1;
                while index < lines.len() && !is_tag_or_end(lines[index]) {
                    index += 1;
                }
                continue;
            }
            kept.push(lines[index]);
            index += 1;
        }
        if kept.len() == lines.len() {
            return;
        }

        let is_empty = |line: &str| {
            let content = line.trim().trim_start_matches("/**").trim_end_matches("*/").trim_start_matches('*').trim();
            content.is_empty()
        };
        if kept.iter().all(|line| is_empty(line)) {
            self.edits.delete(docblock.0.start.offset, self.skip_whitespace(docblock.0.end.offset));
            return;
        }
        // A blank line left right before the closing `*/` goes too.
        while kept.len() > 2 && is_empty(kept[kept.len() - 2]) && !kept[kept.len() - 2].contains("/**") {
            kept.remove(kept.len() - 2);
        }
        self.edits.replace(docblock.0, kept.join("\n"));
    }

    /// The docblock right before `declaration`, with nothing but whitespace between them, and its text.
    pub(crate) fn docblock_before(&self, declaration: Span) -> Option<(Span, &[u8])> {
        let start = declaration.start.offset;
        self.input
            .program
            .trivia
            .iter()
            .rev()
            .find(|trivia| {
                trivia.kind.is_docblock()
                    && trivia.span.end.offset <= start
                    && self.source[trivia.span.end.offset as usize..start as usize].iter().all(u8::is_ascii_whitespace)
            })
            .map(|trivia| (trivia.span, trivia.value))
    }

    /// Refuses `name`, the name a `kind` of declaration takes at `span`, when it breaks the case spec section 24 gives
    /// that kind, and gives whether it did. The PHP# checker warns on such a name, and a `.sharp` file holds no
    /// warning, while renaming it changes the name PHP code reaches it by, so the PHP takes the new name first.
    pub(crate) fn misnamed(&mut self, name: &[u8], span: Span, kind: &str, case: NameCase) -> bool {
        let (holds, written, fixed) = match case {
            NameCase::Pascal => (mago_casing::is_class_case(name), "Pascal case", mago_casing::to_class_case(name)),
            NameCase::Camel => (mago_casing::is_camel_case(name), "camel case", mago_casing::to_camel_case(name)),
            NameCase::UpperSnake => {
                (mago_casing::is_constant_case(name), "upper snake case", mago_casing::to_constant_case(name))
            }
        };
        if holds {
            return false;
        }
        let (shown, fixed) = (String::from_utf8_lossy(name), String::from_utf8_lossy(&fixed));
        self.refuse(
            span,
            format!("the {kind} `{shown}` has no PHP# name: PHP# names a {kind} in {written}, as `{fixed}` (spec section 24)"),
        );

        true
    }

    /// Refuses a member whose type `metadata` comes from a PHPDoc `tag` of the file that narrows the PHP# type the
    /// member is written with, as `@return non-empty-string` narrows `string`, at `span`, and gives whether it did. The
    /// analysis of a `.sharp` file reads no PHPDoc tag, so the narrowing would leave every check of the member's
    /// callers.
    pub(crate) fn narrowed_by_docblock(&mut self, metadata: Option<&TypeMetadata>, tag: &str, span: Span) -> bool {
        // A tag of this file's own docblocks, which an extension host's refinement and an inherited type are not.
        let written = |metadata: &&TypeMetadata| {
            let at = metadata.span;
            metadata.from_docblock
                && self.input.program.trivia.iter().any(|trivia| {
                    trivia.kind.is_docblock()
                        && trivia.span.file_id == at.file_id
                        && trivia.span.start.offset <= at.start.offset
                        && at.end.offset <= trivia.span.end.offset
                })
        };
        let Some(metadata) = metadata.filter(written) else {
            return false;
        };
        let Some(narrower) = narrowing(&metadata.type_union) else {
            return false;
        };
        // A type PHP# can't write refuses where the member writes it.
        let Ok(general) = type_text(&metadata.type_union, self.input.codebase, &mut self.imports) else {
            return false;
        };
        self.refuse(
            span,
            format!("`{tag}` gives `{narrower}`, which narrows the `{general}` PHP# writes, and the analysis of a `.sharp` file reads no PHPDoc, so converting would drop the narrowing"),
        );

        true
    }

    /// The offset `public ` goes at for a member whose modifiers name no visibility, which PHP makes public.
    fn missing_visibility(modifiers: &Sequence<'_, Modifier<'_>>, keyword: Span) -> Option<u32> {
        let has_visibility = modifiers
            .iter()
            .any(|modifier| matches!(modifier, Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_)));

        (!has_visibility).then(|| modifiers.first().map_or(keyword, HasSpan::span).start.offset)
    }
}

/// Whether a docblock line starts a tag or closes the docblock, which ends the text of the tag before it.
fn is_tag_or_end(line: &str) -> bool {
    let content = line.trim().trim_start_matches('*').trim();

    content.starts_with('@') || line.trim().starts_with("*/") || line.trim().is_empty()
}
