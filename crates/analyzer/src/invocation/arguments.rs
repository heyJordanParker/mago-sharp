use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;

use foldhash::HashMap;

use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::add_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::array::list::TList;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::callable::TCallableSignature;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::cast::cast_atomic_to_callable;
use mago_codex::ttype::combiner::CombinerOptions;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator::can_expression_types_be_identical;
use mago_codex::ttype::comparator::union_comparator::is_contained_by;
use mago_codex::ttype::expander::get_signature_of_function_like_identifier;
use mago_codex::ttype::get_crossing_array;
use mago_codex::ttype::get_iterable_value_parameter;
use mago_codex::ttype::get_map_key_types;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_codex::ttype::get_php_array_form;
use mago_codex::ttype::get_php_array_key_type;
use mago_codex::ttype::takes_array;
use mago_codex::ttype::template::TemplateResult;
use mago_codex::ttype::template::inferred_type_replacer;
use mago_codex::ttype::union::TUnion;
use mago_names::ResolvedNames;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Expression;
use mago_syntax::dialect::Dialect;
use mago_word::Word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::artifacts::get_expression_range;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::is_refused_type;
use crate::invocation::InvocationTarget;
use crate::invocation::InvocationTargetParameter;
use crate::invocation::analyzer::adjust_offset_for_variadic;
use crate::resolver::extension::is_collection_member;
use crate::statement::function_like::rejected_nullable_parameter::writes_null;
use crate::utils::expression::is_referenceable;
use crate::utils::get_type_diff;
use crate::utils::int_refused_for_float;
use crate::utils::names::and_list;
use crate::utils::names::display_atomic;
use crate::utils::names::display_function_holder;
use crate::utils::names::display_nullable_type;
use crate::utils::names::display_sharp_parameter_type;
use crate::utils::names::display_sharp_type;
use crate::utils::names::display_type;
use crate::utils::names::display_value_type;
use crate::utils::names::display_variable_name;
use crate::utils::names::sharp_effect_name;
use crate::utils::quoted_source;
use crate::utils::report_int_for_float;
use crate::utils::template::explain_blocked_substitution;
use crate::utils::with_function_fit_message;
use crate::utils::with_set_conversion_help;

/// The part of `union` that may share a value with another type. A PHP# type parameter and its class type, as `TItem`
/// and `Class<TItem>`, are opaque, so with `is_sharp` they share a value with no other type, and a union of them alone
/// has no such part.
fn shareable_part(union: &TUnion, is_sharp: bool) -> Option<Cow<'_, TUnion>> {
    if !is_sharp {
        return Some(Cow::Borrowed(union));
    }

    let types: Vec<TAtomic> = union
        .types
        .iter()
        .filter(|atomic| {
            !atomic.is_generic_parameter()
                && !matches!(atomic, TAtomic::Scalar(TScalar::ClassLikeString(class_string)) if class_string.is_generic())
        })
        .cloned()
        .collect();

    (!types.is_empty()).then(|| Cow::Owned(TUnion::from_vec(types)))
}

/// The type a plain PHP parameter declared `parameter_type` receives for `argument`, an argument of the call at `call`,
/// where PHP# passes it a value of `passed`, spec section 12, or None where it receives the value as it is.
///
/// It is the PHP array the codex's [`get_php_array_form`] gives, as the bridge passes it, where the parameter takes
/// one, as the codex's [`takes_array`] says, or declares no type, which the call's analysis reads as `mixed`. It is
/// recorded in the call's entry of [`AnalysisArtifacts::get_received_argument_types`], for the call's providers, hooks
/// and external requests, and a later read of the same argument in the same check of the call reads it from there.
pub(super) fn record_received_argument_type(
    artifacts: &mut AnalysisArtifacts,
    call: Span,
    argument: &Expression<'_>,
    passed: &TUnion,
    parameter_type: &TUnion,
    codebase: &CodebaseMetadata,
) -> Option<TUnion> {
    let argument = get_expression_range(argument);
    if let Some(recorded) = artifacts.get_received_argument_types(call).and_then(|received| received.get(&argument)) {
        return Some((**recorded).clone());
    }
    let received = takes_array(parameter_type).then(|| get_php_array_form(passed, codebase)).flatten()?;
    artifacts
        .received_argument_types
        .entry(get_expression_range(&call))
        .or_default()
        .insert(argument, Rc::new(received.clone()));

    Some(received)
}

/// Spec section 12 refuses a call where plain PHP's parameter would take the object of a `Map` or a `Set` it takes as
/// a PHP array, rather than failing where it runs.
///
/// `passed` is the PHP# type of the argument. A parameter whose type names `array` refuses a value that can hold a `Map`, at any depth, keyed by a type no PHP
/// array key always holds, as a unit enum, a struct, a `float` or a type parameter, as the codex's
/// [`get_map_key_types`] finds each key, since the bridge passes such a value as the object. A `Map` whose two keys
/// are one PHP array key, as `5` and `"5"` are, fails where it crosses, since only its data shows that. A reference
/// parameter that takes an array refuses a value that may be several collections, since the array plain PHP writes
/// back doesn't say which of them it is. Returns whether the parameter takes the argument, false when it refused the call.
pub(super) fn check_array_crossing<A>(
    context: &mut Context<'_, '_, A>,
    passed: &TUnion,
    parameter: InvocationTargetParameter<'_>,
    parameter_type: &TUnion,
    argument_offset: usize,
    input_expression: &Expression<'_>,
    invocation_target: &InvocationTarget<'_>,
) -> bool
where
    A: Arena,
{
    let refused_key = parameter_type
        .types
        .iter()
        .any(|atomic| matches!(atomic, TAtomic::Array(_)))
        .then(|| {
            get_map_key_types(passed, context.codebase).into_iter().find(|key_type| {
                !get_php_array_key_type(key_type, context.codebase)
                    .is_some_and(|array_key_type| array_key_type.is_always_array_key(true))
            })
        })
        .flatten();
    let refused_reference = parameter.is_by_reference()
        && takes_array(parameter_type)
        && get_php_array_form(passed, context.codebase).is_some()
        && get_crossing_array(passed, context.codebase).is_none();
    if refused_key.is_none() && !refused_reference {
        return true;
    }

    let parameter = parameter.get_name().map_or_else(
        || format!("argument #{}", invocation_target.argument_number(argument_offset)),
        |name| format!("parameter `{}`", display_variable_name(context, name.0.as_bytes())),
    );
    let target_kind = invocation_target.guess_kind(context);
    let target = invocation_target.guess_name(context);
    let input = display_type(context, passed);
    let Some(key_type) = refused_key else {
        context.collector.report_with_code(
            IssueCode::InvalidArgument,
            Issue::error(format!(
                "`{input}` can't be passed to the reference {parameter} of {target_kind} `{target}`, which takes a PHP array: the array plain PHP writes back doesn't say which of these collections it is."
            ))
            .with_annotation(
                Annotation::primary(input_expression.span()).with_message("This value may be one of several collections"),
            )
            .with_annotation(Annotation::secondary(invocation_target.span()).with_message("Plain PHP declares this call"))
            .with_help("Pass a local whose type names one collection, such as one of the types of this union."),
        );

        return false;
    };
    let key = display_type(context, &key_type);
    context.collector.report_with_code(
        IssueCode::InvalidArgument,
        Issue::error(format!(
            "`{input}` can't be passed to the {parameter} of {target_kind} `{target}`, which takes a PHP array: a PHP array's keys are `int` or `string`, and this `Map`'s keys are `{key}`."
        ))
        .with_annotation(Annotation::primary(input_expression.span()).with_message(format!("This `Map` is keyed by `{key}`")))
        .with_annotation(Annotation::secondary(invocation_target.span()).with_message("Plain PHP declares this call"))
        .with_help("Pass a `Map` keyed by `int` or `string`, such as one keyed by a field of each key."),
    );

    false
}

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
    expected_type: Option<&TUnion>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    if argument_offset != usize::MAX && analyzed_argument_types.contains_key(&argument_offset) {
        return Ok(());
    }

    let expected_type = expected_type.map(|expected| (argument_expression.unparenthesized().span(), expected.clone()));
    let expected_type = std::mem::replace(&mut artifacts.expected_type, expected_type);

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
    artifacts.expected_type = expected_type;

    let argument_type = artifacts.get_expression_type(argument_expression).cloned().unwrap_or_else(get_mixed);

    if requires_referenceable_argument(invocation_target, referenced_parameter)
        && !is_argument_referenceable(argument_expression, &argument_type, context.resolved_names)
    {
        let target_kind_str = invocation_target.guess_kind(context);
        let target_name_str = invocation_target.guess_name(context);
        let parameter_label = invocation_target.get_effective_parameter_name(argument_offset).map_or_else(
            || format!("#{}", invocation_target.argument_number(argument_offset)),
            |name| format!("`{}`", display_variable_name(context, name.as_bytes())),
        );

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

/// `parameter_type` as an issue shows it. A type parameter of a PHP# collection method that the call left open, as `R`
/// of `flatMap`, shows as its bound, as TypeScript shows `unknown`, since no code the user writes names it.
fn shown_parameter_type<'parameter, A>(
    context: &Context<'_, '_, A>,
    invocation_target: &InvocationTarget<'_>,
    parameter_type: &'parameter TUnion,
) -> Cow<'parameter, TUnion>
where
    A: Arena,
{
    let (Some(FunctionLikeIdentifier::Method(class, _)), Some(metadata)) =
        (invocation_target.get_function_like_identifier(), invocation_target.get_function_like_metadata())
    else {
        return Cow::Borrowed(parameter_type);
    };
    if metadata.template_types.is_empty() || !is_collection_member(*class, metadata) {
        return Cow::Borrowed(parameter_type);
    }

    let mut bounds = TemplateResult::default();
    for (name, template) in &metadata.template_types {
        bounds.add_lower_bound(*name, template.defining_entity, template.constraint.clone());
    }

    Cow::Owned(inferred_type_replacer::replace(parameter_type, &bounds, context.codebase))
}

/// Verifies an argument's type against the expected parameter type, and returns whether the argument fits it.
pub fn verify_argument_type<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    artifacts: &AnalysisArtifacts,
    input_type: &TUnion,
    parameter_type: &TUnion,
    argument_offset: usize,
    input_expression: &Expression<'arena>,
    invocation_target: &InvocationTarget<'_>,
) -> bool
where
    A: Arena,
{
    let target_kind_str = invocation_target.guess_kind(context);
    let effective_parameter_name = invocation_target.get_effective_parameter_name(argument_offset);
    let argument_label = effective_parameter_name.map_or_else(
        || format!("argument #{}", invocation_target.argument_number(argument_offset)),
        |name| format!("`{}`", display_variable_name(context, name.as_bytes())),
    );
    let argument_subject = effective_parameter_name.map_or_else(
        || format!("Argument #{}", invocation_target.argument_number(argument_offset)),
        |name| format!("Argument `{}`", display_variable_name(context, name.as_bytes())),
    );
    let shown_parameter_type = shown_parameter_type(context, invocation_target, parameter_type);

    if input_type.is_never() {
        let target_name_str = invocation_target.guess_name(context);
        // PHP# code writes `never` only as a return type, so its error says what the code does: the argument's
        // expression always throws or exits, and the call never runs.
        if context.dialect.is_sharp() {
            context.collector.report_with_code(
                IssueCode::NoValue,
                Issue::error(format!(
                    "{argument_subject} passed to {target_kind_str} `{target_name_str}` gets no value: its expression always throws or exits, so the call never runs."
                ))
                .with_annotation(Annotation::primary(input_expression.span()).with_message("Never returns a value."))
                .with_help("Write the expression as a statement of its own, or pass a value."),
            );

            return false;
        }
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

        return false;
    }

    // A refused parameter type, as a type parameter whose bound the checker refused, takes any value, null too, and its
    // one error is the refusal's.
    if is_refused_type(parameter_type) {
        return true;
    }

    let takes_null =
        if context.dialect.is_sharp() { writes_null(parameter_type) } else { parameter_type.accepts_null() };
    if !takes_null {
        if input_type.is_null() {
            let target_name_str = invocation_target.guess_name(context);
            let parameter_type_str = display_type(context, &shown_parameter_type);
            let nullable_parameter_type_str =
                display_nullable_type(context, &shown_parameter_type, format!("{parameter_type_str}|null"));
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
                    "Provide a non-null value, or declare the parameter as nullable (e.g., `{nullable_parameter_type_str}`)."
                )),
            );

            return false;
        }

        if input_type.is_nullable() && !input_type.ignore_nullable_issues() {
            let target_name_str = invocation_target.guess_name(context);
            let input_type_str = display_value_type(context, input_type, parameter_type);
            let parameter_type_str = display_type(context, &shown_parameter_type);
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
            let parameter_type_str = display_type(context, &shown_parameter_type);
            let call_site = Annotation::secondary(invocation_target.span())
                .with_message(format!("Arguments to this {target_kind_str} are incorrect"));
            // PHP# cannot write `int|false`, so its help asks only for another value.
            let help = if context.dialect.is_sharp() {
                "Provide a different value.".to_owned()
            } else {
                format!(
                    "Provide a different value, or update the parameter type to accept false (e.g., `{parameter_type_str}|false`)."
                )
            };
            context.collector.report_with_code(
                IssueCode::FalseArgument,
                Issue::error(format!(
                    "{argument_subject} of {target_kind_str} `{target_name_str}` is `false`, but parameter type `{parameter_type_str}` does not accept it.",
                ))
                .with_annotation(Annotation::primary(input_expression.span()).with_message("This argument is `false`"))
                .with_annotation(call_site)
                .with_help(help),
            );

            return false;
        }

        if input_type.is_falsable() && !input_type.ignore_falsable_issues() {
            let target_name_str = invocation_target.guess_name(context);
            let input_type_str = display_value_type(context, input_type, parameter_type);
            let parameter_type_str = display_type(context, &shown_parameter_type);
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

    // Spec section 12: "Only a literal takes its collection type from where it lands." A PHP# literal argument has the
    // `Set` type its parameter gives it already, and a `List` variable is refused. A PHP# method or lambda makes a `Set`
    // of each parameter it declares `Set` before its body runs, so that parameter takes any list from plain PHP.
    let list_accepting_parameter_type = invocation_target
        .get_function_like_metadata()
        .filter(|function| function.flags.is_sharp())
        .filter(|_| !context.dialect.is_sharp())
        .and_then(|_| invocation_target.get_parameter(adjust_offset_for_variadic(invocation_target, argument_offset)))
        .and_then(|parameter| parameter.get_type())
        .filter(|declared_type| is_declared_set(declared_type))
        .and_then(|_| get_list_accepting_set_type(parameter_type));

    let mut union_comparison_result = ComparisonResult::for_dialect(context.dialect);
    let type_match_found = is_contained_by(
        context.codebase,
        input_type,
        list_accepting_parameter_type.as_ref().unwrap_or(parameter_type),
        true,
        true,
        false,
        &mut union_comparison_result,
    );

    if type_match_found {
        return true;
    }

    if let Some(value) = int_refused_for_float(&union_comparison_result, artifacts, input_expression) {
        report_int_for_float(context, IssueCode::InvalidArgument, value, parameter_type);

        return false;
    }

    // Spec section 29: a function value whose type fits but for its effects is refused for those effects, as a lambda
    // is, with the parameter's type as it is written.
    if context.dialect.is_sharp()
        && let Some(outside) = effects_outside(context.codebase, input_type, parameter_type)
    {
        let parameter = invocation_target.get_parameter(adjust_offset_for_variadic(invocation_target, argument_offset));
        let display = display_for_parameter(parameter);
        let effects: Vec<Word> = outside
            .into_iter()
            .map(|effect| {
                word(sharp_effect_name(context.codebase, &context.imported_names, &context.short_name_counts, effect))
            })
            .collect();
        let has = if effects.len() == 1 { "the effect" } else { "the effects" };
        let holder = display_function_holder(parameter_type, &|union| display(context, union));
        context.collector.report_with_code(
            IssueCode::InvalidArgument,
            Issue::error(format!(
                "`{}` has {has} {}, so {holder} cannot hold it.",
                quoted_source(context, input_expression.span()),
                and_list(&effects)
            ))
            .with_annotation(Annotation::primary(input_expression.span())),
        );

        return false;
    }

    let target_name_str = invocation_target.guess_name(context);
    let input_type_str = display_value_type(context, input_type, parameter_type);
    let parameter_type_str = display_type(context, &shown_parameter_type);
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

        return false;
    }

    let is_coerced = union_comparison_result.type_coerced.unwrap_or(false);
    let (kind, mut issue) = if is_coerced {
        let (issue_kind, annotation_msg, note_msg) = if union_comparison_result
            .type_coerced_from_nested_mixed
            .unwrap_or(false)
        {
            let mixed = display_type(context, &get_mixed());
            (
                IssueCode::LessSpecificNestedArgumentType,
                format!("Provided type `{input_type_str}` is too general due to nested `{mixed}`."),
                format!("The structure contains `{mixed}`, making it incompatible."),
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

        let issue = Issue::error(format!(
            "Argument type mismatch for {} of `{}`: expected `{}`, but provided type `{}` is less specific.",
            argument_label, target_name_str, parameter_type_str, input_type_str
        ))
        .with_annotation(Annotation::primary(input_expression.span()).with_message(annotation_msg))
        .with_annotation(call_site)
        .with_note(note_msg)
        .with_help(format!(
            "Provide a value that more precisely matches `{parameter_type_str}` or adjust the parameter type."
        ));

        (issue_kind, issue)
    } else {
        let parameter_requires_closure = parameter_type
            .types
            .iter()
            .all(|atomic| matches!(atomic, TAtomic::Callable(callable) if callable.is_closure()));

        let input_can_be_closure = input_type
            .types
            .iter()
            .any(|atomic| matches!(atomic, TAtomic::Callable(callable) if callable.is_closure()));

        let is_sharp = context.dialect.is_sharp();
        let types_can_be_identical = (!parameter_requires_closure || input_can_be_closure)
            && shareable_part(input_type, is_sharp).zip(shareable_part(parameter_type, is_sharp)).is_some_and(
                |(input_type, parameter_type)| {
                    can_argument_be_identical(context.codebase, &input_type, &parameter_type, is_sharp)
                },
            );

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
                return true;
            }
        }

        if types_can_be_identical {
            let issue = Issue::error(format!(
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

            (IssueCode::PossiblyInvalidArgument, issue)
        } else {
            let issue = Issue::error(format!(
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

            (IssueCode::InvalidArgument, issue)
        }
    };

    if let Some(type_diff) = get_type_diff(context, parameter_type, input_type) {
        issue = issue.with_note(type_diff);
    }
    let issue = with_set_conversion_help(context, Some(input_expression), (parameter_type, input_type), issue);
    let issue = with_function_fit_message(context, Some(input_expression), (parameter_type, input_type), issue);

    let (kind, issue) =
        explain_blocked_substitution(context, input_type, parameter_type, IssueCode::InvalidArgument, (kind, issue));
    context.collector.report_with_code(kind, issue);

    false
}

/// Whether an argument of `input_type` can hold a value of `parameter_type`. A function value whose type lists an
/// effect a function type does not list never holds a value of that function type, as assigning or returning it
/// reports. In PHP#, `is_sharp`, neither does one with fewer parameters than the function type, spec section 14.1.
fn can_argument_be_identical(
    codebase: &CodebaseMetadata,
    input_type: &TUnion,
    parameter_type: &TUnion,
    is_sharp: bool,
) -> bool {
    input_type.types.iter().any(|input| {
        let parameter_parts: Vec<TAtomic> = parameter_type
            .types
            .iter()
            .filter(|parameter| {
                !matches!(
                    (input, parameter),
                    (TAtomic::Callable(TCallable::Signature(input)), TAtomic::Callable(TCallable::Signature(parameter)))
                        if input.has_effect_outside(parameter)
                            || (is_sharp && parameter.takes_more_parameters_than(input))
                )
            })
            .cloned()
            .collect();

        !parameter_parts.is_empty()
            && can_expression_types_be_identical(
                codebase,
                &TUnion::from_atomic(input.clone()),
                &TUnion::from_vec(parameter_parts),
                false,
                false,
            )
    })
}

/// The effects a function value of `input_type` has that a function type of `parameter_type` does not list, when they
/// are all that keep the value from `parameter_type`, spec section 29.
fn effects_outside(codebase: &CodebaseMetadata, input_type: &TUnion, parameter_type: &TUnion) -> Option<Vec<Word>> {
    let lists = |atomic: &TAtomic| match atomic {
        TAtomic::Callable(TCallable::Signature(signature)) => signature.effects.clone(),
        _ => None,
    };
    let allowed: Vec<Vec<Word>> = parameter_type.types.iter().filter_map(lists).collect();
    let mut outside: Vec<Word> = Vec::new();
    for effect in input_type.types.iter().filter_map(lists).flatten() {
        let listed = allowed
            .iter()
            .all(|effects| effects.iter().any(|known| known.as_bytes().eq_ignore_ascii_case(effect.as_bytes())));
        if !listed && !outside.contains(&effect) {
            outside.push(effect);
        }
    }
    if allowed.is_empty() || outside.is_empty() {
        return None;
    }

    // The value's type with no effect list, as a plain PHP function's, fits every function type's effects.
    let without_effects = TUnion::from_vec(
        input_type
            .types
            .iter()
            .map(|atomic| match atomic {
                TAtomic::Callable(TCallable::Signature(signature)) => {
                    TAtomic::Callable(TCallable::Signature(TCallableSignature { effects: None, ..signature.clone() }))
                }
                atomic => atomic.clone(),
            })
            .collect(),
    );

    is_contained_by(
        codebase,
        &without_effects,
        parameter_type,
        true,
        true,
        false,
        &mut ComparisonResult::for_dialect(Dialect::Sharp),
    )
    .then_some(outside)
}

/// How a message writes the type of `parameter`, spec section 29: a PHP# parameter that writes a function type writes
/// a pure one with `uses none`, since one without `uses` takes on each caller's effects, and a constructor parameter
/// that declares a field writes it as the field does, pure without `uses`. A parameter written as a type parameter, as
/// `List.add`'s `T item`, takes the type its type argument writes.
pub(super) fn display_for_parameter<A>(
    parameter: Option<InvocationTargetParameter<'_>>,
) -> fn(&Context<'_, '_, A>, &TUnion) -> String
where
    A: Arena,
{
    match parameter {
        Some(InvocationTargetParameter::FunctionLike(metadata))
            if !metadata.flags.is_promoted_property()
                && metadata.type_metadata.as_ref().is_some_and(|declared| declared.type_union.has_callable()) =>
        {
            display_sharp_parameter_type
        }
        _ => display_sharp_type,
    }
}

/// Whether a parameter declared `declared_type` is written `Set<T>` or `Set<T>?`, the parameters the lowering converts.
fn is_declared_set(declared_type: &TUnion) -> bool {
    declared_type.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(TArray::Set(_))))
        && declared_type.types.iter().all(|atomic| matches!(atomic, TAtomic::Array(TArray::Set(_)) | TAtomic::Null))
}

/// Returns `parameter_type` with each `Set` also taking a `List` of its elements, or `None` when it has no `Set`.
fn get_list_accepting_set_type(parameter_type: &TUnion) -> Option<TUnion> {
    let lists: Vec<TAtomic> = parameter_type
        .types
        .iter()
        .filter_map(|atomic| match atomic {
            TAtomic::Array(TArray::Set(element_type)) => {
                Some(TAtomic::Array(TArray::List(TList::new(Arc::clone(element_type)))))
            }
            _ => None,
        })
        .collect();

    if lists.is_empty() {
        return None;
    }

    Some(TUnion::from_vec(parameter_type.types.iter().cloned().chain(lists).collect()))
}

/// Gets the element type when unpacking an argument with the spread operator. In PHP# a type that may not be iterable
/// adds no element type, as `report_non_list_spread` reports it, and neither does an empty list, as in
/// `Math.max(0, ...[])`, so a value of only such types gives `None`.
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
        if context.dialect.is_sharp() && atomic_type.is_empty_array() {
            continue;
        }

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
/// `Traversable` or an `int`, whatever parameters it fills. Spec section 7 spreads an existing list, and `[]` is the
/// empty one.
pub fn report_non_list_spread<A>(context: &mut Context<'_, '_, A>, argument_value_type: &TUnion, span: Span)
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return;
    }

    let Some(atomic_type) = argument_value_type
        .types
        .iter()
        .find(|atomic_type| !atomic_type.is_list() && !atomic_type.is_never() && !atomic_type.is_empty_array())
    else {
        return;
    };
    let type_str = display_atomic(context, atomic_type);

    context.collector.report_with_code(
        IssueCode::InvalidArgument,
        Issue::error(format!("Cannot spread a value of type `{type_str}`: PHP# spreads only a list."))
            .with_annotation(Annotation::primary(span).with_message(format!("Type `{type_str}` is not a list")))
            .with_note("PHP# spreads an existing list into a call, as in `Money.sum(...prices)`.")
            .with_help("Spread a list, such as a variadic parameter or a `list<int>` from plain PHP."),
    );
}
