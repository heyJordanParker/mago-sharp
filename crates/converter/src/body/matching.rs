//! `match` and `switch` (spec section 21). A PHP arm compares the value with `===`, and a PHP# pattern takes types and
//! constants only, so an arm's constants become patterns joined by `or`, each passing the `===` rule of section 19. A
//! `switch` is a `match` statement with block arms.

use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Break;
use mago_syntax::cst::Continue;
use mago_syntax::cst::DoWhile;
use mago_syntax::cst::Expression;
use mago_syntax::cst::For;
use mago_syntax::cst::Foreach;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Match;
use mago_syntax::cst::MatchArm;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Switch;
use mago_syntax::cst::SwitchBody;
use mago_syntax::cst::SwitchCase;
use mago_syntax::cst::SwitchCaseSeparator;
use mago_syntax::cst::While;
use mago_syntax::walker::MutWalker;

use super::Kind;
use super::operators::identity;
use crate::Converter;

impl Converter<'_, '_> {
    /// `switch` as a `match` statement with block arms, when no case falls through into the next: each case's
    /// statements end in `break`, `return` or `throw`, or the `switch` ends. A case with no statements of its own joins
    /// the next case's arm with `or`, and a `switch` without `default` gains an empty one, as PHP runs nothing where no
    /// case fits. PHP compares a case with `==`, which a constant pattern keeps for a constant of the value's own
    /// scalar type, but for a numeric string, which `==` compares as a number, and `null` (spec sections 19 and 21).
    pub(super) fn switch_statement(&mut self, statement: &Statement<'_>, switch: &Switch<'_>) {
        let SwitchBody::BraceDelimited(body) = &switch.body else {
            return self.refuse(
                statement.span(),
                "a `switch` with `:` and `endswitch` has no PHP# form: write its cases in braces",
            );
        };
        let subject = switch.expression;
        if self.untyped(subject, "a `switch`") {
            return;
        }
        self.edits.replace(switch.switch.span, "match");

        let count = body.cases.len();
        let mut has_default = false;
        // The end of a case with no statements of its own, which joins the next case's arm.
        let mut joined: Option<u32> = None;
        // The constants of the arm being read, which become its patterns once the arm converts.
        let mut constants = Vec::new();
        for (index, case) in body.cases.iter().enumerate() {
            let is_last = index + 1 == count;
            let indentation = self.indentation(case.span().start.offset);
            let (separator, statements) = match case {
                SwitchCase::Expression(case) => {
                    let numeric = matches!(case.expression, Expression::Literal(Literal::String(string))
                        if string.value.and_then(|value| std::str::from_utf8(value).ok())
                            .is_some_and(|value| value.trim().parse::<f64>().is_ok()));
                    if numeric || matches!(case.expression, Expression::Literal(Literal::Null(_))) {
                        return self.refuse(
                            case.expression.span(),
                            "a `case` of a numeric string or `null` compares by PHP's `==`, which no PHP# pattern does (spec section 19)",
                        );
                    }
                    constants.push(case.expression);
                    match joined.take() {
                        Some(end) => self.edits.replace_range(end, case.expression.span().start.offset, " or "),
                        None => self.edits.delete(case.case.span.start.offset, case.expression.span().start.offset),
                    }
                    if case.statements.is_empty() && !is_last {
                        joined = Some(case.expression.span().end.offset);
                        continue;
                    }
                    (&case.separator, &case.statements)
                }
                SwitchCase::Default(case) => {
                    if joined.is_some() {
                        return self.refuse(
                            case.default.span,
                            "a `case` that joins `default` has no PHP# pattern: `default` covers it, so remove the `case`",
                        );
                    }
                    has_default = true;
                    (&case.separator, &case.statements)
                }
            };
            let separator = match separator {
                SwitchCaseSeparator::Colon(span) | SwitchCaseSeparator::SemiColon(span) => *span,
            };
            if !self.arm(statements.as_slice(), separator, is_last, &indentation) {
                return;
            }
            for constant in std::mem::take(&mut constants) {
                self.pattern(subject, constant);
            }
        }
        if !has_default {
            let indentation = self.indentation(statement.span().start.offset);
            let line = body.right_brace.start.offset - u32::try_from(indentation.len()).unwrap_or(0);
            self.edits.insert(line, format!("{indentation}    default => {{}},\n"));
        }
    }

    /// A case's `statements` as a block arm, `=> { … },`, after its `separator`. Its `break` leaves, and any other
    /// way out of the `switch`, or into the next case, refuses. Gives whether it converted.
    fn arm(&mut self, statements: &[Statement<'_>], separator: Span, is_last: bool, indentation: &str) -> bool {
        let (kept, ends) = match statements.split_last() {
            Some((Statement::Break(Break { level: None, .. }), kept)) => (kept, true),
            Some((Statement::Return(_), _)) => (statements, true),
            Some((Statement::Expression(expression), _)) if matches!(expression.expression, Expression::Throw(_)) => {
                (statements, true)
            }
            _ => (statements, false),
        };
        if !ends && !is_last {
            self.refuse(
                separator,
                "a `case` whose statements run on into the next case has no PHP# form: a `match` arm never falls through (spec section 21)",
            );
            return false;
        }
        let mut leaves = Leaves { depth: 0, span: None };
        for statement in kept {
            leaves.walk_statement(statement, &mut ());
        }
        if let Some(span) = leaves.span {
            self.refuse(
                span,
                "a `break` or `continue` that leaves the `switch` before its case ends has no PHP# form: a `match` arm leaves at its end (spec section 21)",
            );
            return false;
        }
        if let (Some(Statement::Break(r#break)), true) = (statements.last(), kept.len() < statements.len()) {
            self.delete_lines(r#break.span());
        }

        let Some(last) = kept.last() else {
            self.edits.replace(separator, " => {},");
            return true;
        };
        self.edits.replace(separator, " => {");
        let span = Span::new(separator.file_id, kept[0].span().start, last.span().end);
        if let Some(locals) = self.locals() {
            locals.blocks.push(span);
        }
        for statement in kept {
            self.statement(statement);
        }
        if let Some(locals) = self.locals() {
            locals.blocks.pop();
        }
        self.edits.insert(last.span().end.offset, format!("\n{indentation}}},"));

        true
    }
    /// `match`, whose arms take blocks when it stands as a statement, as a PHP# `match` statement's do.
    pub(super) fn match_expression(&mut self, r#match: &Match<'_>, statement: bool) {
        let subject = r#match.expression;
        if !r#match.arms.iter().any(MatchArm::is_default) && !self.is(subject, &[Kind::Enum]) {
            let shown = self.shown(subject);
            return self.refuse(
                r#match.r#match.span,
                format!("a `match` on `{shown}` with no `default` arm is not converted yet: PHP throws `UnhandledMatchError` where no arm fits, and a PHP# `match` on a value that is no enum has a `default` arm (spec section 21)"),
            );
        }

        self.expression(subject, None);
        for arm in r#match.arms.iter() {
            let value = match arm {
                MatchArm::Expression(arm) => {
                    for condition in arm.conditions.iter() {
                        self.pattern(subject, condition);
                    }
                    // `a, b =>` is `a or b =>`, and a comma after the last condition goes.
                    let count = arm.conditions.len();
                    for (index, token) in arm.conditions.tokens.iter().enumerate() {
                        let start = token.start.offset;
                        let end = start + u32::try_from(token.value.len()).unwrap_or(1);
                        if index + 1 < count {
                            self.edits.replace_range(start, end, " or");
                        } else {
                            self.edits.delete(start, end);
                        }
                    }
                    arm.expression
                }
                MatchArm::Default(arm) => {
                    if let Some(comma) = arm.comma {
                        self.edits.delete(comma.start.offset, comma.end.offset);
                    }
                    arm.expression
                }
            };
            if statement {
                let span = value.span();
                self.edits.insert(span.start.offset, "{ ");
                self.edits.insert(span.end.offset, "; }");
            }
            self.expression(value, None);
        }
    }

    /// One of an arm's conditions, which PHP compares with the value using `===`, as a constant pattern.
    fn pattern(&mut self, subject: &Expression<'_>, condition: &Expression<'_>) {
        let constant = matches!(
            condition,
            Expression::Literal(_) | Expression::ConstantAccess(_) | Expression::Access(Access::ClassConstant(_))
        );
        if !constant {
            return self.refuse(
                condition.span(),
                "a `match` arm on a value that is no constant is not converted yet: a PHP# pattern takes types and constants only (spec section 21)",
            );
        }
        if !matches!(identity(self.type_of(subject), self.type_of(condition)), Ok(true)) {
            let (compared, constant) = (self.shown(subject), self.shown(condition));
            return self.refuse(
                condition.span(),
                format!("a `match` arm compares `{compared}` with `{constant}` by PHP's `===`, which a PHP# pattern gives only between one scalar type, enums, or `null` (spec sections 19 and 21)"),
            );
        }
        self.expression(condition, None);
    }
}

/// Finds a `break` or `continue` that leaves the `switch` the walked statements sit in: one outside any loop, or one
/// whose level reaches past the loops around it.
struct Leaves {
    /// How many loops around the walk is in.
    depth: usize,
    span: Option<Span>,
}

impl Leaves {
    fn leave(&mut self, span: Span, level: Option<&Expression<'_>>) {
        let levels = match level {
            None => 1,
            Some(Expression::Literal(Literal::Integer(integer))) => integer.value.unwrap_or(u64::MAX),
            Some(_) => u64::MAX,
        };
        if self.span.is_none() && levels > self.depth as u64 {
            self.span = Some(span);
        }
    }
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for Leaves {
    fn walk_in_break(&mut self, r#break: &'ast Break<'arena>, _: &mut ()) {
        self.leave(r#break.span(), r#break.level);
    }

    fn walk_in_continue(&mut self, r#continue: &'ast Continue<'arena>, _: &mut ()) {
        self.leave(r#continue.span(), r#continue.level);
    }

    fn walk_in_foreach(&mut self, _: &'ast Foreach<'arena>, _: &mut ()) {
        self.depth += 1;
    }

    fn walk_out_foreach(&mut self, _: &'ast Foreach<'arena>, _: &mut ()) {
        self.depth -= 1;
    }

    fn walk_in_for(&mut self, _: &'ast For<'arena>, _: &mut ()) {
        self.depth += 1;
    }

    fn walk_out_for(&mut self, _: &'ast For<'arena>, _: &mut ()) {
        self.depth -= 1;
    }

    fn walk_in_while(&mut self, _: &'ast While<'arena>, _: &mut ()) {
        self.depth += 1;
    }

    fn walk_out_while(&mut self, _: &'ast While<'arena>, _: &mut ()) {
        self.depth -= 1;
    }

    fn walk_in_do_while(&mut self, _: &'ast DoWhile<'arena>, _: &mut ()) {
        self.depth += 1;
    }

    fn walk_out_do_while(&mut self, _: &'ast DoWhile<'arena>, _: &mut ()) {
        self.depth -= 1;
    }

    fn walk_in_switch(&mut self, _: &'ast Switch<'arena>, _: &mut ()) {
        self.depth += 1;
    }

    fn walk_out_switch(&mut self, _: &'ast Switch<'arena>, _: &mut ()) {
        self.depth -= 1;
    }
}
