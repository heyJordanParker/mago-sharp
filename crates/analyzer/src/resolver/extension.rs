//! The members PHP# extensions add to a receiver's type, spec section 26.

use mago_allocator::Arena;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::flags::MetadataFlags;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::LocalIdentifier;
use mago_word::Word;
use mago_word::ascii_lowercase_word;

use crate::code::IssueCode;
use crate::context::Context;
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
/// or `None` when no extension of that name fits the receiver. The receiver is the value that is not `null`: `?.`
/// skips `null`, and the access reports a `.` on a receiver that may be `null` as it does for any member.
///
/// The receiver's type picks among the extensions the file imports and the standard library's on the types every file
/// has them for.
/// Two that fit are an error naming both classes, and one that fits but the file does not import is an error naming
/// the import to add. Either still reaches its member, so the access is checked as it would run.
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
    let fitting: Vec<(MethodIdentifier, &'ctx FunctionLikeMetadata)> = codebase
        .extensions
        .get(&ascii_lowercase_word(name.value))?
        .iter()
        .filter_map(|(_, method)| Some((*method, codebase.get_method_by_id(method)?)))
        .filter(|(_, metadata)| fits(codebase, receiver, metadata))
        .collect();
    let (in_scope, elsewhere): (Vec<_>, Vec<_>) =
        fitting.iter().partition(|(method, metadata)| is_in_scope(context, method.get_class_name(), metadata));
    let (in_scope, other_kind): (Vec<_>, Vec<_>) =
        in_scope.into_iter().partition(|(_, metadata)| metadata.flags.contains(flag));
    let elsewhere = elsewhere.into_iter().find(|(_, metadata)| metadata.flags.contains(flag));
    let receiver = display_type(context, receiver);
    let (code, access) = if flag == MetadataFlags::EXTENSION_METHOD {
        (IssueCode::NonExistentMethod, "Called here.")
    } else {
        (IssueCode::NonExistentProperty, "Read here.")
    };

    match (in_scope.as_slice(), other_kind.first(), elsewhere) {
        ([(method, _)], ..) => Some(Extension::Member(*method)),
        ([(method, _), others @ ..], ..) => {
            let classes: Vec<String> = std::iter::once(method)
                .chain(others.iter().map(|(other, _)| other))
                .map(|method| format!("`{}`", display_class_like_name(context, method.get_class_name())))
                .collect();
            let (last, rest) = classes.split_last()?;
            context.collector.report_with_code(
                if flag == MetadataFlags::EXTENSION_METHOD {
                    IssueCode::AmbiguousObjectMethodAccess
                } else {
                    IssueCode::AmbiguousObjectPropertyAccess
                },
                Issue::error(format!(
                    "The imported extensions {} and {last} each add `{}` to `{receiver}`.",
                    rest.join(", "),
                    String::from_utf8_lossy(name.value)
                ))
                .with_annotation(Annotation::primary(name.span()).with_message(access))
                .with_help("Import only one of them."),
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

/// Whether a value of type `receiver` fits the receiver of the extension member `metadata`, its first parameter.
fn fits(codebase: &CodebaseMetadata, receiver: &TUnion, metadata: &FunctionLikeMetadata) -> bool {
    metadata.parameters.first().and_then(|parameter| parameter.get_type_metadata()).is_some_and(|parameter| {
        union_comparator::is_contained_by(
            codebase,
            receiver,
            &parameter.type_union,
            false,
            false,
            false,
            &mut ComparisonResult::new(),
        )
    })
}

/// Whether the extension member `metadata` of the static class `class` applies in this file: the file imports the
/// class, is in the class's namespace, as the declaring file is, or every file has it.
fn is_in_scope<A>(context: &Context<'_, '_, A>, class: Word, metadata: &FunctionLikeMetadata) -> bool
where
    A: Arena,
{
    let name = class.as_bytes();
    let namespace = name.iter().rposition(|byte| *byte == b'\\').map_or(&b""[..], |end| &name[..end]);

    namespace.eq_ignore_ascii_case(context.scope.namespace_name().unwrap_or_default())
        || context.imported_names.contains_key(&ascii_lowercase_word(name))
        || is_imported_by_default(context, class, metadata)
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
