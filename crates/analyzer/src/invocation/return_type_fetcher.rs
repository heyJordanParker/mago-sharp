use std::sync::Arc;

use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::ttype::add_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::array::keyed::TKeyedArray;
use mago_codex::ttype::combiner::CombinerOptions;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::get_int;
use mago_codex::ttype::get_list;
use mago_codex::ttype::get_map_entry;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_php_received_type;
use mago_codex::ttype::get_sharp_received_type;
use mago_codex::ttype::is_key_type;
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
use crate::invocation::InvocationArgumentsSource;
use crate::invocation::InvocationTarget;
use crate::invocation::resolver::resolve_invocation_type;
use crate::resolver::extension::is_collection_member;
use crate::statement::class_like::r#struct::report_class_held_by_key;
use crate::statement::function_like::collection_key_error;
use crate::statement::function_like::known_type_or_error;
use crate::utils::names::display_sharp_type;
use crate::utils::names::is_prelude_stub;

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

    if context.dialect.is_sharp()
        && (is_method(&invocation.target, b"Sharp\\Internal\\SetMethods", b"filter")
            || is_function(&invocation.target, b"Sharp\\Internal\\toSet"))
    {
        return Ok(fetch_set_return_type(context, invocation, template_result, parameters));
    }

    if context.dialect.is_sharp() && is_function(&invocation.target, b"Sharp\\Internal\\entries") {
        return Ok(fetch_indexed_return_type(context, invocation, template_result, parameters));
    }

    if context.dialect.is_sharp() && is_method(&invocation.target, b"Sharp\\Internal\\MapMethods", b"toList") {
        return Ok(fetch_entries_return_type(context, invocation, template_result, parameters));
    }

    // `sortedBy` orders by the key its function returns, and `groupBy` and `associateBy` key the `Map` they return by it.
    // An extension member's call passes its receiver first, and its last type argument is the key's.
    let selector_index =
        usize::from(matches!(invocation.arguments_source, InvocationArgumentsSource::ExtensionReceiver(..)));
    if context.dialect.is_sharp()
        && let Some(argument) =
            invocation.arguments_source.get_argument(selector_index).filter(|argument| !argument.is_unpacked())
        && let Some(selector) = argument.value()
        && let Some(key_type) = artifacts
            .inferred_type_arguments
            .get(&(invocation.span.start.offset, invocation.span.end.offset))
            .and_then(|type_arguments| type_arguments.last())
    {
        // A key is checked only where a function fills the key parameter. Otherwise the argument check refused the
        // call, and the key it left is no key the code wrote.
        let fills_key_parameter = argument.get_parameter_name().is_none_or(|name| name == b"key")
            && artifacts.get_expression_type(selector).is_some_and(TUnion::is_callable);

        if is_collection_method(&invocation.target, b"sortedBy") {
            check_sorted_by_key(context, selector, key_type);
        } else if keys_a_map_by_its_function(&invocation.target) && fills_key_parameter {
            // A key that keys no `Map`, or a struct that holds a class instance, spec section 10, is refused.
            let is_refused = if is_key_type(key_type, context.codebase) {
                report_class_held_by_key(context, key_type, false, selector.span())
            } else {
                let key_name = display_sharp_type(context, key_type);
                let error = collection_key_error(key_name.as_str(), false, selector.span());
                context.collector.report_with_code(IssueCode::TemplateConstraintViolation, error);

                true
            };
            if is_refused {
                // The refused key is an error, so the code that reads the `Map` adds no second issue.
                let declared = fetch_declared_invocation_return_type(context, invocation, template_result, parameters);
                let value_type = declared
                    .get_single_array()
                    .map_or_else(get_mixed, |array| get_array_parameters(array, context.codebase).1);
                let map = TKeyedArray::new_with_parameters(
                    Arc::new(TUnion::from_atomic(TAtomic::Error)),
                    Arc::new(value_type),
                );

                return Ok(TUnion::from_atomic(TAtomic::Array(TArray::Keyed(map))));
            }
        }
    }

    if let Some(return_type) = fetch_invocation_provider_return_type(context, block_context, artifacts, invocation) {
        return Ok(return_type);
    }

    Ok(fetch_declared_invocation_return_type(context, invocation, template_result, parameters))
}

/// Gives PHP#'s `List.wrap(value)` its type `List<T>`, and refuses it when `T` could itself be a list, as spec section
/// 24 decides. `wrap` returns a `List` as it is, so it cannot tell a value that is a list from a list of values. A `Map`
/// or a `Set` is one object, which `wrap` holds in a one-element `List` as any other value. `T` is the value's parts that
/// are not lists plus the elements of its lists, or the value's whole type when every part of it is a list. A refused
/// call keeps `List<T>`, so the code around it adds no second issue.
fn fetch_list_wrap_return_type<A>(
    context: &mut Context<'_, '_, A>,
    artifacts: &AnalysisArtifacts,
    invocation: &Invocation<'_, '_, '_>,
) -> Option<TUnion>
where
    A: Arena,
{
    if !is_method(&invocation.target, b"Sharp\\List", b"wrap") {
        return None;
    }

    let value = invocation.arguments_source.get_argument(0).filter(|argument| !argument.is_unpacked())?.value()?;
    let value_type = artifacts.get_expression_type(value)?;
    let codebase = context.codebase;

    let is_list = |atomic: &TAtomic| matches!(atomic, TAtomic::Array(TArray::List(_)));
    let element_type = if value_type.types.iter().all(is_list) {
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
        let lists: Vec<TAtomic> = element_type.types.iter().filter(|atomic| is_list(atomic)).cloned().collect();
        if lists.is_empty() {
            return Some(get_list(element_type));
        }

        let reason = if lists.len() == element_type.types.len() { "itself a" } else { "which can be a" };
        let list_type = display_sharp_type(context, &TUnion::from_vec(lists));

        format!(
            "T is {}, {reason} list; write `{source} is {list_type} one ? [one] : {source}`",
            display_sharp_type(context, &element_type)
        )
    };

    context.collector.report_with_code(
        IssueCode::InvalidArgument,
        Issue::error(message)
            .with_annotation(Annotation::primary(value.span()).with_message(format!("This is `{value_text}`.")))
            .with_note("`wrap` returns a `List` as it is."),
    );

    Some(get_list(element_type))
}

/// Gives a PHP# `Set<T>`'s `filter` and the engine's `Sharp\Internal\toSet`, which a `List<T>`'s `toSet` calls, their
/// type `Set<T>`. A docblock writes no `Set`, so `Sharp\Internal\SetMethods::filter` and `Sharp\Internal\toSet` declare
/// an array of the elements, and the call holds them as a `Set`.
fn fetch_set_return_type<A>(
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

/// Gives the engine's `Sharp\Internal\entries`, which a `List<T>`'s `entries` calls, its type `Map<int, T>`. A plain PHP
/// function's `array<int, T>` reads as a `List`, so the call holds the elements under their indexes as a `Map`.
fn fetch_indexed_return_type<A>(
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
    let map = TKeyedArray::new_with_parameters(Arc::new(get_int()), Arc::new(element_type));

    TUnion::from_atomic(TAtomic::Array(TArray::Keyed(map)))
}

/// Gives a PHP# `Map<K, V>`'s `toList` its type `List<[K, V]>`, spec section 12, as Kotlin's gives pairs. A docblock
/// writes no tuple, so `Sharp\Internal\MapMethods::toList` declares the map, and the call holds each entry as a tuple.
fn fetch_entries_return_type<A>(
    context: &Context<'_, '_, A>,
    invocation: &Invocation<'_, '_, '_>,
    template_result: &TemplateResult,
    parameters: &WordMap<TUnion>,
) -> TUnion
where
    A: Arena,
{
    let declared = fetch_declared_invocation_return_type(context, invocation, template_result, parameters);
    let (key_type, value_type) = declared
        .get_single_array()
        .map_or_else(|| (get_mixed(), get_mixed()), |array| get_array_parameters(array, context.codebase));

    get_list(get_map_entry(key_type, value_type))
}

/// Whether `target` is a PHP# `List`'s or `Map`'s `groupBy` or `associateBy`, or the engine function a `List`'s runs,
/// which key a `Map` by what their function returns. A `.sharp` call checks that key by spec sections 10 and 12's rule,
/// where a backed enum or a struct keys a `Map` too, so it infers their `G` without the `array-key` bound a `.php` call
/// checks it by.
pub(crate) fn keys_a_map_by_its_function(target: &InvocationTarget<'_>) -> bool {
    is_collection_method(target, b"groupBy")
        || is_collection_method(target, b"associateBy")
        || is_function(target, b"Sharp\\Internal\\groupBy")
        || is_function(target, b"Sharp\\Internal\\associateBy")
}

/// Whether `target` is the function `function`, by its full name.
fn is_function(target: &InvocationTarget<'_>, function: &[u8]) -> bool {
    matches!(
        target.get_function_like_identifier(),
        Some(FunctionLikeIdentifier::Function(called)) if called.as_bytes().eq_ignore_ascii_case(function)
    )
}

/// Whether `target` is the method `method` of a PHP# `List`, `Map` or `Set`: the standard library's member, spec section
/// 12, or a method of the prelude's `Sharp\Internal\MapMethods` or `Sharp\Internal\SetMethods`.
fn is_collection_method(target: &InvocationTarget<'_>, method: &[u8]) -> bool {
    [b"Sharp\\Internal\\MapMethods".as_slice(), b"Sharp\\Internal\\SetMethods"]
        .into_iter()
        .any(|class| is_method(target, class, method))
        || matches!(
            (target.get_function_like_identifier(), target.get_function_like_metadata()),
            (Some(FunctionLikeIdentifier::Method(class, called)), Some(metadata))
                if called.as_bytes().eq_ignore_ascii_case(method) && is_collection_member(*class, metadata)
        )
}

/// Whether `target` is the method `method` of the class `class`.
fn is_method(target: &InvocationTarget<'_>, class: &[u8], method: &[u8]) -> bool {
    matches!(
        target.get_function_like_identifier(),
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

    // What plain PHP returns arrives in PHP# as PHP# reads it, and what PHP# returns reaches plain PHP as plain PHP reads
    // it. A function value with no declaration, as a PHP# lambda, returns the PHP# type its function type writes.
    let function_like_metadata = invocation.target.get_function_like_metadata();
    match function_like_metadata {
        Some(metadata) if metadata.flags.is_sharp() => {
            resulting_type = known_type_or_error(resulting_type);
            if !context.dialect.is_sharp() {
                resulting_type = get_php_received_type(&resulting_type, context.codebase);
            }
        }
        Some(_)
            if context.dialect.is_sharp()
                && !matches!(
                    invocation.target.get_function_like_identifier(),
                    Some(FunctionLikeIdentifier::Method(class, _)) if is_prelude_stub(*class)
                ) =>
        {
            resulting_type = get_sharp_received_type(&resulting_type);
        }
        _ => {}
    }

    if function_like_metadata.is_some_and(|metadata| metadata.flags.is_by_reference()) {
        resulting_type.set_by_reference(true);
    }

    resulting_type
}
