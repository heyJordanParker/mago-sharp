use std::borrow::Cow;
use std::sync::Arc;

use foldhash::HashMap;

use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::add_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::cast::cast_atomic_to_callable;
use mago_codex::ttype::combiner::CombinerOptions;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator::can_expression_types_be_identical;
use mago_codex::ttype::comparator::union_comparator::is_contained_by;
use mago_codex::ttype::expander::get_signature_of_function_like_identifier;
use mago_codex::ttype::get_backing_key_type;
use mago_codex::ttype::get_iterable_value_parameter;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_codex::ttype::union::TUnion;
use mago_names::ResolvedNames;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Expression;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::invocation::InvocationTarget;
use crate::statement::function_like::closure_parameter_types;
use crate::utils::expression::is_referenceable;
use crate::utils::get_type_diff;

/// Checks if an argument can be passed by reference.
pub(super) fn is_argument_referenceable(
    argument_expression: &Expression,
    argument_type: &TUnion,
    resolved_names: &ResolvedNames<'_>,
) -> bool {
    is_referenceable(argument_expression, false, resolved_names)
        || (is_referenceable(argument_expression, true, resolved_names) && argument_type.by_reference())
}

pub(super) fn is_argument_mutated_by_reference(
    invocation_target: &InvocationTarget<'_>,
    argument_offset: usize,
    argument_type: &TUnion,
    declared_by_reference: bool,
) -> bool {
    if declared_by_reference && argument_offset > 0 && is_array_multisort(invocation_target) {
        return !argument_type.is_int();
    }

    declared_by_reference
}

fn requires_referenceable_argument(invocation_target: &InvocationTarget<'_>, declared_by_reference: bool) -> bool {
    declared_by_reference && !is_array_multisort(invocation_target)
}

pub(super) fn is_array_multisort(invocation_target: &InvocationTarget<'_>) -> bool {
    matches!(
        invocation_target.get_function_like_identifier(),
        Some(FunctionLikeIdentifier::Function(name)) if name.as_bytes().eq_ignore_ascii_case(b"array_multisort")
    )
}

fn is_empty_container_construction(expression: &Expression) -> bool {
    if let Expression::Instantiation(instantiation) = expression
        && let Some(argument_list) = &instantiation.argument_list
        && argument_list.arguments.len() == 1
        && let Some(first_arg) = argument_list.arguments.iter().next()
    {
        let argument_value = first_arg.value();

        match argument_value {
            Expression::Array(array_expr) => array_expr.elements.is_empty(),
            Expression::LegacyArray(array_expr) => array_expr.elements.is_empty(),
            _ => false,
        }
    } else {
        false
    }
}

/// Analyzes an argument expression and stores its inferred type.
pub fn analyze_and_store_argument_type<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    invocation_target: &InvocationTarget<'ctx>,
    argument_expression: &Expression<'arena>,
    argument_offset: usize,
    analyzed_argument_types: &mut HashMap<usize, (TUnion, Span)>,
    referenced_parameter: bool,
    closure_parameter_type: Option<&TUnion>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    if argument_offset != usize::MAX && analyzed_argument_types.contains_key(&argument_offset) {
        return Ok(());
    }

    let inferred_parameter_types =
        closure_parameter_type.map(|closure_parameter_type| closure_parameter_types(context, closure_parameter_type));

    let inferred_parameter_types = std::mem::replace(&mut artifacts.inferred_parameter_types, inferred_parameter_types);

    let was_inside_general_use = block_context.flags.inside_general_use();
    let was_inside_call = block_context.flags.inside_call();
    let was_inside_variable_reference = block_context.flags.inside_variable_reference();

    block_context.flags.set_inside_general_use(true);
    block_context.flags.set_inside_call(true);
    block_context.flags.set_inside_variable_reference(referenced_parameter);

    argument_expression.analyze(context, block_context, artifacts)?;

    block_context.flags.set_inside_general_use(was_inside_general_use);
    block_context.flags.set_inside_call(was_inside_call);
    block_context.flags.set_inside_variable_reference(was_inside_variable_reference);
    artifacts.inferred_parameter_types = inferred_parameter_types;

    let argument_type = artifacts.get_expression_type(argument_expression).cloned().unwrap_or_else(get_mixed);

    if requires_referenceable_argument(invocation_target, referenced_parameter)
        && !is_argument_referenceable(argument_expression, &argument_type, context.resolved_names)
    {
        let target_kind_str = invocation_target.guess_kind();
        let target_name_str = invocation_target.guess_name(context);
        let parameter_label = invocation_target
            .get_effective_parameter_name(argument_offset)
            .map_or_else(|| format!("#{}", argument_offset + 1), |name| format!("`{}`", BytesDisplay(name.as_bytes())));

        context.collector.report_with_code(
            IssueCode::InvalidPassByReference,
            Issue::error(format!(
                "Invalid argument for by-reference parameter {parameter_label} in call to {target_kind_str} `{target_name_str}`.",
            ))
            .with_annotation(
                Annotation::primary(argument_expression.span())
                    .with_message("This expression cannot be passed by reference."),
            )
            .with_note(
                "You can only pass variables, properties, array elements, or the result of another function that itself returns a reference."
            )
            .with_help("To fix this, assign this value to a variable first, and then pass that variable to the function."),
        );
    }

    if argument_offset != usize::MAX {
        analyzed_argument_types.insert(argument_offset, (argument_type, argument_expression.span()));
    }

    Ok(())
}

/// Verifies an argument's type against the expected parameter type.
pub fn verify_argument_type<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    input_type: &TUnion,
    parameter_type: &TUnion,
    argument_offset: usize,
    input_expression: &Expression<'arena>,
    invocation_target: &InvocationTarget<'_>,
) where
    A: Arena,
{
    let target_kind_str = invocation_target.guess_kind();
    let effective_parameter_name = invocation_target.get_effective_parameter_name(argument_offset);
    let argument_label = effective_parameter_name.map_or_else(
        || format!("argument #{}", argument_offset + 1),
        |name| format!("`{}`", BytesDisplay(name.as_bytes())),
    );
    let argument_subject = effective_parameter_name.map_or_else(
        || format!("Argument #{}", argument_offset + 1),
        |name| format!("Argument `{}`", BytesDisplay(name.as_bytes())),
    );

    if input_type.is_never() {
        let target_name_str = invocation_target.guess_name(context);
        context.collector.report_with_code(
            IssueCode::NoValue,
            Issue::error(format!(
                "{argument_subject} passed to {target_kind_str} `{target_name_str}` has type `never`, meaning it cannot produce a value.",
            ))
            .with_annotation(
                Annotation::primary(input_expression.span())
                    .with_message("This argument expression results in type `never`")
            )
            .with_note(
                "The `never` type means no value can reach this point at runtime - this code path is unreachable."
            )
            .with_note(
                "This often occurs in unreachable code, due to impossible conditional logic, or if an expression always exits (e.g., `throw`, `exit()`)."
            )
            .with_help(
                "Review preceding logic to ensure this argument can receive a value, or remove if unreachable."
            ),
        );

        return;
    }

    if !parameter_type.accepts_null() {
        if input_type.is_null() {
            let target_name_str = invocation_target.guess_name(context);
            let parameter_type_str = parameter_type.get_id();
            let call_site = Annotation::secondary(invocation_target.span())
                .with_message(format!("Arguments to this {target_kind_str} are incorrect"));
            context.collector.report_with_code(
                IssueCode::NullArgument,
                Issue::error(format!(
                    "{argument_subject} of {target_kind_str} `{target_name_str}` is `null`, but parameter type `{parameter_type_str}` does not accept it.",
                ))
                .with_annotation(Annotation::primary(input_expression.span()).with_message("This argument is `null`"))
                .with_annotation(call_site)
                .with_help(format!(
                    "Provide a non-null value, or declare the parameter as nullable (e.g., `{parameter_type_str}|null`)."
                )),
            );

            return;
        }

        if input_type.is_nullable() && !input_type.ignore_nullable_issues() {
            let target_name_str = invocation_target.guess_name(context);
            let input_type_str = input_type.get_id();
            let parameter_type_str = parameter_type.get_id();
            let call_site = Annotation::secondary(invocation_target.span())
                .with_message(format!("Arguments to this {target_kind_str} are incorrect"));
            context.collector.report_with_code(
                IssueCode::PossiblyNullArgument,
                Issue::error(format!(
                    "{argument_subject} of {target_kind_str} `{target_name_str}` is possibly `null`, but parameter type `{parameter_type_str}` does not accept it.",
                ))
                .with_annotation(
                    Annotation::primary(input_expression.span())
                        .with_message(format!("This argument of type `{input_type_str}` might be `null`")),
                )
                .with_annotation(call_site)
                .with_help("Add a `null` check before this call to ensure the value is not `null`."),
            );
        }
    }

    if !parameter_type.accepts_false() {
        if input_type.is_false() {
            let target_name_str = invocation_target.guess_name(context);
            let parameter_type_str = parameter_type.get_id();
            let call_site = Annotation::secondary(invocation_target.span())
                .with_message(format!("Arguments to this {target_kind_str} are incorrect"));
            context.collector.report_with_code(
                IssueCode::FalseArgument,
                Issue::error(format!(
                    "{argument_subject} of {target_kind_str} `{target_name_str}` is `false`, but parameter type `{parameter_type_str}` does not accept it.",
                ))
                .with_annotation(Annotation::primary(input_expression.span()).with_message("This argument is `false`"))
                .with_annotation(call_site)
                .with_help(format!(
                    "Provide a different value, or update the parameter type to accept false (e.g., `{parameter_type_str}|false`)."
                )),
            );

            return;
        }

        if input_type.is_falsable() && !input_type.ignore_falsable_issues() {
            let target_name_str = invocation_target.guess_name(context);
            let input_type_str = input_type.get_id();
            let parameter_type_str = parameter_type.get_id();
            let call_site = Annotation::secondary(invocation_target.span())
                .with_message(format!("Arguments to this {target_kind_str} are incorrect"));
            context.collector.report_with_code(
                IssueCode::PossiblyFalseArgument,
                Issue::error(format!(
                    "{argument_subject} of {target_kind_str} `{target_name_str}` is possibly `false`, but parameter type `{parameter_type_str}` does not accept it.",
                ))
                .with_annotation(
                    Annotation::primary(input_expression.span())
                        .with_message(format!("This argument of type `{input_type_str}` might be `false`")),
                )
                .with_annotation(call_site)
                .with_help("Add a check to ensure the value is not `false` before this call."),
            );
        }
    }

    // Spec section 11 checks a value from plain PHP where it enters PHP#, so a plain PHP caller passes the backing
    // values where PHP# takes a `Map` keyed by a backed enum.
    let backing_parameter_type =
        if context.dialect.is_sharp() { None } else { get_backing_array_type(parameter_type, context.codebase) };
    let parameter_type = backing_parameter_type.as_ref().unwrap_or(parameter_type);

    let mut union_comparison_result = ComparisonResult::new();
    let type_match_found =
        is_contained_by(context.codebase, input_type, parameter_type, true, true, false, &mut union_comparison_result);

    if type_match_found {
        return;
    }

    let target_name_str = invocation_target.guess_name(context);
    let input_type_str = input_type.get_id();
    let parameter_type_str = parameter_type.get_id();
    let call_site = Annotation::secondary(invocation_target.span())
        .with_message(format!("Arguments to this {target_kind_str} are incorrect"));

    if input_type.is_mixed() {
        context.collector.report_with_code(
            IssueCode::MixedArgument,
            Issue::error(format!(
                "Invalid argument type for {} of `{}`: expected `{}`, but found `{}`.",
                argument_label,
                target_name_str,
                parameter_type_str,
                input_type_str
            ))
            .with_annotation(
                Annotation::primary(input_expression.span())
                    .with_message(format!("Argument has type `{input_type_str}`")),
            )
            .with_annotation(call_site)
            .with_note(format!(
                "The type `{input_type_str}` is too general and does not match the expected type `{parameter_type_str}`."
            ))
            .with_help("Add specific type hints or assertions to the argument value."),
        );

        return;
    }

    let is_empty_container = input_type.is_empty_array() || is_empty_container_construction(input_expression);
    if union_comparison_result.type_coerced.unwrap_or(false) && !input_type.is_mixed() && !is_empty_container {
        let (issue_kind, annotation_msg, note_msg) = if union_comparison_result
            .type_coerced_from_nested_mixed
            .unwrap_or(false)
        {
            (
                IssueCode::LessSpecificNestedArgumentType,
                format!("Provided type `{input_type_str}` is too general due to nested `mixed`."),
                "The structure contains `mixed`, making it incompatible.".to_string(),
            )
        } else {
            (
                IssueCode::LessSpecificArgument,
                format!("Provided type `{input_type_str}` is too general."),
                format!(
                    "The provided type `{input_type_str}` can be assigned to `{parameter_type_str}`, but is wider (less specific)."
                ),
            )
        };

        let mut issue = Issue::error(format!(
            "Argument type mismatch for {} of `{}`: expected `{}`, but provided type `{}` is less specific.",
            argument_label, target_name_str, parameter_type_str, input_type_str
        ))
        .with_annotation(Annotation::primary(input_expression.span()).with_message(annotation_msg))
        .with_annotation(call_site)
        .with_note(note_msg)
        .with_help(format!(
            "Provide a value that more precisely matches `{parameter_type_str}` or adjust the parameter type."
        ));

        if let Some(type_diff) = get_type_diff(parameter_type, input_type) {
            issue = issue.with_note(type_diff);
        }

        context.collector.report_with_code(issue_kind, issue);
    } else if !union_comparison_result.type_coerced.unwrap_or(false) {
        let parameter_requires_closure = parameter_type
            .types
            .iter()
            .all(|atomic| matches!(atomic, TAtomic::Callable(callable) if callable.is_closure()));

        let input_can_be_closure = input_type
            .types
            .iter()
            .any(|atomic| matches!(atomic, TAtomic::Callable(callable) if callable.is_closure()));

        let types_can_be_identical = (!parameter_requires_closure || input_can_be_closure)
            && can_expression_types_be_identical(context.codebase, input_type, parameter_type, false, false);

        if types_can_be_identical && parameter_type.is_callable() && !parameter_requires_closure {
            let all_inputs_are_resolvable_aliases = input_type.types.iter().all(|atomic| {
                if matches!(atomic, TAtomic::Callable(_)) {
                    false
                } else if let Some(callable) = cast_atomic_to_callable(atomic, context.codebase, None) {
                    match callable.as_ref() {
                        TCallable::Alias(id) => {
                            get_signature_of_function_like_identifier(id, context.codebase).is_some()
                        }
                        TCallable::Signature(_) => false,
                    }
                } else {
                    false
                }
            });

            if all_inputs_are_resolvable_aliases {
                return;
            }
        }

        let kind;
        let mut issue;
        if types_can_be_identical {
            kind = IssueCode::PossiblyInvalidArgument;

            issue = Issue::error(format!(
                "Possible argument type mismatch for {} of `{}`: expected `{}`, but possibly received `{}`.",
                argument_label, target_name_str, parameter_type_str, input_type_str
            ))
            .with_annotation(
                Annotation::primary(input_expression.span())
                    .with_message(format!("This might not be type `{parameter_type_str}`")),
            )
            .with_annotation(call_site)
            .with_note(format!(
                "The provided type `{input_type_str}` overlaps with `{parameter_type_str}` but is not fully contained."
            ))
            .with_help("Ensure the argument always has the expected type using checks or assertions.");
        } else {
            kind = IssueCode::InvalidArgument;
            issue = Issue::error(format!(
                "Invalid argument type for {} of `{}`: expected `{}`, but found `{}`.",
                argument_label, target_name_str, parameter_type_str, input_type_str
            ))
            .with_annotation(
                Annotation::primary(input_expression.span()).with_message(format!("This has type `{input_type_str}`")),
            )
            .with_annotation(call_site)
            .with_note(format!(
                "The provided type `{input_type_str}` is not compatible with the expected type `{parameter_type_str}`."
            ))
            .with_help(format!(
                "Change the argument value to match `{parameter_type_str}`, or update the parameter's type declaration."
            ));
        }

        if let Some(type_diff) = get_type_diff(parameter_type, input_type) {
            issue = issue.with_note(type_diff);
        }

        context.collector.report_with_code(kind, issue);
    }
}

/// Returns `parameter_type` with each `Map` keyed by a backed enum keyed by the backing type instead, or `None` when
/// it has no such `Map`.
fn get_backing_array_type(parameter_type: &TUnion, codebase: &CodebaseMetadata) -> Option<TUnion> {
    let backing_atomic = |atomic: &TAtomic| {
        let TAtomic::Array(TArray::Keyed(keyed_array)) = atomic else {
            return None;
        };
        let (key_type, value_type) = keyed_array.parameters.as_ref()?;
        let Cow::Owned(backing_key_type) = get_backing_key_type(key_type, codebase) else {
            return None;
        };

        let mut backing_array = keyed_array.clone();
        backing_array.parameters = Some((Arc::new(backing_key_type), Arc::clone(value_type)));
        Some(TAtomic::Array(TArray::Keyed(backing_array)))
    };

    if !parameter_type.types.iter().any(|atomic| backing_atomic(atomic).is_some()) {
        return None;
    }

    Some(TUnion::from_vec(
        parameter_type.types.iter().map(|atomic| backing_atomic(atomic).unwrap_or_else(|| atomic.clone())).collect(),
    ))
}

/// Gets the element type when unpacking an argument with the spread operator. In PHP# a type that may not be iterable
/// adds no element type, as `report_non_list_spread` reports it, so a value of only such types gives `None`.
pub fn get_unpacked_argument_type<A>(
    context: &mut Context<'_, '_, A>,
    argument_value_type: &TUnion,
    span: Span,
) -> Option<TUnion>
where
    A: Arena,
{
    let mut potential_element_types = Vec::new();
    let mut reported_an_error = false;

    for atomic_type in argument_value_type.types.as_ref() {
        if let Some(value_parameter) = get_iterable_value_parameter(atomic_type, context.codebase) {
            potential_element_types.push(value_parameter);

            continue;
        }

        match atomic_type {
            TAtomic::Never => {
                potential_element_types.push(get_never());
            }
            _ if context.dialect.is_sharp() => {}
            TAtomic::Mixed(_) => {
                if !reported_an_error {
                    context.collector.report_with_code(
                        IssueCode::MixedArgument,
                        Issue::error(format!(
                            "Cannot unpack argument of type `{}` because it is not guaranteed to be iterable.",
                            atomic_type.get_id()
                        ))
                        .with_annotation(Annotation::primary(span).with_message("Expected an `iterable` for unpacking"))
                        .with_note("Argument unpacking `...` requires an `iterable` (e.g., `array` or `Traversable`).")
                        .with_note("The type `mixed` provides no guarantee of iterability.")
                        .with_help("Ensure the value is an `iterable` using type hints, checks, or assertions."),
                    );
                    reported_an_error = true;
                }

                potential_element_types.push(get_mixed());
            }
            _ => {
                if !reported_an_error {
                    let type_str = atomic_type.get_id();
                    context.collector.report_with_code(
                        IssueCode::InvalidArgument,
                        Issue::error(format!(
                            "Cannot unpack argument of type `{type_str}` because it is not an iterable type."
                        ))
                        .with_annotation(
                            Annotation::primary(span).with_message(format!("Type `{type_str}` is not `iterable`")),
                        )
                        .with_note("Argument unpacking `...` requires an `iterable` (e.g., `array` or `Traversable`).")
                        .with_help("Ensure the value being unpacked is an `iterable`."),
                    );
                    reported_an_error = true;
                }
                potential_element_types.push(get_mixed());
            }
        }
    }

    let element_type = potential_element_types
        .into_iter()
        .reduce(|acc, element_type| add_union_type(acc, &element_type, context.codebase, CombinerOptions::default()));

    if context.dialect.is_sharp() { element_type } else { Some(element_type.unwrap_or_else(get_never)) }
}

/// Reports a spread, in a PHP# file, of a value that may not be a list, such as an `array<string, int>`, a
/// `Traversable` or an `int`, whatever parameters it fills. Spec section 7 spreads an existing list.
pub fn report_non_list_spread<A>(context: &mut Context<'_, '_, A>, argument_value_type: &TUnion, span: Span)
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return;
    }

    let Some(atomic_type) =
        argument_value_type.types.iter().find(|atomic_type| !atomic_type.is_list() && !atomic_type.is_never())
    else {
        return;
    };
    let type_str = atomic_type.get_id();

    context.collector.report_with_code(
        IssueCode::InvalidArgument,
        Issue::error(format!("Cannot spread a value of type `{type_str}`: PHP# spreads only a list."))
            .with_annotation(Annotation::primary(span).with_message(format!("Type `{type_str}` is not a list")))
            .with_note("Spec section 7 spreads an existing list into a call, as in `Money.sum(...prices)`.")
            .with_help("Spread a list, such as a variadic parameter or a `list<int>` from plain PHP."),
    );
}
