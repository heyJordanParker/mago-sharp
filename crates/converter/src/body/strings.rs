//! Literals and string concatenation (spec section 18): `.` between two strings is `+`, and with a number a template,
//! as an interpolated string and a heredoc are.

use std::fmt::Write;

use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::CompositeString;
use mago_syntax::cst::DocumentIndentation;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LiteralStringKind;
use mago_syntax::cst::StringPart;

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

    /// `"Hi $name"`, a heredoc and a nowdoc as a template, `` `Hi ${name}` `` (spec section 18). A template shows an
    /// `int`, a `float` and a `string` as PHP does, so each value it shows is one of them. A heredoc's text loses the
    /// indentation of its closing label and the line break before it, as PHP's does.
    pub(super) fn template(&mut self, expression: &Expression<'_>, string: &CompositeString<'_>) {
        let (parts, indentation) = match string {
            CompositeString::Interpolated(string) => (&string.parts, 0),
            CompositeString::Document(document) => match document.indentation {
                DocumentIndentation::None => (&document.parts, 0),
                DocumentIndentation::Whitespace(count) | DocumentIndentation::Tab(count) => (&document.parts, count),
                DocumentIndentation::Mixed(..) => {
                    return self.refuse(
                        expression.span(),
                        "a heredoc indented with both spaces and tabs has no template form: indent it with one of them",
                    );
                }
            },
            CompositeString::ShellExecute(_) => {
                return self.refuse(expression.span(), "a shell command in backticks has no PHP# form");
            }
        };

        let mut template = String::from("`");
        // A heredoc's text starts a line, and text after a value continues the value's line.
        let mut starts_line = true;
        let last = parts.len().saturating_sub(1);
        for (index, part) in parts.iter().enumerate() {
            let value = match part {
                StringPart::Literal(literal) => {
                    let Some(value) = literal.value else {
                        return self.refuse(literal.span, "the analysis reads no text in this part of the string");
                    };
                    let value = match string {
                        CompositeString::Document(_) if index == last => value.strip_suffix(b"\n").unwrap_or(value),
                        _ => value,
                    };
                    let Some(written) = template_text(&dedent(value, indentation, starts_line)) else {
                        return self.refuse(literal.span, "a string of bytes that are no UTF-8 has no template form");
                    };
                    template.push_str(&written);
                    continue;
                }
                StringPart::Expression(value) => *value,
                StringPart::BracedExpression(braced) => braced.expression,
            };
            starts_line = false;
            if !self.is(value, &[Kind::Int, Kind::Float, Kind::String]) {
                let shown = self.shown(value);
                return self.refuse(
                    value.span(),
                    format!("a string shows `{shown}` PHP's way, and a template shows only `int`, `float` and `string` that way (spec section 18)"),
                );
            }
            self.expression(value, None);
            let rendered = self.render(value);
            template.push_str("${");
            template.push_str(&rendered);
            template.push('}');
        }
        template.push('`');
        self.edits.replace(expression.span(), template);
    }
}

/// `text` with up to `indentation` spaces or tabs taken from the start of each line, as PHP takes a heredoc's closing
/// label's indentation from each of its lines. Its first line is a line of its own only when `starts_line` says so.
fn dedent(text: &[u8], indentation: usize, starts_line: bool) -> Vec<u8> {
    let mut dedented = Vec::with_capacity(text.len());
    for (index, line) in text.split(|byte| *byte == b'\n').enumerate() {
        if index > 0 {
            dedented.push(b'\n');
        }
        let skipped = if index > 0 || starts_line {
            line.iter().take(indentation).take_while(|byte| matches!(byte, b' ' | b'\t')).count()
        } else {
            0
        };
        dedented.extend_from_slice(&line[skipped..]);
    }

    dedented
}

/// `text` as a template writes it, with JavaScript's escapes for `\`, a backtick, `$` and the control characters a
/// template doesn't keep as they are, or `None` when `text` is no UTF-8.
fn template_text(text: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(text).ok()?;
    let mut written = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => written.push_str("\\\\"),
            '`' => written.push_str("\\`"),
            '$' => written.push_str("\\$"),
            '\r' => written.push_str("\\r"),
            '\n' | '\t' => written.push(character),
            control if control.is_control() => {
                let _ = write!(written, "\\u{{{:x}}}", u32::from(control));
            }
            other => written.push(other),
        }
    }

    Some(written)
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
