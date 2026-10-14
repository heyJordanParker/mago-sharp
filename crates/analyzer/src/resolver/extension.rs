//! The members PHP# extensions add to a receiver's type, spec section 26.

use std::borrow::Cow;

use foldhash::HashMap;
use mago_allocator::Arena;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::flags::MetadataFlags;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::template::TemplateResult;
use mago_codex::ttype::template::inferred_type_replacer;
use mago_codex::ttype::union::TUnion;
use mago_names::short_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::LocalIdentifier;
use mago_word::Word;
use mago_word::ascii_lowercase_word;

use crate::code::IssueCode;
use crate::context::Context;
use crate::invocation::infer_unfixed_templates;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_missing_imports;
use crate::utils::names::display_type;
use crate::utils::names::imports_by_default;

/// What a member written on a receiver reaches among the extensions of its name.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Extension {
    /// The static method of the extension member the access runs.
    Member(MethodIdentifier),
    /// An extension member of the other kind, a method read as a property or a property called, already reported.
    Refused,
}

/// The extension member that `name`, written on a value of type `receiver`, reaches in this file: a method when
/// `flag` is [`MetadataFlags::EXTENSION_METHOD`], a computed property when it is [`MetadataFlags::EXTENSION_PROPERTY`],
/// or `None` when the type declares or inherits a real member of that name or no extension of that name fits the
/// receiver. The receiver is the value that is not `null`: `?.` skips `null`, and the access reports a `.` on a
/// receiver that may be `null` as it does for any member.
///
/// The receiver's type picks among the extensions the file imports or its namespace declares, and only where none of
/// those fits, among the standard library's on the types every file has them for. Of those, the one whose receiver
/// fits inside every other's runs. Two that fit with neither inside the other are an error naming where each comes
/// from, and one that fits but the file does not import is an error naming the import to add. Either still reaches its
/// member, so the access is checked as it would run.
pub(crate) fn resolve_extension<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    receiver: &TUnion,
    name: &LocalIdentifier<'_>,
    flag: MetadataFlags,
) -> Option<Extension>
where
    A: Arena,
{
    let receiver = &receiver.to_non_nullable();
    // A receiver that is always `null` has no value left, and `never` fits every receiver.
    if receiver.is_never() {
        return None;
    }

    let codebase = context.codebase;
    let extensions = codebase.extensions.get(&ascii_lowercase_word(name.value))?;
    // A real member of the type the code shows wins over an extension, spec section 26. The access is analyzed again
    // when a class of the type gains or loses one, because its type traces to a signature that names the class.
    if !codebase.get_classes_hidden_by_extension(receiver, name.value).is_empty() {
        return None;
    }

    let fitting: Vec<(MethodIdentifier, &'ctx FunctionLikeMetadata)> = extensions
        .iter()
        .filter_map(|(_, method)| Some((*method, codebase.get_method_by_id(method)?)))
        .filter(|(_, metadata)| fits(context, receiver, metadata))
        .collect();
    let (own, others): (Vec<_>, Vec<_>) =
        fitting.iter().partition(|(method, _)| is_imported_or_in_namespace(context, method.get_class_name()));
    let (default, elsewhere): (Vec<_>, Vec<_>) = others
        .into_iter()
        .partition(|(method, metadata)| is_imported_by_default(context, method.get_class_name(), metadata));
    let in_scope = if own.is_empty() { default } else { own };
    let (in_scope, other_kind): (Vec<_>, Vec<_>) =
        in_scope.into_iter().partition(|(_, metadata)| metadata.flags.contains(flag));
    let in_scope = most_specific(context, in_scope);
    let elsewhere = elsewhere.into_iter().find(|(_, metadata)| metadata.flags.contains(flag));
    let receiver = display_type(context, receiver);
    let (code, access) = if flag == MetadataFlags::EXTENSION_METHOD {
        (IssueCode::NonExistentMethod, "Called here.")
    } else {
        (IssueCode::NonExistentProperty, "Read here.")
    };

    match (in_scope.as_slice(), other_kind.first(), elsewhere) {
        ([(method, _)], ..) => Some(Extension::Member(*method)),
        ([(method, _), ..], ..) => {
            let written = String::from_utf8_lossy(name.value);
            let (message, help) = ambiguity(context, &in_scope, &written);
            context.collector.report_with_code(
                if flag == MetadataFlags::EXTENSION_METHOD {
                    IssueCode::AmbiguousObjectMethodAccess
                } else {
                    IssueCode::AmbiguousObjectPropertyAccess
                },
                Issue::error(format!("{message} each add `{written}` to `{receiver}`."))
                    .with_annotation(Annotation::primary(name.span()).with_message(access))
                    .with_help(help),
            );

            Some(Extension::Member(*method))
        }
        ([], Some((_, metadata)), _) => {
            let written = String::from_utf8_lossy(name.value);
            let message = if flag == MetadataFlags::EXTENSION_METHOD {
                format!("`{written}` is a computed property of `{receiver}`, not a method: read it without `(…)`.")
            } else {
                format!("`{written}` is a method of `{receiver}`, not a computed property: call it as `{written}(…)`.")
            };
            let declared = metadata.name_span.unwrap_or(metadata.span);
            context.collector.report_with_code(
                code,
                Issue::error(message)
                    .with_annotation(Annotation::primary(name.span()).with_message(access))
                    .with_annotation(Annotation::secondary(declared).with_message("Declared here.")),
            );

            Some(Extension::Refused)
        }
        ([], None, Some((method, _))) => {
            let Some(import) = display_missing_imports(context, [method.get_class_name()]) else {
                unreachable!("a class the file binds has its extensions in scope");
            };
            context.collector.report_with_code(
                code,
                Issue::error(format!(
                    "`{}` is an extension this file does not import. {import}",
                    String::from_utf8_lossy(name.value)
                ))
                .with_annotation(Annotation::primary(name.span()).with_message(access)),
            );

            Some(Extension::Member(method))
        }
        ([], None, None) => None,
    }
}

/// Whether a value of type `receiver` fits the receiver of the extension member `metadata`, its first parameter. The
/// type parameters of an extension block, `T` in `extension<T>(List<T> items)`, are first inferred from `receiver`, as
/// a call infers them from its argument, so a `List<Order>` fits as a `List<Order>`. A receiver outside a type
/// parameter's bound fits none, and so does every receiver of a member whose own receiver has no type, which only a
/// file the parser refused declares.
fn fits<A>(context: &Context<'_, '_, A>, receiver: &TUnion, metadata: &FunctionLikeMetadata) -> bool
where
    A: Arena,
{
    metadata.parameters.first().and_then(|parameter| parameter.get_type_metadata()).is_some_and(|parameter| {
        let parameter = if metadata.template_types.is_empty() {
            Cow::Borrowed(&parameter.type_union)
        } else {
            let mut template_result = TemplateResult::new(
                metadata.template_types.iter().map(|(name, template)| (*name, vec![template.clone()])).collect(),
                HashMap::default(),
            );
            if !infer_unfixed_templates(context, &parameter.type_union, receiver, &mut template_result).is_empty() {
                return false;
            }

            Cow::Owned(inferred_type_replacer::replace(&parameter.type_union, &template_result, context.codebase))
        };

        union_comparator::is_contained_by(
            context.codebase,
            receiver,
            &parameter,
            false,
            false,
            false,
            &mut ComparisonResult::new(),
        )
    })
}

/// The members of `candidates` whose receiver fits inside every other candidate's, as C# overload resolution and
/// Kotlin pick the most specific receiver, spec section 26: an extension on `AuditedWallet` over one on `Wallet`. All
/// of `candidates` when none does, so they stay ambiguous.
fn most_specific<'ctx, A>(
    context: &Context<'_, '_, A>,
    candidates: Vec<(MethodIdentifier, &'ctx FunctionLikeMetadata)>,
) -> Vec<(MethodIdentifier, &'ctx FunctionLikeMetadata)>
where
    A: Arena,
{
    let specific: Vec<_> = candidates
        .iter()
        .filter(|(_, metadata)| {
            metadata
                .parameters
                .first()
                .and_then(|parameter| parameter.get_type_metadata())
                .is_some_and(|own| candidates.iter().all(|(_, other)| fits(context, &own.type_union, other)))
        })
        .copied()
        .collect();

    if specific.is_empty() { candidates } else { specific }
}

/// The start of the error for the extensions `candidates`, which compete for the member `written`, naming where each
/// comes from, and the help whose fix removes the competition. The candidates are all of one tier: the file's
/// namespace and its imports, or else the standard library's that every file has.
fn ambiguity<A>(
    context: &Context<'_, '_, A>,
    candidates: &[(MethodIdentifier, &FunctionLikeMetadata)],
    written: &str,
) -> (String, String)
where
    A: Arena,
{
    let mut own = Vec::new();
    let mut imported = Vec::new();
    let mut default = Vec::new();
    for (method, _) in candidates {
        let class = method.get_class_name();
        let named = format!("`{}`", display_class_like_name(context, class));
        if is_in_namespace(context, class) {
            own.push(named);
        } else if let Some(alias) = context.imported_names.get(&ascii_lowercase_word(class.as_bytes())) {
            let full = context.codebase.get_class_like(class.as_bytes()).map_or(class, |class| class.original_name);
            let renamed = if alias.as_bytes().eq_ignore_ascii_case(short_name(full).as_bytes()) {
                String::new()
            } else {
                format!(" as {alias}")
            };
            imported.push((named, format!("`import {}{renamed};`", full.as_str_lossy().replace('\\', "."))));
        } else {
            default.push(named);
        }
    }
    own.sort();
    imported.sort();
    default.sort();

    let namespace = match context.scope.namespace_name() {
        Some(namespace) if !namespace.is_empty() => {
            format!("namespace `{}`", String::from_utf8_lossy(namespace).replace('\\', "."))
        }
        _ => "the global namespace".to_owned(),
    };
    let (imported, imports): (Vec<String>, Vec<String>) = imported.into_iter().unzip();
    let groups: Vec<String> = [
        (!own.is_empty()).then(|| format!("{} of {namespace}", extensions("extension", &own))),
        (!imported.is_empty()).then(|| extensions("imported extension", &imported)),
        (!default.is_empty()).then(|| extensions("standard library extension", &default)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let message = format!("The {}", groups.join(" and the "));

    let help = match (own.as_slice(), imports.as_slice(), default.as_slice()) {
        ([], [_, _, ..], _) => format!("Remove all but one of {}.", listed(&imports)),
        ([_], [_, ..], _) => format!("Remove {}.", listed(&imports)),
        ([], [], [_, ..]) => {
            "Import the class of the one to call. An imported extension shadows the standard library's.".to_owned()
        }
        _ => format!("Rename `{written}` in all but one of them."),
    };

    (message, help)
}

/// `{kind} A` for one name, and `{kind}s A and B` for more.
fn extensions(kind: &str, names: &[String]) -> String {
    if let [name] = names { format!("{kind} {name}") } else { format!("{kind}s {}", listed(names)) }
}

/// `a`, `a and b`, or `a, b and c`.
fn listed(items: &[String]) -> String {
    match items.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => unreachable!("an ambiguity names two extensions or more, and each list holds one of them at least"),
    }
}

/// Whether the static class `class` is in this file's namespace, as the declaring file is, so its extensions apply with
/// no import, spec section 26.
fn is_in_namespace<A>(context: &Context<'_, '_, A>, class: Word) -> bool
where
    A: Arena,
{
    let name = class.as_bytes();
    let namespace = name.iter().rposition(|byte| *byte == b'\\').map_or(&b""[..], |end| &name[..end]);

    namespace.eq_ignore_ascii_case(context.scope.namespace_name().unwrap_or_default())
}

/// Whether the extensions of the static class `class` apply in this file ahead of the ones every file has, spec
/// sections 23 and 26: the file imports the class, or is in the class's namespace, as the declaring file is.
fn is_imported_or_in_namespace<A>(context: &Context<'_, '_, A>, class: Word) -> bool
where
    A: Arena,
{
    is_in_namespace(context, class) || context.imported_names.contains_key(&ascii_lowercase_word(class.as_bytes()))
}

/// Whether this file has the extension member `metadata` of the static class `class` with no import, spec section
/// 23: the default import of `Sharp` binds the class, or the member is the standard library's, under `Sharp\`, on
/// `string`, `int`, `float`, `List`, `Map` or `Set`.
pub(crate) fn is_imported_by_default<A>(
    context: &Context<'_, '_, A>,
    class: Word,
    metadata: &FunctionLikeMetadata,
) -> bool
where
    A: Arena,
{
    imports_by_default(&context.scope, class)
        || class.as_bytes().get(..6).is_some_and(|root| root.eq_ignore_ascii_case(b"sharp\\"))
            && metadata.parameters.first().and_then(|receiver| receiver.get_type_metadata()).is_some_and(|receiver| {
                receiver.type_union.types.iter().all(|atomic| {
                    matches!(
                        atomic,
                        TAtomic::Scalar(TScalar::String(_) | TScalar::Integer(_) | TScalar::Float(_))
                            | TAtomic::Array(_)
                    )
                })
            })
}

/// Whether `name` is a member of `Class`, spec section 25: a member the standard library's extensions, under `Sharp\`,
/// add to every class value, as `attributes`. A call or read of it through a class value reaches it, not a static of
/// the class the value holds, and no PHP# class may declare a static of its name.
pub(crate) fn is_class_member(codebase: &CodebaseMetadata, name: &[u8]) -> bool {
    codebase.extensions.get(&ascii_lowercase_word(name)).is_some_and(|members| {
        members.iter().any(|(_, method)| {
            method.get_class_name().as_bytes().get(..6).is_some_and(|root| root.eq_ignore_ascii_case(b"sharp\\"))
                && codebase
                    .get_method_by_id(method)
                    .and_then(|metadata| metadata.parameters.first())
                    .and_then(|receiver| receiver.get_type_metadata())
                    .is_some_and(|receiver| {
                        receiver
                            .type_union
                            .types
                            .iter()
                            .all(|atomic| matches!(atomic, TAtomic::Scalar(TScalar::ClassLikeString(_))))
                    })
        })
    })
}
