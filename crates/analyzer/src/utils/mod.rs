use diffy::PatchFormatter;
use mago_allocator::Arena;

use mago_codex::ttype::TType;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LiteralInteger;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;

use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;

pub mod availability;
pub mod casing;
pub mod conditional;
pub mod docblock;
pub mod experimental;
pub mod expression;
pub mod misc;
pub mod missing_type_hints;
pub mod names;
pub mod php_emulation;
pub mod symbol_existence;
pub mod template;

/// Generates a diff between two complex types if both are complex.
///
/// # Arguments
///
/// * `context` - The analysis context containing settings.
/// * `container` - The first type to compare (e.g., a parameter type).
/// * `input` - The second type to compare (e.g., an argument type).
///
/// # Returns
///
/// An `Option<String>` containing the formatted diff if both types are complex, or `None` otherwise.
pub fn get_type_diff<A>(context: &Context<'_, '_, A>, container: &TUnion, input: &TUnion) -> Option<String>
where
    A: Arena,
{
    if !container.is_complex() || !input.is_complex() {
        return None;
    }

    let mut formatter = PatchFormatter::new().missing_newline_message(false).suppress_blank_empty(false);
    if context.settings.use_colors {
        formatter = formatter.with_color();
    }

    let (container_id, input_id) = if context.dialect.is_sharp() {
        (names::display_sharp_type(context, container), names::display_sharp_type(context, input))
    } else {
        (container.get_pretty_id().to_string(), input.get_pretty_id().to_string())
    };
    let patch = diffy::create_patch(&container_id, &input_id);
    let diff = formatter.fmt_patch(&patch);

    Some(format!("{diff}"))
}

/// The `int` a failed PHP# comparison of `value` refused only because a `float` was expected: `value` itself, or the
/// first element of a literal list or map in it whose type is an `int`. A value of any other type, such as a
/// collection of ints, has none, so its place keeps the message of any value of the wrong type.
pub fn int_refused_for_float<'expression, 'arena>(
    comparison_result: &ComparisonResult,
    artifacts: &AnalysisArtifacts,
    value: &'expression Expression<'arena>,
) -> Option<&'expression Expression<'arena>> {
    if !comparison_result.int_for_float {
        return None;
    }

    match value.unparenthesized() {
        Expression::Array(literal) => literal.elements.iter().find_map(|element| match element {
            ArrayElement::Value(element) => int_refused_for_float(comparison_result, artifacts, element.value),
            ArrayElement::KeyValue(element) => int_refused_for_float(comparison_result, artifacts, element.value),
            ArrayElement::Variadic(_) | ArrayElement::Missing(_) => None,
        }),
        _ => artifacts.get_expression_type(value).is_some_and(TUnion::has_int).then_some(value),
    }
}

/// Reports `value`, a PHP# `int` that lands where `declared` expects a `float`, with the `code` of the place it lands.
/// The message quotes the one fix for the code written: an int literal's float form, as "write 2.0", or a cast around
/// the expression, as "convert with (float) count".
pub fn report_int_for_float<A>(
    context: &mut Context<'_, '_, A>,
    code: IssueCode,
    value: &Expression<'_>,
    declared: &TUnion,
) where
    A: Arena,
{
    let fix = match float_literal(value) {
        Some(float) => format!("write {float}"),
        None => format!("convert with {}", float_cast(context, value)),
    };
    let declared = names::display_type(context, declared);

    context.collector.report_with_code(
        code,
        Issue::error(format!("`int` is not `float`: {fix}"))
            .with_annotation(Annotation::primary(value.span()).with_message(format!("Declared `{declared}`."))),
    );
}

/// The cast that makes `value` a `float`, around its source text, as `(float) count`. A cast binds tighter than any
/// binary, ternary or assignment operator, so only an operand that binds at least as tightly as the cast is quoted
/// bare, and `a + b` gives `(float) (a + b)`.
pub(crate) fn float_cast<A>(context: &Context<'_, '_, A>, value: &Expression<'_>) -> String
where
    A: Arena,
{
    let text = quoted_source(context, value.span());

    if matches!(
        value,
        Expression::Parenthesized(_)
            | Expression::Literal(_)
            | Expression::Variable(_)
            | Expression::ConstantAccess(_)
            | Expression::Access(_)
            | Expression::Call(_)
            | Expression::ArrayAccess(_)
            | Expression::UnaryPrefix(_)
            | Expression::UnaryPostfix(_)
    ) {
        format!("(float) {text}")
    } else {
        format!("(float) ({text})")
    }
}

/// The source text at `span`, as one line: every run of whitespace that holds a newline becomes one space, so a value
/// written over several lines keeps a message's title on one line.
pub(crate) fn quoted_source<A>(context: &Context<'_, '_, A>, span: Span) -> String
where
    A: Arena,
{
    let text = String::from_utf8_lossy(&context.source_file.contents[span.to_range_usize()]);
    let mut quoted = String::with_capacity(text.len());
    let mut whitespace = String::new();

    for character in text.chars() {
        if character.is_whitespace() {
            whitespace.push(character);
            continue;
        }

        if whitespace.contains('\n') {
            quoted.push(' ');
        } else {
            quoted.push_str(&whitespace);
        }

        whitespace.clear();
        quoted.push(character);
    }

    quoted
}

/// The float form of an int literal, negated or not, as `2` gives `2.0` and `-3` gives `-3.0`. A decimal literal keeps
/// the digits written, so `1_000_000` gives `1_000_000.0`. A hex, octal or binary literal gives its value, as `0x1F`
/// gives `31.0`, since `0x1F.0` is no float and `017.0` is seventeen.
pub(crate) fn float_literal(value: &Expression<'_>) -> Option<String> {
    let integer = |expression: &Expression<'_>| match expression.unparenthesized() {
        Expression::Literal(Literal::Integer(LiteralInteger { raw, value: Some(integer), .. })) => {
            let decimal = *raw == b"0"
                || (raw.first().is_some_and(|digit| matches!(digit, b'1'..=b'9'))
                    && raw.iter().all(|byte| byte.is_ascii_digit() || *byte == b'_'));

            Some(if decimal { String::from_utf8_lossy(raw).into_owned() } else { integer.to_string() })
        }
        _ => None,
    };

    match value.unparenthesized() {
        Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::Negation(_), operand }) => {
            integer(operand).map(|integer| format!("-{integer}.0"))
        }
        literal => integer(literal).map(|integer| format!("{integer}.0")),
    }
}
