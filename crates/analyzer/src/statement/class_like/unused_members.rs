//! Checks for unused private/protected class members (properties and methods).
//!
//! This module detects properties and methods that are declared but never used:
//! - Private members in any class
//! - Protected members in final classes (since they can't be accessed by subclasses)
//!
//! It also performs transitive analysis: if a member is only referenced by other
//! unused members, it is also considered unused.
//!
//! Members with names starting with underscore (`_`) are considered intentionally unused
//! and are not reported. This also covers magic methods (`__construct`, `__call`, etc.)
//! since they all start with double underscore.
//!
//! [`too_wide_members`] refuses, once the whole project is analyzed, a PHP# `public`
//! member whose every use a narrower access level reaches, and a `public` type that nothing
//! outside its namespace uses, which spec section 5 calls too wide.

use foldhash::HashMap;
use foldhash::HashSet;
use mago_allocator::Arena;

use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::attribute::AttributeMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::reference::ReferenceOrigin;
use mago_codex::reference::SymbolReferenceKind;
use mago_codex::reference::SymbolReferences;
use mago_codex::symbol::SymbolIdentifier;
use mago_database::file::FileId;
use mago_names::DYNAMICALLY_ACCESSED;
use mago_names::EXPORT;
use mago_names::PROGRAM_ENTRY;
use mago_names::display_sharp_member;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_span::HasSpan;
use mago_span::Span;
use mago_text_edit::Safety;
use mago_text_edit::TextEdit;
use mago_word::Word;
use mago_word::WordMap;
use mago_word::empty_word;
use mago_word::word;

use crate::code::IssueCode;
use crate::context::Context;
use crate::statement::class_like::r#struct::data_cases;
use crate::utils::names::display_property_name;
use crate::utils::names::sharp_class_like_name;
use crate::visibility::is_within_namespace;
use crate::visibility::namespace_of;

/// Represents a checkable member (property or method) with its metadata.
#[derive(Clone)]
struct CheckableMember {
    symbol_id: SymbolIdentifier,
    display_name: Word,
    span: Span,
    is_property: bool,
}

#[derive(Debug)]
pub(crate) struct UnusedMemberSpans {
    pub unused_methods: HashSet<Span>,
    pub unused_properties: HashSet<Span>,
    pub read_properties: HashSet<Span>,
}

/// Check for unused members (properties and methods) with transitive analysis.
///
/// This function detects:
/// 1. Members with no references (directly unused)
/// 2. Members whose only references come from other unused members (transitively unused)
///
/// Reports private members (and protected members in final classes) that are unused.
/// Returns the set of unused member symbol IDs for use by other checks.
pub fn check_unused_members_with_transitivity<A>(
    class_name: Word,
    class_span: Span,
    class_like_metadata: &ClassLikeMetadata,
    symbol_references: &SymbolReferences,
    additional_symbol_references: Option<&SymbolReferences>,
    context: &mut Context<'_, '_, A>,
) -> HashSet<SymbolIdentifier>
where
    A: Arena,
{
    let checkable_members = collect_checkable_members(class_name, class_like_metadata, context.codebase);
    let unused_members = find_unused_members(&checkable_members, symbol_references, additional_symbol_references);

    for member in &checkable_members {
        if unused_members.contains(&member.symbol_id) {
            if member.is_property {
                report_unused_property(context, class_span, member.display_name, member.span);
            } else {
                report_unused_method(context, class_span, member.display_name, member.span);
            }
        }
    }

    unused_members
}

fn collect_checkable_members(
    class_name: Word,
    class_like_metadata: &ClassLikeMetadata,
    codebase: &CodebaseMetadata,
) -> Vec<CheckableMember> {
    if class_like_metadata.kind.is_trait() || class_like_metadata.kind.is_interface() {
        return Vec::new();
    }

    let is_final = class_like_metadata.flags.is_final() || class_like_metadata.kind.is_enum();
    let mut checkable_members = Vec::new();

    for (property_name, property) in &class_like_metadata.properties {
        if let Some(declaring_class) = class_like_metadata.declaring_property_ids.get(property_name)
            && *declaring_class != class_name
        {
            continue;
        }

        if property.read_visibility.is_public() {
            continue;
        }

        if property.read_visibility.is_protected() && !is_final {
            continue;
        }

        if property_name.as_bytes().starts_with(b"$_") {
            continue;
        }

        if !property.hooks.is_empty() {
            continue;
        }

        // A magic `@property*` annotation of the same name exposes this property through
        // `__get`/`__set`, so it can be reached from outside the class (and typically via
        // dynamic access inside the magic methods) where this analysis cannot see it.
        if class_like_metadata.magic_property_ids.contains_key(property_name) {
            continue;
        }

        if !property.read_visibility.is_private()
            && class_like_metadata.overridden_property_ids.contains_key(property_name)
        {
            continue;
        }

        if class_like_metadata
            .used_traits
            .iter()
            .any(|trait_name| codebase.property_exists(*trait_name, *property_name))
        {
            // trait property override, could be used in the trait itself.
            continue;
        }

        if let Some(property_span) = property.name_span.or(property.span) {
            checkable_members.push(CheckableMember {
                symbol_id: (class_name, *property_name),
                display_name: *property_name,
                span: property_span,
                is_property: true,
            });
        }
    }

    for method_name in &class_like_metadata.methods {
        if let Some(declaring_method_id) = class_like_metadata.declaring_method_ids.get(method_name)
            && declaring_method_id.get_class_name() != class_name
        {
            continue;
        }

        let Some(method_metadata) = codebase.function_likes.get(&(class_name, *method_name)) else {
            continue;
        };

        let Some(method_meta) = &method_metadata.method_metadata else {
            continue;
        };

        if method_meta.visibility.is_public() {
            continue;
        }

        if method_meta.visibility.is_protected() && !is_final {
            continue;
        }

        if method_name.as_bytes().starts_with(b"_") {
            continue;
        }

        // The engine runs the PHP# static constructor, `#statics`, when the app first uses the class's statics.
        if method_name.as_bytes().starts_with(b"#") {
            continue;
        }

        // The engine runs a PHP# listener when an event it lists is raised, spec section 15.
        if method_meta.is_abstract || !method_metadata.listens.is_empty() {
            continue;
        }

        if class_like_metadata.overridden_method_ids.contains_key(method_name) {
            continue;
        }

        if class_like_metadata.used_traits.iter().any(|trait_name| codebase.method_exists(*trait_name, *method_name)) {
            // non-abstract trait method override, could be used in the trait itself.
            continue;
        }

        let method_span = method_metadata.name_span.unwrap_or(method_metadata.span);
        checkable_members.push(CheckableMember {
            symbol_id: (class_name, *method_name),
            display_name: method_metadata.original_name,
            span: method_span,
            is_property: false,
        });
    }

    checkable_members
}

fn find_unused_members(
    checkable_members: &[CheckableMember],
    symbol_references: &SymbolReferences,
    additional_symbol_references: Option<&SymbolReferences>,
) -> HashSet<SymbolIdentifier> {
    let mut unused_members: HashSet<SymbolIdentifier> = HashSet::default();
    for member in checkable_members {
        if !is_member_referenced(symbol_references, additional_symbol_references, &member.symbol_id) {
            unused_members.insert(member.symbol_id);
        }
    }

    let checkable_set: HashSet<SymbolIdentifier> = checkable_members.iter().map(|member| member.symbol_id).collect();
    loop {
        let mut newly_unused: HashSet<SymbolIdentifier> = HashSet::default();

        for member in checkable_members {
            if unused_members.contains(&member.symbol_id) {
                continue;
            }

            if all_references_from_unused(
                symbol_references,
                additional_symbol_references,
                &member.symbol_id,
                &unused_members,
                &checkable_set,
            ) {
                newly_unused.insert(member.symbol_id);
            }
        }

        if newly_unused.is_empty() {
            break;
        }

        unused_members.extend(newly_unused);
    }

    unused_members
}

pub(crate) fn find_unused_member_spans(
    codebase: &CodebaseMetadata,
    symbol_references: &SymbolReferences,
) -> UnusedMemberSpans {
    let mut referencing_symbols = HashMap::<SymbolIdentifier, HashSet<SymbolIdentifier>>::default();
    let mut externally_referenced = HashSet::<SymbolIdentifier>::default();
    let mut read_properties = HashSet::<SymbolIdentifier>::default();
    symbol_references.for_each_reference(|source, target, kind| {
        if matches!(kind, SymbolReferenceKind::Body | SymbolReferenceKind::Signature) {
            match source {
                ReferenceOrigin::Symbol(source) => {
                    referencing_symbols.entry(target).or_default().insert(source);
                }
                ReferenceOrigin::File(_) => {
                    externally_referenced.insert(target);
                }
            }
        } else if kind == SymbolReferenceKind::PropertyRead {
            read_properties.insert(target);
        }
    });

    let mut spans = UnusedMemberSpans {
        unused_methods: HashSet::default(),
        unused_properties: HashSet::default(),
        read_properties: HashSet::default(),
    };
    for (class_name, class_like_metadata) in &codebase.class_likes {
        let checkable_members = collect_checkable_members(*class_name, class_like_metadata, codebase);
        let unused_members =
            find_unused_members_from_index(&checkable_members, &referencing_symbols, &externally_referenced);
        for member in checkable_members {
            if unused_members.contains(&member.symbol_id) {
                if member.is_property {
                    spans.unused_properties.insert(member.span);
                } else {
                    spans.unused_methods.insert(member.span);
                }
            }
        }

        for (property_name, property) in &class_like_metadata.properties {
            let symbol_id = (*class_name, *property_name);
            if read_properties.contains(&symbol_id)
                && let Some(property_span) = property.name_span.or(property.span)
            {
                spans.read_properties.insert(property_span);
            }
        }
    }

    spans
}

fn find_unused_members_from_index(
    checkable_members: &[CheckableMember],
    referencing_symbols: &HashMap<SymbolIdentifier, HashSet<SymbolIdentifier>>,
    externally_referenced: &HashSet<SymbolIdentifier>,
) -> HashSet<SymbolIdentifier> {
    let checkable_set = checkable_members.iter().map(|member| member.symbol_id).collect::<HashSet<_>>();
    let mut unused_members = checkable_members
        .iter()
        .filter(|member| {
            !externally_referenced.contains(&member.symbol_id)
                && referencing_symbols.get(&member.symbol_id).is_none_or(HashSet::is_empty)
        })
        .map(|member| member.symbol_id)
        .collect::<HashSet<_>>();

    loop {
        let mut newly_unused = HashSet::default();
        for member in checkable_members {
            if unused_members.contains(&member.symbol_id) || externally_referenced.contains(&member.symbol_id) {
                continue;
            }

            let all_references_are_unused = referencing_symbols.get(&member.symbol_id).is_none_or(|references| {
                references.iter().all(|source| checkable_set.contains(source) && unused_members.contains(source))
            });

            if all_references_are_unused {
                newly_unused.insert(member.symbol_id);
            }
        }

        if newly_unused.is_empty() {
            break;
        }

        unused_members.extend(newly_unused);
    }

    unused_members
}

/// Checks if all references to a member come from unused members.
///
/// Returns true if:
/// - The member has no references, OR
/// - All referencing symbols are in the unused set
fn all_references_from_unused(
    symbol_references: &SymbolReferences,
    additional_symbol_references: Option<&SymbolReferences>,
    symbol_id: &SymbolIdentifier,
    unused_members: &HashSet<SymbolIdentifier>,
    checkable_set: &HashSet<SymbolIdentifier>,
) -> bool {
    all_references_from_unused_in(symbol_references, symbol_id, unused_members, checkable_set)
        && additional_symbol_references.is_none_or(|references| {
            all_references_from_unused_in(references, symbol_id, unused_members, checkable_set)
        })
}

fn all_references_from_unused_in(
    symbol_references: &SymbolReferences,
    symbol_id: &SymbolIdentifier,
    unused_members: &HashSet<SymbolIdentifier>,
    checkable_set: &HashSet<SymbolIdentifier>,
) -> bool {
    if symbol_references.has_file_reference_to_symbol(*symbol_id) {
        return false;
    }

    let referencing_symbols = symbol_references.get_references_to_symbol(*symbol_id);

    if referencing_symbols.is_empty() {
        return true;
    }

    for referencing in referencing_symbols {
        if checkable_set.contains(referencing) {
            if !unused_members.contains(referencing) {
                return false;
            }
        } else {
            return false;
        }
    }

    true
}

/// Check for write-only properties in a class-like.
///
/// Reports private properties (and protected properties in final classes)
/// that are written to but never read.
///
/// The `unused_members` set contains property symbols that were already reported
/// as unused (via transitive analysis) and should be skipped.
pub fn check_write_only_properties<A>(
    class_name: Word,
    class_span: Span,
    class_like_metadata: &ClassLikeMetadata,
    symbol_references: &SymbolReferences,
    additional_symbol_references: Option<&SymbolReferences>,
    unused_members: &HashSet<SymbolIdentifier>,
    context: &mut Context<'_, '_, A>,
) where
    A: Arena,
{
    if class_like_metadata.kind.is_trait() {
        return;
    }

    if class_like_metadata.kind.is_interface() {
        return;
    }

    let is_final = class_like_metadata.flags.is_final() || class_like_metadata.kind.is_enum();

    for (property_name, property) in &class_like_metadata.properties {
        if let Some(declaring_class) = class_like_metadata.declaring_property_ids.get(property_name)
            && *declaring_class != class_name
        {
            continue;
        }

        if property.read_visibility.is_public() {
            continue;
        }

        if property.read_visibility.is_protected() && !is_final {
            continue;
        }

        if property_name.as_bytes().starts_with(b"$_") {
            continue;
        }

        if !property.hooks.is_empty() {
            continue;
        }

        // A magic `@property*` annotation of the same name exposes this property through
        // `__get`, so reads can occur outside the class (and typically via dynamic access
        // inside `__get`) where this analysis cannot see them.
        if class_like_metadata.magic_property_ids.contains_key(property_name) {
            continue;
        }

        let symbol_id: SymbolIdentifier = (class_name, *property_name);
        if unused_members.contains(&symbol_id) {
            continue;
        }

        if !is_member_referenced(symbol_references, additional_symbol_references, &symbol_id) {
            continue;
        }

        let has_reads = symbol_references.count_property_reads(&symbol_id) > 0
            || additional_symbol_references.is_some_and(|references| references.count_property_reads(&symbol_id) > 0);
        let has_writes = symbol_references.count_property_writes(&symbol_id) > 0
            || additional_symbol_references.is_some_and(|references| references.count_property_writes(&symbol_id) > 0);

        if has_writes
            && !has_reads
            && let Some(property_span) = property.name_span.or(property.span)
        {
            report_write_only_property(context, class_span, *property_name, property_span);
        }
    }
}

/// Checks if a member (property or method) is referenced anywhere in the codebase.
#[inline(always)]
fn is_member_referenced(
    symbol_references: &SymbolReferences,
    additional_symbol_references: Option<&SymbolReferences>,
    symbol_id: &SymbolIdentifier,
) -> bool {
    is_member_referenced_in(symbol_references, symbol_id)
        || additional_symbol_references.is_some_and(|references| is_member_referenced_in(references, symbol_id))
}

#[inline(always)]
fn is_member_referenced_in(symbol_references: &SymbolReferences, symbol_id: &SymbolIdentifier) -> bool {
    symbol_references.count_referencing_symbols(symbol_id, false) > 0
        || symbol_references.count_referencing_symbols(symbol_id, true) > 0
}

/// Reports an unused property.
fn report_unused_property<A>(
    context: &mut Context<'_, '_, A>,
    class_span: Span,
    property_name: Word,
    property_span: Span,
) where
    A: Arena,
{
    let (prefix, name_offset) = if context.dialect.is_sharp() { ("_", 0) } else { ("$_", 1) };
    let property_name = display_property_name(context, property_name);
    let issue = Issue::help(format!("Property `{property_name}` is never used."))
        .with_code(IssueCode::UnusedProperty)
        .with_annotations([
            Annotation::primary(property_span).with_message(format!("Property `{property_name}` is declared here.")),
            Annotation::secondary(class_span),
        ])
        .with_note("This property is declared but never read or written within the class.")
        .with_help(format!(
            "Consider prefixing the property with an underscore (`{prefix}`) to indicate that it is intentionally unused, or remove it if it is not needed.",
        ));

    context.collector.propose(issue, |edits| {
        edits.push(
            TextEdit::insert(property_span.start_offset() + name_offset, "_").with_safety(Safety::PotentiallyUnsafe),
        );
    });
}

/// Reports a write-only property.
fn report_write_only_property<A>(
    context: &mut Context<'_, '_, A>,
    class_span: Span,
    property_name: Word,
    property_span: Span,
) where
    A: Arena,
{
    let property_name = display_property_name(context, property_name);
    let issue = Issue::help(format!("Property `{property_name}` is written to but never read."))
        .with_code(IssueCode::WriteOnlyProperty)
        .with_annotations([
            Annotation::primary(property_span).with_message(format!("Property `{property_name}` is declared here.")),
            Annotation::secondary(class_span),
        ])
        .with_note("This property is assigned values but its value is never read within the class.")
        .with_help(
            "Consider removing the property if its value is not needed, or read the value somewhere if it should be used.",
        );

    context.collector.report(issue);
}

/// Reports an unused method.
fn report_unused_method<A>(context: &mut Context<'_, '_, A>, class_span: Span, method_name: Word, method_span: Span)
where
    A: Arena,
{
    let issue = Issue::help(format!("Method `{method_name}()` is never used."))
        .with_code(IssueCode::UnusedMethod)
        .with_annotations([
            Annotation::primary(method_span).with_message(format!("Method `{method_name}()` is declared here.")),
            Annotation::secondary(class_span),
        ])
        .with_note("This method is declared but never called within the class.")
        .with_help(
            "Consider prefixing the method with an underscore (`_`) to indicate that it is intentionally unused, or remove it if it is not needed.",
        );

    context.collector.propose(issue, |edits| {
        edits.push(TextEdit::insert(method_span.start_offset(), "_").with_safety(Safety::PotentiallyUnsafe));
    });
}

/// The access levels of spec section 5 narrower than `public`, narrowest first, which [`too_wide_members`] tries in
/// that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    /// The class's own code.
    Private,
    /// The class's and its subclasses' code, in any namespace.
    Protected,
    /// The code of the class's namespace and the namespaces below it.
    Internal,
}

/// Refuses each `public` member whose uses a narrower access level reaches, and names the narrowest one, and each
/// `public` type that nothing outside its namespace uses.
///
/// Spec section 5 calls such a member or type too wide, a compile error in an application. A type is as wide as its
/// uses, as a member is, and each kind alike: a class, an interface, a struct, an enum and an event. A use of a type is
/// a use of it or of a member of it, an `extends` or an `implements` included, and plain PHP reaching an `[Export]`
/// method, the engine running a program's `main` or a framework reading a marked attribute of a member is one from
/// outside the namespace. A module and an enum's case are no types anyone names. The pass reads `references` of every file, so it runs once the whole project
/// is analyzed, beside [`Effects::solve`](crate::effects::Effects::solve). The levels are tried as [`Access`] orders them, so a member nothing uses is `private`. A use from code outside any
/// class or function needs `public`, because its namespace is not known here. A member must be visible to each class
/// that overrides or implements it, so an override is a use from the class that declares it. A `virtual` or
/// `abstract` member is never `private`, since a private member is never replaced.
///
/// Only a public PHP# class of a project has such members, a file whose nearest `composer.json` says `"type":
/// "project"`, of the files analyzed: a library's uses are in the projects that install it, which the analysis never
/// sees. An `internal` class's members reach no further than it does (a member is never more visible than its class),
/// interface members are exempt because they have no narrower form, as an event's parameters are, its payload, which take
/// no modifier, spec section 15, and plain PHP has no `internal`. A member that
/// overrides or implements another keeps that one's access, PHP calls a magic method itself, `new` calls the
/// constructor, which takes its class's reach, and the code that reads a member's or its class's marked attribute
/// reaches the member through reflection, which no use records, so none of them is refused.
#[must_use]
pub fn too_wide_members(
    codebase: &CodebaseMetadata,
    references: &SymbolReferences,
    test_files: &[FileId],
) -> IssueCollection {
    let uses_on = uses_on(codebase, references, test_files);
    let namespace_types = namespace_types(codebase, &uses_on);
    // The same uses by the member and each class-like the class the code names it on is an instance of, as
    // `CodebaseMetadata::is_instance_of` decides.
    let mut member_uses = HashMap::<(Word, Word), Vec<&[Option<Word>]>>::default();
    for ((member, on), code_in) in &uses_on {
        let on = codebase.class_key(*on);
        let metadata = codebase.get_class_like(on);
        let instance_of = metadata.into_iter().flat_map(|metadata| {
            std::iter::once(metadata.name).chain(metadata.instance_of().into_iter().flatten().copied())
        });
        for class in std::iter::once(on).chain(instance_of) {
            member_uses.entry((*member, class)).or_default().push(code_in);
        }
    }

    let mut issues = IssueCollection::default();
    for (class_name, class) in &codebase.class_likes {
        if !class.flags.is_user_defined() || !class.flags.is_project() || class.flags.is_internal_reach() {
            continue;
        }

        let namespace = namespace_of(class.original_name.as_bytes());
        let dotted_namespace = String::from_utf8_lossy(namespace).replace('\\', ".");
        let class_key = codebase.class_key(*class_name);
        let class_label = sharp_class_like_name(codebase, &WordMap::default(), *class_name);
        // Whether `access` reaches the code a use is in, spec section 5's table.
        let reaches = |access: Access, code_in: Option<Word>| {
            code_in.is_some_and(|symbol| match access {
                Access::Private => symbol == class_key,
                Access::Protected => codebase.is_instance_of(symbol, class_key),
                Access::Internal => is_within_namespace(namespace_of(symbol.as_bytes()), namespace),
            })
        };

        if namespace_types.contains(&class_key) {
            issues.push(
                Issue::error(format!("`{class_label}` is public, and nothing outside `{dotted_namespace}` uses it."))
                    .with_code(IssueCode::TooWideMember)
                    .with_annotation(
                        Annotation::primary(class.name_span.unwrap_or(class.span))
                            .with_message("Declared `public` here."),
                    )
                    .with_note("A type is as wide as its uses, so one that only its namespace uses is `internal`.")
                    .with_help("Declare it `internal`, or, when a framework reaches it by name or by reflection, put an attribute whose class is marked `[DynamicallyAccessed]` on it."),
            );
        }

        if class.kind.is_interface() {
            continue;
        }

        for (member, label, span, is_open) in public_members(codebase, *class_name, class) {
            let uses: Vec<Option<Word>> = member_uses
                .get(&(member, class_key))
                .into_iter()
                .flatten()
                .flat_map(|uses| uses.iter().copied())
                .collect();
            let Some(access) = [Access::Private, Access::Protected, Access::Internal]
                .into_iter()
                .filter(|access| *access != Access::Private || !is_open)
                .find(|access| uses.iter().all(|code_in| reaches(*access, *code_in)))
            else {
                continue;
            };

            let (keyword, reach, note) = match access {
                Access::Private => (
                    "private",
                    format!("`{class_label}`"),
                    "A member is as wide as its uses, so one that only its class uses is `private`.",
                ),
                Access::Protected => (
                    "protected",
                    format!("`{class_label}` and its subclasses"),
                    "A member is as wide as its uses, so one that only its class and its subclasses use is `protected`, and a `virtual` or `abstract` one is never `private`.",
                ),
                Access::Internal => (
                    "internal",
                    format!("`{dotted_namespace}`"),
                    "A member is as wide as its uses, so one that only its namespace uses is `internal`.",
                ),
            };
            issues.push(
                Issue::error(format!(
                    "`{}` is public, and nothing outside {reach} uses it.",
                    display_sharp_member(&class_label, label)
                ))
                .with_code(IssueCode::TooWideMember)
                .with_annotation(Annotation::primary(span).with_message("Declared `public` here."))
                .with_note(note)
                .with_help(format!("Declare it `{keyword}`, or, when a framework reaches it by name or by reflection, put an attribute whose class is marked `[DynamicallyAccessed]` on its class.")),
            );
        }
    }

    issues
}

/// Each type nothing outside its namespace uses, by its class key, which spec section 5 makes `internal`.
///
/// They are the types [`too_wide_members`] refuses as public, and the types `mago convert` writes `internal` in a
/// project, so the two never disagree. Each user-defined class-like counts, a plain PHP one included, as it is before
/// its conversion. A use of a type is a use of it or of a member of it, an `extends`, an `implements` and an override of one of its
/// members included, from the code of its namespace or one below it. Code outside any class or function is outside it,
/// since its namespace is not known here. A module and an enum's case are no types anyone names, and plain PHP calling
/// an `[Export]` method or a framework reading a marked attribute of the type or of one of its members reaches it from
/// outside any namespace, so none of them is in the set.
#[must_use]
pub fn types_only_their_namespace_uses(
    codebase: &CodebaseMetadata,
    references: &SymbolReferences,
    test_files: &[FileId],
) -> HashSet<Word> {
    namespace_types(codebase, &uses_on(codebase, references, test_files))
}

/// The code each use of a member is in, the class or function, `None` for code outside both, by the member and the
/// class the code names it on, an empty member for a use of the class itself. A member must be visible to each class
/// that overrides or implements it, so each override uses the member it replaces from the class that declares the
/// override. Each reach the checker can't see is a use from outside, as [`unseen_reaches`] records it.
fn uses_on(
    codebase: &CodebaseMetadata,
    references: &SymbolReferences,
    test_files: &[FileId],
) -> HashMap<(Word, Word), Vec<Option<Word>>> {
    let mut uses_on = HashMap::<(Word, Word), Vec<Option<Word>>>::default();
    references.for_each_reference(|origin, (class, member), _| {
        let code_in = match origin {
            ReferenceOrigin::Symbol((symbol, _)) => Some(symbol),
            ReferenceOrigin::File(_) => None,
        };
        uses_on.entry((member, class)).or_default().push(code_in);
    });
    for (class_name, class) in &codebase.class_likes {
        for (method, replaced) in &class.overridden_method_ids {
            if class.methods.contains(method) {
                for replaced in replaced.values() {
                    let (on, member) = replaced.get_key();
                    uses_on.entry((member, on)).or_default().push(Some(*class_name));
                }
            }
        }
        for (property, replaced) in &class.overridden_property_ids {
            if class.declaring_property_ids.get(property) == Some(class_name) {
                for on in replaced {
                    uses_on.entry((*property, *on)).or_default().push(Some(*class_name));
                }
            }
        }
    }
    for (member, on) in unseen_reaches(codebase, test_files) {
        uses_on.entry((member, on)).or_default().push(None);
    }

    uses_on
}

/// Each reach of spec section 5 the checker can't see, as the member and the class it is on, an empty member for the
/// class itself, each a use from outside any namespace:
///
/// - a class that carries a marked attribute, whose framework reaches the class and every member it has, the ones it
///   inherits included, each on the class that declares it, so that class is reached as a type too
/// - a member that carries a marked attribute, an `[Export]` method, which plain PHP calls through the global function
///   it declares, section 23, and a program's `main`, which the engine calls, section 27: the member and its class
/// - a class in one of `test_files`, the files of an `autoload-dev` folder, which its test runner creates, as PHPUnit
///   creates a test class
fn unseen_reaches(codebase: &CodebaseMetadata, test_files: &[FileId]) -> Vec<(Word, Word)> {
    let test_files: HashSet<FileId> = test_files.iter().copied().collect();
    let mut reaches = Vec::new();
    for (class_name, class) in &codebase.class_likes {
        if is_dynamically_accessed(codebase, &class.attributes) {
            reaches.push((empty_word(), *class_name));
            reaches.extend(
                class.declaring_method_ids.iter().map(|(method, declared)| (*method, declared.get_class_name())),
            );
            reaches.extend(class.declaring_property_ids.iter().map(|(property, declaring)| (*property, *declaring)));
            reaches.extend(class.constants.keys().map(|constant| (*constant, *class_name)));
            continue;
        }
        if test_files.contains(&class.span.file_id) {
            reaches.push((empty_word(), *class_name));
        }
        let mut members: Vec<Word> = class
            .properties
            .iter()
            .filter_map(|(name, property)| is_dynamically_accessed(codebase, &property.attributes).then_some(*name))
            .collect();
        members.extend(
            class.constants.iter().filter_map(|(name, constant)| {
                is_dynamically_accessed(codebase, &constant.attributes).then_some(*name)
            }),
        );
        members.extend(class.methods.iter().copied().filter(|name| {
            codebase.function_likes.get(&(*class_name, *name)).is_some_and(|method| {
                method.has_attribute(codebase, word(EXPORT))
                    || is_program_entry(class, method)
                    || is_dynamically_accessed(codebase, &method.attributes)
            })
        }));
        if !members.is_empty() {
            reaches.push((empty_word(), *class_name));
            reaches.extend(members.into_iter().map(|member| (member, *class_name)));
        }
    }

    reaches
}

/// [`types_only_their_namespace_uses`], of the uses [`uses_on`] gives.
fn namespace_types(codebase: &CodebaseMetadata, uses_on: &HashMap<(Word, Word), Vec<Option<Word>>>) -> HashSet<Word> {
    let mut type_uses = HashMap::<Word, Vec<Option<Word>>>::default();
    for ((_, on), code_in) in uses_on {
        type_uses.entry(codebase.class_key(*on)).or_default().extend(code_in.iter().copied());
    }

    codebase
        .class_likes
        .iter()
        .filter_map(|(class_name, class)| {
            if !class.flags.is_user_defined() || class.flags.is_module() || class.flags.is_enum_case_class() {
                return None;
            }
            let class_key = codebase.class_key(*class_name);
            let namespace = namespace_of(class.original_name.as_bytes());
            let is_used_outside = type_uses.get(&class_key).into_iter().flatten().any(|code_in| {
                code_in.is_none_or(|symbol| !is_within_namespace(namespace_of(symbol.as_bytes()), namespace))
            });

            (!is_used_outside).then_some(class_key)
        })
        .collect()
}

/// Refuses each class of an application that no code uses, spec section 5, so it never runs.
///
/// No code creates such a class, binds it in a module, names it as a type, extends or implements it, or names it in
/// `typeof` or an attribute, as a plugin nothing wires in. The pass reads `references` of every file, so it runs once
/// the whole project is analyzed, beside [`too_wide_members`].
///
/// A use is any reference to the class or a member of it from code outside the class and its own cases, a plain PHP
/// file's included. Only a PHP# class-like of a project has none to find: a library's users are in the projects that
/// install it, which the analysis never sees. A module runs no code, PHP# creates each class that declares an `on`
/// method and each that declares `main`, which runs as a program, plain PHP calls an `[Export]` method by the name of
/// its global function, and the code that reads a marked attribute of a class or of one of its members reaches the
/// class through reflection, which no use records, so none of them is refused. A class in one of `test_files`, the files of an
/// `autoload-dev` folder, runs under its test runner, as PHPUnit creates a test class, so it is never refused either,
/// nor is a class that extends or implements a class-like of a dev-only package, which `production-uses-dev-package`
/// already refuses with where tests go.
#[must_use]
pub fn unused_classes(
    codebase: &CodebaseMetadata,
    references: &SymbolReferences,
    test_files: &[FileId],
) -> IssueCollection {
    let mut used: HashSet<Word> = unseen_reaches(codebase, test_files).into_iter().map(|reach| reach.1).collect();
    references.for_each_reference(|origin, (class, _), _| {
        let from_inside = match origin {
            ReferenceOrigin::Symbol((symbol, _)) => {
                symbol == class
                    || codebase
                        .get_class_like(symbol)
                        .is_some_and(|case| case.flags.is_enum_case_class() && case.direct_parent_class == Some(class))
            }
            ReferenceOrigin::File(_) => false,
        };
        if !from_inside {
            used.insert(class);
        }
    });

    let mut issues = IssueCollection::default();
    for (class_name, class) in &codebase.class_likes {
        if !class.flags.is_user_defined()
            || !class.flags.is_project()
            || class.flags.is_module()
            || class.flags.is_enum_case_class()
            || class.direct_parent_class.iter().chain(&class.direct_parent_interfaces).any(|parent| {
                codebase
                    .get_class_like(*parent)
                    .is_some_and(|parent| codebase.dev_package(class.span.file_id, parent.span.file_id).is_some())
            })
            || used.contains(class_name)
            || class
                .methods
                .iter()
                .filter_map(|name| codebase.function_likes.get(&(*class_name, *name)))
                .any(|method| !method.listens.is_empty())
        {
            continue;
        }
        let class_label = sharp_class_like_name(codebase, &WordMap::default(), *class_name);
        issues.push(
            Issue::error(format!("Nothing uses `{class_label}`, so it never runs."))
                .with_code(IssueCode::UnusedClass)
                .with_annotation(Annotation::primary(class.name_span.unwrap_or(class.span)).with_message("Declared here."))
                .with_note("No code creates it, binds it in a module, names it as a type, extends or implements it, or names it in `typeof` or an attribute.")
                .with_help("Use it where the application runs, bind it in a module, or remove it. A framework that reaches it by reflection reads an attribute whose class is marked `[DynamicallyAccessed]`."),
        );
    }

    issues
}

/// Whether `method` of `class` is the entry of a program, spec section 27: a class's `main`, which the engine calls
/// when PHP runs the class's file as its script.
fn is_program_entry(class: &ClassLikeMetadata, method: &FunctionLikeMetadata) -> bool {
    class.kind.is_class() && !class.flags.is_struct() && method.original_name.as_bytes() == PROGRAM_ENTRY.as_bytes()
}

/// The `public` members `class` declares that [`too_wide_members`] checks, each as its key in `references`, its name
/// as PHP# writes it, the span the error points at, and whether it is open, `virtual` or `abstract`. The codex reads a
/// closed PHP# method or property as final, and a constructor's parameter declares no open property.
fn public_members(
    codebase: &CodebaseMetadata,
    class_name: Word,
    class: &ClassLikeMetadata,
) -> Vec<(Word, Word, Span, bool)> {
    let mut members = Vec::new();
    // The codex gives each case of a machine or of an enum whose case carries data a constant, or a factory method
    // when the case carries data, with the case's span. A case takes no access modifier, spec section 20, since its
    // reach is its type's.
    let case_spans: Vec<Span> = data_cases(codebase, class).iter().map(|case| case.span).collect();

    for (name, property) in &class.properties {
        if !class.flags.is_event()
            && class.declaring_property_ids.get(name) == Some(&class_name)
            && property.read_visibility.is_public()
            && !property.flags.is_internal_reach()
            && !class.overridden_property_ids.contains_key(name)
            && let Some(span) = property.name_span.or(property.span)
        {
            let is_open = !property.flags.is_final() && !property.flags.is_promoted_property();
            members.push((*name, word(mago_bytes::trim_start_byte(name.as_bytes(), b'$')), span, is_open));
        }
    }

    let methods =
        class.methods.iter().filter_map(|name| Some((name, codebase.function_likes.get(&(class_name, *name))?)));
    for (name, method) in methods {
        // A method the codex gives the class, such as section 10's `parse`, spans the whole class, and nobody wrote it.
        // PHP calls a magic method itself, and `new` calls the constructor, which takes the reach of its class.
        if method.span != class.span
            && !case_spans.contains(&method.span)
            && class.declaring_method_ids.get(name).is_none_or(|declaring| declaring.get_class_name() == class_name)
            && let Some(metadata) = method.method_metadata.as_ref()
            && metadata.visibility.is_public()
            && !method.flags.is_internal_reach()
            && !name.as_bytes().starts_with(b"__")
            && !class.overridden_method_ids.contains_key(name)
        {
            members.push((*name, method.original_name, method.name_span.unwrap_or(method.span), !metadata.is_final));
        }
    }

    // A class holds each constant it inherits as the parent declares it, so a constant is the class's own when the
    // class's body holds it, in the class's file: an offset alone falls inside a class of another file too.
    for (name, constant) in &class.constants {
        if constant.span.file_id == class.span.file_id
            && class.span.contains(&constant.span)
            && !case_spans.contains(&constant.span)
            && constant.visibility.is_public()
            && !constant.flags.is_internal_reach()
        {
            members.push((*name, *name, constant.span, false));
        }
    }

    members
}

/// Whether one of `attributes` is of a class marked [`DYNAMICALLY_ACCESSED`], as a router's `[Route]`, whose framework
/// reaches the member it marks, or each public member of the class it marks, by reflection, which the checker never
/// sees. Any other attribute, as `[Deprecated]`, says nothing about who uses the member.
fn is_dynamically_accessed(codebase: &CodebaseMetadata, attributes: &[AttributeMetadata]) -> bool {
    let mark = codebase.class_key(word(DYNAMICALLY_ACCESSED));

    attributes.iter().any(|attribute| {
        codebase
            .get_class_like(attribute.name)
            .is_some_and(|class| class.attributes.iter().any(|marked| codebase.class_key(marked.name) == mark))
    })
}
