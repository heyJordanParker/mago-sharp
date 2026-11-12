//! `match` and `switch` (spec section 21). A PHP arm compares the value with `===`, and a PHP# pattern takes types and
//! constants only, so an arm's constants become patterns joined by `or`, each passing the `===` rule of section 19. A
//! `switch` is a `match` statement with block arms.

use mago_codex::ttype::TType;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::BinaryOperator;
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
use super::kinds;
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
                    if let Err(reason) = self.loose_case(subject, case.expression) {
                        return self.refuse(case.expression.span(), reason);
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

    /// Whether PHP's `==` between the `switch`'s `subject` and a case's constant `case` holds just where its pattern
    /// matches, by the constant's value, a literal's or a named constant's alike. `==` compares a numeric string as a
    /// number, and `null` equals each value PHP reads as `false`: `false`, `0`, `0.0` and `''` (spec section 19). The
    /// reason it doesn't, otherwise.
    fn loose_case(&self, subject: &Expression<'_>, case: &Expression<'_>) -> Result<(), String> {
        let value = self.type_of(case);
        let numeric = value
            .and_then(TUnion::get_single_literal_string_value)
            .and_then(|value| std::str::from_utf8(value).ok())
            .is_some_and(|value| value.trim().parse::<f64>().is_ok());
        if numeric || value.is_some_and(TUnion::is_null) {
            return Err("a `case` of a numeric string or `null` compares by PHP's `==`, which no PHP# pattern does (spec section 19)".to_owned());
        }
        if !self.type_of(subject).is_some_and(TUnion::is_nullable) {
            return Ok(());
        }
        let shown = String::from_utf8_lossy(self.text(case.span()));
        let Some(value) = value else {
            return Err(format!(
                "the analysis gives the `case` of `{shown}` no value, so PHP's `null == {shown}` may hold for the `null` the `switch`'s value holds"
            ));
        };
        let truthy = value.is_true()
            || value.get_single_literal_int_value().is_some_and(|value| value != 0)
            || value.get_single_literal_float_value().is_some_and(|value| value != 0.0)
            || value.get_single_literal_string_value().is_some_and(|value| !value.is_empty())
            || kinds(value) == [Kind::Enum];
        if truthy {
            return Ok(());
        }
        if value.is_false()
            || value.get_single_literal_int_value() == Some(0)
            || value.get_single_literal_float_value() == Some(0.0)
            || value.get_single_literal_string_value() == Some(b"")
        {
            return Err(format!(
                "a `case` of `{shown}` runs for `null` too, since PHP's `null == {shown}` holds, and its pattern doesn't match `null` (spec sections 19 and 21)"
            ));
        }

        Err(format!(
            "a `case` of `{}` may run for `null` too, since PHP's `null == {shown}` holds for each value PHP reads as `false`, and its pattern doesn't match `null` (spec sections 19 and 21)",
            value.get_id()
        ))
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
    pub(super) fn match_expression(&mut self, r#match: &Match<'_>, place: MatchPlace) {
        let statement = place == MatchPlace::Statement;
        let subject = r#match.expression;
        if !r#match.arms.iter().any(MatchArm::is_default) && !self.is(subject, &[Kind::Enum]) {
            let shown = self.shown(subject);
            return self.refuse(
                r#match.r#match.span,
                format!("a `match` on `{shown}` with no `default` arm is not converted yet: PHP throws `UnhandledMatchError` where no arm fits, and a PHP# `match` on a value that is no enum has a `default` arm (spec section 21)"),
            );
        }

        let guarded = r#match
            .arms
            .iter()
            .any(|arm| matches!(arm, MatchArm::Expression(arm) if arm.conditions.iter().any(|condition| !is_constant(condition))));
        if guarded && matches!(subject, Expression::Literal(Literal::True(_))) {
            return self.match_true(r#match, place);
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

    /// `match (true)`, which runs the arm of the first condition that is `true`, as a chain of ternaries, `a ? x : b ? y
    /// : z`, or as `if … else if … else` where it stands as a statement (spec section 21). PHP compares each condition
    /// with `true` by `===`, which holds for a `bool` just when it is `true`, and tries them in order, as the chain does.
    /// The `match` has a `default` arm, which ends the chain. The chain takes parentheses where an operator around it
    /// would bind tighter than `?:`, as `match (true) { … } + $extra` is `(express ? 5 : 0) + extra`.
    fn match_true(&mut self, r#match: &Match<'_>, place: MatchPlace) {
        let statement = place == MatchPlace::Statement;
        let mut arms: Vec<(Option<String>, String)> = Vec::new();
        for arm in r#match.arms.iter() {
            let (conditions, value) = match arm {
                MatchArm::Expression(arm) => (Some(&arm.conditions), arm.expression),
                MatchArm::Default(arm) => (None, arm.expression),
            };
            let test = match conditions {
                Some(conditions) => {
                    let mut tests = Vec::new();
                    for condition in conditions.iter() {
                        if !self.is(condition, &[Kind::Bool]) {
                            let shown = self.shown(condition);
                            return self.refuse(
                                condition.span(),
                                format!("a `match (true)` arm on `{shown}` compares it with `true` by PHP's `===`, which a PHP# condition gives only for a `bool` (spec sections 19 and 21)"),
                            );
                        }
                        if !self.proven(condition) {
                            return;
                        }
                        self.expression(condition, None);
                        tests.push(grouped(condition, self.render(condition)));
                    }
                    Some(tests.join(" || "))
                }
                None => None,
            };
            self.expression(value, None);
            let rendered = self.render(value);
            // A statement's arm is a statement of its own, where nothing binds around it.
            arms.push((test, if statement { rendered } else { grouped(value, rendered) }));
        }
        let Some(default) = arms.iter().position(|(test, _)| test.is_none()) else {
            return;
        };
        // The arms after `default` never run, as PHP's `default` runs only when no arm fits.
        let (_, otherwise) = arms.remove(default);
        let tested: Vec<(String, String)> =
            arms.into_iter().filter_map(|(test, value)| test.map(|test| (test, value))).collect();
        let indentation = self.indentation(r#match.span().start.offset);
        let text = if statement {
            let branches: Vec<String> = tested
                .iter()
                .map(|(test, value)| format!("if ({test}) {{\n{indentation}    {value};\n{indentation}}}"))
                .collect();
            format!("{} else {{\n{indentation}    {otherwise};\n{indentation}}}", branches.join(" else "))
        } else {
            let arms: Vec<String> = tested.iter().map(|(test, value)| format!("{test} ? {value}")).collect();
            let chain = format!("{}\n{indentation}    : {otherwise}", arms.join(&format!("\n{indentation}    : ")));
            if place == MatchPlace::Operand { format!("({chain})") } else { chain }
        };
        self.edits.replace(r#match.span(), text);
    }

    /// One of an arm's conditions, which PHP compares with the value using `===`, as a constant pattern.
    fn pattern(&mut self, subject: &Expression<'_>, condition: &Expression<'_>) {
        if !is_constant(condition) {
            return self.refuse(
                condition.span(),
                "a `match` arm on a value that is no constant is not converted yet: a PHP# pattern takes types and constants only (spec section 21)",
            );
        }
        if !matches!(identity(self.type_of(subject), self.type_of(condition)), Ok(true)) {
            let (compared, constant) = (self.shown(subject), self.shown(condition));
            return self.refuse(
                condition.span(),
                format!("a `match` arm compares `{compared}` with `{constant}` by PHP's `===`, which a PHP# pattern gives only between scalars of a type they share, with no `int` beside a `float`, enums, or `null` (spec sections 19 and 21)"),
            );
        }
        self.expression(condition, None);
    }
}

/// Where a `match` stands, which decides its PHP# form.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MatchPlace {
    /// A statement of its own.
    Statement,
    /// The whole value a `return` gives or an assignment writes, where nothing binds around it.
    Value,
    /// Inside another expression, as an operand.
    Operand,
}

/// `rendered`, the PHP# text of `expression`, in parentheses where an operator of a ternary chain around it would bind
/// tighter than its own: a ternary, an assignment and `??`.
fn grouped(expression: &Expression<'_>, rendered: String) -> String {
    let loose = match expression {
        Expression::Conditional(_) | Expression::Assignment(_) => true,
        Expression::Binary(binary) => matches!(binary.operator, BinaryOperator::NullCoalesce(_)),
        _ => false,
    };
    if loose { format!("({rendered})") } else { rendered }
}

/// Whether `condition` is a constant a PHP# pattern takes: a literal or a named constant (spec section 21).
fn is_constant(condition: &Expression<'_>) -> bool {
    matches!(
        condition,
        Expression::Literal(_) | Expression::ConstantAccess(_) | Expression::Access(Access::ClassConstant(_))
    )
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
