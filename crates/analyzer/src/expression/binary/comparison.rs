use std::rc::Rc;

use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::get_bool;
use mago_codex::ttype::get_false;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_true;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::php_operator_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Parenthesized;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::Variable;
use mago_text_edit::TextEdit;
use mago_word::Word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::artifacts::get_expression_range;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::binary::utils::analyze_operator_call;
use crate::expression::binary::utils::are_definitely_loosely_equal;
use crate::expression::binary::utils::are_definitely_not_identical;
use crate::expression::binary::utils::are_definitely_not_loosely_equal;
use crate::expression::binary::utils::can_take_operands;
use crate::expression::binary::utils::display_operand;
use crate::expression::binary::utils::get_operator_method;
use crate::expression::binary::utils::is_always_greater_than;
use crate::expression::binary::utils::is_always_greater_than_or_equal;
use crate::expression::binary::utils::is_always_identical_to;
use crate::expression::binary::utils::is_always_less_than;
use crate::expression::binary::utils::is_always_less_than_or_equal;
use crate::utils::expression::get_literal_array_key;
use crate::utils::misc::unwrap_expression;
use crate::utils::names::display_bool;
use crate::utils::names::display_type;

/// Analyzes standard comparison operations (e.g., `==`, `===`, `<`, `<=`, `>`, `>=`).
///
/// All these operations result in a boolean. This function:
/// 1. Analyzes both left and right operands.
/// 2. Calls `check_comparison_operand` to validate each operand's type for comparison.
/// 3. Sets the result type of the binary expression to `bool`.
/// 4. Reports warnings for potentially problematic comparisons (e.g., array with int).
/// 5. Reports errors for invalid comparisons (e.g., involving `mixed`).
/// 6. Reports hints for redundant comparisons where the outcome is statically known.
/// 7. Establishes data flow from operands to the expression node.
pub fn analyze_comparison_operation<'ctx, 'arena, A>(
    binary: &Binary<'arena>,
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let was_inside_general_use = block_context.flags.inside_general_use();
    block_context.flags.set_inside_general_use(true);
    binary.lhs.analyze(context, block_context, artifacts)?;
    binary.rhs.analyze(context, block_context, artifacts)?;
    block_context.flags.set_inside_general_use(was_inside_general_use);

    let fallback_type = Rc::new(get_mixed());
    let lhs_type = &Rc::clone(artifacts.get_rc_expression_type(&binary.lhs).unwrap_or(&fallback_type));
    let rhs_type = &Rc::clone(artifacts.get_rc_expression_type(&binary.rhs).unwrap_or(&fallback_type));

    // PHP# `==` and `!=` run as `===` and `!==`, so only an `Any?` operand keeps a check of its own.
    let refusal = if context.dialect.is_sharp() {
        sharp_refusal(&binary.operator, lhs_type, rhs_type, context.codebase)
    } else {
        None
    };
    if let Some(refusal) = refusal {
        report_sharp_refusal(context, binary, refusal, lhs_type, rhs_type);
    }
    let refused = refusal.is_some();

    // A PHP# comparison of instances runs the operator their class declares, which returns the `bool` it decides.
    if !refused
        && context.dialect.is_sharp()
        && analyze_declared_comparison(context, artifacts, binary, lhs_type, rhs_type).is_some()
    {
        artifacts.expression_types.insert(get_expression_range(binary), Rc::new(get_bool()));

        return Ok(());
    }

    let sharp_equality = context.dialect.is_sharp() && binary.operator.is_equality();
    if !refused && !binary.operator.is_identity() {
        if !sharp_equality || lhs_type.is_mixed() {
            check_comparison_operand(context, binary.lhs, lhs_type, rhs_type, "Left", &binary.operator);
        }
        if !sharp_equality || rhs_type.is_mixed() {
            check_comparison_operand(context, binary.rhs, rhs_type, lhs_type, "Right", &binary.operator);
        }
    }

    if context.settings.no_boolean_literal_comparison
        // Only consider equality/inequality operators.
        && binary.operator.is_equality()
        // Skip synthetic comparisons (e.g., those fabricated for match-arm analysis),
        // whose operator carries `Span::zero()` and cannot be safely rewritten.
        && !binary.operator.span().file_id.is_zero()
        // Identify if one side is a boolean literal and the other side's type is `bool`.
        && let Some((variable_expr, literal_expr, literal_value)) =
            if let Some(literal_value) = get_boolean_literal(binary.rhs) {
                if lhs_type.is_bool() { Some((binary.lhs, binary.rhs, literal_value)) } else { None }
            } else if let Some(literal_value) = get_boolean_literal(binary.lhs) {
                if rhs_type.is_bool() { Some((binary.rhs, binary.lhs, literal_value)) } else { None }
            } else {
                None
            }
    {
        // Determine if the simplified expression should be negated.
        let should_negate = if binary.operator.is_negated_equality() && literal_value {
            // `!= true`, `!== true`, or `<> true` becomes `!`
            true
        } else if !binary.operator.is_negated_equality() && !literal_value {
            // `== false` or `=== false` becomes `!`
            true
        } else {
            // `== true`, `=== true`, `!= false`, `!== false`, `<> false` are non-negated
            false
        };

        let issue = Issue::warning("Avoid direct comparison with boolean literals.")
            .with_annotation(Annotation::primary(binary.span()).with_message(format!(
                "This comparison with `{}` is redundant",
                if literal_value { "true" } else { "false" }
            )))
            .with_note("Comparing a value directly to `true` or `false` is verbose and can be simplified.")
            .with_help(if should_negate {
                "This can be simplified to `!<expression>`."
            } else {
                "This can be simplified to just `<expression>`."
            });

        context.collector.propose_with_code(IssueCode::RedundantComparison, issue, |edits| {
            // Determine which part of the expression to remove (the operator and the literal).
            let redundant_range = if variable_expr.start_position() < literal_expr.start_position() {
                // Case: `$variable op $literal`
                binary.operator.span().join(literal_expr.span())
            } else {
                // Case: `$literal op $variable`
                literal_expr.span().join(binary.operator.span())
            };

            edits.push(TextEdit::delete(redundant_range));

            if should_negate {
                edits.push(TextEdit::insert(variable_expr.start_offset(), "!"));
            }
        });
    }

    let mut reported_general_invalid_operand = refused;

    if !refused && !lhs_type.is_mixed() && !rhs_type.is_mixed() {
        let op_str = BytesDisplay(binary.operator.as_bytes());
        let is_relational = binary.operator.is_comparison() && !binary.operator.is_equality();
        let lhs_has_array = lhs_type.has_array() || lhs_type.has_iterable();
        let rhs_has_array = rhs_type.has_array() || rhs_type.has_iterable();
        let lhs_is_only_array = lhs_type.is_array();
        let rhs_is_only_array = rhs_type.is_array();

        if is_relational && lhs_is_only_array && !rhs_has_array && !rhs_type.is_null() {
            let rhs = display_type(context, rhs_type);

            context.collector.report_with_code(
                IssueCode::InvalidOperand,
                Issue::warning(format!("Comparing an `array` with a non-array type `{rhs}` using `{op_str}`."))
                .with_annotation(Annotation::primary(binary.lhs.span()).with_message("This is an array"))
                .with_annotation(Annotation::secondary(binary.rhs.span()).with_message(format!("This has type `{rhs}`")))
                .with_note("PHP's comparison rules for arrays against other types can be non-obvious (e.g., an array is usually considered 'greater' than non-null scalars).")
                .with_help("Ensure both operands are of comparable types or explicitly cast/convert them before comparison if this behavior is not intended."),
            );

            reported_general_invalid_operand = true;
        } else if is_relational && !lhs_has_array && rhs_is_only_array && !lhs_type.is_null() {
            let lhs = display_type(context, lhs_type);

            context.collector.report_with_code(
                IssueCode::InvalidOperand,
                Issue::warning(format!("Comparing a non-array type `{lhs}` with an `array` using `{op_str}`."))
                .with_annotation(Annotation::primary(binary.lhs.span()).with_message(format!("This has type `{lhs}`")))
                .with_annotation(Annotation::secondary(binary.rhs.span()).with_message("This is an array"))
                .with_note("PHP's comparison rules for arrays against other types can be non-obvious.")
                .with_help("Ensure both operands are of comparable types or explicitly cast/convert them before comparison if this behavior is not intended."),
            );

            reported_general_invalid_operand = true;
        } else if is_relational && lhs_has_array && !rhs_has_array && !rhs_type.is_null() {
            let lhs = display_type(context, lhs_type);
            let rhs = display_type(context, rhs_type);

            context.collector.report_with_code(
                IssueCode::PossiblyInvalidOperand,
                Issue::warning(format!(
                    "Left operand may be an `array` when compared with non-array type `{rhs}` using `{op_str}`."
                ))
                .with_annotation(Annotation::primary(binary.lhs.span()).with_message(format!("This may be an array (type `{lhs}`)")))
                .with_annotation(Annotation::secondary(binary.rhs.span()).with_message(format!("This has type `{rhs}`")))
                .with_note("PHP's comparison rules for arrays against other types can be non-obvious (an array is usually considered 'greater' than non-null scalars), so this comparison's result depends on which variant of the union the array side resolves to at runtime.")
                .with_help("Narrow the array side to a non-array type before comparing, or handle the array case separately."),
            );

            reported_general_invalid_operand = true;
        } else if is_relational && !lhs_has_array && rhs_has_array && !lhs_type.is_null() {
            let lhs = display_type(context, lhs_type);
            let rhs = display_type(context, rhs_type);

            context.collector.report_with_code(
                IssueCode::PossiblyInvalidOperand,
                Issue::warning(format!(
                    "Right operand may be an `array` when compared with non-array type `{lhs}` using `{op_str}`."
                ))
                .with_annotation(Annotation::primary(binary.lhs.span()).with_message(format!("This has type `{lhs}`")))
                .with_annotation(Annotation::secondary(binary.rhs.span()).with_message(format!("This may be an array (type `{rhs}`)")))
                .with_note("PHP's comparison rules for arrays against other types can be non-obvious, so this comparison's result depends on which variant of the union the array side resolves to at runtime.")
                .with_help("Narrow the array side to a non-array type before comparing, or handle the array case separately."),
            );

            reported_general_invalid_operand = true;
        }
    }

    let result_type = if reported_general_invalid_operand {
        get_bool()
    } else if context.dialect.is_sharp()
        && let Some((operand, operand_type)) = get_never_null_operand_compared_with_null(binary, lhs_type, rhs_type)
    {
        if !block_context.flags.inside_loop_expressions() {
            report_redundant_null_comparison(context, binary, operand, operand_type);
        }

        if binary.operator.is_negated_equality() { get_true() } else { get_false() }
    } else {
        match binary.operator {
            BinaryOperator::LessThan(_) => {
                if is_always_less_than(lhs_type, rhs_type) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(context, artifacts, binary, "always less than", "`true`");
                    }

                    get_true()
                } else if is_always_greater_than_or_equal(lhs_type, rhs_type) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(context, artifacts, binary, "never less than", "`false`");
                    }

                    get_false()
                } else {
                    get_bool()
                }
            }
            BinaryOperator::LessThanOrEqual(_) => {
                if is_always_less_than_or_equal(lhs_type, rhs_type) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(
                            context,
                            artifacts,
                            binary,
                            "always less than or equal to",
                            "`true`",
                        );
                    }

                    get_true()
                } else if is_always_greater_than(lhs_type, rhs_type) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(
                            context,
                            artifacts,
                            binary,
                            "never less than or equal to",
                            "`false`",
                        );
                    }

                    get_false()
                } else {
                    get_bool()
                }
            }
            BinaryOperator::GreaterThan(_) => {
                if is_always_greater_than(lhs_type, rhs_type) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(context, artifacts, binary, "always greater than", "`true`");
                    }

                    get_true()
                } else if is_always_less_than_or_equal(lhs_type, rhs_type) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(context, artifacts, binary, "never greater than", "`false`");
                    }

                    get_false()
                } else {
                    get_bool()
                }
            }
            BinaryOperator::GreaterThanOrEqual(_) => {
                if is_always_greater_than_or_equal(lhs_type, rhs_type) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(
                            context,
                            artifacts,
                            binary,
                            "always greater than or equal to",
                            "`true`",
                        );
                    }

                    get_true()
                } else if is_always_less_than(lhs_type, rhs_type) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(
                            context,
                            artifacts,
                            binary,
                            "never greater than or equal to",
                            "`false`",
                        );
                    }

                    get_false()
                } else {
                    get_bool()
                }
            }
            BinaryOperator::Equal(_) => {
                let should_be_specific =
                    should_use_specific_equality_inference(block_context, binary.lhs, binary.rhs, false);

                if !should_be_specific {
                    get_bool()
                } else if are_expressions_always_identical(binary.lhs, binary.rhs, lhs_type, rhs_type, artifacts)
                    || are_definitely_loosely_equal(context.settings.version, lhs_type, rhs_type)
                {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(context, artifacts, binary, "always equal to", "`true`");
                    }

                    get_true()
                } else if are_definitely_not_loosely_equal(
                    context.codebase,
                    context.settings.version,
                    lhs_type,
                    rhs_type,
                ) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(context, artifacts, binary, "never equal to", "`false`");
                    }

                    get_false()
                } else {
                    get_bool()
                }
            }
            BinaryOperator::NotEqual(_) | BinaryOperator::AngledNotEqual(_) => {
                let should_be_specific =
                    should_use_specific_equality_inference(block_context, binary.lhs, binary.rhs, false);

                if !should_be_specific {
                    get_bool()
                } else if are_expressions_always_identical(binary.lhs, binary.rhs, lhs_type, rhs_type, artifacts)
                    || are_definitely_loosely_equal(context.settings.version, lhs_type, rhs_type)
                {
                    if !block_context.flags.inside_loop_expressions() {
                        let description = format!("never equal to (always {} for !=)", display_bool(context, false));
                        report_redundant_comparison(context, artifacts, binary, &description, "`false`");
                    }

                    get_false()
                } else if are_definitely_not_loosely_equal(
                    context.codebase,
                    context.settings.version,
                    lhs_type,
                    rhs_type,
                ) {
                    if !block_context.flags.inside_loop_expressions() {
                        let description =
                            format!("always not equal to (always {} for !=)", display_bool(context, true));
                        report_redundant_comparison(context, artifacts, binary, &description, "`true`");
                    }

                    get_true()
                } else {
                    get_bool()
                }
            }
            BinaryOperator::Identical(_) => {
                let should_be_specific =
                    should_use_specific_equality_inference(block_context, binary.lhs, binary.rhs, true);

                if !should_be_specific {
                    get_bool()
                } else if are_expressions_always_identical(binary.lhs, binary.rhs, lhs_type, rhs_type, artifacts) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(context, artifacts, binary, "always identical to", "`true`");
                    }

                    get_true()
                } else if are_definitely_not_identical(context.codebase, lhs_type, rhs_type, false) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(context, artifacts, binary, "never identical to", "`false`");
                    }

                    get_false()
                } else {
                    get_bool()
                }
            }
            BinaryOperator::NotIdentical(_) => {
                let should_be_specific =
                    should_use_specific_equality_inference(block_context, binary.lhs, binary.rhs, true);

                if !should_be_specific {
                    get_bool()
                } else if are_expressions_always_identical(binary.lhs, binary.rhs, lhs_type, rhs_type, artifacts) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(
                            context,
                            artifacts,
                            binary,
                            "never identical to (always false for !==)",
                            "`false`",
                        );
                    }

                    get_false()
                } else if are_definitely_not_identical(context.codebase, lhs_type, rhs_type, false) {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(context, artifacts, binary, "always not identical to", "`true`");
                    }

                    get_true()
                } else {
                    get_bool()
                }
            }
            _ => get_bool(),
        }
    };

    artifacts.expression_types.insert(get_expression_range(binary), Rc::new(result_type));

    Ok(())
}

/// Attempts to extract a boolean literal from an expression, looking through parentheses.
fn get_boolean_literal(expr: &Expression<'_>) -> Option<bool> {
    match expr {
        Expression::Literal(Literal::True(_)) => Some(true),
        Expression::Literal(Literal::False(_)) => Some(false),
        Expression::Parenthesized(Parenthesized { expression, .. }) => get_boolean_literal(expression),
        _ => None,
    }
}

fn are_expressions_always_identical(
    lhs: &Expression<'_>,
    rhs: &Expression<'_>,
    lhs_type: &TUnion,
    rhs_type: &TUnion,
    artifacts: &AnalysisArtifacts,
) -> bool {
    let lhs = unwrap_expression(lhs);
    let rhs = unwrap_expression(rhs);

    let lhs_elements = match lhs {
        Expression::Array(array) => Some(array.elements.as_slice()),
        Expression::LegacyArray(array) => Some(array.elements.as_slice()),
        _ => None,
    };

    let rhs_elements = match rhs {
        Expression::Array(array) => Some(array.elements.as_slice()),
        Expression::LegacyArray(array) => Some(array.elements.as_slice()),
        _ => None,
    };

    let (Some(lhs_elements), Some(rhs_elements)) = (lhs_elements, rhs_elements) else {
        return is_always_identical_to(lhs_type, rhs_type);
    };

    if lhs_elements.len() != rhs_elements.len() {
        return false;
    }

    lhs_elements.iter().zip(rhs_elements).all(|(lhs, rhs)| match (lhs, rhs) {
        (ArrayElement::Value(lhs), ArrayElement::Value(rhs)) => {
            let Some(lhs_type) = artifacts.get_expression_type(lhs.value) else {
                return false;
            };

            let Some(rhs_type) = artifacts.get_expression_type(rhs.value) else {
                return false;
            };

            are_expressions_always_identical(lhs.value, rhs.value, lhs_type, rhs_type, artifacts)
        }
        (ArrayElement::KeyValue(lhs), ArrayElement::KeyValue(rhs)) => {
            let Some(lhs_key) = get_literal_array_key(lhs.key, artifacts) else {
                return false;
            };

            let Some(rhs_key) = get_literal_array_key(rhs.key, artifacts) else {
                return false;
            };

            let Some(lhs_type) = artifacts.get_expression_type(lhs.value) else {
                return false;
            };

            let Some(rhs_type) = artifacts.get_expression_type(rhs.value) else {
                return false;
            };

            lhs_key == rhs_key && are_expressions_always_identical(lhs.value, rhs.value, lhs_type, rhs_type, artifacts)
        }
        _ => false,
    })
}

fn should_use_specific_equality_inference(
    block_context: &BlockContext<'_>,
    lhs: &Expression<'_>,
    rhs: &Expression<'_>,
    identity: bool,
) -> bool {
    if identity {
        !involves_external_reference(lhs, block_context)
            && !involves_external_reference(rhs, block_context)
            && !involves_static_variable(lhs, block_context)
            && !involves_static_variable(rhs, block_context)
    } else {
        !block_context.flags.inside_loop()
            && !involves_external_reference(lhs, block_context)
            && !involves_external_reference(rhs, block_context)
            && !involves_static_variable(lhs, block_context)
            && !involves_static_variable(rhs, block_context)
    }
}

/// Checks if an expression involves a static variable.
fn involves_static_variable(expr: &Expression<'_>, block_context: &BlockContext<'_>) -> bool {
    matches!(unwrap_expression(expr), Expression::Variable(Variable::Direct(var)) if block_context.static_locals.contains(&word(var.name)))
}

/// Checks if an expression involves a variable captured by reference from an outer scope.
fn involves_external_reference(expr: &Expression<'_>, block_context: &BlockContext<'_>) -> bool {
    matches!(unwrap_expression(expr), Expression::Variable(Variable::Direct(var)) if block_context.references_to_external_scope.contains(&word(var.name)))
}

/// Checks a single operand of a comparison operation for problematic types. `other_type` is the other operand's.
fn check_comparison_operand<'ast, 'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    operand: &'ast Expression<'arena>,
    operand_type: &TUnion,
    other_type: &TUnion,
    side: &'static str,
    operator: &'ast BinaryOperator<'arena>,
) where
    A: Arena,
{
    let op_str = BytesDisplay(operator.as_bytes());

    if operand_type.is_null() {
        context.collector.report_with_code(
            IssueCode::NullOperand,
            Issue::error(format!(
                "{side} operand in `{op_str}` comparison is `null`."
            ))
            .with_annotation(Annotation::primary(operand.span()).with_message("This is `null`"))
            .with_note(format!("Comparing `null` with `{op_str}` can lead to unexpected results due to PHP's type coercion rules (e.g., `null == 0` is true)."))
            .with_help("Ensure this operand is non-null and has a comparable type. Explicitly check for `null` if it's an expected state."),
        );
    } else if operand_type.can_be_null() && !operand_type.is_mixed() {
        let operand_type_str = display_type(context, operand_type);

        context.collector.report_with_code(
            IssueCode::PossiblyNullOperand,
            Issue::warning(format!(
                "{side} operand in `{op_str}` comparison might be `null` (type `{operand_type_str}`)."
            ))
            .with_annotation(Annotation::primary(operand.span()).with_message("This might be `null`"))
            .with_note(format!("If this operand is `null` at runtime, PHP's specific comparison rules for `null` with `{op_str}` will apply."))
            .with_help("Ensure this operand is non-null or that comparison with `null` is intended and handled safely."),
        );
    } else if operand_type.is_mixed()
        && !(context.dialect.is_sharp() && operator.is_equality() && compares_by_value(other_type, context.codebase))
    {
        context.collector.report_with_code(
            IssueCode::MixedOperand,
            Issue::error(format!("{side} operand in `{op_str}` comparison has `mixed` type."))
                .with_annotation(Annotation::primary(operand.span()).with_message("This has type `mixed`"))
                .with_note(format!(
                    "The result of comparing `mixed` types with `{op_str}` is unpredictable and can hide bugs."
                ))
                .with_help("Ensure this operand has a known, comparable type before using this comparison operator."),
        );
    } else if operand_type.is_false() {
        context.collector.report_with_code(
            IssueCode::FalseOperand,
            Issue::error(format!(
               "{side} operand in `{op_str}` comparison is `false`."
            ))
            .with_annotation(Annotation::primary(operand.span()).with_message("This is `false`"))
            .with_note(format!("PHP compares `false` with other types according to specific rules (e.g., `false == 0` is true using `{op_str}`). This can hide bugs."))
            .with_help("Ensure this operand is not `false` or explicitly handle the `false` case if it represents a distinct state (e.g., an error from a function)."),
        );
    } else if operand_type.is_falsable() && !operand_type.ignore_falsable_issues() {
        let operand_type_str = display_type(context, operand_type);

        context.collector.report_with_code(
            IssueCode::PossiblyFalseOperand,
            Issue::warning(format!(
                "{side} operand in `{op_str}` comparison might be `false` (type `{operand_type_str}`)."
            ))
            .with_annotation(Annotation::primary(operand.span()).with_message("This might be `false`"))
            .with_note(format!("If this operand is `false` at runtime, PHP's specific comparison rules for `false` with `{op_str}` will apply."))
            .with_help("Ensure this operand is non-false or that comparison with `false` is intended and handled safely."),
        );
    }
}

/// Whether a PHP# `Any?` compares with a value of `other_type` by value, as spec section 19 decides for a string, a
/// number and an enum, and for `null`, which equals only `null`. Any other value must be checked with `is`, `as` or
/// `match` first.
fn compares_by_value(other_type: &TUnion, codebase: &CodebaseMetadata) -> bool {
    comparands(other_type, codebase).iter().all(|comparand| {
        matches!(comparand, Comparand::String | Comparand::Int | Comparand::Float | Comparand::Enum(_))
    })
}

/// What spec section 19 compares a PHP# value as. Two values that share no comparand are never equal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Comparand {
    String,
    Int,
    Float,
    Bool,
    Enum(Word),
    Instance,
    Collection,
    Any,
}

impl Comparand {
    /// Whether a value of this comparand has no order: only numbers, strings and classes that declare `operator <=>`
    /// have one, so `<`, `<=`, `>`, `>=` and `<=>` refuse a `bool`, an enum case and a collection.
    const fn is_unordered(self) -> bool {
        matches!(self, Comparand::Bool | Comparand::Enum(_) | Comparand::Collection)
    }
}

/// The comparands of the values of `union`, leaving out `null`, which `==` lifts.
fn comparands(union: &TUnion, codebase: &CodebaseMetadata) -> Vec<Comparand> {
    let mut found = Vec::new();
    for atomic in union.types.iter() {
        match atomic {
            TAtomic::Scalar(TScalar::String(_) | TScalar::ClassLikeString(_)) => found.push(Comparand::String),
            TAtomic::Scalar(TScalar::Integer(_)) => found.push(Comparand::Int),
            TAtomic::Scalar(TScalar::Float(_)) => found.push(Comparand::Float),
            TAtomic::Scalar(TScalar::Bool(_)) => found.push(Comparand::Bool),
            TAtomic::Scalar(TScalar::ArrayKey) => found.extend([Comparand::Int, Comparand::String]),
            TAtomic::Scalar(TScalar::Numeric) => found.extend([Comparand::Int, Comparand::Float, Comparand::String]),
            TAtomic::Scalar(TScalar::Generic) => {
                found.extend([Comparand::String, Comparand::Int, Comparand::Float, Comparand::Bool]);
            }
            TAtomic::Mixed(_) => found.push(Comparand::Any),
            TAtomic::Object(object) => {
                found.push(match object.get_name().and_then(|name| codebase.get_enum(name.as_bytes())) {
                    Some(r#enum) => Comparand::Enum(r#enum.name),
                    None => Comparand::Instance,
                })
            }
            TAtomic::Callable(_) => found.push(Comparand::Instance),
            TAtomic::Array(_) | TAtomic::Iterable(_) => found.push(Comparand::Collection),
            TAtomic::GenericParameter(parameter) => found.extend(comparands(&parameter.constraint, codebase)),
            _ => {}
        }
    }

    found
}

/// Why spec section 19 gives a PHP# comparison no meaning.
#[derive(Clone, Copy)]
pub(crate) enum Refusal {
    /// `===` or `!==` on a value that is no class instance.
    Identity,
    /// `==` or `!=` on a `List`, `Map` or `Set`.
    Collection,
    /// `==` or `!=` on an instance of a class that declares no `operator ==`, or `<=>` or an ordering on one whose class
    /// declares no `operator <=>`.
    Instance,
    /// `<=>` or an ordering with a side that may be `null`, an instance or any other value, which only `==` and `!=`
    /// lift.
    NullableOperand,
    /// `<=>` or an ordering with a side that may be a `bool`, an enum case or a collection, which have no order.
    UnorderedOperand,
    /// `==` or `!=` on two values of types that never match, a string ordered against another type, or an instance
    /// compared with a value its class's operator never takes, as `money < 5`.
    DifferentTypes,
}

/// Whether spec section 19 compares a value of `lhs_type` with one of `rhs_type` as two floats: both are numbers, `null`
/// aside, and one side may hold an `int` where a side may hold a `float`. Numbers compare by value, so `1 == 1.0`.
pub(crate) fn mixes_numbers(lhs_type: &TUnion, rhs_type: &TUnion, codebase: &CodebaseMetadata) -> bool {
    let (lhs, rhs) = (comparands(lhs_type, codebase), comparands(rhs_type, codebase));
    let both = || lhs.iter().chain(&rhs);

    !lhs.is_empty()
        && !rhs.is_empty()
        && both().all(|comparand| matches!(comparand, Comparand::Int | Comparand::Float))
        && both().any(|comparand| *comparand == Comparand::Int)
        && both().any(|comparand| *comparand == Comparand::Float)
}

/// Why a PHP# file may not compare a value of `lhs_type` with one of `rhs_type` by `operator`, or `None` when it may:
/// `==` or `!=` on two values spec section 19 cannot compare strictly, `===` or `!==` on a value that is no class
/// instance, an ordering with a side that may be a `bool`, an enum case or a collection, an ordering of a string against
/// any other type, an ordering with a side that may be `null`, and a comparison of instances whose class declares no
/// operator for it, or whose operator never takes the other side, are refused.
pub(crate) fn sharp_refusal(
    operator: &BinaryOperator<'_>,
    lhs_type: &TUnion,
    rhs_type: &TUnion,
    codebase: &CodebaseMetadata,
) -> Option<Refusal> {
    // A comparison no source writes, as the `===` a value pattern runs as, has no operator token to refuse.
    if operator.span().length() == 0 {
        return None;
    }

    let (lhs, rhs) = (comparands(lhs_type, codebase), comparands(rhs_type, codebase));
    let has = |comparand: Comparand| lhs.contains(&comparand) || rhs.contains(&comparand);

    match operator {
        BinaryOperator::Identical(_) | BinaryOperator::NotIdentical(_) => {
            let instances = !(lhs.is_empty() && rhs.is_empty())
                && lhs.iter().chain(&rhs).all(|comparand| *comparand == Comparand::Instance);

            (!instances).then_some(Refusal::Identity)
        }
        // A side that is only `null` makes a null test, which `==` lifts.
        BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_) if lhs.is_empty() || rhs.is_empty() => None,
        BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_) => {
            if has(Comparand::Collection) {
                Some(Refusal::Collection)
            } else if has(Comparand::Instance) {
                match get_comparison_method(operator, lhs_type, rhs_type, codebase) {
                    None => Some(Refusal::Instance),
                    Some(method) => {
                        let operands = [&lhs_type.to_non_nullable(), &rhs_type.to_non_nullable()];

                        (!can_take_operands(codebase, method, &operands)).then_some(Refusal::DifferentTypes)
                    }
                }
            } else if has(Comparand::Any)
                || lhs.iter().any(|comparand| rhs.contains(comparand))
                || mixes_numbers(lhs_type, rhs_type, codebase)
            {
                None
            } else {
                Some(Refusal::DifferentTypes)
            }
        }
        BinaryOperator::LessThan(_)
        | BinaryOperator::LessThanOrEqual(_)
        | BinaryOperator::GreaterThan(_)
        | BinaryOperator::GreaterThanOrEqual(_)
        | BinaryOperator::Spaceship(_) => ordering_refusal(lhs_type, rhs_type, codebase),
        _ => None,
    }
}

/// Why PHP# cannot order a value of `lhs_type` against one of `rhs_type`, as `<`, `<=`, `>`, `>=`, `<=>` and `sortedBy`
/// order, or `None` when it can. PHP# orders an instance by the `operator <=>` its class declares, and a string by its
/// bytes, so only against a string. A `bool`, an enum case and a collection have no order, even against an `Any?`. Only
/// `==` and `!=` take `null`, so a side that may be `null` is refused whatever its type. An `Any?` keeps its own report.
fn ordering_refusal(lhs_type: &TUnion, rhs_type: &TUnion, codebase: &CodebaseMetadata) -> Option<Refusal> {
    let (lhs, rhs) = (comparands(lhs_type, codebase), comparands(rhs_type, codebase));
    let has = |comparand: Comparand| lhs.contains(&comparand) || rhs.contains(&comparand);

    if lhs.iter().chain(&rhs).any(|comparand| comparand.is_unordered()) {
        return Some(Refusal::UnorderedOperand);
    }

    let different_types = if has(Comparand::Instance) {
        let ordering = BinaryOperator::Spaceship(Span::zero());
        let Some(method) = get_comparison_method(&ordering, lhs_type, rhs_type, codebase) else {
            return Some(Refusal::Instance);
        };

        !can_take_operands(codebase, method, &[&lhs_type.to_non_nullable(), &rhs_type.to_non_nullable()])
    } else {
        has(Comparand::String)
            && !lhs.iter().chain(&rhs).all(|comparand| matches!(comparand, Comparand::String | Comparand::Any))
    };

    if different_types {
        Some(Refusal::DifferentTypes)
    } else if !has(Comparand::Any) && (lhs_type.can_be_null() || rhs_type.can_be_null()) {
        Some(Refusal::NullableOperand)
    } else {
        None
    }
}

/// The static method a PHP# comparison of instances runs: `op_Equality` for `==` and `!=`, and `op_Comparison` for
/// `<=>` and the orderings, which the class of an instance side declares or inherits. `== null` tests for `null`, and
/// runs none.
pub(crate) fn get_comparison_method<'ctx>(
    operator: &BinaryOperator<'_>,
    lhs_type: &TUnion,
    rhs_type: &TUnion,
    codebase: &'ctx CodebaseMetadata,
) -> Option<(MethodIdentifier, &'ctx FunctionLikeMetadata)> {
    let declared = match operator {
        BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_) if lhs_type.is_null() || rhs_type.is_null() => {
            return None;
        }
        BinaryOperator::Equal(span) | BinaryOperator::NotEqual(span) => BinaryOperator::Equal(*span),
        BinaryOperator::LessThan(span)
        | BinaryOperator::LessThanOrEqual(span)
        | BinaryOperator::GreaterThan(span)
        | BinaryOperator::GreaterThanOrEqual(span)
        | BinaryOperator::Spaceship(span) => BinaryOperator::Spaceship(*span),
        _ => return None,
    };

    get_operator_method(codebase, php_operator_name(&declared, 2)?, &[lhs_type, rhs_type])
}

/// Runs the operator a PHP# comparison of instances runs, checked as the static call it runs as, and returns what it
/// returns. `==` lifts a nullable side itself.
pub(crate) fn analyze_declared_comparison<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    artifacts: &mut AnalysisArtifacts,
    binary: &Binary<'arena>,
    lhs_type: &TUnion,
    rhs_type: &TUnion,
) -> Option<TUnion>
where
    A: Arena,
{
    let method = get_comparison_method(&binary.operator, lhs_type, rhs_type, context.codebase)?;
    let (lhs_type, rhs_type) = if matches!(binary.operator, BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_)) {
        (lhs_type.to_non_nullable(), rhs_type.to_non_nullable())
    } else {
        (lhs_type.clone(), rhs_type.clone())
    };

    let operands = [(binary.lhs, &lhs_type), (binary.rhs, &rhs_type)];

    Some(analyze_operator_call(context, artifacts, method, &operands, binary.span()))
}

/// Reports `refusal` on `binary`, naming both types and the comparison to write instead.
pub(crate) fn report_sharp_refusal<A>(
    context: &mut Context<'_, '_, A>,
    binary: &Binary<'_>,
    refusal: Refusal,
    lhs_type: &TUnion,
    rhs_type: &TUnion,
) where
    A: Arena,
{
    let codebase = context.codebase;
    let operator = &binary.operator;
    let (lhs_name, rhs_name) = (display_operand(context, lhs_type), display_operand(context, rhs_type));
    let op = BytesDisplay(operator.as_bytes());
    let pair = format!("`{op}` cannot compare `{lhs_name}` with `{rhs_name}`");
    // The part of an operand that holds a refused comparand, the left one first.
    let part = |refused: fn(Comparand) -> bool| {
        let side =
            if lhs_type.types.iter().any(|atomic| holds(atomic, refused, codebase)) { lhs_type } else { rhs_type };

        display_refused_part(context, side, refused)
    };

    let (code, issue) = match refusal {
        Refusal::Identity => {
            let equality = if operator.is_negated_equality() { "!=" } else { "==" };

            (
                IssueCode::InvalidOperand,
                Issue::error(format!("{pair}: it tests whether two class instances are the same object."))
                    .with_note("`==` compares every other value strictly.")
                    .with_help(format!("Use `{equality}` to compare the values.")),
            )
        }
        Refusal::Collection => (
            IssueCode::NotSupportedYet,
            Issue::error(format!(
                "`{op}` on a collection is not supported yet, so it cannot compare `{lhs_name}` with `{rhs_name}`."
            ))
            .with_note("A `List`, `Map` or `Set` is a PHP array at runtime, which `==` cannot compare strictly yet.")
            .with_help("Compare the elements one by one."),
        ),
        Refusal::Instance => {
            let class = part(|comparand| comparand == Comparand::Instance);

            let issue = if matches!(operator, BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_)) {
                let identity = if operator.is_negated_equality() { "!==" } else { "===" };

                Issue::error(format!("{pair}: `{class}` declares no `operator ==`."))
                    .with_note("`==` on a class instance exists only where its class declares `operator ==`.")
                    .with_help(format!("Use `{identity}` to test whether both sides are the same object."))
            } else {
                Issue::error(format!("{pair}: `{class}` declares no `operator <=>`."))
                    .with_note(format!(
                        "`{op}` on a class instance exists only where its class declares `operator <=>`."
                    ))
                    .with_help("Compare values the instances hold, such as their properties.")
            };

            (IssueCode::InvalidOperand, issue)
        }
        Refusal::NullableOperand => (
            IssueCode::InvalidOperand,
            Issue::error(format!("{pair}: only `==` and `!=` take `null`, so test the value for `null` first."))
                .with_note("PHP# lifts `==` and `!=` over `null`, and no other operator.")
                .with_help("Test it with `!= null` before the comparison."),
        ),
        Refusal::UnorderedOperand => {
            (IssueCode::InvalidOperand, unordered_issue(op, &part(Comparand::is_unordered), refusal))
        }
        Refusal::DifferentTypes => (
            IssueCode::InvalidOperand,
            Issue::error(format!("{pair}."))
                .with_note("PHP# compares values strictly, so values of two different types never match.")
                .with_help("Convert one side so both sides have the same type."),
        ),
    };

    context.collector.report_with_code(
        code,
        issue
            .with_annotation(Annotation::primary(binary.lhs.span()).with_message(format!("This is `{lhs_name}`.")))
            .with_annotation(Annotation::secondary(binary.rhs.span()).with_message(format!("This is `{rhs_name}`."))),
    );
}

/// Reports `selector`, the function a PHP# `sortedBy` orders by, when the key it returns, of `key_type`, has no order.
/// `sortedBy` orders two keys as `<=>` orders them, so it refuses a key `<=>` refuses for having no order. A key that may
/// be `null` still orders.
pub(crate) fn check_sorted_by_key<A>(context: &mut Context<'_, '_, A>, selector: &Expression<'_>, key_type: &TUnion)
where
    A: Arena,
{
    let (refusal, refused): (Refusal, fn(Comparand) -> bool) =
        match ordering_refusal(key_type, key_type, context.codebase) {
            Some(refusal @ Refusal::UnorderedOperand) => (refusal, Comparand::is_unordered),
            Some(refusal @ Refusal::Instance) => (refusal, |comparand| comparand == Comparand::Instance),
            _ => return,
        };
    let key_name = display_operand(context, key_type);
    let issue = unordered_issue("sortedBy", &display_refused_part(context, key_type, refused), refusal);

    context.collector.report_with_code(
        IssueCode::InvalidArgument,
        issue.with_annotation(Annotation::primary(selector.span()).with_message(format!("This returns `{key_name}`."))),
    );
}

/// The issue that refuses ordering a value whose part `part` has no order by `orderer`, an operator or `sortedBy`, as
/// `refusal` gives the reason: a `bool`, an enum case or a collection, or an instance of a class that declares no
/// `operator <=>`.
fn unordered_issue(orderer: impl std::fmt::Display, part: &str, refusal: Refusal) -> Issue {
    let note = if matches!(refusal, Refusal::Instance) {
        "A class instance has an order only where its class declares `operator <=>`."
    } else {
        "A `bool`, an enum case and a collection have no order."
    };

    Issue::error(format!(
        "`{orderer}` orders numbers, strings and classes that declare `operator <=>`, and `{part}` is none of them."
    ))
    .with_note(note)
    .with_help("Order a number or a string taken from the value instead.")
}

/// Whether `atomic` holds a comparand `refused` accepts.
fn holds(atomic: &TAtomic, refused: fn(Comparand) -> bool, codebase: &CodebaseMetadata) -> bool {
    comparands(&TUnion::from_atomic(atomic.clone()), codebase).into_iter().any(refused)
}

/// The part of `union` that holds a comparand `refused` accepts, as `Cart` of a `Cart|int`.
fn display_refused_part<A>(context: &Context<'_, '_, A>, union: &TUnion, refused: fn(Comparand) -> bool) -> String
where
    A: Arena,
{
    let part = union.types.iter().filter(|atomic| holds(atomic, refused, context.codebase)).cloned().collect();

    display_operand(context, &TUnion::from_vec(part))
}

/// The operand that `==`, `!=`, `===` or `!==` compares with `null` when its type cannot be `null`, as in
/// `customer != null` on a `Customer`.
fn get_never_null_operand_compared_with_null<'ast, 'arena, 'types>(
    binary: &'ast Binary<'arena>,
    lhs_type: &'types TUnion,
    rhs_type: &'types TUnion,
) -> Option<(&'ast Expression<'arena>, &'types TUnion)> {
    if !matches!(
        binary.operator,
        BinaryOperator::Equal(_)
            | BinaryOperator::NotEqual(_)
            | BinaryOperator::Identical(_)
            | BinaryOperator::NotIdentical(_)
    ) {
        return None;
    }

    let is_never_null = |operand_type: &TUnion| {
        !operand_type.can_be_null() && !operand_type.possibly_undefined() && !operand_type.is_never()
    };

    if rhs_type.is_null() && is_never_null(lhs_type) {
        Some((binary.lhs, lhs_type))
    } else if lhs_type.is_null() && is_never_null(rhs_type) {
        Some((binary.rhs, rhs_type))
    } else {
        None
    }
}

fn report_redundant_null_comparison<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    binary: &Binary<'arena>,
    operand: &Expression<'arena>,
    operand_type: &TUnion,
) where
    A: Arena,
{
    let operator_span = binary.operator.span();
    if operator_span.is_zero() {
        // this is a synthetic node, do not report it.
        return;
    }

    let operand_type_str = display_type(context, operand_type);
    let check = match written_null_pattern(context, binary) {
        Some(pattern) => format!("`{pattern}` check"),
        None => format!("`{}` comparison", BytesDisplay(binary.operator.as_bytes())),
    };
    let issue = context.as_null_check_error(
        Issue::help(format!("Redundant {check}: `{operand_type_str}` is never `null`."))
            .with_annotation(
                Annotation::primary(operand.span())
                    .with_message(format!("This is `{operand_type_str}`, which is never `null`")),
            )
            .with_annotation(Annotation::secondary(operator_span).with_message("This null check cannot matter"))
            .with_help("Remove the null check."),
    );

    context.collector.report_with_code(IssueCode::RedundantComparison, issue);
}

/// The PHP# null pattern a check runs, as written: `is null`, `is not null`, or a `match` arm's `null`. The pattern
/// runs as PHP's `===` of the value it tests, so the check is the pattern of the forms being analyzed that holds that
/// value. A written operator is no pattern, and gives none.
fn written_null_pattern<A>(context: &Context<'_, '_, A>, binary: &Binary<'_>) -> Option<String>
where
    A: Arena,
{
    let value = binary.rhs.span();
    let leading = context
        .patterns
        .iter()
        .find_map(|(is, pattern)| written_words(pattern, is.iter().map(|is| is.value).collect(), value))?;
    let words = leading.into_iter().chain([&context.source_file.contents[value.to_range_usize()]]);

    Some(words.map(String::from_utf8_lossy).collect::<Vec<_>>().join(" "))
}

/// The keywords written right before the pattern of `value` inside `pattern`, given the `leading` keywords written right
/// before `pattern`: each `not` on the way, and none after `and`, `or`, `(` or a property's `:`.
fn written_words<'arena>(
    pattern: &Pattern<'arena>,
    mut leading: Vec<&'arena [u8]>,
    value: Span,
) -> Option<Vec<&'arena [u8]>> {
    match pattern {
        Pattern::Not(not) => {
            leading.push(not.not.value);

            written_words(not.pattern, leading, value)
        }
        Pattern::Binary(binary) => {
            written_words(binary.left, leading, value).or_else(|| written_words(binary.right, Vec::new(), value))
        }
        Pattern::Parenthesized(parenthesized) => written_words(parenthesized.pattern, Vec::new(), value),
        Pattern::Properties(properties) => {
            properties.properties.iter().find_map(|property| written_words(property.pattern, Vec::new(), value))
        }
        Pattern::Type(_) | Pattern::Value(_) => (pattern.span() == value).then_some(leading),
        Pattern::Comparison(_) => None,
    }
}

/// Helper to report redundant comparison issues.
fn report_redundant_comparison<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    artifacts: &AnalysisArtifacts,
    binary: &Binary<'arena>,
    comparison_description: &str,
    result_value_str: &str,
) where
    A: Arena,
{
    let operator_span = binary.operator.span();
    if operator_span.is_zero() {
        // this is a synthetic node, do not report it.
        return;
    }

    let left = match artifacts.get_expression_type(&binary.lhs) {
        Some(t) => format!("Left operand is `{}`", display_type(context, t)),
        None => "Left operand type is unknown".to_string(),
    };
    let right = match artifacts.get_expression_type(&binary.rhs) {
        Some(t) => format!("Right operand is `{}`", display_type(context, t)),
        None => "Right operand type is unknown".to_string(),
    };

    context.collector.report_with_code(
        IssueCode::RedundantComparison,
        Issue::help(format!(
            "Redundant `{}` comparison: left-hand side is {} right-hand side.",
            BytesDisplay(binary.operator.as_bytes()),
            comparison_description
        ))
        .with_annotation(Annotation::primary(binary.lhs.span()).with_message(left))
        .with_annotation(Annotation::secondary(binary.rhs.span()).with_message(right))
        .with_note(format!(
            "The `{}` operator will always return {} in this case.",
            BytesDisplay(binary.operator.as_bytes()),
            result_value_str
        ))
        .with_help(format!(
            "Consider simplifying or removing this comparison as it always evaluates to {result_value_str}."
        )),
    );
}
