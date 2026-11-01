use mago_allocator::Arena;
use std::rc::Rc;

use mago_word::Word;
use mago_word::WordMap;
use mago_word::word;

use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::expander::StaticClassType;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_codex::ttype::template::TemplateResult;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::Binding;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::NullSafeMethodCall;
use mago_syntax::cst::TypeArgumentList;
use mago_syntax_core::stack::ensure_sufficient_stack;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::artifacts::CallTarget;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::effects::check::check_foreign_static_write;
use crate::error::AnalysisError;
use crate::expression::access::analyze_rest_of_chain;
use crate::expression::access::analyze_through_value;
use crate::expression::access::refuse_statics_of_no_class;
use crate::expression::array_access::check_change_through_index;
use crate::expression::assignment::PropertyWriteKind;
use crate::expression::assignment::property_assignment;
use crate::expression::assignment::static_property_assignment;
use crate::expression::call::analyze_extension_access;
use crate::expression::call::analyze_extension_call;
use crate::expression::call::analyze_invocation_targets;
use crate::expression::call::function_call::resolve_callable_targets;
use crate::expression::call::record_external_method_call;
use crate::expression::call::record_external_method_call_targets;
use crate::expression::call::report_comparisons;
use crate::expression::call::static_method_call::analyze_static_method_call;
use crate::expression::constant_access::field_storage;
use crate::expression::instantiation::bare_generic_class;
use crate::expression::is_refused_type;
use crate::invocation::Invocation;
use crate::invocation::InvocationArgumentsSource;
use crate::invocation::InvocationTarget;
use crate::invocation::MethodInvocationKind;
use crate::invocation::MethodTargetContext;
use crate::invocation::analyzer::analyze_invocation;
use crate::invocation::analyzer::apply_callable_signature;
use crate::invocation::post_process::post_invocation_process;
use crate::invocation::return_type_fetcher::fetch_declared_invocation_return_type;
use crate::invocation::return_type_fetcher::fetch_function_like_provider_return_type;
use crate::invocation::return_type_fetcher::fetch_invocation_return_type;
use crate::invocation::template_result::populate_template_result_from_invocation;
use crate::invocation::template_result::seed_method_type_arguments;
use crate::plugin::ExpressionHookResult;
use crate::plugin::context::HookContext;
use crate::plugin::hook::StaticCall;
use crate::resolver::method::CalledProperty;
use crate::resolver::method::UndocumentedMethod;
use crate::resolver::method::UnresolvedMethod;
use crate::resolver::method::get_declared_collection;
use crate::resolver::method::report_non_documented_method;
use crate::resolver::method::report_non_existent_method;
use crate::resolver::method::resolve_method_targets;
use crate::resolver::property::check_redundant_nullsafe;
use crate::resolver::static_property::StaticProperty;
use crate::resolver::static_property::StaticPropertyName;
use crate::utils::expression::get_block_expression_id;
use crate::utils::expression::is_this;
use crate::utils::names::display_member;
use crate::visibility::check_method_visibility;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for MethodCall<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        if context.plugin_registry.has_method_call_hooks() {
            let mut hook_context = HookContext::new(context, block_context, artifacts);
            let result = context.plugin_registry.before_method_call(self, &mut hook_context)?;
            for reported in hook_context.take_issues() {
                context.collector.report_with_code(reported.code, reported.issue);
            }

            match result {
                ExpressionHookResult::Continue => {}
                ExpressionHookResult::Skip => {
                    return Ok(());
                }
                ExpressionHookResult::SkipWithType(ty) => {
                    artifacts.set_expression_type(&self.span(), ty);
                    return Ok(());
                }
            }
        }

        analyze_method_call(
            context,
            block_context,
            artifacts,
            self.object,
            &self.method,
            self.type_arguments.as_ref(),
            &self.argument_list,
            false, // is_nullsafe
            self.span(),
        )?;

        if context.plugin_registry.has_method_call_hooks() {
            let mut hook_context = HookContext::after_call(context, block_context, artifacts, self.span());
            context.plugin_registry.after_method_call(self, &mut hook_context)?;
            for reported in hook_context.take_issues() {
                context.collector.report_with_code(reported.code, reported.issue);
            }
        }

        Ok(())
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for NullSafeMethodCall<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        if context.plugin_registry.has_nullsafe_method_call_hooks() {
            let mut hook_context = HookContext::new(context, block_context, artifacts);
            let result = context.plugin_registry.before_nullsafe_method_call(self, &mut hook_context)?;
            for reported in hook_context.take_issues() {
                context.collector.report_with_code(reported.code, reported.issue);
            }

            match result {
                ExpressionHookResult::Continue => {}
                ExpressionHookResult::Skip => {
                    return Ok(());
                }
                ExpressionHookResult::SkipWithType(ty) => {
                    artifacts.set_expression_type(&self.span(), ty);
                    return Ok(());
                }
            }
        }

        analyze_method_call(
            context,
            block_context,
            artifacts,
            self.object,
            &self.method,
            self.type_arguments.as_ref(),
            &self.argument_list,
            true, // is_nullsafe
            self.span(),
        )?;

        // Mago reports a redundant `?->` only on a property. Spec section 14.4 rejects every null check that cannot
        // matter, so a PHP# file checks the method call too.
        if context.dialect.is_sharp()
            && let Some(object_type) = artifacts.get_expression_type(self.object).cloned()
        {
            check_redundant_nullsafe(context, self.question_mark_arrow, self.object, &object_type, false);
        }

        if context.plugin_registry.has_nullsafe_method_call_hooks() {
            let mut hook_context = HookContext::after_call(context, block_context, artifacts, self.span());
            context.plugin_registry.after_nullsafe_method_call(self, &mut hook_context)?;
            for reported in hook_context.take_issues() {
                context.collector.report_with_code(reported.code, reported.issue);
            }
        }

        Ok(())
    }
}

/// Analyzes an implicit method call that doesn't correspond to a direct call expression
/// in the source code.
///
/// This function simulates a method invocation to determine its return type and analyze
/// potential side effects. It is primarily used for operations that trigger "magic methods"
/// at runtime.
///
/// ### Use Cases
///
/// - **String Casting**: Analyzing the `__toString()` method when an object is cast
///   to a string (e.g., `(string) $obj`).
/// - **Cloning**: Analyzing the `__clone()` method when an object is cloned
///   (e.g., `clone $obj`).
///
/// ### Process
///
/// 1. Checks if the method is visible from the current context.
/// 2. Constructs a synthetic `Invocation` to represent the call with no arguments.
/// 3. Resolves template parameters from the target object type.
/// 4. Fetches the method's return type.
/// 5. Performs post-invocation analysis.
///
/// ### Arguments
///
/// - `object_type`: The type of the object on which the method is implicitly called.
/// - `method_identifier`: The identifier for the method (e.g., `__toString`).
/// - `span`: The code span of the expression triggering the call (e.g., the cast expression).
///
/// ### Returns
///
/// A `Result` containing the `TUnion` type of the method's return value. If the method
/// is not visible, it returns the `never` type.
pub fn analyze_implicit_method_call<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    object_type: &TObject,
    object_variable: Option<&[u8]>,
    method_identifier: MethodIdentifier,
    class_like_metadata: &'ctx ClassLikeMetadata,
    method_metadata: &'ctx FunctionLikeMetadata,
    arguments_source: Option<InvocationArgumentsSource<'_, 'arena>>,
    span: Span,
) -> Result<TUnion, AnalysisError>
where
    A: Arena,
{
    if !check_method_visibility(
        context,
        block_context.scope.get_class_like_name(),
        method_identifier.get_class_name().as_bytes(),
        method_identifier.get_method_name().as_bytes(),
        span,
        None,
    ) {
        return Ok(get_never()); // Not visible, return never type.
    }

    let mut template_result = TemplateResult::default();

    let method_target_context = MethodTargetContext {
        invocation_kind: MethodInvocationKind::Instance,
        declaring_method_id: Some(method_identifier),
        class_like_metadata,
        class_type: StaticClassType::Object(object_type.clone()),
        declaring_object_type: None,
    };

    let invocation = Invocation::new(
        InvocationTarget::FunctionLike {
            identifier: FunctionLikeIdentifier::Method(
                method_identifier.get_class_name(),
                method_identifier.get_method_name(),
            ),
            metadata: method_metadata,
            inferred_return_type: None,
            effective_signature: None,
            method_context: Some(method_target_context),
            span,
        },
        arguments_source.unwrap_or(InvocationArgumentsSource::None(span)),
        span,
    );

    populate_template_result_from_invocation(context, &invocation, &mut template_result);

    let result = fetch_invocation_return_type(
        context,
        block_context,
        artifacts,
        &invocation,
        &template_result,
        &WordMap::default(),
    )?;

    post_invocation_process(
        context,
        block_context,
        artifacts,
        &invocation,
        object_variable,
        &template_result,
        &WordMap::default(),
        false,
    )?;

    Ok(result)
}

#[allow(clippy::expect_used)]
fn analyze_method_call<'ctx, 'ast, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    object: &'ast Expression<'arena>,
    selector: &'ast ClassLikeMemberSelector<'arena>,
    type_arguments: Option<&'ast TypeArgumentList<'arena>>,
    argument_list: &'ast ArgumentList<'arena>,
    is_null_safe: bool,
    span: Span,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    if block_context.flags.collect_initializations()
        && let ClassLikeMemberSelector::Identifier(method_ident) = selector
        && is_this_or_self_returning_chain(object, context, block_context)
    {
        let method_name = mago_word::ascii_lowercase_word(method_ident.value);
        block_context.definitely_called_methods.insert(method_name);
        block_context.called_methods.insert(method_name);
    }

    // The receiver's issues wait until the call is known: spec section 12 refuses a change through a `Map` index once,
    // as a change to a copy, so the bare read of that index is not reported beside it.
    let (method_resolution, receiver_issues) = context.record(|context| {
        resolve_method_targets(context, block_context, artifacts, object, selector, is_null_safe, span)
    });
    let mut method_resolution = method_resolution?;

    if let Some(extension) = method_resolution.extension {
        context.collector.extend(receiver_issues);

        return analyze_rest_of_chain(
            context,
            block_context,
            artifacts,
            object,
            is_null_safe,
            |context, block_context, artifacts| {
                analyze_extension_access(
                    context,
                    block_context,
                    artifacts,
                    extension,
                    object,
                    type_arguments,
                    Some(argument_list),
                    is_null_safe,
                    span,
                )
            },
        );
    }

    // PHP# calls a static method through a class value, `type.m()`, as PHP's `$type::m()`, spec section 25.
    if method_resolution.class_value {
        context.collector.extend(receiver_issues);
        if refuse_statics_of_no_class(context, artifacts, object, span) {
            return argument_list.analyze(context, block_context, artifacts);
        }
        let call = StaticCall { class: object, method: selector, argument_list, span };

        return analyze_through_value(
            context,
            block_context,
            artifacts,
            object,
            is_null_safe,
            span,
            |context, block_context, artifacts| {
                analyze_static_method_call(context, block_context, artifacts, call, type_arguments, true)
            },
        );
    }

    // Each kind of collection among the receiver's parts runs the standard library's member of that kind, analyzed on
    // those parts, and its objects run their own methods below, spec section 12, as `held.contains("tea")` on a
    // `List<string>|Set<string>|Box` runs the member or the method of the kind the value holds.
    // A member that changes its receiver runs on each part of the kind alone, so each part fixes its own type arguments
    // and an `add` on a value that may be a `List<int>` or a `List<string>` takes only what both take, as a union of
    // collections allows only what each of them allows.
    let members: Vec<_> = std::mem::take(&mut method_resolution.collection_members)
        .into_iter()
        .flat_map(|(member, parts)| {
            if changes_receiver(context, &member) {
                parts.types.iter().map(|part| (member, TUnion::from_atomic(part.clone()))).collect()
            } else {
                vec![(member, parts)]
            }
        })
        .collect();
    // A call one kind of the receiver lacks the member for is refused once, so its place is not checked too.
    let changes =
        !method_resolution.has_invalid_target && members.iter().any(|(member, _)| changes_receiver(context, member));
    // A receiver whose every part is a collection, or null, runs the members alone, as any extension's.
    if !members.is_empty()
        && method_resolution.resolved_methods.is_empty()
        && method_resolution.unresolved_methods.is_empty()
        && method_resolution.called_properties.is_empty()
        && method_resolution.undocumented_methods.is_empty()
    {
        let copy = if changes { check_changed_collection(context, block_context, artifacts, object) } else { Ok(None) };
        report_receiver_issues(context, receiver_issues, &copy);
        copy?;
        // `?.` skips the null the receiver may hold, as on any extension's receiver, and a null an earlier `?.` of the
        // chain gives makes the call's value null too.
        let nullsafe_null = artifacts
            .get_expression_type(object)
            .filter(|written| written.is_nullable())
            .map(TUnion::has_nullsafe_null);
        let members: Vec<_> = members
            .into_iter()
            .map(|(member, kind)| match nullsafe_null {
                Some(nullsafe_null) => {
                    let mut kind = kind.as_nullable();
                    kind.set_nullsafe_null(nullsafe_null);
                    (member, kind)
                }
                None => (member, kind),
            })
            .collect();

        return analyze_rest_of_chain(
            context,
            block_context,
            artifacts,
            object,
            is_null_safe,
            |context, block_context, artifacts| {
                let runs_one_of = members.len() > 1;
                analyze_collection_members(
                    context,
                    block_context,
                    artifacts,
                    members,
                    runs_one_of,
                    object,
                    type_arguments,
                    argument_list,
                    is_null_safe,
                    span,
                )
                .map(|_| ())
            },
        );
    }
    let collection_copy = changes.then(|| check_changed_collection(context, block_context, artifacts, object));
    let collection_result = analyze_collection_members(
        context,
        block_context,
        artifacts,
        members,
        true,
        object,
        type_arguments,
        argument_list,
        is_null_safe,
        span,
    )?;

    let undocumented_template_result =
        (!method_resolution.undocumented_methods.is_empty()).then(|| method_resolution.template_result.clone());

    let mut invocation_targets = vec![];
    for resolved_method in method_resolution.resolved_methods {
        let metadata = context
            .codebase
            .get_class_like(resolved_method.classname.as_bytes())
            .expect("class-like metadata should exist for resolved method");

        let method_metadata = context
            .codebase
            .get_method_by_id(&resolved_method.method_identifier)
            .expect("method metadata should exist for resolved method");

        let method_display = display_member(
            context,
            resolved_method.method_identifier.get_class_name(),
            resolved_method.method_identifier.get_method_name(),
        );
        crate::utils::availability::check_method_availability(context, method_metadata, &method_display, span);

        let method_target_context = MethodTargetContext {
            invocation_kind: MethodInvocationKind::Instance,
            declaring_method_id: Some(resolved_method.method_identifier),
            class_like_metadata: metadata,
            class_type: resolved_method.static_class_type,
            declaring_object_type: resolved_method.declaring_object,
        };

        invocation_targets.push(InvocationTarget::FunctionLike {
            identifier: FunctionLikeIdentifier::Method(
                resolved_method.method_identifier.get_class_name(),
                resolved_method.method_identifier.get_method_name(),
            ),
            metadata: method_metadata,
            inferred_return_type: None,
            effective_signature: None,
            method_context: Some(method_target_context),
            span,
        });
    }

    // The receiver's collection parts checked its place already, as a place for every part of it.
    let copy = collection_copy.unwrap_or(Ok(None));
    report_receiver_issues(context, receiver_issues, &copy);
    copy?;

    if !method_resolution.unresolved_methods.is_empty() {
        method_resolution.has_invalid_target |= prepare_unresolved_method_targets(
            context,
            artifacts,
            std::mem::take(&mut method_resolution.unresolved_methods),
            InvocationArgumentsSource::ArgumentList(argument_list),
            span,
            MethodInvocationKind::Instance,
            &mut invocation_targets,
        )?;
    }

    method_resolution.has_invalid_target |= add_called_property_targets(
        context,
        block_context,
        artifacts,
        std::mem::take(&mut method_resolution.called_properties),
        &mut method_resolution.template_result,
        span,
        object.span().join(selector.span()),
        &mut invocation_targets,
    );

    let has_resolved_methods = !invocation_targets.is_empty();

    record_external_method_call(context, artifacts, &invocation_targets, span);
    if !method_resolution.undocumented_methods.is_empty() {
        record_external_method_call_targets(
            context,
            artifacts,
            method_resolution.undocumented_methods.iter().map(|method| (method.classname, method.method_name)),
            span,
        );
    }

    let this_variable = get_block_expression_id(object, context, block_context);
    if is_null_safe && let Some(var_id) = this_variable.as_ref() {
        artifacts
            .true_branch_only_assertions
            .entry((span.start.offset, span.end.offset))
            .or_default()
            .entry(*var_id)
            .or_default()
            .push(vec![mago_codex::assertion::Assertion::IsNotType(mago_codex::ttype::atomic::TAtomic::Null)]);
    }

    if let Some(type_arguments) = type_arguments {
        seed_method_type_arguments(
            context,
            block_context,
            artifacts,
            type_arguments,
            &invocation_targets,
            &mut method_resolution.template_result,
        );
    }

    let lacks_sharp_method = method_resolution.lacks_sharp_method;
    if has_resolved_methods || method_resolution.undocumented_methods.is_empty() {
        analyze_rest_of_chain(
            context,
            block_context,
            artifacts,
            object,
            is_null_safe,
            |context, block_context, artifacts| {
                analyze_invocation_targets(
                    context,
                    block_context,
                    artifacts,
                    method_resolution.template_result,
                    invocation_targets,
                    InvocationArgumentsSource::ArgumentList(argument_list),
                    span,
                    this_variable,
                    method_resolution.has_invalid_target,
                    method_resolution.encountered_mixed,
                    is_null_safe && method_resolution.encountered_null,
                    artifacts.get_expression_type(object).is_some_and(|t| t.has_nullsafe_null()),
                )
            },
        )?;
    }

    if !method_resolution.undocumented_methods.is_empty() {
        analyze_rest_of_chain(
            context,
            block_context,
            artifacts,
            object,
            is_null_safe,
            |context, block_context, artifacts| {
                analyze_undocumented_method_return_type(
                    context,
                    block_context,
                    artifacts,
                    method_resolution.undocumented_methods,
                    undocumented_template_result.as_ref().expect("undocumented calls have a template result"),
                    argument_list,
                    span,
                    MethodInvocationKind::Instance,
                    !has_resolved_methods,
                )
            },
        )?;
    }

    // A call on a refused receiver, or on an `Any`, which `mixed-method-access` refuses, is refused once: its result is
    // refused too, so nothing that uses it reports again. So is a call of a method one kind of a collection value
    // lacks, as `contains` on a value that may be a `List` or a `Map`, or that a `string` or a number lacks.
    if let Some(object_type) = artifacts.get_rc_expression_type(object).cloned() {
        if is_refused_type(&object_type) {
            artifacts.set_rc_expression_type(&span, object_type);
        } else if context.dialect.is_sharp()
            && (lacks_sharp_method
                || (!has_resolved_methods
                    && object_type.types.iter().any(TAtomic::is_mixed)
                    && object_type.types.iter().all(|atomic| atomic.is_mixed() || atomic.is_null())))
        {
            artifacts.set_expression_type(&span, TUnion::from_atomic(TAtomic::Error));
        }
    }
    if let Some(collection_result) = collection_result {
        let result = add_optional_union_type(collection_result, artifacts.get_expression_type(&span), context.codebase);
        artifacts.set_expression_type(&span, result);
    }

    Ok(())
}

/// Analyzes the call of each of `members`, a kind's member with the receiver's parts of that kind, or with one of them
/// for a member that changes its receiver, on those parts of `object`, types the call `span` as the join of what each
/// call gives, and gives that join, or None for no member.
/// Where the call runs one of several, a member or an object's own method, each member's type arguments and type are
/// recorded apart, as [`AnalysisArtifacts::get_member_call`] reads them, and the call's own are the last member's.
/// What every member compares is reported together once it is all known, so an argument each kind's member searches
/// or orders by is reported once for the call.
#[allow(clippy::too_many_arguments)]
fn analyze_collection_members<'ctx, 'ast, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    members: Vec<(MethodIdentifier, TUnion)>,
    runs_one_of: bool,
    object: &'ast Expression<'arena>,
    type_arguments: Option<&'ast TypeArgumentList<'arena>>,
    argument_list: &'ast ArgumentList<'arena>,
    is_null_safe: bool,
    span: Span,
) -> Result<Option<TUnion>, AnalysisError>
where
    A: Arena,
{
    let mut joined: Option<TUnion> = None;
    let mut comparisons = Vec::new();
    for (member, kind) in members {
        // A member that isn't generic records no type arguments, so it keeps none of the member before it.
        artifacts.inferred_type_arguments.remove(&(span.start.offset, span.end.offset));
        comparisons.extend(with_receiver_type(artifacts, object, kind, |artifacts| {
            analyze_extension_call(
                context,
                block_context,
                artifacts,
                member,
                object,
                type_arguments,
                Some(argument_list),
                is_null_safe,
                span,
            )
        })?);
        let Some(given) = artifacts.get_expression_type(&span).cloned() else {
            unreachable!("the analysis of a PHP# member's call gives it a type, an error's included");
        };
        if runs_one_of {
            let callee = FunctionLikeIdentifier::Method(member.get_class_name(), member.get_method_name());
            artifacts.record_member_call(span, callee, given.clone(), context.codebase);
        }
        let given = add_optional_union_type(given, joined.as_ref(), context.codebase);
        artifacts.set_expression_type(&span, given.clone());
        joined = Some(given);
    }
    // The receiver has its written type again, every part of it, which the help of a search reads.
    report_comparisons(context, object, artifacts.get_expression_type(object), &comparisons, is_null_safe);

    Ok(joined)
}

/// Runs `analyze` with the receiver `object` typed `receiver_type`, as an extension member's call takes it, and types
/// `object` as before afterwards.
fn with_receiver_type<T>(
    artifacts: &mut AnalysisArtifacts,
    object: &Expression<'_>,
    receiver_type: TUnion,
    analyze: impl FnOnce(&mut AnalysisArtifacts) -> T,
) -> T {
    let Some(written_type) = artifacts.get_rc_expression_type(object).cloned() else {
        unreachable!("the call's receiver is analyzed before its member, so it has its type");
    };
    artifacts.set_expression_type(object, receiver_type);
    let analyzed = analyze(artifacts);
    artifacts.set_rc_expression_type(object, written_type);

    analyzed
}

/// Reports the issues the receiver's analysis found, but the read of the `Map` index at `copy`, which the change through
/// it was refused at once, as a change to a copy, spec section 12.
fn report_receiver_issues<A>(
    context: &mut Context<'_, '_, A>,
    receiver_issues: IssueCollection,
    copy: &Result<Option<Span>, AnalysisError>,
) where
    A: Arena,
{
    let copy_span = copy.as_ref().ok().copied().flatten();
    context.collector.extend(receiver_issues.into_iter().filter(|issue| {
        issue.code.as_deref() != Some(IssueCode::PossiblyUndefinedArrayIndex.as_str())
            || copy_span.is_none_or(|copy| issue.primary_span() != Some(copy))
    }));
}

/// Whether the extension member `member` takes its receiver by `ref`, spec section 26, and so changes it.
pub(super) fn changes_receiver<A>(context: &Context<'_, '_, A>, member: &MethodIdentifier) -> bool
where
    A: Arena,
{
    context
        .codebase
        .get_method_by_id(member)
        .and_then(|metadata| metadata.parameters.first())
        .is_some_and(|receiver| receiver.flags.is_by_reference())
}

/// A method that changes a static collection, as `Cache.all.add(this.store)`, stores its arguments in the static, which
/// holds no `foreign` object, spec section 29.
pub(super) fn check_stored_in_static<A>(
    context: &mut Context<'_, '_, A>,
    artifacts: &AnalysisArtifacts,
    collection: &Expression<'_>,
    argument_list: &ArgumentList<'_>,
) where
    A: Arena,
{
    let Expression::Access(Access::Property(access)) = collection.unparenthesized() else {
        return;
    };
    let Some(StaticProperty {
        class: Expression::ConstantAccess(class),
        name: StaticPropertyName::Identifier(name),
        ..
    }) = StaticProperty::from_property_access(access, context.resolved_names)
    else {
        return;
    };

    // An argument the analysis gave no type stores nothing the check can read.
    let class = word(context.resolved_names.get(&class.name));
    let stored = argument_list.arguments.iter().map(Argument::value);
    for (value, stored) in stored.filter_map(|value| Some((value, artifacts.get_expression_type(value)?))) {
        check_foreign_static_write(context, class, word(name.value), stored, value.span());
    }
}

/// A method that changes a PHP# collection writes it back where it lives, as spec section 12 decides, so the
/// collection is a place the caller can write: a local or a parameter, `field`, which its accessor writes as the
/// storage, a property whose `set` the caller reaches, which the property write check decides as it does for an
/// index write, or a `List` index of a place, which changes that place too. The change may fill the collection past
/// any literal the analyzer saw in it, so the place holds the collections it is declared as from then on, beside
/// whatever else, such as `null`, it held. A collection reached through `?.` changes only where every `?.` receiver on
/// its left is not null, spec section 14.4, so it never holds the `null` those links give. Gives the span of the `Map`
/// index the change is refused through as a copy.
fn check_changed_collection<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    collection: &Expression<'arena>,
) -> Result<Option<Span>, AnalysisError>
where
    A: Arena,
{
    let mut changed_type = artifacts
        .get_expression_type(collection)
        .map_or_else(get_mixed, |read| if read.has_nullsafe_null() { read.to_non_nullable() } else { read.clone() });
    if let Some(declared) = get_declared_collection(context, block_context, artifacts, collection) {
        let others = changed_type.types.iter().filter(|atomic| !matches!(atomic, TAtomic::Array(_))).cloned();
        changed_type = changed_type.clone_with_types(others.chain(declared.types.iter().cloned()).collect());
    }

    match collection.unparenthesized() {
        Expression::ConstantAccess(name)
            if matches!(context.resolved_names.binding(&name.name), Some(Binding::Local(_) | Binding::Field)) =>
        {
            let place = match context.resolved_names.binding(&name.name) {
                Some(Binding::Field) => field_storage(name, context, block_context),
                _ => Some(collection),
            };
            if let Some(place) = place.and_then(|place| get_block_expression_id(place, context, block_context)) {
                block_context.locals.insert(place, Rc::new(changed_type));
            }

            Ok(None)
        }
        Expression::Access(Access::Property(access)) => {
            let written = match StaticProperty::from_property_access(access, context.resolved_names) {
                // Reading `Counter.items` reported its missing type arguments, and the write back would again.
                Some(static_property) if bare_generic_class(context, static_property.class).is_some() => Ok(()),
                // The read of the collection analyzed its class.
                Some(static_property) => static_property_assignment::analyze(
                    context,
                    block_context,
                    artifacts,
                    static_property,
                    &changed_type,
                    None,
                    get_block_expression_id(collection, context, block_context),
                    true,
                ),
                None => property_assignment::analyze(
                    context,
                    block_context,
                    artifacts,
                    access,
                    false,
                    &changed_type,
                    None,
                    PropertyWriteKind::Mutation,
                ),
            };

            written.map(|()| None)
        }
        Expression::Access(Access::NullSafeProperty(access)) => property_assignment::analyze_null_safe(
            context,
            block_context,
            artifacts,
            access,
            &changed_type,
            None,
            PropertyWriteKind::Mutation,
        )
        .map(|()| None),
        Expression::ArrayAccess(access) => {
            if check_change_through_index(access, context, block_context, artifacts) {
                check_changed_collection(context, block_context, artifacts, access.array)
            } else {
                Ok(Some(access.span()))
            }
        }
        _ => {
            context.collector.report_with_code(
                IssueCode::InvalidPassByReference,
                Issue::error("This method changes the collection, which has no place to be written back to.")
                    .with_annotation(
                        Annotation::primary(collection.span()).with_message("This collection is a value, not a place."),
                    )
                    .with_help("Store the collection in a local or a property, then call the method on it."),
            );

            Ok(None)
        }
    }
}

/// Adds the functions the `called_properties` hold to `invocation_targets`, spec section 14: the call at `span` reads
/// each property, written at `callee` as `this.scale`, and runs what it holds. Answers whether one holds no function.
pub(super) fn add_called_property_targets<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    called_properties: Vec<CalledProperty>,
    template_result: &mut TemplateResult,
    span: Span,
    callee: Span,
    invocation_targets: &mut Vec<InvocationTarget<'ctx>>,
) -> bool
where
    A: Arena,
{
    let mut has_invalid_targets = false;
    for property in called_properties {
        artifacts.symbol_references.add_reference_for_property_read(
            &block_context.scope,
            property.declaring_class,
            property.property_name,
        );
        artifacts.record_call_target(
            span,
            CallTarget::Property { class: property.declaring_class, property: property.property_name },
        );

        let (targets, has_invalid_target) =
            resolve_callable_targets(context, &property.property_type, callee, template_result);
        invocation_targets.extend(targets);
        has_invalid_targets |= has_invalid_target;
    }

    has_invalid_targets
}

/// Gives external callable-signature providers an opportunity to establish
/// otherwise unresolved methods before Mago reports them as non-existent.
pub(super) fn prepare_unresolved_method_targets<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    artifacts: &AnalysisArtifacts,
    unresolved_methods: Vec<UnresolvedMethod>,
    arguments: InvocationArgumentsSource<'_, 'arena>,
    span: Span,
    invocation_kind: MethodInvocationKind,
    targets: &mut Vec<InvocationTarget<'ctx>>,
) -> Result<bool, AnalysisError>
where
    A: Arena,
{
    targets.reserve(unresolved_methods.len());
    let mut has_invalid_target = false;
    for unresolved in unresolved_methods {
        let Some(class_like_metadata) = context.codebase.get_class_like(unresolved.classname.as_bytes()) else {
            report_non_existent_method(
                context,
                unresolved.target_span,
                unresolved.selector_span,
                unresolved.classname,
                unresolved.method_name,
            );

            has_invalid_target = true;
            continue;
        };

        let identifier = FunctionLikeIdentifier::Method(class_like_metadata.original_name, unresolved.method_name);
        let target = InvocationTarget::ExternalMethod {
            identifier,
            effective_signature: None,
            method_context: MethodTargetContext {
                invocation_kind,
                declaring_method_id: None,
                class_like_metadata,
                class_type: unresolved.class_type,
                declaring_object_type: None,
            },
            span,
        };

        let mut invocation = Invocation::new(target, arguments, span);
        if apply_callable_signature(context, artifacts, &identifier, &mut invocation) {
            targets.push(invocation.target);
        } else if !class_like_metadata.has_incomplete_hierarchy() {
            report_non_existent_method(
                context,
                unresolved.target_span,
                unresolved.selector_span,
                unresolved.classname,
                unresolved.method_name,
            );

            has_invalid_target = true;
        }
    }

    Ok(has_invalid_target)
}

/// Gives return-type providers the first opportunity to describe an
/// undocumented magic call, then falls back to the magic method's declaration.
#[allow(clippy::expect_used)]
pub(super) fn analyze_undocumented_method_return_type<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    undocumented_methods: Vec<UndocumentedMethod>,
    template_result: &TemplateResult,
    argument_list: &ArgumentList<'arena>,
    span: Span,
    invocation_kind: MethodInvocationKind,
    analyze_arguments: bool,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let mut arguments_analyzed = !analyze_arguments;
    let mut resulting_type: Option<TUnion> = None;
    for undocumented_method in undocumented_methods {
        let magic_call_method = undocumented_method.magic_method;
        let class_like_metadata = context
            .codebase
            .get_class_like(magic_call_method.classname.as_bytes())
            .expect("class-like metadata should exist for resolved magic call method");

        let method_metadata = context
            .codebase
            .get_method_by_id(&magic_call_method.method_identifier)
            .expect("method metadata should exist for resolved magic call method");

        let requested_identifier =
            FunctionLikeIdentifier::Method(undocumented_method.classname, undocumented_method.method_name);
        let magic_identifier = FunctionLikeIdentifier::Method(
            magic_call_method.method_identifier.get_class_name(),
            magic_call_method.method_identifier.get_method_name(),
        );
        if context.dialect.is_sharp() {
            artifacts.record_call_target(
                span,
                CallTarget::MagicMethod {
                    callee: magic_identifier,
                    class: undocumented_method.classname,
                    method: undocumented_method.method_name,
                },
            );
        }

        let target = InvocationTarget::FunctionLike {
            identifier: magic_identifier,
            metadata: method_metadata,
            inferred_return_type: None,
            effective_signature: None,
            method_context: Some(MethodTargetContext {
                invocation_kind,
                declaring_method_id: Some(magic_call_method.method_identifier),
                class_like_metadata,
                class_type: magic_call_method.static_class_type,
                declaring_object_type: magic_call_method.declaring_object,
            }),
            span,
        };

        let mut invocation = Invocation::new(target, InvocationArgumentsSource::ArgumentList(argument_list), span);
        let signature_handled = context.external_analysis_session.is_some()
            && context.plugin_registry.may_have_callable_signature_provider(&requested_identifier)
            && apply_callable_signature(context, artifacts, &requested_identifier, &mut invocation);

        let mut invocation_template_result = template_result.clone();
        let mut parameter_types = WordMap::default();
        if signature_handled && analyze_arguments {
            analyze_invocation(
                context,
                block_context,
                artifacts,
                &mut invocation,
                None,
                None,
                &mut invocation_template_result,
                &mut parameter_types,
            )?;

            arguments_analyzed = true;
        } else if !arguments_analyzed {
            argument_list.analyze(context, block_context, artifacts)?;
            arguments_analyzed = true;
        }

        let return_type = match fetch_function_like_provider_return_type(
            context,
            block_context,
            artifacts,
            &requested_identifier,
            &invocation,
        ) {
            Some(return_type) => return_type,
            None => {
                if !signature_handled {
                    report_non_documented_method(
                        context,
                        undocumented_method.target_span,
                        undocumented_method.selector_span,
                        undocumented_method.classname,
                        undocumented_method.method_name,
                    );
                }

                fetch_declared_invocation_return_type(
                    context,
                    &invocation,
                    &invocation_template_result,
                    &parameter_types,
                )
            }
        };

        resulting_type = Some(add_optional_union_type(return_type, resulting_type.as_ref(), context.codebase));
    }

    let mut resulting_type = resulting_type.unwrap_or_else(get_mixed);
    if let Some(existing_type) = artifacts.get_expression_type(&span) {
        resulting_type = add_optional_union_type(resulting_type, Some(existing_type), context.codebase);
    }

    artifacts.set_expression_type(&span, resulting_type);

    Ok(())
}

/// Checks if an expression ultimately derives from `$this` through a chain of method calls
/// where each method returns `self`/`static`/the same class type.
///
/// This enables tracking method calls in chains like `$this->foo()->bar()->baz()`
/// where `foo()` and `bar()` return `self` or `static`.
fn is_this_or_self_returning_chain<'ctx, 'arena, A>(
    expr: &Expression<'arena>,
    context: &Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
) -> bool
where
    A: Arena,
{
    ensure_sufficient_stack(|| match expr {
        _ if is_this(expr, context.resolved_names) => true,
        Expression::Call(Call::Method(method_call)) => {
            if !is_this_or_self_returning_chain(method_call.object, context, block_context) {
                return false;
            }

            let ClassLikeMemberSelector::Identifier(method_ident) = &method_call.method else {
                return false;
            };

            let Some(class_like) = block_context.scope.get_class_like() else {
                return false;
            };

            let method_name = mago_word::ascii_lowercase_word(method_ident.value);
            method_returns_self_or_static(context, class_like.name, method_name)
        }
        Expression::Call(Call::NullSafeMethod(method_call)) => {
            if !is_this_or_self_returning_chain(method_call.object, context, block_context) {
                return false;
            }

            let ClassLikeMemberSelector::Identifier(method_ident) = &method_call.method else {
                return false;
            };

            let Some(class_like) = block_context.scope.get_class_like() else {
                return false;
            };

            let method_name = mago_word::ascii_lowercase_word(method_ident.value);
            method_returns_self_or_static(context, class_like.name, method_name)
        }
        Expression::Parenthesized(paren) => is_this_or_self_returning_chain(paren.expression, context, block_context),
        _ => false,
    })
}

/// Checks if a method returns `self`, `static`, or the same class type.
fn method_returns_self_or_static<A>(context: &Context<'_, '_, A>, class_name: Word, method_name: Word) -> bool
where
    A: Arena,
{
    let method_id = MethodIdentifier::new(class_name, method_name);
    let Some(method_meta) = context.codebase.get_method_by_id(&method_id) else {
        return false;
    };

    if let Some(return_type_meta) = &method_meta.return_type_declaration_metadata {
        for atomic in return_type_meta.type_union.types.iter() {
            match atomic {
                TAtomic::Object(TObject::Named(named_obj)) if named_obj.is_static => return true,
                TAtomic::Object(TObject::Named(named_obj))
                    if named_obj.name.as_bytes().eq_ignore_ascii_case(class_name.as_bytes()) =>
                {
                    return true;
                }
                _ => {}
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use crate::code::IssueCode;
    use crate::test_analysis;

    test_analysis! {
        name = nullsafe_method_call_on_null,
        code = indoc! {r#"
            <?php

            declare(strict_types=1);

            interface WriteInterface
            {
                /**
                 * @param non-empty-string $data
                 */
                public function write(string $data): void;
            }

            function get_writer(): null|WriteInterface
            {
                return null;
            }

            function write_line(string $message): void
            {
                $message = $message . "\n";

                get_writer()?->write($message);
            }
        "#}
    }

    test_analysis! {
        name = possible_method_call_on_null,
        code = indoc! {r#"
            <?php

            declare(strict_types=1);

            interface WriteInterface
            {
                /**
                 * @param non-empty-string $data
                 */
                public function write(string $data): void;
            }

            function get_writer(): null|WriteInterface
            {
                return null;
            }

            function write_line(string $message): void
            {
                $message = $message . "\n";

                get_writer()->write($message);
            }
        "#},
        issues = [
            IssueCode::PossibleMethodAccessOnNull
        ]
    }

    test_analysis! {
        name = method_call_on_mixed,
        code = indoc! {r#"
            <?php

            declare(strict_types=1);

            function get_mixed(): mixed
            {
                return "Hello, World!";
            }

            function call_method_on_mixed(): void
            {
                $mixed = get_mixed();
                $mixed->someMethod();
            }
        "#},
        issues = [
            IssueCode::MixedAssignment,
            IssueCode::MixedMethodAccess
        ]
    }

    test_analysis! {
        name = method_call_on_undefined_variable,
        code = indoc! {"
            <?php

            declare(strict_types=1);

            function method_call_on_undefined_variable(): void
            {
                $mixed->someMethod();
            }
        "},
        issues = [
            IssueCode::UndefinedVariable,
            IssueCode::MixedMethodAccess
        ]
    }

    test_analysis! {
        name = method_call_on_non_object,
        code = indoc! {"
            <?php

            declare(strict_types=1);

            function call_method_on_non_object(): void
            {
                $non_object = 42;
                $non_object->someMethod();
            }
        "},
        issues = [
            IssueCode::InvalidMethodAccess
        ]
    }

    test_analysis! {
        name = method_call_on_generic_parameter,
        code = indoc! {"
            <?php

            class A
            {
                public function getString(): string
                {
                    return 'Hello, world!';
                }
            }

            class B
            {
                public function getString(): string
                {
                    return 'Hello, world!';
                }
            }

            /**
             * @template T of A|B
             *
             * @param T $object
             */
            function foo(A|B $object): string
            {
                return $object->getString();
            }
        "},
    }

    test_analysis! {
        name = ambiguous_object_method_call,
        code = indoc! {"
            <?php

            declare(strict_types=1);

            function call_ambiguous_method(object $obj): void
            {
                $obj->someMethod();
            }
        "},
        issues = [
            IssueCode::AmbiguousObjectMethodAccess
        ]
    }

    test_analysis! {
        name = template_resolution,
        code = indoc! {"
            <?php

            /**
             * @template-covariant T
             */
            interface TypeInterface
            {
                /**
                 * @param mixed $value
                 * @return T
                 */
                public function assert(mixed $value): mixed;
            }

            /**
             * @param TypeInterface<non-empty-string> $type
             *
             * @return string
             */
            function to_string(mixed $value, TypeInterface $type): string
            {
                return $type->assert($value);
            }
        "},
    }

    test_analysis! {
        name = intersection_read_write_calls,
        code = indoc! {"
            <?php

            interface ReadHandle {
                public function read(): string;
            }

            interface WriteHandle {
                public function write(string $data): void;
            }

            /**
             * @template T as array-key
             * @param iterable<T, ReadHandle&WriteHandle> $handles
             * @return array<T, string>
             */
            function task(iterable $handles): array {
                $result = [];
                foreach ($handles as $index => $handle) {
                    $data = $handle->read();
                    $handle->write($data);

                    $result[$index] = $data;
                }
                return $result;
            }
        "},
    }

    test_analysis! {
        name = intersection_template_resolution,
        code = indoc! {"
            <?php

            interface MockObject
            {
            }

            abstract class TestCase
            {
                /**
                 * @template T of object
                 *
                 * @param class-string<T> $className
                 *
                 * @return MockObject&T
                 */
                protected function createMock(string $className): MockObject
                {
                    exit('Not implemented');
                }

                /**
                 * @template T of object
                 *
                 * @param class-string<T> $className
                 *
                 * @return T&MockObject
                 */
                protected function createMockTwo(string $className): MockObject
                {
                    exit('Not implemented');
                }
            }

            interface ServiceInterface
            {
            }

            class MyTestCase extends TestCase
            {
                private null|(MockObject&ServiceInterface) $service = null;

                public function setup(): void
                {
                    $this->service = $this->createMock(ServiceInterface::class);
                    $this->service = $this->createMockTwo(ServiceInterface::class);
                }
            }
        "},
        issues = [
            IssueCode::WriteOnlyProperty,
        ]
    }

    test_analysis! {
        name = trait_method_access,
        code = indoc! {r#"
            <?php

            trait A {
                private function x(): void {
                    echo "hello 1";
                }

                protected function y(): void {
                    echo "hello 2";
                }
            }

            class B {
                use A;

                public function c(): void {
                    $this->x();
                    $this->y();
                }
            }

            new B()->c();
        "#},
    }

    test_analysis! {
        name = calling_method_on_parent_class,
        code = indoc! {"
            <?php

            /**
             * @template TKey of array-key
             * @template-covariant T
             */
            interface ReadableCollection
            {
                /**
                 * @return list<T>
                 */
                public function getValues(): array;
            }

            /**
             * @template TKey of array-key
             * @template T
             *
             * @template-extends ReadableCollection<TKey, T>
             */
            interface Collection extends ReadableCollection
            {
            }

            class Filing
            {
            }

            class Storage
            {
                /**
                 * @var Collection<string, Filing>
                 */
                private $filings;

                /**
                 * @param Collection<string, Filing> $filings
                 */
                public function __construct(Collection $filings)
                {
                    $this->filings = $filings;
                }

                /**
                 * @return list<Filing>
                 */
                public function getFilings(): array
                {
                    return $this->filings->getValues();
                }
            }
        "},
        issues = [
            // ReadableCollection: TKey not used in interface body
            IssueCode::UnusedTemplateParameter,
        ]
    }

    test_analysis! {
        name = where_constraints,
        code = indoc! {"
            <?php

            interface Stringable
            {
                public function __toString(): string;
            }

            function take_string(string $s): void
            {
                take_string($s);
            }

            function take_int(int $i): void
            {
                take_int($i);
            }

            function take_array(array $arr): void
            {
                take_array($arr);
            }

            /** @param scalar|Stringable $value */
            function take_scalar_or_stringable(mixed $value): void
            {
                take_scalar_or_stringable($value);
            }

            final class Message implements Stringable
            {
                public function __construct(
                    private string $message,
                ) {}

                public function __toString(): string
                {
                    return $this->message;
                }
            }

            /**
             * @template-covariant T
             */
            final class Box
            {
                /**
                 * @param T $value
                 */
                public function __construct(
                    public mixed $value,
                ) {}

                /**
                 * @where T is string|int|float|Stringable
                 */
                public function toString(): string
                {
                    take_scalar_or_stringable($this->value);

                    return (string) $this->value;
                }

                /**
                 * @template Y
                 * @template Z
                 *
                 * @where T is list{Y, Z}
                 *
                 * @return list{Box<Y>, Box<Z>}
                 */
                public function unzip(): array
                {
                    take_array($this->value);

                    [$first, $second] = $this->value;

                    return [
                        new Box($first),
                        new Box($second),
                    ];
                }
            }

            $a = new Box('Hello, World!');
            take_string($a->toString()); // OK

            $b = new Box(42);
            take_string($b->toString()); // OK

            $c = new Box(3.14);
            take_string($c->toString()); // OK

            $d = new Box(new Message('This is a message.'));
            take_string($d->toString()); // OK

            $f = new Box(['foo', 123]);
            [$g, $h] = $f->unzip(); // OK

            take_string($g->value); // OK
            take_int($h->value); // OK
        "},
    }

    test_analysis! {
        name = where_constraints_violation,
        code = indoc! {"
            <?php

            /**
             * @template-covariant T
             */
            final class Box
            {
                /**
                 * @param T $value
                 */
                public function __construct(
                    public mixed $value,
                ) {}

                /**
                 * @where T is string|int|float
                 */
                public function toString(): string
                {
                    return (string) $this->value;
                }
            }

            $a = new Box(['foo', 123]);
            $a->toString(); // violation of @where constraint
        "},
        issues = [
            IssueCode::WhereConstraintViolation
        ]
    }
}
