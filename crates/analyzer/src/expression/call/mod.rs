use mago_allocator::Arena;
use std::sync::Arc;

use mago_algebra::assertion_set::AssertionSet;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::parameter::FunctionLikeParameterMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::named::TNamedObject;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::expander::StaticClassType;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_codex::ttype::get_void;
use mago_codex::ttype::is_key_type;
use mago_codex::ttype::template::TemplateResult;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::TypeArgumentList;
use mago_word::Word;
use mago_word::WordMap;
use mago_word::ascii_lowercase_word;
use mago_word::concat_word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::artifacts::CallTarget;
use crate::artifacts::ResolvedMethodCall;
use crate::artifacts::get_expression_range;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::context::scope::control_action::ControlAction;
use crate::error::AnalysisError;
use crate::expression::access::report_full_name;
use crate::expression::access::report_member_of_mixed_kinds;
use crate::expression::binary::comparison::check_element_equality;
use crate::expression::binary::comparison::check_ordered_key;
use crate::expression::binary::comparison::search_refusal;
use crate::invocation::Invocation;
use crate::invocation::InvocationArgumentsSource;
use crate::invocation::InvocationTarget;
use crate::invocation::MethodInvocationKind;
use crate::invocation::MethodTargetContext;
use crate::invocation::analyzer::analyze_invocation;
use crate::invocation::post_process::post_invocation_process;
use crate::invocation::return_type_fetcher::fetch_invocation_return_type;
use crate::invocation::template_result::seed_extension_type_arguments;
use crate::plugin::hook::NamedCall;
use crate::plugin::hook::StaticCall;
use crate::reconciler::assertion_reconciler;
use crate::resolver::extension::is_collection_member;
use crate::utils::names::collection_type_parameters;
use crate::utils::names::display_function_like_identifier;
use crate::utils::quoted_source;

pub mod function_call;
pub mod method_call;
pub mod pipe;
pub mod static_method_call;

fn record_external_method_call<A>(
    context: &Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    targets: &[InvocationTarget<'_>],
    span: Span,
) where
    A: Arena,
{
    record_external_method_call_targets(
        context,
        artifacts,
        targets.iter().filter_map(|target| {
            let FunctionLikeIdentifier::Method(class, method) = target.get_function_like_identifier()? else {
                return None;
            };
            Some((
                target
                    .get_method_context()
                    .map_or(*class, |method_context| method_context.class_like_metadata.original_name),
                *method,
            ))
        }),
        span,
    );
}

pub(super) fn record_external_method_call_targets<A>(
    context: &Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    targets: impl IntoIterator<Item = (Word, Word)>,
    span: Span,
) where
    A: Arena,
{
    if !context.plugin_registry.has_external_method_call_analysis_hooks() {
        return;
    }

    let span = (span.start.offset, span.end.offset);
    for (class, method) in targets {
        if artifacts
            .resolved_method_calls
            .iter()
            .rev()
            .take_while(|target| target.span == span)
            .any(|target| target.class == class && target.method == method)
        {
            continue;
        }
        artifacts.resolved_method_calls.push(ResolvedMethodCall { span, class, method });
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Call<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        analyze_call(self, context, block_context, artifacts)?;
        report_void_result_used(context, block_context, artifacts, self);

        Ok(())
    }
}

/// Refuses the result of a call that returns nothing where code uses it as a value, as an argument, an assigned value,
/// an element or an operand, as C# and Java refuse it: a `void` method or function value gives no value, spec section
/// 14.1. Through `?.` the call's type is the `null` it adds, so the targets' `void` decides, while a part of the receiver
/// whose method returns a value, as a `List`'s `count` beside a `void` one, leaves the call a value. The call then has
/// the type of a refused value, so nothing that reads its value reports the same mistake again.
fn report_void_result_used<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    call: &Call<'_>,
) where
    A: Arena,
{
    let Some(result) = artifacts.get_expression_type(call) else {
        return;
    };
    if !context.dialect.is_sharp()
        || !block_context.flags.inside_general_use()
        || !(result.is_void()
            || (result.is_null() && artifacts.null_safe_void_calls.contains(&get_expression_range(call))))
    {
        return;
    }

    let written = quoted_source(context, call.span());
    context.collector.report_with_code(
        IssueCode::NoValue,
        Issue::error(format!(
            "`{written}` returns nothing, so it has no value to use: call it as a statement of its own."
        ))
        .with_annotation(Annotation::primary(call.span()).with_message("Used as a value here.")),
    );
    artifacts.set_expression_type(call, TUnion::from_atomic(TAtomic::Error));
}

/// Analyzes `expression` by the kind of call it is.
fn analyze_call<'ctx, 'arena, A>(
    expression: &Call<'arena>,
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    match expression {
        Call::Function(call) => match NamedCall::from_call(context.resolved_names, context.codebase, expression) {
            Some(named_call) => function_call::analyze_with_function_call_hooks(
                context,
                block_context,
                artifacts,
                &named_call,
                |context, block_context, artifacts| call.analyze(context, block_context, artifacts),
            ),
            None => call.analyze(context, block_context, artifacts),
        },
        // As a call with an invalid target, it is an error, and only its arguments are analyzed.
        Call::Method(call)
            if report_full_name(
                context,
                call.object,
                match &call.method {
                    ClassLikeMemberSelector::Identifier(method) => Some(method),
                    _ => None,
                },
            ) =>
        {
            call.argument_list.analyze(context, block_context, artifacts)?;
            artifacts.set_expression_type(expression, TUnion::from_atomic(TAtomic::Error));

            Ok(())
        }
        // PHP# calls a plain PHP function through its namespace, `Pdf.render(html)` or `Foundation.abort(404)`.
        Call::Method(call)
            if context.dialect.is_sharp()
                && let Some(named_call) =
                    NamedCall::from_call(context.resolved_names, context.codebase, expression) =>
        {
            function_call::analyze_with_function_call_hooks(
                context,
                block_context,
                artifacts,
                &named_call,
                |context, block_context, artifacts| {
                    function_call::analyze_namespace_function_call(
                        context,
                        block_context,
                        artifacts,
                        call,
                        named_call.name,
                    )
                },
            )
        }
        // PHP# writes the static call `Class::m()` as `Class.m()`, with a bare name the binder bound to a class.
        Call::Method(call)
            if context.dialect.is_sharp()
                && let Some(static_call) = StaticCall::from_method_call(call, context.resolved_names) =>
        {
            static_method_call::analyze_static_method_call(
                context,
                block_context,
                artifacts,
                static_call,
                call.type_arguments.as_ref(),
                false,
            )
        }
        Call::Method(call) => {
            call.analyze(context, block_context, artifacts)?;
            report_member_of_mixed_kinds(context, artifacts, call.object, &call.method, true);

            Ok(())
        }
        Call::NullSafeFunction(call) => {
            function_call::analyze_null_safe_function_call(context, block_context, artifacts, call)
        }
        Call::NullSafeMethod(call) => {
            call.analyze(context, block_context, artifacts)?;
            report_member_of_mixed_kinds(context, artifacts, call.object, &call.method, true);

            Ok(())
        }
        Call::StaticMethod(call) => call.analyze(context, block_context, artifacts),
    }
}

/// The type `resulting_type` of the call at `call_span` with the `null` of `?->`: when its object may be null,
/// `should_add_null`, or when a `?->` before it in the chain gave the object a null, `object_has_nullsafe_null`, and
/// the call's own result holds no null. Each declared, magic and static call gets it here.
pub(super) fn with_null_safe_null(
    artifacts: &mut AnalysisArtifacts,
    call_span: Span,
    resulting_type: TUnion,
    should_add_null: bool,
    object_has_nullsafe_null: bool,
) -> TUnion {
    let holds_own_null = resulting_type.is_nullable();
    let adds_null = should_add_null || (object_has_nullsafe_null && !holds_own_null);
    if !adds_null {
        return resulting_type;
    }

    // A `void` call gives no value to use beside the `null` of `?.`, spec section 14.1.
    if resulting_type.is_void() {
        artifacts.null_safe_void_calls.insert((call_span.start.offset, call_span.end.offset));
    }
    // The `null` stands beside the result's parts, so `maybe?.feed()` keeps each `List` a method may return, as
    // `shelf.feed()` does, spec section 14.4.
    let mut result_with_null = resulting_type.as_nullable();
    result_with_null.set_nullsafe_null(!holds_own_null);
    result_with_null
}

fn analyze_invocation_targets<'ctx, 'ast, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    mut template_result: TemplateResult,
    invocation_targets: Vec<InvocationTarget<'ctx>>,
    invocation_arguments: InvocationArgumentsSource<'ast, 'arena>,
    call_span: Span,
    this_variable: Option<Word>,
    encountered_invalid_targets: bool,
    encountered_mixed_targets: bool,
    should_add_null: bool,
    object_has_nullsafe_null: bool,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let method_name_for_assertions: Option<Word> = invocation_targets.iter().find_map(|target| {
        if let InvocationTarget::FunctionLike {
            identifier: FunctionLikeIdentifier::Method(_, _),
            method_context: Some(_),
            metadata,
            ..
        } = target
        {
            Some(metadata.original_name)
        } else {
            None
        }
    });

    let method_call_is_stable = matches!(
        invocation_arguments,
        InvocationArgumentsSource::ArgumentList(arguments) if arguments.arguments.is_empty()
    ) && !invocation_targets.is_empty()
        && invocation_targets.iter().all(|target| {
            let InvocationTarget::FunctionLike { metadata, method_context: Some(_), .. } = target else {
                return false;
            };

            (metadata.flags.is_pure() || metadata.flags.is_mutation_free()) && !metadata.flags.suspends_fiber()
        });

    let class_related_argument_variables = this_variable
        .map(|receiver| block_context.get_class_type_relation_sources(receiver))
        .filter(|variables| !variables.is_empty());

    // A loop's first pass can infer `never` for a call that returns, so a call inferred as `never` ends no block inside
    // a loop. A function declared to return `never`, as PHP's `exit` is, never returns in any pass, so in a `.sharp`
    // file it ends its block inside a loop too, and spec section 7 refuses the code after it.
    let declared_never = context.dialect.is_sharp()
        && !invocation_targets.is_empty()
        && invocation_targets.iter().all(|target| {
            matches!(target, InvocationTarget::FunctionLike { metadata, .. }
                if metadata.return_type_declaration_metadata.as_ref().is_some_and(|declared| declared.type_union.is_never()))
        });

    let mut resulting_type = None;
    for target in invocation_targets {
        if let InvocationTarget::FunctionLike { metadata, .. } = &target {
            let name = metadata.name;
            match true {
                _ if name.as_bytes().eq_ignore_ascii_case(b"mago\\inspect") => {
                    inspect_arguments(context, block_context, artifacts, &target, &invocation_arguments)?;

                    resulting_type =
                        Some(add_optional_union_type(get_void(), resulting_type.as_ref(), context.codebase));

                    continue;
                }
                _ if name.as_bytes().eq_ignore_ascii_case(b"mago\\confirm") => {
                    confirm_argument_type(context, block_context, artifacts, &target, &invocation_arguments)?;

                    resulting_type =
                        Some(add_optional_union_type(get_void(), resulting_type.as_ref(), context.codebase));

                    continue;
                }
                _ => {}
            }
        }

        if let Some(identifier) = target.get_function_like_identifier() {
            if context.dialect.is_sharp() {
                let class = target.get_method_context().map(|method_context| method_context.class_like_metadata.name);
                artifacts.record_call_target(call_span, CallTarget::FunctionLike { callee: *identifier, class });
            }

            match identifier {
                FunctionLikeIdentifier::Function(function_name) => {
                    let normalized_name = ascii_lowercase_word(function_name.as_ref());
                    artifacts.symbol_references.add_reference_to_symbol(&block_context.scope, normalized_name, false);
                }
                FunctionLikeIdentifier::Method(class_name, method_name) => {
                    artifacts.symbol_references.add_reference_for_method_call(
                        &block_context.scope,
                        &MethodIdentifier::new(*class_name, *method_name),
                    );
                }
                _ => {
                    // Closures don't need reference tracking for invalidation
                }
            }
        }

        let mut invocation: Invocation<'ctx, 'ast, 'arena> = Invocation::new(target, invocation_arguments, call_span);
        let mut argument_types = WordMap::default();

        analyze_invocation(
            context,
            block_context,
            artifacts,
            &mut invocation,
            None,
            class_related_argument_variables.as_ref(),
            &mut template_result,
            &mut argument_types,
        )?;

        let return_type = fetch_invocation_return_type(
            context,
            block_context,
            artifacts,
            &invocation,
            &template_result,
            &argument_types,
        )?;

        resulting_type = Some(add_optional_union_type(return_type, resulting_type.as_ref(), context.codebase));

        post_invocation_process(
            context,
            block_context,
            artifacts,
            &invocation,
            this_variable.as_ref().map(Word::as_bytes),
            &template_result,
            &argument_types,
            true,
        )?;
    }

    // A PHP# call with a target the analysis refused, as an unknown function or a method called on `null`, has the type
    // of a refused value, so nothing that reads its value reports the same mistake again.
    let refused = context.dialect.is_sharp() && encountered_invalid_targets;

    let resulting_type = if let Some(resulting_type) = resulting_type {
        if encountered_invalid_targets {
            if refused {
                artifacts.set_expression_type(&call_span, TUnion::from_atomic(TAtomic::Error));
            }

            return Ok(());
        } else if encountered_mixed_targets {
            get_mixed()
        } else {
            with_null_safe_null(artifacts, call_span, resulting_type, should_add_null, object_has_nullsafe_null)
        }
    } else {
        analyze_invocation_arguments_source(context, block_context, artifacts, &invocation_arguments)?;

        if refused {
            TUnion::from_atomic(TAtomic::Error)
        } else if encountered_mixed_targets {
            get_mixed()
        } else {
            return Ok(());
        }
    };

    if method_call_is_stable
        && let (Some(this_variable), Some(method_name)) = (this_variable, method_name_for_assertions)
    {
        block_context.stable_method_calls.insert(concat_word!(this_variable, "->", method_name, "()"));
    }

    let resulting_type = apply_method_call_assertions(
        context,
        block_context,
        this_variable.as_ref().map(Word::as_bytes),
        method_name_for_assertions,
        method_call_is_stable,
        resulting_type,
    );

    if resulting_type.is_never() && (declared_never || !block_context.flags.inside_loop()) {
        artifacts.set_expression_type(&call_span, resulting_type);

        block_context.flags.set_has_returned(true);
        block_context.control_actions.insert(ControlAction::End);
        return Ok(());
    }
    artifacts.set_expression_type(&call_span, resulting_type);

    Ok(())
}

/// Analyzes a member a PHP# extension adds to its receiver's type, spec section 26, as the call of the static method
/// `extension` it runs as with `arguments`, whose first argument is the receiver. `type_arguments` are the member's
/// own, written after its name. Through `?.`, a `null` receiver skips the call, so the result may be `null` too, as it
/// may below a `?.` earlier in the chain.
#[allow(clippy::expect_used)]
pub(crate) fn analyze_extension_access<'ctx, 'ast, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    extension: MethodIdentifier,
    receiver: &'ast Expression<'arena>,
    type_arguments: Option<&'ast TypeArgumentList<'arena>>,
    arguments: Option<&'ast ArgumentList<'arena>>,
    is_null_safe: bool,
    span: Span,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let class = extension.get_class_name();
    let metadata = context.codebase.get_method_by_id(&extension).expect("an extension is a method of its class");
    let mut template_result = TemplateResult::default();
    if let Some(type_arguments) = type_arguments {
        seed_extension_type_arguments(
            context,
            block_context,
            artifacts,
            type_arguments,
            metadata,
            &mut template_result,
        );
    }
    let class_like_metadata =
        context.codebase.get_class_like(class.as_bytes()).expect("an extension's class is a class-like");
    // A collection's member is the collection's own, spec section 12, so its class carries the receiver's element
    // types, which an issue names it with, as `List<int>.getOrNull`.
    let class_type = match artifacts.get_expression_type(receiver).filter(|_| is_collection_member(class, metadata)) {
        Some(receiver_type) => StaticClassType::Object(TObject::Named(TNamedObject::new_with_type_parameters(
            class,
            Some(collection_type_parameters(&receiver_type.to_non_nullable(), context.codebase)),
        ))),
        None => StaticClassType::Name(class),
    };
    let target = InvocationTarget::FunctionLike {
        identifier: FunctionLikeIdentifier::Method(class, extension.get_method_name()),
        metadata,
        inferred_return_type: None,
        effective_signature: None,
        method_context: Some(MethodTargetContext {
            invocation_kind: MethodInvocationKind::Extension,
            declaring_method_id: Some(extension),
            class_like_metadata,
            class_type,
            declaring_object_type: None,
        }),
        span,
    };
    let skips_null = is_null_safe && artifacts.get_expression_type(receiver).is_some_and(TUnion::is_nullable);
    let receiver_has_nullsafe_null = artifacts.get_expression_type(receiver).is_some_and(TUnion::has_nullsafe_null);

    analyze_invocation_targets(
        context,
        block_context,
        artifacts,
        template_result,
        vec![target],
        InvocationArgumentsSource::ExtensionReceiver(receiver, arguments),
        span,
        None,
        false,
        false,
        skips_null,
        receiver_has_nullsafe_null,
    )?;

    let Some(arguments) = arguments.filter(|_| context.dialect.is_sharp()) else {
        return Ok(());
    };
    // The arguments a member that changes its receiver stores have their types once the invocation is analyzed.
    if method_call::changes_receiver(context, &extension) {
        method_call::check_stored_in_static(context, artifacts, receiver, arguments);
    }
    // A collection member compares the value of a parameter marked `ComparedByEquality` with each element of a `List`,
    // or each key of a `Map` or a `Set`, as `==` does, spec section 12, once the value's type is known. A value that is
    // no element is refused by its argument check.
    for (parameter, argument) in metadata.parameters.iter().skip(1).zip(&arguments.arguments) {
        // A collection member orders by the keys the function in a parameter marked `ComparedByOrder` returns, as
        // `sortedBy` does, so a key type `<` cannot order is refused.
        if is_marked(parameter, COMPARED_BY_ORDER)
            && !argument.is_unpacked()
            && let Some(TAtomic::Callable(TCallable::Signature(function))) = artifacts
                .get_expression_type(argument.value())
                .filter(|function| function.is_single())
                .map(TUnion::get_single)
            && let Some(key_type) = function.get_return_type()
        {
            let key_type = key_type.clone();
            check_ordered_key(context, &metadata.original_name.as_str_lossy(), argument.value(), &key_type);
        }
        if is_marked(parameter, COMPARED_BY_EQUALITY)
            && let Argument::Positional(_) = argument
            && let Some(value_type) = artifacts.get_expression_type(argument.value())
            && let Some(receiver_type) = artifacts.get_expression_type(receiver)
            && let Some(element_type) = searched_element_type(receiver_type, context.codebase)
            && union_comparator::is_contained_by(
                context.codebase,
                value_type,
                &element_type,
                false,
                false,
                false,
                &mut ComparisonResult::new(),
            )
        {
            let method = metadata.original_name;
            check_element_equality(
                context,
                &method.as_str_lossy(),
                receiver,
                argument.value(),
                &element_type,
                value_type,
                is_null_safe,
            );
        }
    }

    Ok(())
}

/// The type of the elements a search of a value of `receiver_type` compares by `==` with the value of a collection
/// member's parameter marked `ComparedByEquality`, spec section 12: each element of a `List`, or each key of a `Map` or
/// a `Set`. None where no search is checked: a receiver that may be no `List`, `Map` or `Set`, or a `Map` or a `Set`
/// whose key type keys one. A `Map` and a `Set` find a key by its `hash()` first, so a key of another type is never
/// compared, and a search is refused only where a key declaration is, by `is_key_type`.
fn searched_element_type(receiver_type: &TUnion, codebase: &CodebaseMetadata) -> Option<TUnion> {
    if !receiver_type.types.iter().all(|atomic| matches!(atomic, TAtomic::Array(_) | TAtomic::Null)) {
        return None;
    }

    let element_type = receiver_type
        .types
        .iter()
        .filter_map(|atomic| match atomic {
            TAtomic::Array(array @ TArray::List(_)) => Some(get_array_parameters(array, codebase).1),
            TAtomic::Array(array) => Some(get_array_parameters(array, codebase).0),
            _ => None,
        })
        .fold(None, |element_type, other| Some(add_optional_union_type(other, element_type.as_ref(), codebase)))
        .unwrap_or_else(get_never);
    let searches_list =
        receiver_type.types.iter().all(|atomic| matches!(atomic, TAtomic::Array(TArray::List(_)) | TAtomic::Null));

    (searches_list || !is_key_type(&element_type, codebase)).then_some(element_type)
}

/// Whether a call of the collection member `metadata` on a value of `receiver_type` can pass an element to each of
/// its parameters marked `ComparedByEquality`, which a search compares with the receiver's elements by `==`, as
/// [`search_refusal`] decides, spec section 11. A `List<TItem : Object>` has no `contains`, `indexOf` or `remove`,
/// since `==` cannot compare two `TItem`s, and a `List<int>` has all three. A nullable element without `==` hides
/// them too, as `List<Cart?>` does for a `Cart` without `operator ==`, though its `contains(null)` passes, since `==`
/// lifts `null`.
pub(crate) fn can_compare_elements(
    metadata: &FunctionLikeMetadata,
    receiver_type: &TUnion,
    codebase: &CodebaseMetadata,
) -> bool {
    !metadata.parameters.iter().skip(1).any(|parameter| is_marked(parameter, COMPARED_BY_EQUALITY))
        || searched_element_type(receiver_type, codebase)
            .is_none_or(|element_type| search_refusal(&element_type, &element_type, codebase).is_none())
}

/// The prelude attribute that marks a collection member's parameter compared with the elements by `==`.
const COMPARED_BY_EQUALITY: &[u8] = b"Sharp\\Internal\\ComparedByEquality";

/// The prelude attribute that marks a collection member's parameter whose function gives the keys it orders by.
const COMPARED_BY_ORDER: &[u8] = b"Sharp\\Internal\\ComparedByOrder";

/// Whether `parameter` carries the prelude attribute `mark`.
fn is_marked(parameter: &FunctionLikeParameterMetadata, mark: &[u8]) -> bool {
    parameter.attributes.iter().any(|attribute| attribute.name.as_bytes().eq_ignore_ascii_case(mark))
}

/// Applies active method call assertions to narrow the return type.
///
/// When inside a conditional like `if ($obj->isValid())` where `isValid` has method call
/// assertions like `@phpstan-assert-if-true Statement $this->first()`, this function
/// narrows the return type of `first()` from `Statement|null` to `Statement`.
fn apply_method_call_assertions<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    block_context: &BlockContext<'ctx>,
    this_variable: Option<&[u8]>,
    method_name: Option<Word>,
    method_is_stable: bool,
    mut return_type: TUnion,
) -> TUnion
where
    A: Arena,
{
    let Some(this_var) = this_variable else {
        return return_type;
    };

    let Some(method) = method_name else {
        return return_type;
    };

    let method_call_key = concat_word!(this_var, "->", method, "()");

    let mut apply_assertions = |assertions: &AssertionSet| {
        for clause in assertions {
            for assertion in clause {
                return_type = assertion_reconciler::reconcile(
                    context,
                    assertion,
                    Some(&return_type),
                    None,
                    block_context.flags.inside_loop(),
                    None,
                    false,
                    false,
                );
            }
        }
    };

    if method_is_stable && let Some(assertions) = block_context.stable_method_call_assertions.get(&method_call_key) {
        apply_assertions(assertions);
    }

    if let Some(assertions) = block_context.active_method_call_assertions.get(&method_call_key) {
        apply_assertions(assertions);
    }

    return_type
}

fn get_function_like_target<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    function_like: FunctionLikeIdentifier,
    alternative: Option<FunctionLikeIdentifier>,
    span: Span,
    inferred_return_type: Option<Arc<TUnion>>,
) -> Option<InvocationTarget<'ctx>>
where
    A: Arena,
{
    get_function_like_target_inner(context, function_like, alternative, span, inferred_return_type, false)
}

pub(super) fn get_function_like_target_with_skip<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    function_like: FunctionLikeIdentifier,
    alternative: Option<FunctionLikeIdentifier>,
    span: Span,
    inferred_return_type: Option<Arc<TUnion>>,
    skip_error_on_not_found: bool,
) -> Option<InvocationTarget<'ctx>>
where
    A: Arena,
{
    get_function_like_target_inner(
        context,
        function_like,
        alternative,
        span,
        inferred_return_type,
        skip_error_on_not_found,
    )
}

/// Reports a `.sharp` file's call of `function`, which the codebase does not declare, at `span`: the error a misspelled
/// name gets. A `.sharp` file calls a plain PHP function through its namespace, `Pdf.render` or `Foundation.abort`.
pub(crate) fn report_missing_function<A>(
    context: &mut Context<'_, '_, A>,
    function: &FunctionLikeIdentifier,
    span: Span,
) where
    A: Arena,
{
    let title_str = function.title_kind_str();
    let kind_str = function.kind_str();
    let name_str = display_function_like_identifier(context, function);
    let php_name = function.as_string();
    let place = php_name
        .rsplit_once('\\')
        .map(|(namespace, function)| {
            format!(
                ", or declare `function {function}()` in a `.php` file of the namespace `{}`",
                namespace.trim_start_matches('\\').replace('\\', ".")
            )
        })
        .unwrap_or_default();

    context.collector.report_with_code(
        IssueCode::NonExistentFunction,
        Issue::error(format!("{title_str} `{name_str}` does not exist."))
            .with_annotation(
                Annotation::primary(span).with_message(format!("Undefined {kind_str} `{name_str}` called here")),
            )
            .with_note(format!("This often means the {kind_str} is misspelled, or not defined or autoloaded."))
            .with_help(format!("Check the spelling of `{name_str}`{place}.")),
    );
}

fn get_function_like_target_inner<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    function_like: FunctionLikeIdentifier,
    alternative: Option<FunctionLikeIdentifier>,
    span: Span,
    inferred_return_type: Option<Arc<TUnion>>,
    skip_error_on_not_found: bool,
) -> Option<InvocationTarget<'ctx>>
where
    A: Arena,
{
    let mut identifier = function_like;
    let original_class_for_method_context =
        if let FunctionLikeIdentifier::Method(class_name, _) = function_like { Some(class_name) } else { None };

    let metadata = context
        .codebase
        .get_function_like(&identifier)
        .or_else(|| {
            // If this is a method and we can't find it, try looking up the inheritance chain
            if let FunctionLikeIdentifier::Method(class_name, method_name) = identifier {
                if let Some(class_metadata) = context.codebase.get_class_like(class_name.as_bytes()) {
                    // Try to find the method in parent classes
                    if let Some(declaring_method_id) = class_metadata.declaring_method_ids.get(&method_name) {
                        let declaring_class_id = declaring_method_id.get_class_name();
                        context
                            .codebase
                            .get_function_like(&FunctionLikeIdentifier::Method(declaring_class_id, method_name))
                            .inspect(|_| {
                                identifier = FunctionLikeIdentifier::Method(declaring_class_id, method_name);
                            })
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else if let Some(alternative) = alternative {
                context.codebase.get_function_like(&alternative).inspect(|_| {
                    identifier = alternative;
                })
            } else {
                None
            }
        })
        .or_else(|| {
            if let Some(alternative) = alternative {
                context.codebase.get_function_like(&alternative).inspect(|_| {
                    identifier = alternative;
                })
            } else {
                None
            }
        });

    let Some(metadata) = metadata else {
        if !skip_error_on_not_found && context.dialect.is_sharp() {
            report_missing_function(context, &alternative.unwrap_or(function_like), span);
        } else if !skip_error_on_not_found {
            let title_str = function_like.title_kind_str();
            let kind_str = function_like.kind_str();
            let name_str = display_function_like_identifier(context, &function_like);

            let issue = if let Some(alt_id) = alternative {
                let alt_name_str = display_function_like_identifier(context, &alt_id);

                Issue::error(format!(
                    "Could not find definition for {kind_str} `{name_str}` (also tried as `{alt_name_str}` in a broader scope)."
                )).with_annotation(
                    Annotation::primary(span).with_message(format!("Attempted to use {kind_str} `{name_str}` which is undefined")),
                ).with_note(
                    format!("Neither `{name_str}` (e.g., in current namespace) nor `{alt_name_str}` (e.g., global fallback) could be resolved."),
                )
            } else {
                Issue::error(format!("{title_str} `{name_str}` could not be found.")).with_annotation(
                    Annotation::primary(span).with_message(format!("Undefined {kind_str} `{name_str}` called here")),
                )
            };

            context.collector.report_with_code(
                IssueCode::NonExistentFunction,
                issue.with_note("This often means the function/method is misspelled, not imported correctly (e.g., missing `use` statement for namespaced functions), or not defined/autoloaded.")
                    .with_help(format!("Check for typos in `{name_str}`. Verify namespace imports if applicable, and ensure the {kind_str} is defined and accessible."))
            );
        }

        return None;
    };

    if !metadata.is_available_in_version(context.settings.version) {
        let display_name = display_function_like_identifier(context, &identifier);
        match identifier {
            FunctionLikeIdentifier::Function(_) => {
                crate::utils::availability::check_function_availability(context, metadata, &display_name, span);
            }
            FunctionLikeIdentifier::Method(..) => {
                crate::utils::availability::check_method_availability(context, metadata, &display_name, span);
            }
            FunctionLikeIdentifier::Closure(..) => {}
        }
    }

    // If this is a method, we need to create a method context so that static types can be resolved properly
    let method_context = if let Some(original_class_name) = original_class_for_method_context {
        // Look up the class metadata for the class this method is being called on (not where it's declared)
        if let Some(class_like_metadata) = context.codebase.get_class_like(original_class_name.as_bytes()) {
            // Create the method identifier using the looked up identifier (which points to where the method is declared)
            let declaring_method_id = if let FunctionLikeIdentifier::Method(class_name, method_name) = identifier {
                Some(MethodIdentifier::new(class_name, method_name))
            } else {
                None
            };

            Some(MethodTargetContext {
                invocation_kind: MethodInvocationKind::Static,
                declaring_method_id,
                class_like_metadata,
                class_type: StaticClassType::Name(original_class_name),
                declaring_object_type: None,
            })
        } else {
            None
        }
    } else {
        None
    };

    Some(InvocationTarget::FunctionLike {
        identifier,
        metadata,
        inferred_return_type,
        effective_signature: None,
        method_context,
        span,
    })
}

fn inspect_arguments<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    target: &InvocationTarget<'ctx>,
    invocation_arguments: &InvocationArgumentsSource<'_, 'arena>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    analyze_invocation_arguments_source(context, block_context, artifacts, invocation_arguments)?;

    let mut argument_annotations = vec![];
    for (idx, argument) in invocation_arguments.iter_arguments().enumerate() {
        let Some(argument_expression) = argument.value() else {
            continue;
        };

        let argument_span = argument_expression.span();
        let argument_type_string = artifacts
            .get_expression_type(argument_expression)
            .map_or_else(|| "<unknown type>".to_string(), |t| t.get_id().to_string());

        argument_annotations.push(
            Annotation::secondary(argument_span)
                .with_message(format!("Argument #{} type: `{argument_type_string}`", idx + 1,)),
        );
    }

    let mut issue = Issue::help("Type information for arguments of `Mago\\inspect()` call.")
        .with_annotation(Annotation::primary(target.span()).with_message("Type inspection point"));

    for annotation in argument_annotations {
        issue = issue.with_annotation(annotation);
    }

    context.collector.report_with_code(
        IssueCode::TypeInspection,
        issue
            .with_note(
                "The `Mago\\inspect()` function is a static analysis debugging utility; it has no effect at runtime.",
            )
            .with_help("Remember to remove `Mago\\inspect()` calls before deploying to production."),
    );

    Ok(())
}

fn confirm_argument_type<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    target: &InvocationTarget<'ctx>,
    invocation_arguments: &InvocationArgumentsSource<'_, 'arena>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    analyze_invocation_arguments_source(context, block_context, artifacts, invocation_arguments)?;

    let argument_count = invocation_arguments.argument_count();

    if argument_count != 2 {
        context.collector.report_with_code(
            IssueCode::TypeConfirmation,
            Issue::error(format!(
                "`Mago\\confirm()` expects exactly 2 arguments (a value and an expected type string), but {} {} provided.",
                argument_count,
                if argument_count == 1 { "was" } else { "were" }
            ))
            .with_annotation(Annotation::primary(target.span())
                .with_message(if argument_count < 2 {
                    "Too few arguments provided: expected a value and a type string."
                } else {
                    "Too many arguments provided: expected only a value and a type string."
                }))
            .with_note("The `Mago\\confirm()` function is a debugging utility and requires these two specific arguments to function.")
            .with_help("Usage: `Mago\\confirm($value_to_check, \"ExpectedTypeAsString\");`. Remember to remove before committing."),
        );

        return Ok(());
    }

    let Some(value_to_check_argument) = invocation_arguments.get_argument(0) else {
        return Ok(());
    };
    let Some(expected_type_string_argument) = invocation_arguments.get_argument(1) else {
        return Ok(());
    };

    let Some(value_expression) = value_to_check_argument.value() else {
        return Ok(());
    };

    let Some(expected_type_expression) = expected_type_string_argument.value() else {
        return Ok(());
    };

    let Some(actual_argument_type) = artifacts.get_expression_type(value_expression) else {
        context.collector.report_with_code(
            IssueCode::TypeConfirmation,
            Issue::error("Cannot determine the type of the first argument passed to `Mago\\confirm()`.")
                .with_annotation(
                    Annotation::primary(value_expression.span())
                        .with_message("The type of this expression could not be determined here"),
                )
                .with_annotation(Annotation::secondary(target.span()).with_message(
                    "`Mago\\confirm()` expects a value to check and a string representing the expected type.",
                ))
                .with_note("`Mago\\confirm()` needs to know the type of the value to perform the confirmation.")
                .with_note("This debugging utility (`Mago\\confirm()`) should be removed before committing code.")
                .with_help("Ensure the expression is well-formed and its type can be inferred by the analyzer."),
        );

        return Ok(());
    };

    let Some(expected_type_expression_type) = artifacts.get_expression_type(expected_type_expression) else {
        context.collector.report_with_code(
            IssueCode::TypeConfirmation,
            Issue::error(
                "Cannot determine the type of the second argument (the expected type string) passed to `Mago\\confirm()`."
            )
            .with_annotation(
                Annotation::primary(expected_type_expression.span())
                    .with_message("The type of this expression (expected to be a literal string) is unknown"),
            )
            .with_annotation(Annotation::secondary(target.span())
                .with_message("`Mago\\confirm()` expects a value to check and a string representing the expected type."))
            .with_note("`Mago\\confirm()` requires the second argument to be a literal string representing the type.")
            .with_note("This debugging utility (`Mago\\confirm()`) should be removed before committing code.")
            .with_help("Ensure the second argument is a literal string (e.g., `\"int\"`)."),
        );

        return Ok(());
    };

    let Some(expected_type_literal_string) = expected_type_expression_type.get_single_literal_string_value() else {
        context.collector.report_with_code(
            IssueCode::TypeConfirmation,
            Issue::error(format!(
                "Second argument to `Mago\\confirm()` must be a literal string, but found type `{}`.",
                expected_type_expression_type.get_id()
            ))
            .with_annotation(Annotation::primary(expected_type_expression.span())
                .with_message(format!("Expected a literal string here, not type `{}`", expected_type_expression_type.get_id())))
            .with_annotation(Annotation::secondary(target.span())
                .with_message("`Mago\\confirm()` expects a value to check and a string representing the expected type."))
            .with_note("`Mago\\confirm()` uses the second argument as a string representation of the expected type for comparison.")
            .with_note("This debugging utility (`Mago\\confirm()`) should be removed before committing code.")
            .with_help("Provide the expected type as a literal string, e.g., `\"int\"`, `\"literal-string\"`, or `\"Collection<int>\"`."),
        );

        return Ok(());
    };

    let actual_argument_type_string = actual_argument_type.get_id();
    let is_match = expected_type_literal_string.eq_ignore_ascii_case(actual_argument_type_string.as_bytes());
    let expected_type_literal_string = mago_bytes::BytesDisplay(expected_type_literal_string);

    if is_match {
        context.collector.report_with_code(
            IssueCode::TypeConfirmation,
            Issue::help(format!("Type of expression is `{actual_argument_type_string}` as expected.",))
                .with_annotation(
                    Annotation::primary(value_expression.span())
                        .with_message(format!("Confirmed type: `{actual_argument_type_string}`")),
                )
                .with_annotation(
                    Annotation::secondary(expected_type_expression.span())
                        .with_message(format!("Matches expected type: `{expected_type_literal_string}`")),
                )
                .with_note("`Mago\\confirm()` successfully confirmed the type of the expression.")
                .with_help("This debugging utility (`Mago\\confirm()`) should be removed before committing code."),
        );
    } else {
        context.collector.report_with_code(
            IssueCode::TypeConfirmation,
            Issue::error(format!(
                "Type of expression is `{actual_argument_type_string}`, but expected `{expected_type_literal_string}`."
            ))
            .with_annotation(
                Annotation::primary(value_expression.span())
                    .with_message(format!("Actual type: `{actual_argument_type_string}`")),
            )
            .with_annotation(
                Annotation::secondary(expected_type_expression.span())
                    .with_message(format!("Expected type: `{expected_type_literal_string}`")),
            )
            .with_note("`Mago\\confirm()` failed to confirm the type of the expression.")
            .with_help("This debugging utility (`Mago\\confirm()`) should be removed before committing code."),
        );
    }

    Ok(())
}

fn analyze_invocation_arguments_source<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    invocation_arguments: &InvocationArgumentsSource<'_, 'arena>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    match invocation_arguments {
        InvocationArgumentsSource::ArgumentList(argument_list) => {
            argument_list.analyze(context, block_context, artifacts)?;
        }
        InvocationArgumentsSource::PipeInput(pipe) => {
            let was_inside_call = block_context.flags.inside_call();
            let was_inside_general_use = block_context.flags.inside_general_use();
            block_context.flags.set_inside_call(true);
            block_context.flags.set_inside_general_use(true);
            pipe.input.analyze(context, block_context, artifacts)?;
            block_context.flags.set_inside_call(was_inside_call);
            block_context.flags.set_inside_general_use(was_inside_general_use);
        }
        _ => {}
    }

    Ok(())
}
