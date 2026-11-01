use std::mem::discriminant;
use std::rc::Rc;

use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::property::PropertyMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::bound_is_equatable;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::get_bool;
use mago_codex::ttype::get_false;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_named_object;
use mago_codex::ttype::get_true;
use mago_codex::ttype::is_key_type;
use mago_codex::ttype::template::inferred_type_replacer;
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
use mago_syntax::cst::ListPatternElement;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Parenthesized;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::TypePattern;
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
use crate::expression::ancestor_type_arguments;
use crate::expression::array::check_sharp_literal_kind;
use crate::expression::array::get_empty_literal_type;
use crate::expression::array::get_set_literal_type;
use crate::expression::binary::utils::DeclaredOperator;
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
use crate::expression::binary::utils::report_tied_operator;
use crate::expression::binary::utils::takes_operands;
use crate::expression::is_refused_type;
use crate::expression::with_type_arguments;
use crate::statement::class_like::r#struct::data_enum;
use crate::statement::class_like::r#struct::declares_equality;
use crate::statement::class_like::r#struct::value_properties;
use crate::utils::expression::get_literal_array_key;
use crate::utils::misc::unwrap_expression;
use crate::utils::names::article;
use crate::utils::names::display_bool;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_type;
use crate::utils::quoted_source;
use crate::utils::template::class_type_arguments;

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
    if context.dialect.is_sharp() && matches!(binary.operator, BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_)) {
        type_collection_literal(context, block_context, artifacts, binary.lhs, binary.rhs);
        type_collection_literal(context, block_context, artifacts, binary.rhs, binary.lhs);
    }

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
        let op = BytesDisplay(binary.operator.as_bytes());
        let subject = |side: &str| {
            if context.dialect.is_sharp() {
                format!("{side} operand of `{op}`")
            } else {
                format!("{side} operand in `{op}` comparison")
            }
        };
        let (lhs_subject, rhs_subject) = (subject("Left"), subject("Right"));
        if !sharp_equality || lhs_type.is_mixed() {
            check_comparison_operand(context, binary.lhs, lhs_type, rhs_type, &lhs_subject, &binary.operator);
        }
        if !sharp_equality || rhs_type.is_mixed() {
            check_comparison_operand(context, binary.rhs, rhs_type, lhs_type, &rhs_subject, &binary.operator);
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
                    || (!context.dialect.is_sharp()
                        && are_definitely_loosely_equal(context.settings.version, lhs_type, rhs_type))
                {
                    if !block_context.flags.inside_loop_expressions() {
                        report_redundant_comparison(context, artifacts, binary, "always equal to", "`true`");
                    }

                    get_true()
                } else if never_equal(context, lhs_type, rhs_type) {
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
                    || (!context.dialect.is_sharp()
                        && are_definitely_loosely_equal(context.settings.version, lhs_type, rhs_type))
                {
                    if !block_context.flags.inside_loop_expressions() {
                        let description = format!("never equal to (always {} for !=)", display_bool(context, false));
                        report_redundant_comparison(context, artifacts, binary, &description, "`false`");
                    }

                    get_false()
                } else if never_equal(context, lhs_type, rhs_type) {
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

/// Gives `literal`, a side of a PHP# `==` or `!=` written as a collection literal, the collection type of `other`, the
/// other side, as a place gives its literal one, spec section 12: a list literal compared with a `Set` is a `Set`, at
/// any depth, `[]` is the other side's `List` or `Set`, and `[:]` its `Map`. A literal of a kind the other side is not,
/// as `[]` compared with a `Map`, is refused as [`check_sharp_literal_kind`] refuses it at a place, and takes the other
/// side's type, so the comparison adds no second issue. A literal compared with a literal or with no collection keeps
/// its own type.
fn type_collection_literal<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    literal: &Expression<'arena>,
    other: &Expression<'_>,
) where
    A: Arena,
{
    let Expression::Array(array) = literal.unparenthesized() else {
        return;
    };
    let Some(other_type) = artifacts
        .get_expression_type(other)
        .filter(|other_type| other_type.has_array() && !matches!(other.unparenthesized(), Expression::Array(_)))
        .cloned()
    else {
        return;
    };

    let literal_type = if check_sharp_literal_kind(context, Some(block_context), artifacts, literal, &other_type) {
        Some(other_type)
    } else if array.elements.is_empty() {
        get_empty_literal_type(Some(literal), &other_type)
    } else {
        get_set_literal_type(context, artifacts, literal, &other_type, block_context.scope.get_function_like())
    };
    if let Some(literal_type) = literal_type {
        artifacts.set_expression_type(array, literal_type.clone());
        artifacts.set_expression_type(literal, literal_type);
    }
}

/// Whether `==` never holds for a value of `lhs_type` and one of `rhs_type`. PHP# `==` is `===` on every pair but an
/// int with a float and two collections, spec section 19, so it never holds where `===` never does. PHP's `==` never
/// holds where no juggling makes the values equal.
fn never_equal<A>(context: &Context<'_, '_, A>, lhs_type: &TUnion, rhs_type: &TUnion) -> bool
where
    A: Arena,
{
    let codebase = context.codebase;
    if context.dialect.is_sharp()
        && !mixes_numbers(lhs_type, rhs_type, codebase)
        && !lhs_type.has_array()
        && !rhs_type.has_array()
    {
        are_definitely_not_identical(codebase, lhs_type, rhs_type, false)
    } else {
        are_definitely_not_loosely_equal(codebase, context.settings.version, lhs_type, rhs_type)
    }
}

/// Attempts to extract a boolean literal from an expression.
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

/// Checks a single operand of a comparison by `operator` for problematic types, naming it `subject`, as "Left operand in
/// `<` comparison" names it in PHP and "Left operand of `<`" in PHP#. `other_type` is the other operand's. In PHP# an
/// `Any` operand is named `Any`, with the check that narrows it.
fn check_comparison_operand<A>(
    context: &mut Context<'_, '_, A>,
    operand: &Expression<'_>,
    operand_type: &TUnion,
    other_type: &TUnion,
    subject: &str,
    operator: &BinaryOperator<'_>,
) where
    A: Arena,
{
    let op_str = BytesDisplay(operator.as_bytes());

    if operand_type.is_null() {
        context.collector.report_with_code(
            IssueCode::NullOperand,
            Issue::error(format!(
                "{subject} is `null`."
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
                "{subject} might be `null` (type `{operand_type_str}`)."
            ))
            .with_annotation(Annotation::primary(operand.span()).with_message("This might be `null`"))
            .with_note(format!("If this operand is `null` at runtime, PHP's specific comparison rules for `null` with `{op_str}` will apply."))
            .with_help("Ensure this operand is non-null or that comparison with `null` is intended and handled safely."),
        );
    } else if operand_type.is_mixed()
        && !(context.dialect.is_sharp() && operator.is_equality() && compares_by_value(other_type, context.codebase))
    {
        let issue = if context.dialect.is_sharp() {
            let (reason, note) = if operator.is_equality() {
                (ANY_EQUALITY, "An `Any` may hold any value, so `==` cannot compare it strictly until it is checked.")
            } else {
                (
                    "has no order",
                    "An `Any` may hold any value, and PHP# orders only numbers, strings and classes that declare `operator <=>`.",
                )
            };

            Issue::error(format!("{subject} is `{}`, which {reason}.", display_type(context, operand_type)))
                .with_annotation(Annotation::primary(operand.span()))
                .with_note(note)
                .with_help("Check the value with `is` or `match` first, as in `if (value is int n)`, and compare `n`.")
        } else {
            Issue::error(format!("{subject} has `mixed` type."))
                .with_annotation(Annotation::primary(operand.span()).with_message("This has type `mixed`"))
                .with_note(format!(
                    "The result of comparing `mixed` types with `{op_str}` is unpredictable and can hide bugs."
                ))
                .with_help("Ensure this operand has a known, comparable type before using this comparison operator.")
        };

        context.collector.report_with_code(IssueCode::MixedOperand, issue);
    } else if operand_type.is_false() {
        context.collector.report_with_code(
            IssueCode::FalseOperand,
            Issue::error(format!(
               "{subject} is `false`."
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
                "{subject} might be `false` (type `{operand_type_str}`)."
            ))
            .with_annotation(Annotation::primary(operand.span()).with_message("This might be `false`"))
            .with_note(format!("If this operand is `false` at runtime, PHP's specific comparison rules for `false` with `{op_str}` will apply."))
            .with_help("Ensure this operand is non-false or that comparison with `false` is intended and handled safely."),
        );
    }
}

/// Whether a PHP# `Any?` compares with a value of `other_type` by value, as spec sections 19 and 24 decide for a string,
/// a number, a `bool`, an enum, a struct and a collection, and for `null`, which equals only `null`. Any other value
/// must be checked with `is`, `as` or `match` first.
fn compares_by_value(other_type: &TUnion, codebase: &CodebaseMetadata) -> bool {
    comparands(other_type, codebase).iter().all(|comparand| {
        matches!(
            comparand,
            Comparand::String
                | Comparand::Int
                | Comparand::Float
                | Comparand::Bool
                | Comparand::Enum(_)
                | Comparand::Struct(_)
                | Comparand::Collection
        )
    })
}

/// What spec section 19 compares a PHP# value as. Two values that share no comparand are never equal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Comparand {
    String,
    Int,
    Float,
    Bool,
    /// A case of the enum of this name, spec section 20. A case that carries data compares by value, field by field.
    Enum(Word),
    /// An instance of a class that is no struct, or a function.
    Instance,
    /// An instance of the struct of this name, spec section 10, which compares by value: through the `operator ==` it
    /// declares, or else property by property.
    Struct(Word),
    Collection,
    Any,
}

impl Comparand {
    /// Whether a value of this comparand is an object that `==` compares through its class's `operator ==`.
    const fn is_instance(self) -> bool {
        matches!(self, Comparand::Instance | Comparand::Struct(_))
    }

    /// Whether a value of this comparand has no order: only [`ORDERED_TYPES`] have one, so `<`, `<=`, `>`, `>=` and
    /// `<=>` refuse a `bool`, an enum case and a collection.
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
                let class = object.get_name().and_then(|name| codebase.get_class_like(name.as_bytes()));
                found.push(match class {
                    Some(class) if class.kind.is_enum() => Comparand::Enum(class.name),
                    Some(class) if class.flags.is_struct() => Comparand::Struct(class.name),
                    Some(class) if let Some(r#enum) = data_enum(codebase, class) => Comparand::Enum(r#enum),
                    _ => Comparand::Instance,
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

/// The one collection a PHP# collection type is, a `List`, a `Map` or a `Set`, with the type of the values `==`
/// compares: a list's or a set's elements, and a map's values, spec section 19. `None` when `union` is not one
/// collection, besides `null`, as an iterable is not.
fn collection_parts<'union>(union: &'union TUnion, codebase: &CodebaseMetadata) -> Option<(&'union TArray, TUnion)> {
    let mut collections = union.types.iter().filter(|atomic| !atomic.is_null());
    let (Some(TAtomic::Array(array)), None) = (collections.next(), collections.next()) else {
        return None;
    };
    let (_, value) = get_array_parameters(array, codebase);

    Some((array, value))
}

/// The side of `lhs_type` and `rhs_type` that [`Refusal::Collection`] refuses: the first that holds an `Iterable`, or
/// else the first that is not one collection, as a type parameter bound by one.
fn unsupported_collection<'union>(
    lhs_type: &'union TUnion,
    rhs_type: &'union TUnion,
    codebase: &CodebaseMetadata,
) -> &'union TUnion {
    let not_one_collection = if collection_parts(lhs_type, codebase).is_none() { lhs_type } else { rhs_type };

    [lhs_type, rhs_type].into_iter().find(|side| side.has_iterable()).unwrap_or(not_one_collection)
}

/// Why `==` does not compare `part`, which [`Refusal::Collection`] refuses, spec section 19: it compares a `List`, a
/// `Map` or a `Set`, and an `Iterable` is none of them, while a type parameter bound by one may be another type. Each
/// is an `Iterable`, whose `toList()` gives a `List` that `==` compares.
fn collection_reason<A>(context: &Context<'_, '_, A>, part: &TUnion) -> String
where
    A: Arena,
{
    if part.has_iterable() {
        "`==` compares a `List`, a `Map` or a `Set`, and an `Iterable` is none of them".to_owned()
    } else {
        format!(
            "`==` compares a `List`, a `Map` or a `Set`, and `{}` may be another type",
            display_operand(context, part)
        )
    }
}

/// Whether PHP# `==` compares a value of `lhs_type` with one of `rhs_type` as two collections, by value, spec section
/// 19, which tests no identity a type could narrow by.
pub(crate) fn compares_collections(lhs_type: &TUnion, rhs_type: &TUnion, codebase: &CodebaseMetadata) -> bool {
    collection_parts(lhs_type, codebase).is_some() && collection_parts(rhs_type, codebase).is_some()
}

/// Why `operator`, `==` or `!=`, may not compare a value of `lhs_type` with one of `rhs_type`, either of which holds a
/// collection, spec section 19: two lists compare element by element in order, two maps compare the values under each
/// key regardless of order, and two sets compare their elements regardless of order, each with its own `==`. A list, a
/// map and a set never equal each other, or a value of another type. A local set to `[]` has PHP's type of an empty
/// array, whose elements are `never`, which equals a list or a map, and a set equals only a set. A union that holds a
/// collection beside another type compares as each of its types does. Any other value that holds a collection, such
/// as an `Iterable`, compares only through its `toList()`.
fn collection_refusal(
    operator: &BinaryOperator<'_>,
    lhs_type: &TUnion,
    rhs_type: &TUnion,
    codebase: &CodebaseMetadata,
    visiting: &mut Vec<Word>,
) -> Option<Refusal> {
    let is_union = |union: &TUnion| union.types.iter().filter(|atomic| !atomic.is_null()).nth(1).is_some();
    let ((lhs_array, lhs_value), (rhs_array, rhs_value)) =
        match (collection_parts(lhs_type, codebase), collection_parts(rhs_type, codebase)) {
            (Some(lhs), Some(rhs)) => (lhs, rhs),
            _ if lhs_type.has_iterable() || rhs_type.has_iterable() => return Some(Refusal::Collection),
            _ if is_union(lhs_type) || is_union(rhs_type) => {
                return union_refusal(operator, lhs_type, rhs_type, codebase, visiting);
            }
            (lhs, rhs) => {
                let holds_no_collection =
                    |union: &TUnion| !comparands(union, codebase).contains(&Comparand::Collection);
                let one_beside_other = (lhs.is_some() && holds_no_collection(rhs_type))
                    || (rhs.is_some() && holds_no_collection(lhs_type));

                return Some(if one_beside_other { Refusal::DifferentTypes } else { Refusal::Collection });
            }
        };
    let is_set = |array: &TArray| matches!(array, TArray::Set(_));
    if discriminant(lhs_array) != discriminant(rhs_array)
        && (is_set(lhs_array) || is_set(rhs_array) || (!lhs_value.is_never() && !rhs_value.is_never()))
    {
        return Some(Refusal::DifferentTypes);
    }

    refusal(operator, &lhs_value, &rhs_value, codebase, visiting).map(|_| Refusal::CollectionElement)
}

/// Why `operator` may not compare a value of `lhs_type` with one of `rhs_type`, where one is a union that holds a
/// collection beside another type, spec section 19: each type of one side meets each type of the other, `null` aside.
/// The first pair that may meet and cannot compare refuses the comparison with its reason, and where no pair may meet,
/// the two sides are of different types.
fn union_refusal(
    operator: &BinaryOperator<'_>,
    lhs_type: &TUnion,
    rhs_type: &TUnion,
    codebase: &CodebaseMetadata,
    visiting: &mut Vec<Word>,
) -> Option<Refusal> {
    let parts = |union: &TUnion| -> Vec<TUnion> {
        union
            .types
            .iter()
            .filter(|atomic| !atomic.is_null())
            .map(|atomic| TUnion::from_atomic(atomic.clone()))
            .collect()
    };
    let (lhs_parts, rhs_parts) = (parts(lhs_type), parts(rhs_type));
    let mut meet = false;
    for (lhs, rhs) in lhs_parts.iter().flat_map(|lhs| rhs_parts.iter().map(move |rhs| (lhs, rhs))) {
        match refusal(operator, lhs, rhs, codebase, visiting) {
            Some(Refusal::DifferentTypes) => {}
            Some(refused) => return Some(refused),
            None => meet = true,
        }
    }

    (!meet).then_some(Refusal::DifferentTypes)
}

/// Why `operator` cannot compare the elements of a collection of `lhs_type` with those of one of `rhs_type`, read
/// through nested collections, as "`Cart` declares no `operator ==`".
fn element_reason<A>(
    context: &Context<'_, '_, A>,
    operator: &BinaryOperator<'_>,
    lhs_type: &TUnion,
    rhs_type: &TUnion,
) -> String
where
    A: Arena,
{
    let codebase = context.codebase;
    // A union compares as each of its types does, so its reason is that of its first two collections of one kind whose
    // elements `==` cannot compare.
    let collections = |union: &TUnion| -> Vec<TUnion> {
        union
            .types
            .iter()
            .filter(|atomic| matches!(atomic, TAtomic::Array(_)))
            .map(|atomic| TUnion::from_atomic(atomic.clone()))
            .collect()
    };
    let (lhs_collections, rhs_collections) = (collections(lhs_type), collections(rhs_type));
    let Some((lhs, rhs)) = lhs_collections
        .iter()
        .flat_map(|lhs| rhs_collections.iter().map(move |rhs| (lhs, rhs)))
        .filter_map(|(lhs, rhs)| {
            let ((lhs_array, lhs), (rhs_array, rhs)) =
                (collection_parts(lhs, codebase)?, collection_parts(rhs, codebase)?);

            (discriminant(lhs_array) == discriminant(rhs_array)).then_some((lhs, rhs))
        })
        .find(|(lhs, rhs)| sharp_refusal(operator, lhs, rhs, codebase).is_some())
    else {
        unreachable!("`==` refuses elements only of two collections of one kind");
    };

    refusal_reason(context, operator, &lhs, &rhs, sharp_refusal(operator, &lhs, &rhs, codebase))
}

/// Why `operator` cannot compare a value of `lhs_type` with one of `rhs_type`, as `refused` gives the reason, read
/// through nested collections, as "`Cart` declares no `operator ==`".
fn refusal_reason<A>(
    context: &Context<'_, '_, A>,
    operator: &BinaryOperator<'_>,
    lhs_type: &TUnion,
    rhs_type: &TUnion,
    refused: Option<Refusal>,
) -> String
where
    A: Arena,
{
    let codebase = context.codebase;
    match refused {
        Some(Refusal::Instance) => {
            let class = display_refused_side(context, lhs_type, rhs_type, Comparand::is_instance);

            format!("`{class}` declares no `operator ==`")
        }
        Some(Refusal::CollectionElement) => element_reason(context, operator, lhs_type, rhs_type),
        Some(Refusal::StructProperty) => {
            struct_property_issue(context, lhs_type, rhs_type).message.trim_end_matches('.').to_owned()
        }
        Some(Refusal::Collection) => collection_reason(context, unsupported_collection(lhs_type, rhs_type, codebase)),
        Some(Refusal::TypeParameter) => type_parameter_reason(lhs_type, rhs_type, codebase),
        _ => format!("`{}` never equals `{}`", display_operand(context, lhs_type), display_operand(context, rhs_type)),
    }
}

/// Why spec section 19 gives a PHP# comparison no meaning.
#[derive(Clone, Copy)]
pub(crate) enum Refusal {
    /// `===` or `!==` on a value that is no class instance.
    Identity,
    /// `===` or `!==` on a struct, which has no identity: spec section 10's "`===` works on classes only".
    StructIdentity,
    /// `==` or `!=` on a struct with a property its own `==` cannot compare, spec section 10.
    StructProperty,
    /// `==` or `!=` on two collections whose elements `==` cannot compare, spec section 19.
    CollectionElement,
    /// `==` or `!=` on an `Iterable`, or on a type parameter bound by a `List`, a `Map` or a `Set`.
    Collection,
    /// `==` or `!=` on an instance of a class that declares no `operator ==`, or `<=>` or an ordering on one whose class
    /// declares no `operator <=>`.
    Instance,
    /// `==` or `!=` on an unchecked `Any?` and an instance, which can't tell the same object from an equal value.
    UncheckedObject,
    /// `<=>` or an ordering with a side that may be `null`, an instance or any other value, which only `==` and `!=`
    /// lift.
    NullableOperand,
    /// `<=>` or an ordering with a side that may be a `bool`, an enum case or a collection, which have no order.
    UnorderedOperand,
    /// `==` or `!=` on two values of types that never match, a string ordered against another type, or an instance
    /// compared with a value its class's operator never takes, as `money < 5`.
    DifferentTypes,
    /// A comparison of instances that two operators take, of which neither is more specific.
    Tied,
    /// `==` or `!=` on a value of a type parameter whose bound gives it no `==`, spec section 11: one with no bound.
    TypeParameter,
}

/// The type parameter a value of `union` may be whose bound gives it no `==`, spec section 11: one bound by neither
/// `Equatable` nor a type with `==`, which leaves its values `Any?`. A bound of a class that declares no `operator ==`
/// keeps the refusal of an instance of that class.
pub(crate) fn unequatable_parameter(union: &TUnion, codebase: &CodebaseMetadata) -> Option<Word> {
    union.types.iter().find_map(|atomic| match atomic {
        TAtomic::GenericParameter(parameter)
            if !bound_is_equatable(&parameter.constraint)
                && comparands(&parameter.constraint, codebase).contains(&Comparand::Any) =>
        {
            Some(parameter.parameter_name)
        }
        _ => None,
    })
}

/// Whether every value of `union` but `null` is a value of a type parameter whose bound gives it `==` whatever its
/// type argument, spec section 11.
fn is_equatable_parameter(union: &TUnion) -> bool {
    union.types.iter().filter(|atomic| !atomic.is_null()).all(
        |atomic| matches!(atomic, TAtomic::GenericParameter(parameter) if bound_is_equatable(&parameter.constraint)),
    )
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
    // A comparison no source writes, as the `===` a list pattern's count runs as, has no operator token to refuse. A
    // refused side, as a value of a type parameter whose bound the checker refused, has its one error already, as an
    // arithmetic operand has.
    if operator.span().length() == 0 || is_refused_type(lhs_type) || is_refused_type(rhs_type) {
        return None;
    }

    refusal(operator, lhs_type, rhs_type, codebase, &mut Vec::new())
}

/// [`sharp_refusal`] inside the structs `visiting` holds, whose properties are being compared, so a struct that holds
/// itself, as in `Node? next`, compares by the rest.
fn refusal(
    operator: &BinaryOperator<'_>,
    lhs_type: &TUnion,
    rhs_type: &TUnion,
    codebase: &CodebaseMetadata,
    visiting: &mut Vec<Word>,
) -> Option<Refusal> {
    let (lhs, rhs) = (comparands(lhs_type, codebase), comparands(rhs_type, codebase));
    let has = |comparand: Comparand| lhs.contains(&comparand) || rhs.contains(&comparand);
    // A function has no `==`, spec section 19, beside any value but `null`, which `==` lifts, whatever the other side.
    let compares_function = !lhs.is_empty()
        && !rhs.is_empty()
        && [lhs_type, rhs_type]
            .iter()
            .any(|side| side.types.iter().any(|atomic| matches!(atomic, TAtomic::Callable(_))));

    match operator {
        BinaryOperator::Identical(_) | BinaryOperator::NotIdentical(_) => {
            if lhs.iter().chain(&rhs).any(|comparand| matches!(comparand, Comparand::Struct(_))) {
                return Some(Refusal::StructIdentity);
            }
            // An unchecked `Any?` beside an instance is the same object or not, spec section 24.
            let instances = lhs.iter().chain(&rhs).any(|comparand| *comparand == Comparand::Instance)
                && lhs.iter().chain(&rhs).all(|comparand| matches!(comparand, Comparand::Instance | Comparand::Any));

            (!instances).then_some(Refusal::Identity)
        }
        // A side that is only `null` makes a null test, which `==` lifts.
        BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_) if lhs.is_empty() || rhs.is_empty() => None,
        // A type parameter has `==` only through its bound, spec section 11, and `Equatable` gives it one.
        BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_)
            if [lhs_type, rhs_type].into_iter().any(|side| unequatable_parameter(side, codebase).is_some()) =>
        {
            Some(Refusal::TypeParameter)
        }
        BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_)
            if is_equatable_parameter(lhs_type) && is_equatable_parameter(rhs_type) =>
        {
            None
        }
        BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_) if compares_function => Some(Refusal::Instance),
        BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_) => {
            let structs: Vec<Word> = lhs
                .iter()
                .chain(&rhs)
                .filter_map(|comparand| match comparand {
                    Comparand::Struct(name) => Some(*name),
                    _ => None,
                })
                .collect();
            // An unchecked `Any?` can't tell an instance's identity from its value, spec section 24, and compares with
            // a collection by value, element by element, as two collections of the other side's type compare.
            let unchecked = if lhs.iter().all(|comparand| *comparand == Comparand::Any) {
                Some(rhs_type)
            } else if rhs.iter().all(|comparand| *comparand == Comparand::Any) {
                Some(lhs_type)
            } else {
                None
            };
            if unchecked.is_some() && has(Comparand::Instance) {
                Some(Refusal::UncheckedObject)
            } else if let Some(collection) = unchecked
                && has(Comparand::Collection)
            {
                // Only a `List`'s, a `Map`'s or a `Set`'s elements are checked, and an `Iterable`'s are not.
                collection_parts(collection, codebase)
                    .and_then(|_| collection_refusal(operator, collection, collection, codebase, visiting))
            } else if has(Comparand::Collection) {
                collection_refusal(operator, lhs_type, rhs_type, codebase, visiting)
            } else if let Some(&name) = structs.first()
                && !declares_equality(codebase, name)
            {
                // A struct equals only an instance of the same struct, or an `Any` that holds one.
                let same = lhs
                    .iter()
                    .chain(&rhs)
                    .all(|comparand| *comparand == Comparand::Struct(name) || *comparand == Comparand::Any);

                // The struct's type arguments are those of a side that is not an unchecked `Any`.
                let typed = if lhs.contains(&Comparand::Struct(name)) { lhs_type } else { rhs_type };
                if !same {
                    Some(Refusal::DifferentTypes)
                } else if struct_property_refusal(name, typed, codebase, visiting).is_some() {
                    Some(Refusal::StructProperty)
                } else {
                    None
                }
            } else if has(Comparand::Instance) || !structs.is_empty() {
                match get_comparison_method(operator, lhs_type, rhs_type, codebase) {
                    None => Some(Refusal::Instance),
                    Some(DeclaredOperator::Tied(_)) => Some(Refusal::Tied),
                    Some(DeclaredOperator::Chosen(method)) => {
                        let operands = [&lhs_type.to_non_nullable(), &rhs_type.to_non_nullable()];

                        (!can_take_operands(codebase, method, &operands)).then_some(Refusal::DifferentTypes)
                    }
                }
            } else if has(Comparand::Any)
                || lhs.iter().any(|comparand| rhs.contains(comparand))
                || mixes_numbers(lhs_type, rhs_type, codebase)
            {
                // A case that carries data compares field by field, as a struct does.
                lhs.iter()
                    .chain(&rhs)
                    .find_map(|comparand| match comparand {
                        Comparand::Enum(name) => {
                            // A generic enum's type arguments are those of the side that gives them.
                            let typed = [lhs_type, rhs_type]
                                .into_iter()
                                .find(|side| enum_type_arguments(*name, [*side], codebase).is_some())
                                .unwrap_or(lhs_type);

                            struct_property_refusal(*name, typed, codebase, visiting)
                        }
                        _ => None,
                    })
                    .map(|_| Refusal::StructProperty)
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
/// order, or `None` when it can. PHP# orders an instance by the `operator <=>` its class declares, a case by the one
/// its enum declares, and a string by its bytes, so only against a string. A `bool`, a case of any other enum and a
/// collection have no order, even against an `Any?`. Only `==` and `!=` take `null`, so a side that may be `null` is
/// refused whatever its type. An `Any?` keeps its own report.
fn ordering_refusal(lhs_type: &TUnion, rhs_type: &TUnion, codebase: &CodebaseMetadata) -> Option<Refusal> {
    let (lhs, rhs) = (comparands(lhs_type, codebase), comparands(rhs_type, codebase));
    let has = |comparand: Comparand| lhs.contains(&comparand) || rhs.contains(&comparand);
    let method = get_comparison_method(&BinaryOperator::Spaceship(Span::zero()), lhs_type, rhs_type, codebase);
    let ordered_case = |comparand: &Comparand| matches!(comparand, Comparand::Enum(_)) && method.is_some();

    if lhs.iter().chain(&rhs).any(|comparand| comparand.is_unordered() && !ordered_case(comparand)) {
        return Some(Refusal::UnorderedOperand);
    }

    let different_types = if lhs.iter().chain(&rhs).any(|comparand| comparand.is_instance() || ordered_case(comparand))
    {
        let method = match method {
            None => return Some(Refusal::Instance),
            Some(DeclaredOperator::Tied(_)) => return Some(Refusal::Tied),
            Some(DeclaredOperator::Chosen(method)) => method,
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
/// `<=>` and the orderings, as [`get_operator_method`] chooses it from the types of both sides. `== null` tests for
/// `null`, and runs none.
pub(crate) fn get_comparison_method<'ctx>(
    operator: &BinaryOperator<'_>,
    lhs_type: &TUnion,
    rhs_type: &TUnion,
    codebase: &'ctx CodebaseMetadata,
) -> Option<DeclaredOperator<'ctx>> {
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
    let method = get_comparison_method(&binary.operator, lhs_type, rhs_type, context.codebase)?.chosen()?;
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
    let operator = &binary.operator;
    let (lhs_name, rhs_name) = (display_operand(context, lhs_type), display_operand(context, rhs_type));
    let op = BytesDisplay(operator.as_bytes());
    let pair = format!("`{op}` cannot compare `{lhs_name}` with `{rhs_name}`");
    let part = |refused: fn(Comparand) -> bool| display_refused_side(context, lhs_type, rhs_type, refused);

    let equality = if operator.is_negated_equality() { "!=" } else { "==" };
    if matches!(refusal, Refusal::Tied) {
        let operands = [&lhs_type.to_non_nullable(), &rhs_type.to_non_nullable()];
        let Some(DeclaredOperator::Tied(tied)) =
            get_comparison_method(operator, operands[0], operands[1], context.codebase)
        else {
            unreachable!("the refusal is a tie of these operands' operators");
        };
        report_tied_operator(context, operator, &operands, &tied, binary.span());

        return;
    }
    let (code, issue) = match refusal {
        Refusal::Tied => unreachable!("a tie is reported with the operators it ties"),
        Refusal::Identity => (
            IssueCode::InvalidOperand,
            Issue::error(format!("{pair}: it tests whether two class instances are the same object."))
                .with_note("`==` compares every other value strictly.")
                .with_help(format!("Use `{equality}` to compare the values.")),
        ),
        Refusal::StructIdentity => {
            let r#struct = part(|comparand| matches!(comparand, Comparand::Struct(_)));

            (
                IssueCode::InvalidOperand,
                Issue::error(format!(
                    "`{op}` tests for the same object, and `{struct}` is a struct: write `{equality}`.",
                    r#struct = r#struct
                ))
                .with_note("A struct is a value, so it has no identity: `==` compares its properties."),
            )
        }
        Refusal::StructProperty => (IssueCode::InvalidOperand, struct_property_issue(context, lhs_type, rhs_type)),
        Refusal::CollectionElement => {
            // An unchecked `Any?` compares as a collection of the other side's type.
            let (lhs_type, rhs_type) = match (lhs_type.is_mixed(), rhs_type.is_mixed()) {
                (true, _) => (rhs_type, rhs_type),
                (_, true) => (lhs_type, lhs_type),
                _ => (lhs_type, rhs_type),
            };
            let (lhs_name, rhs_name) = (display_operand(context, lhs_type), display_operand(context, rhs_type));

            (
                IssueCode::InvalidOperand,
                Issue::error(format!(
                    "`{op}` compares `{lhs_name}` with `{rhs_name}` element by element, and {}.",
                    element_reason(context, operator, lhs_type, rhs_type)
                ))
                .with_note(
                    "Two lists are equal when their elements are, in order, two maps when they hold equal values under the same keys, and two sets when they hold equal elements, in any order.",
                ),
            )
        }
        Refusal::Collection => {
            let part = unsupported_collection(lhs_type, rhs_type, context.codebase);

            (
                IssueCode::InvalidOperand,
                Issue::error(format!(
                    "{}: compare `a.toList() {equality} b.toList()`.",
                    collection_reason(context, part)
                ))
                .with_note(format!("{pair}.")),
            )
        }
        Refusal::Instance => {
            let class = part(Comparand::is_instance);

            let issue = if matches!(operator, BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_)) {
                let identity = if operator.is_negated_equality() { "!==" } else { "===" };

                Issue::error(format!("{pair}: `{class}` declares no `operator ==`."))
                    .with_note("`==` on a class instance exists only where its class declares `operator ==`.")
                    .with_help(format!("Use `{identity}` to test whether both sides are the same object."))
            } else {
                Issue::error(format!("{pair}: `{class}` declares no `operator <=>`."))
                    .with_note(format!("`{op}` orders {ORDERED_TYPES}."))
                    .with_help("Compare values the instances hold, such as their properties.")
            };

            (IssueCode::InvalidOperand, issue)
        }
        Refusal::UncheckedObject => {
            let (unchecked, object) =
                if lhs_type.is_mixed() { (binary.lhs, binary.rhs) } else { (binary.rhs, binary.lhs) };
            let (unchecked, object) = (quoted_source(context, unchecked.span()), quoted_source(context, object.span()));
            let identity = if operator.is_negated_equality() { "!==" } else { "===" };

            (
                IssueCode::InvalidOperand,
                Issue::error(format!(
                    "`{unchecked}` is unchecked: is it the same object as `{object}`, or an equal value? Check it with `is` first."
                ))
                .with_help(format!("Write `{unchecked} {identity} {object}` to test for the same object.")),
            )
        }
        Refusal::NullableOperand => (
            IssueCode::InvalidOperand,
            Issue::error(format!("{pair}: only `==` and `!=` take `null`, so test the value for `null` first."))
                .with_note("PHP# lifts `==` and `!=` over `null`, and no other operator.")
                .with_help("Test it with `!= null` before the comparison."),
        ),
        Refusal::UnorderedOperand => {
            let unordered_enum = unordered_enum(context, lhs_type, rhs_type);

            (
                IssueCode::InvalidOperand,
                unordered_issue(op, &part(Comparand::is_unordered), refusal, unordered_enum.as_deref()),
            )
        }
        Refusal::DifferentTypes => (
            IssueCode::InvalidOperand,
            different_types_issue(&pair, "Convert one side so both sides have the same type."),
        ),
        Refusal::TypeParameter => (
            IssueCode::InvalidOperand,
            Issue::error(format!("{}.", type_parameter_reason(lhs_type, rhs_type, context.codebase)))
                .with_note(TYPE_PARAMETER_EQUALITY),
        ),
    };
    // A pattern's read of its subject has no text of its own, so its refusal is at the operator: the one the pattern
    // writes, or the `==` a value pattern runs as, which spans the value.
    let at = if binary.lhs.span().length() == 0 { binary.operator.span() } else { binary.lhs.span() };

    context.collector.report_with_code(
        code,
        issue
            .with_annotation(Annotation::primary(at).with_message(format!("This is `{lhs_name}`.")))
            .with_annotation(Annotation::secondary(binary.rhs.span()).with_message(format!("This is `{rhs_name}`."))),
    );
}

/// Reports `selector`, the function the PHP# collection member `member` orders by, as `sortedBy` does, when `<` cannot
/// compare two keys it returns, of `key_type`, with the report `<` gives. A `null` key sorts before every other key, so
/// only the other keys must order.
pub(crate) fn check_ordered_key<A>(
    context: &mut Context<'_, '_, A>,
    member: &str,
    selector: &Expression<'_>,
    key_type: &TUnion,
) where
    A: Arena,
{
    let key_name = display_operand(context, key_type);
    let key_type = key_type.to_non_nullable();
    let issue = match ordering_refusal(&key_type, &key_type, context.codebase) {
        None => {
            let ordering = BinaryOperator::LessThan(Span::zero());
            let operand = format!("The `{member}` key");
            check_comparison_operand(context, selector, &key_type, &key_type, &operand, &ordering);

            return;
        }
        Some(Refusal::DifferentTypes) => {
            let comparison = format!("`{member}` cannot compare two keys of `{}`", display_operand(context, &key_type));

            different_types_issue(&comparison, "Convert the key so every key has the same type.")
        }
        Some(refusal @ Refusal::Instance) => {
            unordered_issue(member, &display_refused_part(context, &key_type, Comparand::is_instance), refusal, None)
        }
        Some(refusal @ Refusal::UnorderedOperand) => {
            let unordered_enum = unordered_enum(context, &key_type, &key_type);
            let part = display_refused_part(context, &key_type, Comparand::is_unordered);

            unordered_issue(member, &part, refusal, unordered_enum.as_deref())
        }
        Some(Refusal::Tied) => {
            let ordering = BinaryOperator::Spaceship(Span::zero());
            let Some(DeclaredOperator::Tied(tied)) =
                get_comparison_method(&ordering, &key_type, &key_type, context.codebase)
            else {
                unreachable!("the refusal is a tie of the key type's operators");
            };
            report_tied_operator(context, &ordering, &[&key_type, &key_type], &tied, selector.span());

            return;
        }
        Some(
            Refusal::Identity
            | Refusal::StructIdentity
            | Refusal::StructProperty
            | Refusal::CollectionElement
            | Refusal::Collection
            | Refusal::UncheckedObject
            | Refusal::NullableOperand
            | Refusal::TypeParameter,
        ) => {
            unreachable!("an ordering of a key that is not `null` is refused only for its order or its types")
        }
    };

    context.collector.report_with_code(
        IssueCode::InvalidArgument,
        issue.with_annotation(Annotation::primary(selector.span()).with_message(format!("This returns `{key_name}`."))),
    );
}

/// What a PHP# search compares its value with by `==`, spec section 12, which decides the `===` test a refused search's
/// help offers in its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Searched {
    /// Each element of a `List` or a `Set`, which its `any` reaches, as `carts.any(c => c === cart)`.
    Elements,
    /// Each key of a `Map`, which its `keys()` reaches, as `counts.keys().any(c => c === cart)`, since its `any` reaches
    /// its values.
    Keys,
    /// Each value an `Iterable` gives a loop, which its `toList()` reaches, as `rows.toList().any(c => c === cart)`,
    /// since an `Iterable` has no `any`.
    Iterated,
    /// The elements of a receiver of several kinds, as a `List<Cart>|Map<Cart, int>`, which reach them through
    /// different members, so no one `===` test reaches them all.
    Kinds,
    /// The elements of the list a spread passes, as `contains(...[cart])`, which are no one value a `===` tests.
    Spread,
}

impl Searched {
    /// What a search of a value of `receiver_type`, a `List`, `Iterable`, `Map` or `Set`, or a union of them, compares
    /// its value with.
    pub(crate) fn of(receiver_type: &TUnion) -> Searched {
        receiver_type
            .types
            .iter()
            .filter_map(|atomic| match atomic {
                TAtomic::Array(TArray::Keyed(_)) => Some(Searched::Keys),
                TAtomic::Iterable(_) => Some(Searched::Iterated),
                TAtomic::Array(_) => Some(Searched::Elements),
                _ => None,
            })
            .reduce(Searched::join)
            .unwrap_or(Searched::Elements)
    }

    /// What two searches with one argument compare it with together: what both compare it with, or `Kinds` where they
    /// differ.
    pub(crate) fn join(self, other: Searched) -> Searched {
        if self == other { self } else { Searched::Kinds }
    }

    /// The members the help's `===` test runs through on the receiver, as `any` or `keys().any`, or none.
    const fn identity_test(self) -> Option<&'static str> {
        match self {
            Searched::Elements => Some("any"),
            Searched::Keys => Some("keys().any"),
            Searched::Iterated => Some("toList().any"),
            Searched::Kinds | Searched::Spread => None,
        }
    }
}

/// Why a search of a PHP# collection for a value of `value_type` cannot compare an element of `element_type` with it,
/// or `None` when it can, as spec section 12 decides: "`contains`, a `List`'s `remove`, `indexOf` and `Set<T>` need
/// `==` on the element type." A search refuses what `element == value` refuses, so it agrees with
/// `any(element => element == value)`. `==` checks the operands of a declared operator as the arguments of its call,
/// so `Money::op_Equality` refuses a `Money|Line` element as `element == money` does. An `Any` element is refused
/// only by the value it is compared with, which [`check_element_equality`] checks.
pub(crate) fn search_refusal(
    element_type: &TUnion,
    value_type: &TUnion,
    codebase: &CodebaseMetadata,
) -> Option<Refusal> {
    // A search is always written in the source, so it skips the check `sharp_refusal` makes for a comparison no source
    // writes, and `refusal` reads no span.
    let equality = BinaryOperator::Equal(Span::zero());

    refusal(&equality, element_type, value_type, codebase, &mut Vec::new()).or_else(|| {
        let method = get_comparison_method(&equality, element_type, value_type, codebase)?.chosen()?;
        let operands = [&element_type.to_non_nullable(), &value_type.to_non_nullable()];

        (!element_type.is_mixed() && !takes_operands(codebase, method, &operands)).then_some(Refusal::DifferentTypes)
    })
}

/// Reports `value`, of `value_type`, which a PHP# `List`'s `method` searches `receiver` for, when `==` cannot compare
/// an element of `element_type` with it, as [`search_refusal`] decides: a class that declares no `operator ==` is
/// refused with the reason `==` gives, as is an element the operator's call does not always take, an `Iterable`
/// element is refused, as `==` on it is, and an `Any` element is refused unless the value is a string, a number or an
/// enum case. A `null_safe` call's help keeps its `?.`. What is `searched` decides the `===` test the help offers
/// instead, if any. Gives whether it refused the search.
pub(crate) fn check_element_equality<A>(
    context: &mut Context<'_, '_, A>,
    method: &str,
    receiver: &Expression<'_>,
    value: &Expression<'_>,
    element_type: &TUnion,
    value_type: &TUnion,
    null_safe: bool,
    searched: Searched,
) -> bool
where
    A: Arena,
{
    let equality = BinaryOperator::Equal(value.span());
    let (element_name, value_name) = (
        display_operand(context, &element_type.to_non_nullable()),
        display_operand(context, &value_type.to_non_nullable()),
    );
    let refusal = search_refusal(element_type, value_type, context.codebase);
    let (code, issue) = match refusal {
        None => {
            if element_type.is_mixed() {
                let subject = format!("Each element of `List<{}>`", display_operand(context, element_type));
                check_comparison_operand(context, value, element_type, value_type, &subject, &equality);
            }

            return false;
        }
        Some(Refusal::Instance) => {
            let class = display_refused_side(context, element_type, value_type, Comparand::is_instance);
            let help = match searched.identity_test() {
                None => format!("Declare `operator ==` and `hash()` in `{class}`."),
                Some(test) => {
                    let (receiver, value) =
                        (quoted_source(context, receiver.span()), quoted_source(context, value.span()));
                    // The lambda names an element by the class's initial, as `l` for a `Line`, unless the call already
                    // uses it.
                    let initial =
                        class.chars().next().map(|first| first.to_lowercase().to_string()).unwrap_or_default();
                    let element = if initial == receiver || initial == value { "element".to_owned() } else { initial };
                    let access = if null_safe { "?." } else { "." };

                    format!(
                        "Declare `operator ==` and `hash()` in `{class}`, or test for the same object with \
                         `{receiver}{access}{test}({element} => {element} === {value})`."
                    )
                }
            };

            (
                IssueCode::InvalidArgument,
                Issue::error(format!(
                    "`{method}` compares elements with `==`, and `{class}` declares no `operator ==`."
                ))
                .with_note("`==` on a class instance exists only where its class declares `operator ==`.")
                .with_help(help),
            )
        }
        Some(Refusal::UncheckedObject) => {
            let (receiver, value) = (quoted_source(context, receiver.span()), quoted_source(context, value.span()));
            let access = if null_safe { "?." } else { "." };
            let element = display_operand(context, element_type);
            let same_object =
                if searched == Searched::Spread { format!("an element of `{value}`") } else { format!("`{value}`") };
            let issue = Issue::error(format!(
                "`{method}` compares elements with `==`, and an unchecked `{element}` element can't tell the same object as {same_object} from an equal value."
            ));
            let issue = match searched.identity_test() {
                Some(test) => issue.with_help(format!(
                    "Test for the same object with `{receiver}{access}{test}(element => element === {value})`."
                )),
                None => issue,
            };

            (IssueCode::InvalidArgument, issue)
        }
        Some(Refusal::StructProperty) => {
            let issue = struct_property_issue(context, element_type, value_type);

            (
                IssueCode::InvalidArgument,
                Issue::error(format!("`{method}` compares elements with `==`, and {}", issue.message))
                    .with_note("A search compares each element with the value by `==`."),
            )
        }
        Some(Refusal::CollectionElement) => (
            IssueCode::InvalidArgument,
            Issue::error(format!(
                "`{method}` compares elements with `==`, and {}.",
                element_reason(context, &equality, element_type, value_type)
            ))
            .with_note("A search compares each element with the value by `==`."),
        ),
        Some(Refusal::Collection) => {
            let part = unsupported_collection(element_type, value_type, context.codebase);

            (
                IssueCode::InvalidArgument,
                Issue::error(format!(
                    "`{method}` compares elements with `==`, and {}.",
                    collection_reason(context, part)
                ))
                .with_help("Compare each element's `toList()` with `==`, as in `a.toList() == b.toList()`."),
            )
        }
        // A struct equals only the same struct, and declares no operator to change that.
        Some(Refusal::DifferentTypes)
            if comparands(element_type, context.codebase)
                .into_iter()
                .chain(comparands(value_type, context.codebase))
                .any(|comparand| matches!(comparand, Comparand::Struct(_))) =>
        {
            let comparison = format!(
                "`{method}` compares elements with `==`, which cannot compare `{element_name}` with `{value_name}`"
            );

            (IssueCode::InvalidArgument, different_types_issue(&comparison, "Search a list of one struct type."))
        }
        Some(Refusal::DifferentTypes) => {
            // The value is an element, so its class is the one whose operator runs.
            let class = display_refused_side(context, value_type, element_type, Comparand::is_instance);

            (
                IssueCode::InvalidArgument,
                Issue::error(format!(
                    "`{method}` compares elements with `==`, and the `operator ==` of `{class}` does not take `{element_name}` and `{value_name}`."
                ))
                .with_note("A search compares each element with the value by `==`.")
                .with_help(format!("Make the `operator ==` of `{class}` take `{element_name}` and `{value_name}`.")),
            )
        }
        Some(Refusal::Tied) => {
            let operands = [&element_type.to_non_nullable(), &value_type.to_non_nullable()];
            let Some(DeclaredOperator::Tied(tied)) =
                get_comparison_method(&equality, operands[0], operands[1], context.codebase)
            else {
                unreachable!("the refusal is a tie of the element's and the value's operators");
            };
            report_tied_operator(context, &equality, &operands, &tied, value.span());

            return true;
        }
        Some(Refusal::TypeParameter) => (
            IssueCode::InvalidArgument,
            Issue::error(format!(
                "`{method}` compares elements with `==`, and {}.",
                type_parameter_reason(element_type, value_type, context.codebase)
            ))
            .with_note(TYPE_PARAMETER_EQUALITY),
        ),
        Some(Refusal::Identity | Refusal::StructIdentity | Refusal::NullableOperand | Refusal::UnorderedOperand) => {
            unreachable!("`==` is refused only for a collection, an instance, or the operands of its operator")
        }
    };

    context.collector.report_with_code(
        code,
        issue.with_annotation(
            Annotation::primary(value.span())
                .with_message(format!("Each `{element_name}` element is compared with this by `==`.")),
        ),
    );

    true
}

/// The first property of the struct `name`, in declaration order, or field of a case of the enum `name`, that its own
/// `==` cannot compare with another of its type, with why, spec sections 10 and 20: a struct or a case compares by
/// value only when each of its properties does, a property of a generic struct's type parameter as the type argument
/// `type_of` gives the struct, so `Pair<int, string>` compares and `Pair<Order, int>` does not when `Order` declares no
/// `operator ==`, and a field of a generic enum's case as the type argument `type_of` gives the enum, spec section 31,
/// with the property's type as those type arguments give it. `visiting` holds the structs and enums being checked, so
/// one that holds itself, as in `Node? next`, compares by the rest. It alone decides whether a struct or a case has
/// `==`, for `==`, a search, a `Map` key, a `Set` element and a type argument of `Equatable`, each property by
/// [`equality_refusal`].
pub(crate) fn struct_property_refusal<'codebase>(
    name: Word,
    type_of: &TUnion,
    codebase: &'codebase CodebaseMetadata,
    visiting: &mut Vec<Word>,
) -> Option<(&'codebase PropertyMetadata, TUnion, Option<Refusal>)> {
    let class = codebase.get_class_like(name.as_bytes())?;
    if visiting.contains(&class.name) {
        return None;
    }
    let properties = value_properties(codebase, class)?;
    let type_arguments = type_of.types.iter().find_map(|atomic| match atomic {
        TAtomic::Object(TObject::Named(named)) if named.name.as_bytes().eq_ignore_ascii_case(class.name.as_bytes()) => {
            named.type_parameters.as_deref()
        }
        _ => None,
    });
    let type_arguments = class_type_arguments(class, type_arguments.unwrap_or_default());
    // A field of a case of a generic enum reads the type arguments `type_of` gives the enum, also through a case's
    // value, as `Ok`'s `value` is an `int` in a `Result<int, PaymentError>.Ok`, spec section 31.
    let enum_type_arguments =
        data_enum(codebase, class).and_then(|_| enum_type_arguments(class.name, [type_of], codebase));
    visiting.push(class.name);
    // The span only marks each property's `==` as written in the source, which `refusal` checks. A field has its name's
    // span but no span of its own, so each one is marked at its struct or case.
    let equality = BinaryOperator::Equal(class.span);

    let refused = properties.into_iter().find_map(|property| {
        let declared = &property.type_metadata.as_ref().or(property.type_declaration_metadata.as_ref())?.type_union;
        let r#type = match &enum_type_arguments {
            Some(enum_type_arguments) => with_type_arguments(declared, class.name, Some(enum_type_arguments), codebase),
            None => inferred_type_replacer::replace(declared, &type_arguments, codebase),
        };
        equality_refusal(&equality, &r#type, codebase, visiting).map(|reason| (property, r#type, reason))
    });
    visiting.pop();

    refused
}

/// Why `==`, written as `equality`, cannot compare two values of `r#type`, spec section 19, or `None` when it can: the
/// one check of whether a type has `==`, which a struct's property, a case's field and a type argument of `Equatable`
/// each pass. The reason is `None` for an `Any`, which `==` compares by value only with a value of a known type. A type
/// parameter's bound decides its own, spec section 11.
fn equality_refusal(
    equality: &BinaryOperator<'_>,
    r#type: &TUnion,
    codebase: &CodebaseMetadata,
    visiting: &mut Vec<Word>,
) -> Option<Option<Refusal>> {
    let holds_type_parameter = r#type.types.iter().any(|atomic| matches!(atomic, TAtomic::GenericParameter(_)));
    if !holds_type_parameter && comparands(r#type, codebase).contains(&Comparand::Any) {
        return Some(None);
    }

    refusal(equality, r#type, r#type, codebase, visiting).map(Some)
}

/// Why `r#type`, a type argument a call, a value or a type gives a type parameter bounded by `Equatable`, does not
/// satisfy it, spec section 11, or `None` when it does: "`Cart` declares no `operator ==`". A type argument of
/// `Equatable` has `==`, as [`equality_refusal`] decides, and keys a `Map` and fills a `Set`, as [`is_key_type`]
/// decides, so a class value, which has `==` but keys no `Map`, does not satisfy it. `span` marks where the type
/// argument is written or fixed.
pub(crate) fn missing_equality<A>(context: &Context<'_, '_, A>, r#type: &TUnion, span: Span) -> Option<String>
where
    A: Arena,
{
    let codebase = context.codebase;
    let equality = BinaryOperator::Equal(span);
    let r#type = r#type.to_non_nullable();
    let name = display_operand(context, &r#type);
    let Some(reason) = equality_refusal(&equality, &r#type, codebase, &mut Vec::new()) else {
        return (!is_key_type(&r#type, codebase)).then(|| key_reason(context, &r#type));
    };

    Some(match reason {
        None => format!("`{name}` {ANY_EQUALITY}"),
        Some(Refusal::TypeParameter) => {
            let parameter =
                unequatable_parameter(&r#type, codebase).map_or_else(|| name.clone(), |name| name.to_string());

            format!("`{parameter}` is {}", unbounded_parameter_reason(&parameter))
        }
        Some(Refusal::Instance) if is_function(&r#type) => "a function has no `==`".to_owned(),
        Some(Refusal::DifferentTypes) if r#type.types.len() > 1 => {
            format!("the values of `{name}` may be of two different types, which never match")
        }
        refused => refusal_reason(context, &equality, &r#type, &r#type, refused),
    })
}

/// Why a value of `r#type`, which has `==`, keys no `Map`, spec sections 11 and 12, as "a class value with type
/// arguments can't key a `Map`": a class value runs as its plain class name, which `Class<Box<Order>>` and
/// `Class<Box<Invoice>>` share.
fn key_reason<A>(context: &Context<'_, '_, A>, r#type: &TUnion) -> String
where
    A: Arena,
{
    let has_type_arguments = r#type.types.iter().any(|atomic| match atomic {
        TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::OfType { constraint, .. })) => {
            constraint.types.iter().any(|atomic| {
                matches!(atomic, TAtomic::Object(TObject::Named(named)) if named.type_parameters.as_ref().is_some_and(|arguments| !arguments.is_empty()))
            })
        }
        _ => false,
    });

    if has_type_arguments {
        "a class value with type arguments can't key a `Map`".to_owned()
    } else {
        format!("`{}` can't key a `Map`", display_operand(context, r#type))
    }
}

/// The type arguments a value of one of `unions` gives the generic enum `name`, as a `Result<int, PaymentError>.Ok`
/// gives `Result` `int` and `PaymentError`, spec section 31. None for an enum without type parameters.
fn enum_type_arguments<'union>(
    name: Word,
    unions: impl IntoIterator<Item = &'union TUnion>,
    codebase: &CodebaseMetadata,
) -> Option<Vec<TUnion>> {
    let r#enum = codebase.get_class_like(name.as_bytes())?;

    unions
        .into_iter()
        .flat_map(|union| union.types.iter())
        .find_map(|atomic| ancestor_type_arguments(codebase, atomic, r#enum))
}

/// The issue that refuses `==` on the struct or the enum one of `lhs_type` and `rhs_type` holds, naming the first
/// property or field its own `==` cannot compare, as "`RenewRequest` can't compare by value: `plan` is a `Plan`, which
/// declares no `operator ==`." A function has no equality, as "`Spot` can't be compared: `onMove` is a function."
fn struct_property_issue<A>(context: &Context<'_, '_, A>, lhs_type: &TUnion, rhs_type: &TUnion) -> Issue
where
    A: Arena,
{
    let codebase = context.codebase;
    let sides = [lhs_type, rhs_type].map(|side| (side, comparands(side, codebase)));
    let Some((name, property, property_type, reason)) = sides
        .iter()
        .flat_map(|(side, found)| found.iter().map(move |comparand| (*side, *comparand)))
        .find_map(|(side, comparand)| {
            let (Comparand::Struct(name) | Comparand::Enum(name)) = comparand else {
                return None;
            };
            let (property, property_type, reason) = struct_property_refusal(name, side, codebase, &mut Vec::new())?;

            Some((name, property, property_type, reason))
        })
    else {
        unreachable!("`==` is refused for a struct's property only when one of its properties cannot compare");
    };
    let r#struct = display_operand(context, &get_named_object(name, None));
    let (value, note) = if codebase.get_class_like(name.as_bytes()).is_some_and(|class| class.flags.is_struct()) {
        ("struct", "A struct compares by value, property by property, with each property's own `==`.")
    } else {
        ("case", "An enum case compares by value, field by field, with each field's own `==`.")
    };
    let why = property_reason(context, property, &property_type, reason);
    if is_function(&property_type) {
        return Issue::error(format!("`{struct}` can't be compared: {why}.", struct = r#struct))
            .with_note(format!("A function value has no equality, so a {value} that holds one has none."));
    }

    let issue = Issue::error(format!("`{struct}` can't compare by value: {why}.", struct = r#struct)).with_note(note);

    match reason {
        Some(Refusal::Collection) => {
            issue.with_note(format!("{}.", collection_reason(context, &property_type.to_non_nullable())))
        }
        _ => issue,
    }
}

/// Why `==` cannot compare `property`, of `property_type`, as [`struct_property_refusal`] gives `reason`: "`plan` is a
/// `Plan`, which declares no `operator ==`", or "`onMove` is a function", which has no equality.
pub(crate) fn property_reason<A>(
    context: &Context<'_, '_, A>,
    property: &PropertyMetadata,
    property_type: &TUnion,
    reason: Option<Refusal>,
) -> String
where
    A: Arena,
{
    let name = property.name.0.as_bytes();
    let name = BytesDisplay(name.strip_prefix(b"$").unwrap_or(name));
    if is_function(property_type) {
        return format!("`{name}` is a function");
    }
    let r#type = display_operand(context, &property_type.to_non_nullable());
    let article = article(&r#type);
    let why = match reason {
        Some(Refusal::Instance) => "which declares no `operator ==`",
        Some(Refusal::Collection) => "which `==` does not compare",
        Some(Refusal::CollectionElement) => "whose elements `==` cannot compare",
        Some(Refusal::StructProperty) => "which can't compare by value",
        Some(Refusal::TypeParameter) => {
            return format!("`{name}` is {article} `{type}`, {}", unbounded_parameter_reason(&r#type), type = r#type);
        }
        Some(_) => "which `==` cannot compare with another of its type",
        None => return format!("`{name}` is {article} `{type}`, which {ANY_EQUALITY}", type = r#type),
    };

    format!("`{name}` is {article} `{type}`, {why}", type = r#type)
}

/// Why a value of the type parameter `name` has no `==`, with the fix, spec section 11: "a type parameter with no
/// `Equatable` bound: bound it with `Equatable`, as in `<T : Equatable>`".
pub(crate) fn unbounded_parameter_reason(name: &str) -> String {
    format!("a type parameter with no `Equatable` bound: bound it with `Equatable`, as in `<{name} : Equatable>`")
}

/// Whether a value of `r#type` may be a function.
fn is_function(r#type: &TUnion) -> bool {
    r#type.types.iter().any(|atomic| matches!(atomic, TAtomic::Callable(_)))
}

/// The issue that refuses `comparison`, of values of two types that never match, with `help` naming what to convert.
fn different_types_issue(comparison: &str, help: &str) -> Issue {
    Issue::error(format!("{comparison}."))
        .with_note("PHP# compares values strictly, so values of two different types never match.")
        .with_help(help)
}

/// What `==` compares an `Any` with, spec section 19, as a refusal of `==` on one names it.
const ANY_EQUALITY: &str =
    "compares by value only with a string, a number, a `bool`, an enum case, a struct or a collection";

/// What PHP# orders, spec section 19, as an ordering's refusal names it.
const ORDERED_TYPES: &str = "numbers, strings, and classes, structs and enums that declare `operator <=>`";

/// The rule [`Refusal::TypeParameter`] refuses `==` by, spec section 11.
const TYPE_PARAMETER_EQUALITY: &str = "A type parameter has `==` only through its bound: `Equatable`, or a class or interface that declares `operator ==`.";

/// Why `==` cannot compare a value of `lhs_type` with one of `rhs_type`, which [`Refusal::TypeParameter`] refuses, as
/// "`T` has no `==`: bound it with `Equatable`, as in `<T : Equatable>`".
fn type_parameter_reason(lhs_type: &TUnion, rhs_type: &TUnion, codebase: &CodebaseMetadata) -> String {
    let Some(name) = unequatable_parameter(lhs_type, codebase).or_else(|| unequatable_parameter(rhs_type, codebase))
    else {
        unreachable!("`==` is refused for a type parameter only when a side may be one without `==`");
    };

    format!("`{name}` has no `==`: bound it with `Equatable`, as in `<{name} : Equatable>`")
}

/// The issue that refuses ordering a value whose part `part` has no order by `orderer`, an operator or `sortedBy`, as
/// `refusal` gives the reason: a `bool`, a case of an enum that declares no `operator <=>`, `unordered_enum` when it
/// is one, or a collection, or an instance of a class or a struct that declares none.
fn unordered_issue(
    orderer: impl std::fmt::Display,
    part: &str,
    refusal: Refusal,
    unordered_enum: Option<&str>,
) -> Issue {
    let note = if matches!(refusal, Refusal::Instance) {
        "A class instance or a struct has an order only where its type declares `operator <=>`."
    } else {
        "A `bool` and a collection have no order, and an enum's cases have one only where it declares `operator <=>`."
    };
    let help = match unordered_enum {
        Some(r#enum) => format!(
            "Declare `public static int operator <=>({enum} a, {enum} b)` in `{enum}`: an enum orders its cases once it declares `operator <=>`.",
            r#enum = r#enum
        ),
        None => "Order a number or a string taken from the value instead.".to_owned(),
    };

    Issue::error(format!("`{orderer}` orders {ORDERED_TYPES}, and `{part}` is none of them."))
        .with_note(note)
        .with_help(help)
}

/// The enum a case of `lhs_type` or `rhs_type` belongs to when its cases have no order, as `Status`, for the help
/// that names the `operator <=>` it declares to order them.
fn unordered_enum<A>(context: &Context<'_, '_, A>, lhs_type: &TUnion, rhs_type: &TUnion) -> Option<String>
where
    A: Arena,
{
    comparands(lhs_type, context.codebase).into_iter().chain(comparands(rhs_type, context.codebase)).find_map(
        |comparand| match comparand {
            Comparand::Enum(name) => Some(display_class_like_name(context, name).to_string()),
            _ => None,
        },
    )
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

/// The part of the operand of `lhs_type`, or else of `rhs_type`, that holds a comparand `refused` accepts.
fn display_refused_side<A>(
    context: &Context<'_, '_, A>,
    lhs_type: &TUnion,
    rhs_type: &TUnion,
    refused: fn(Comparand) -> bool,
) -> String
where
    A: Arena,
{
    let side =
        if lhs_type.types.iter().any(|atomic| holds(atomic, refused, context.codebase)) { lhs_type } else { rhs_type };

    display_refused_part(context, side, refused)
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
        operand_type.is_never_null() && !operand_type.possibly_undefined() && !operand_type.is_never()
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
        Pattern::List(list) => list.elements.iter().find_map(|element| match element {
            ListPatternElement::Pattern(pattern) => written_words(pattern, Vec::new(), value),
            ListPatternElement::Rest(_) => None,
        }),
        Pattern::Type(TypePattern { fields, properties, .. }) if pattern.span() != value => fields
            .iter()
            .flat_map(|fields| fields.fields.iter())
            .find_map(|field| written_words(field, Vec::new(), value))
            .or_else(|| {
                properties
                    .iter()
                    .flat_map(|properties| properties.properties.iter())
                    .find_map(|property| written_words(property.pattern, Vec::new(), value))
            }),
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
