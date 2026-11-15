//! Classes and their members: the header, properties and fields, methods and constructors, and their parameters. This
//! module holds what every declaration shares, its attributes, docblock and visibility, and each family has a module.
//!
//! PHP# defaults a class to `internal` and a member to `private` (spec section 5), so the converter writes every class
//! `public`, and each member PHP makes public `public` in a library and as wide as its uses need in a project, whose
//! too wide members the checker refuses. PHP's methods are open unless `final`, and PHP#'s are closed unless
//! `virtual` (spec section 22), so an open PHP method is `virtual`, and one that replaces its parent's is `override`.

mod class;
mod constructor;
mod members;

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;

use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::attribute::AttributeMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::ttype::TypeMetadata;
use mago_codex::reference::ReferenceOrigin;
use mago_codex::reference::SymbolReferenceKind;
use mago_codex::reference::SymbolReferences;
use mago_names::DYNAMICALLY_ACCESSED;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::Sequence;
use mago_word::Word;
use mago_word::word;

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
/// `readonly` one, and a `private set` for PHP's `private(set)` (spec sections 6 and 13). Any other `set` reaches the
/// property's namespace at most, PHP's `protected(set)` and `public(set)` too (spec section 6.1), and is `private`
/// where `setters` says the class alone sets the property.
fn accessors(
    is_public: bool,
    is_readonly: bool,
    modifiers: &Sequence<'_, Modifier<'_>>,
    setters: Setters,
) -> &'static str {
    let set = modifiers.iter().find(|modifier| modifier.is_write_visibility());
    match (is_public, is_readonly, set) {
        (_, true, _) => " { get; }",
        (_, false, Some(Modifier::PrivateSet(_))) => " { get; private set; }",
        (true, false, _) if setters == Setters::Class => " { get; private set; }",
        (true, false, _) => " { get; set; }",
        (false, false, _) => "",
    }
}

/// Who sets a property of the converted class, by the writes the project's analysis records. PHP# sets a property only
/// inside its own namespace, so code of another namespace sets none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Setters {
    /// The class's own code alone, or no code.
    Class,
    /// Other code of the class's namespace too.
    Namespace,
}

/// Who uses a member of the converted class, other than the class's own code, by the project's `references` and the
/// subclasses that replace it. A member the class's own code alone uses has a `Uses` with nothing set, and one nothing
/// uses has none.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Uses {
    /// The code of a subclass, or a subclass that replaces the method.
    subclass: bool,
    /// The code of a subclass outside the class's namespace and the namespaces below it, or one that replaces the method.
    subclass_outside: bool,
    /// Other code of the class's namespace or one below it, or a subclass there that replaces the property.
    namespace: bool,
    /// Any other code, code outside every class and function among it, or a subclass there that replaces the property.
    outside: bool,
}

impl Uses {
    /// The narrowest access level these uses need, as the checker's `too-wide-member` reads them (spec section 5):
    /// `private` for the class's own code, `protected` for its subclasses' too, `internal` for its namespace's, and
    /// `public` otherwise. A subclass's code outside the namespace and other code of the namespace together need
    /// `public`, since neither `protected` nor `internal` reaches both.
    fn access(self) -> &'static str {
        if self.outside || self.namespace && self.subclass_outside {
            "public "
        } else if self.namespace {
            "internal "
        } else if self.subclass {
            "protected "
        } else {
            "private "
        }
    }
}

/// Who uses each member of `class`, by its name in `references`: `$name` for a property, the lowercase name for a
/// method, and the name for a constant. A use through a subclass's object, as `$order->total()` of a subclass of the
/// class that declares `total`, is a use of the member. A subclass that replaces a method needs it at least
/// `protected`, and one that replaces a property needs it a property, which a PHP# field never is, so it counts as code
/// of its namespace or outside it.
fn member_uses(
    class: &ClassLikeMetadata,
    codebase: &CodebaseMetadata,
    references: &SymbolReferences,
) -> HashMap<Word, Uses> {
    let key = codebase.class_key(class.name.as_bytes());
    let full_name = class.original_name.as_str_lossy();
    let own = namespace_of(&full_name);
    // Whether `symbol`, a class's or a function's name, is a subclass of `class`, and whether it is outside `own`.
    let place = |symbol: Word| {
        let name = codebase.get_class_like(symbol.as_bytes()).map_or_else(
            || symbol.as_str_lossy().into_owned(),
            |other| other.original_name.as_str_lossy().into_owned(),
        );
        let is_subclass = codebase.is_instance_of(symbol.as_bytes(), class.name.as_bytes());
        (is_subclass, !is_within(namespace_of(&name), own))
    };

    let mut uses: HashMap<Word, Uses> = HashMap::new();
    references.for_each_reference(|origin, (on, member), _| {
        if member.as_bytes().is_empty() || on != key && !codebase.is_instance_of(on.as_bytes(), class.name.as_bytes()) {
            return;
        }
        let found = uses.entry(member).or_default();
        let ReferenceOrigin::Symbol((symbol, _)) = origin else {
            found.outside = true;
            return;
        };
        if symbol == key {
            return;
        }
        match place(symbol) {
            (true, outside) => {
                found.subclass = true;
                found.subclass_outside |= outside;
            }
            (false, false) => found.namespace = true,
            (false, true) => found.outside = true,
        }
    });

    for (child, metadata) in &codebase.class_likes {
        if *child == key || !codebase.is_instance_of(metadata.name.as_bytes(), class.name.as_bytes()) {
            continue;
        }
        let (_, outside) = place(*child);
        // A subclass holds each member it inherits among those it overrides, and replaces the ones it declares.
        for (method, parents) in &metadata.overridden_method_ids {
            let is_own =
                metadata.declaring_method_ids.get(method).is_some_and(|found| found.get_class_name() == *child);
            if is_own && parents.contains_key(&key) {
                let found = uses.entry(*method).or_default();
                found.subclass = true;
                found.subclass_outside |= outside;
            }
        }
        for (property, parents) in &metadata.overridden_property_ids {
            if metadata.declaring_property_ids.get(property) == Some(child) && parents.contains(&key) {
                let found = uses.entry(*property).or_default();
                if outside {
                    found.outside = true;
                } else {
                    found.namespace = true;
                }
            }
        }
    }

    uses
}

/// Whether one of `attributes` is of a class marked `[DynamicallyAccessed]`, as a router's `[Route]`, whose framework
/// reaches the member it marks, or each public member of the class it marks, by reflection, which no use records
/// (spec section 5).
fn dynamically_accessed(codebase: &CodebaseMetadata, attributes: &[AttributeMetadata]) -> bool {
    let mark = codebase.class_key(DYNAMICALLY_ACCESSED.as_bytes());

    attributes.iter().any(|attribute| {
        codebase.get_class_like(attribute.name.as_bytes()).is_some_and(|class| {
            class.attributes.iter().any(|marked| codebase.class_key(marked.name.as_bytes()) == mark)
        })
    })
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

/// The code outside `class` that sets each of its properties, by the property's name with `$`: the name of each class,
/// function or file whose code sets it, as the project's `references` record each write.
fn writers(
    class: &ClassLikeMetadata,
    codebase: &CodebaseMetadata,
    references: &SymbolReferences,
) -> HashMap<Word, BTreeSet<String>> {
    let key = codebase.class_key(class.name.as_bytes());
    let mut writers: HashMap<Word, BTreeSet<String>> = HashMap::new();
    references.for_each_reference(|origin, (target, property), kind| {
        if kind != SymbolReferenceKind::PropertyWrite || target != key {
            return;
        }
        let writer = match origin {
            ReferenceOrigin::Symbol((writer, _)) if writer == key => return,
            ReferenceOrigin::Symbol((writer, _)) => codebase.get_class_like(writer.as_bytes()).map_or_else(
                || writer.as_str_lossy().into_owned(),
                |writer| writer.original_name.as_str_lossy().into_owned(),
            ),
            ReferenceOrigin::File(file) => file.as_str_lossy().into_owned(),
        };
        writers.entry(property).or_default().insert(writer);
    });

    writers
}

/// Each namespace `class`'s code imports that imports `class`'s namespace back, by its name, with a file of it and the
/// class of `class`'s namespace that file imports, as the project's `references` record each name a symbol's code
/// reaches. PHP# lets no two namespaces import each other, a parent and its child included.
fn cycles(
    class: &ClassLikeMetadata,
    codebase: &CodebaseMetadata,
    references: &SymbolReferences,
) -> BTreeMap<String, (String, String)> {
    let declared = class.original_name.as_str_lossy();
    let own = namespace_of(&declared);
    if own.is_empty() {
        return BTreeMap::new();
    }
    let key = codebase.class_key(class.name.as_bytes());
    let name_of = |symbol: Word| {
        codebase
            .get_class_like(symbol.as_bytes())
            .map_or_else(|| symbol.as_str_lossy().into_owned(), |class| class.original_name.as_str_lossy().into_owned())
    };
    let neighbours: HashSet<Word> = codebase
        .class_likes
        .values()
        .filter(|other| namespace_of(&other.original_name.as_str_lossy()).eq_ignore_ascii_case(own))
        .map(|other| codebase.class_key(other.name.as_bytes()))
        .collect();
    // Each namespace this class imports, and each other namespace that imports one of its neighbours, by lowercase
    // name, as PHP matches one.
    let mut imported: HashMap<String, String> = HashMap::new();
    let mut importers: HashMap<String, (String, String)> = HashMap::new();
    references.for_each_reference(|origin, (target, _), _| {
        let ReferenceOrigin::Symbol((source, _)) = origin else {
            return;
        };
        if source == key {
            let name = name_of(target);
            let namespace = namespace_of(&name);
            if !namespace.is_empty() && !namespace.eq_ignore_ascii_case(own) {
                imported.insert(namespace.to_ascii_lowercase(), namespace.to_owned());
            }
        } else if neighbours.contains(&target) && !neighbours.contains(&source) {
            let name = name_of(source);
            let namespace = namespace_of(&name);
            if namespace.is_empty() || namespace.eq_ignore_ascii_case(own) {
                return;
            }
            let file = codebase
                .get_class_like(source.as_bytes())
                .and_then(|importer| codebase.file_name(importer.span.file_id))
                .map_or_else(
                    || name.clone(),
                    |file| file.as_str_lossy().rsplit('/').next().unwrap_or_default().to_owned(),
                );
            let imports = name_of(target).rsplit('\\').next().unwrap_or_default().to_owned();
            let found =
                importers.entry(namespace.to_ascii_lowercase()).or_insert_with(|| (file.clone(), imports.clone()));
            if (&file, &imports) < (&found.0, &found.1) {
                *found = (file, imports);
            }
        }
    });

    imported
        .into_iter()
        .filter_map(|(lowercase, namespace)| importers.remove(&lowercase).map(|importer| (namespace, importer)))
        .collect()
}

/// The namespace of `name`, a full PHP name, as `App\Shop` of `App\Shop\Checkout`, or nothing in PHP's root namespace.
fn namespace_of(name: &str) -> &str {
    name.rsplit_once('\\').map_or("", |(namespace, _)| namespace)
}

/// Whether the namespace `inner` is `namespace` or one below it, as PHP matches names, so every namespace is within
/// the root one.
fn is_within(inner: &str, namespace: &str) -> bool {
    namespace.is_empty()
        || inner.eq_ignore_ascii_case(namespace)
        || inner.len() > namespace.len()
            && inner.is_char_boundary(namespace.len())
            && inner[..namespace.len()].eq_ignore_ascii_case(namespace)
            && inner.as_bytes()[namespace.len()] == b'\\'
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
            format!("the {kind} `{shown}` has no PHP# name: PHP# names a {kind} in {written} (spec section 24), and `{fixed}` would change the name PHP code reaches it by"),
        );

        true
    }

    /// Who sets the converted class's property `variable` names, or `None` and a refusal at it where code of another
    /// namespace sets it. A PHP# property's `set` reaches its own namespace at most, and plain PHP that sets a PHP#
    /// property is held to it too, so such a write would stop compiling.
    fn setters(&mut self, variable: &DirectVariable<'_>) -> Option<Setters> {
        let Some(class) = self.class.as_ref() else {
            return Some(Setters::Namespace);
        };
        let Some(writers) = class.writers.get(&word(variable.name)) else {
            return Some(Setters::Class);
        };
        let declared = class.metadata.original_name.as_str_lossy();
        let own = namespace_of(&declared);
        let Some(outside) = writers.iter().find(|writer| !namespace_of(writer).eq_ignore_ascii_case(own)) else {
            return Some(Setters::Namespace);
        };
        let reason = format!(
            "`{}::{}` is assigned from `{outside}`, and PHP# sets a property only inside its own namespace",
            class.name,
            String::from_utf8_lossy(variable.name),
        );
        self.refuse(variable.span, reason);

        None
    }

    /// Refuses the class-like `class` at `span` for each namespace it imports that imports its own namespace back,
    /// which the PHP# checker refuses in the converted file.
    fn import_cycles(&mut self, class: &ClassLikeMetadata, span: Span) {
        let cycles = cycles(class, self.input.codebase, self.input.references);
        let own = class.original_name.as_str_lossy();
        for (namespace, (file, imports)) in cycles {
            self.refuse(
                span,
                format!("`{}` and `{namespace}` import each other ({file} imports {imports})", namespace_of(&own)),
            );
        }
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

    /// The access level, as `internal `, of a member PHP makes public, `member` by its name in the project's
    /// references, with its `attributes`. In a project, whose public members the checker refuses as too wide, it is
    /// the narrowest the member's uses need, and `internal` for one nothing the analysis reads uses, as one only a view
    /// template uses is (spec section 5). A member stays `public` in a library, whose uses are in the projects that
    /// install it, in an interface, and where it `replaces` another, whose access it keeps, as do a constructor and a
    /// magic method, which PHP calls itself, and a member a framework reaches by reflection, which a marked attribute on
    /// it or its class says.
    fn public_access(&self, member: Word, attributes: &[AttributeMetadata], replaces: bool) -> &'static str {
        let Some(class) = self.class.as_ref() else {
            return "public ";
        };
        let codebase = self.input.codebase;
        if replaces
            || member.as_bytes().starts_with(b"__")
            || class.metadata.kind.is_interface()
            || !self.in_project()
            || dynamically_accessed(codebase, &class.metadata.attributes)
            || dynamically_accessed(codebase, attributes)
        {
            return "public ";
        }

        class.uses.get(&member).map_or("internal ", |uses| uses.access())
    }

    /// The access level of the converted class's property `variable` names, declared in the class body or by a
    /// constructor's parameter, which PHP makes public.
    fn property_access(&self, variable: &DirectVariable<'_>) -> &'static str {
        let name = word(variable.name);
        let Some(metadata) = self.class.as_ref().map(|class| class.metadata) else {
            return "public ";
        };
        let replaces = metadata.overridden_property_ids.get(&name).is_some_and(|parents| !parents.is_empty());
        let attributes = metadata.properties.get(&name).map_or(&[][..], |property| property.attributes.as_slice());

        self.public_access(name, attributes, replaces)
    }

    /// Writes `access` where `modifiers` declare a member that PHP makes public: in place of its `public`, or at the
    /// start of its declaration, `keyword` where it has no modifier.
    fn write_access(&mut self, modifiers: &Sequence<'_, Modifier<'_>>, keyword: Span, access: &str) {
        match has(modifiers, |modifier| matches!(modifier, Modifier::Public(_))) {
            Some(_) if access == "public " => {}
            Some(public) => self.edits.replace(public, access.trim_end()),
            None => self.edits.insert(modifiers.first().map_or(keyword, HasSpan::span).start.offset, access),
        }
    }
}

/// Whether a docblock line starts a tag or closes the docblock, which ends the text of the tag before it.
fn is_tag_or_end(line: &str) -> bool {
    let content = line.trim().trim_start_matches('*').trim();

    content.starts_with('@') || line.trim().starts_with("*/") || line.trim().is_empty()
}
