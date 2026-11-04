//! Literals and string concatenation (spec section 18): `.` between two strings is `+`, and with a number a template.

use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LiteralStringKind;

use super::Kind;
use super::kinds;
use crate::Converter;

impl Converter<'_, '_> {
    pub(super) fn literal(&mut self, literal: &Literal<'_>, expected: Option<&TUnion>) {
        match literal {
            Literal::True(keyword) => self.edits.replace(keyword.span, "true"),
            Literal::False(keyword) => self.edits.replace(keyword.span, "false"),
            Literal::Null(keyword) => self.edits.replace(keyword.span, "null"),
            // PHP turns an `int` that lands in a `float` into a `float`, and PHP# writes the `float` (spec section 24).
            Literal::Integer(integer)
                if integer.raw.iter().all(u8::is_ascii_digit)
                    && expected.is_some_and(|union| kinds(union) == [Kind::Float]) =>
            {
                self.edits.replace(integer.span, format!("{}.0", String::from_utf8_lossy(integer.raw)));
            }
            Literal::String(_) | Literal::Integer(_) | Literal::Float(_) => {}
        }
    }

    /// A chain of `.`: `+` between strings (spec section 18), or a template when a part is a number, which a template
    /// shows as PHP shows it.
    pub(super) fn concatenation(&mut self, expression: &Expression<'_>) {
        let mut parts = Vec::new();
        flatten_concatenation(expression, &mut parts);

        if parts.iter().all(|part| self.is(part, &[Kind::String])) {
            let mut operators = Vec::new();
            collect_concatenation_operators(expression, &mut operators);
            for span in operators {
                self.edits.replace(span, "+");
            }
            for part in parts {
                self.expression(part, None);
            }
            return;
        }

        let mut template = String::from("`");
        for part in parts {
            if let Expression::Literal(Literal::String(string)) = part
                && let Some(value) = string.value
                && (string.kind == LiteralStringKind::SingleQuoted || !string.raw.contains(&b'\\'))
                && !value.iter().any(|byte| matches!(byte, b'`' | b'$' | b'\\'))
            {
                template.push_str(&String::from_utf8_lossy(value));
                continue;
            }
            if !self.is(part, &[Kind::Int, Kind::Float, Kind::String]) {
                let shown = self.shown(part);
                return self.refuse(
                    part.span(),
                    format!("`.` of `{shown}` shows it PHP's way, and a template shows only `int`, `float` and `string` that way (spec section 18)"),
                );
            }
            self.expression(part, None);
            let rendered = self.render(part);
            template.push_str("${");
            template.push_str(&rendered);
            template.push('}');
        }
        template.push('`');
        self.edits.replace(expression.span(), template);
    }
}

fn flatten_concatenation<'expression, 'arena>(
    expression: &'expression Expression<'arena>,
    parts: &mut Vec<&'expression Expression<'arena>>,
) {
    match expression {
        Expression::Binary(Binary { lhs, operator: BinaryOperator::StringConcat(_), rhs }) => {
            flatten_concatenation(lhs, parts);
            flatten_concatenation(rhs, parts);
        }
        _ => parts.push(expression),
    }
}

fn collect_concatenation_operators(expression: &Expression<'_>, operators: &mut Vec<Span>) {
    if let Expression::Binary(Binary { lhs, operator: BinaryOperator::StringConcat(span), rhs }) = expression {
        collect_concatenation_operators(lhs, operators);
        operators.push(*span);
        collect_concatenation_operators(rhs, operators);
    }
}
