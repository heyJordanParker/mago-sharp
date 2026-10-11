use diffy::PatchFormatter;
use mago_allocator::Arena;

use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::scalar::int::TInteger;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::Span;

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

/// Reports a PHP# `int` at `span` that lands where `declared` expects a `float`, with the `code` of the place it lands.
/// An int literal is named in its float form, so `2` reads "write 2.0".
pub fn report_int_for_float<A>(
    context: &mut Context<'_, '_, A>,
    code: IssueCode,
    span: Span,
    integer: TInteger,
    declared: &TUnion,
) where
    A: Arena,
{
    let float = integer.get_literal_value().map_or_else(|| "0.0".to_owned(), |value| format!("{value}.0"));
    let declared = names::display_type(context, declared);

    context.collector.report_with_code(
        code,
        Issue::error(format!("`int` is not `float`: write {float}, or convert with (float) n"))
            .with_annotation(Annotation::primary(span).with_message(format!("Declared `{declared}`."))),
    );
}
