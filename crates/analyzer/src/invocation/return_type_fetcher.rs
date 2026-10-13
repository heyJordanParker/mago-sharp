use std::sync::Arc;

use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::ttype::add_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::combiner::CombinerOptions;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::get_list;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::template::TemplateResult;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_word::WordMap;

use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::binary::comparison::check_sorted_by_key;
use crate::invocation::Invocation;
use crate::invocation::resolver::resolve_invocation_type;
use crate::utils::names::display_sharp_type;

pub fn fetch_invocation_return_type<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &AnalysisArtifacts,
    invocation: &Invocation<'ctx, '_, 'arena>,
    template_result: &TemplateResult,
    parameters: &WordMap<TUnion>,
) -> Result<TUnion, AnalysisError>
where
    A: Arena,
{
    if context.dialect.is_sharp()
        && let Some(return_type) = fetch_list_wrap_return_type(context, artifacts, invocation)
    {
        return Ok(return_type);
    }

    if context.dialect.is_sharp() && is_method(invocation, b"Sharp\\SetMethods", b"filter") {
        return Ok(fetch_set_filter_return_type(context, invocation, template_result, parameters));
    }

    if context.dialect.is_sharp()
        && is_sorted_by(invocation)
        && let Some(selector) = invocation.arguments_source.get_argument(0).filter(|argument| !argument.is_unpacked())
        && let Some(selector) = selector.value()
        && let Some([key_type]) = artifacts
            .inferred_type_arguments
            .get(&(invocation.span.start.offset, invocation.span.end.offset))
            .map(Vec::as_slice)
    {
        check_sorted_by_key(context, selector, key_type);
    }

    if let Some(return_type) = fetch_invocation_provider_return_type(context, block_context, artifacts, invocation) {
        return Ok(return_type);
    }

    Ok(fetch_declared_invocation_return_type(context, invocation, template_result, parameters))
}

/// Gives PHP#'s `List.wrap(value)` its type `List<T>`, and refuses it when `T` could itself be a list, as spec section
/// 24 decides. `wrap` returns an array as the list it was asked for, and a `List` and a `Map` both run as PHP arrays,
/// so it cannot tell a value that is a list from a list of values. `T` is the value's parts that are not lists plus the
/// elements of its lists, or the value's whole type when every part of it is an array. A refused call keeps `List<T>`,
/// so the code around it adds no second issue.
fn fetch_list_wrap_return_type<A>(
    context: &mut Context<'_, '_, A>,
    artifacts: &AnalysisArtifacts,
    invocation: &Invocation<'_, '_, '_>,
) -> Option<TUnion>
where
    A: Arena,
{
    if !is_method(invocation, b"Sharp\\List", b"wrap") {
        return None;
    }

    let value = invocation.arguments_source.get_argument(0).filter(|argument| !argument.is_unpacked())?.value()?;
    let value_type = artifacts.get_expression_type(value)?;
    let codebase = context.codebase;

    let element_type = if value_type.types.iter().all(TAtomic::is_array) {
        value_type.clone()
    } else {
        value_type.types.iter().fold(None, |element_type: Option<TUnion>, atomic| {
            let part = match atomic {
                TAtomic::Array(list @ TArray::List(_)) => get_array_parameters(list, codebase).1,
                _ => TUnion::from_atomic(atomic.clone()),
            };

            Some(match element_type {
                Some(element_type) => add_union_type(element_type, &part, codebase, CombinerOptions::default()),
                None => part,
            })
        })?
    };

    let source = String::from_utf8_lossy(
        context
            .source_file
            .contents
            .get(value.start_offset() as usize..value.end_offset() as usize)
            .unwrap_or_default(),
    );
    let value_text = display_sharp_type(context, value_type);
    let message = if value_type.has_mixed() {
        format!("T is {value_text}, which could itself be a list; check what `{source}` is with `is` first")
    } else {
        let collections: Vec<TAtomic> = element_type.types.iter().filter(|atomic| atomic.is_array()).cloned().collect();
        if collections.is_empty() {
            return Some(get_list(element_type));
        }

        let kind = if collections.iter().any(|atomic| matches!(atomic, TAtomic::Array(TArray::List(_)))) {
            "list"
        } else {
            "map"
        };
        let reason = if collections.len() == element_type.types.len() { "itself a" } else { "which can be a" };
        let collection_type = display_sharp_type(context, &TUnion::from_vec(collections));

        format!(
            "T is {}, {reason} {kind}; write `{source} is {collection_type} one ? [one] : {source}`",
            display_sharp_type(context, &element_type)
        )
    };

    context.collector.report_with_code(
        IssueCode::InvalidArgument,
        Issue::error(message)
            .with_annotation(Annotation::primary(value.span()).with_message(format!("This is `{value_text}`.")))
            .with_note("`wrap` returns a `List` or a `Map` as it is, because both run as PHP arrays."),
    );

    Some(get_list(element_type))
}

/// Gives a PHP# `Set<T>`'s `filter` its type `Set<T>`. A docblock writes no `Set`, so `Sharp\SetMethods::filter`
/// declares the array a `Set` runs as, and the call keeps that array's elements as a `Set`.
fn fetch_set_filter_return_type<A>(
    context: &Context<'_, '_, A>,
    invocation: &Invocation<'_, '_, '_>,
    template_result: &TemplateResult,
    parameters: &WordMap<TUnion>,
) -> TUnion
where
    A: Arena,
{
    let declared = fetch_declared_invocation_return_type(context, invocation, template_result, parameters);
    let element_type =
        declared.get_single_array().map_or_else(get_mixed, |array| get_array_parameters(array, context.codebase).1);

    TUnion::from_atomic(TAtomic::Array(TArray::Set(Arc::new(element_type))))
}

/// Whether `invocation` calls `sortedBy` on a PHP# `List`, `Map` or `Set`, which orders by the key its function returns.
fn is_sorted_by(invocation: &Invocation<'_, '_, '_>) -> bool {
    [b"Sharp\\ListMethods".as_slice(), b"Sharp\\MapMethods", b"Sharp\\SetMethods"]
        .into_iter()
        .any(|class| is_method(invocation, class, b"sortedBy"))
}

/// Whether `invocation` calls the method `method` of the class `class`.
fn is_method(invocation: &Invocation<'_, '_, '_>, class: &[u8], method: &[u8]) -> bool {
    matches!(
        invocation.target.get_function_like_identifier(),
        Some(FunctionLikeIdentifier::Method(called_class, called_method))
            if called_class.as_bytes().eq_ignore_ascii_case(class) && called_method.as_bytes().eq_ignore_ascii_case(method)
    )
}

/// Requests a custom return type from registered providers and reports provider issues.
///
pub(crate) fn fetch_invocation_provider_return_type<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &AnalysisArtifacts,
    invocation: &Invocation<'ctx, '_, 'arena>,
) -> Option<TUnion>
where
    A: Arena,
{
    let identifier = invocation.target.get_function_like_identifier()?;

    fetch_function_like_provider_return_type(context, block_context, artifacts, identifier, invocation)
}

/// Requests a custom return type for an explicit function-like identifier.
///
/// This permits dynamic method calls to match the method requested by the user
/// while retaining `__call` or `__callStatic` as the invocation's native target.
///
pub(crate) fn fetch_function_like_provider_return_type<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &AnalysisArtifacts,
    identifier: &FunctionLikeIdentifier,
    invocation: &Invocation<'ctx, '_, 'arena>,
) -> Option<TUnion>
where
    A: Arena,
{
    if let Some(result) = context.plugin_registry.get_function_like_return_type(
        context.codebase,
        context.source_file,
        block_context,
        artifacts,
        identifier,
        invocation,
        context.external_analysis_session,
    ) {
        for reported_issue in result.issues {
            context.collector.report_with_code(reported_issue.code, reported_issue.issue);
        }

        if let Some(ty) = result.return_type {
            return Some(ty);
        }
    }

    None
}

/// Resolves the declared return type of an invocation without consulting providers.
pub(crate) fn fetch_declared_invocation_return_type<A>(
    context: &Context<'_, '_, A>,
    invocation: &Invocation<'_, '_, '_>,
    template_result: &TemplateResult,
    parameters: &WordMap<TUnion>,
) -> TUnion
where
    A: Arena,
{
    let mut resulting_type = if let Some(return_type) = invocation.target.get_return_type().cloned() {
        resolve_invocation_type(context, invocation, template_result, parameters, return_type)
    } else {
        get_mixed()
    };

    if let Some(function_like_metadata) = invocation.target.get_function_like_metadata()
        && function_like_metadata.flags.is_by_reference()
    {
        resulting_type.set_by_reference(true);
    }

    resulting_type
}
