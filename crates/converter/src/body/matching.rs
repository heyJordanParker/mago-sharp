//! `match` and `switch` (spec section 21). A PHP arm compares the value with `===`, and a PHP# pattern takes types and
//! constants only, so an arm's constants become patterns joined by `or`, each passing the `===` rule of section 19. A
//! `switch` is a `match` statement with block arms, or an `if … else if` chain where a `case` is on a value that is no
//! constant.

use mago_codex::ttype::TType;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Break;
use mago_syntax::cst::CompositeString;
use mago_syntax::cst::Continue;
use mago_syntax::cst::DoWhile;
use mago_syntax::cst::Expression;
use mago_syntax::cst::For;
use mago_syntax::cst::Foreach;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LiteralString;
use mago_syntax::cst::Match;
use mago_syntax::cst::MatchArm;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Switch;
use mago_syntax::cst::SwitchBody;
use mago_syntax::cst::SwitchCase;
use mago_syntax::cst::SwitchCaseSeparator;
use mago_syntax::cst::While;
use mago_syntax::walker::MutWalker;

use super::Kind;
use super::builtins::runs_nothing;
use super::is_primary;
use super::kinds;
use super::operators::LOOSE;
use super::operators::identity;
use crate::Converter;

impl Converter<'_, '_> {
    /// `switch` as a `match` statement with block arms, when no case falls through into the next: each case's
    /// statements end in `break`, `return` or `throw`, or the `switch` ends. A case with no statements of its own joins
    /// the next case's arm with `or`, and one that joins `default` goes, since `default` covers it. A `switch` without
    /// `default` gains an empty one, as PHP runs nothing where no case fits. PHP compares a case with `==`, which a
    /// constant pattern keeps for a constant of the value's own scalar type, but for a numeric string, which `==`
    /// compares as a number, and `null` (spec sections 19 and 21). Cases between `:` and `endswitch` are cases in
    /// braces.
    pub(super) fn switch_statement(&mut self, statement: &Statement<'_>, switch: &Switch<'_>) {
        let subject = switch.expression;
        if self.untyped(subject, "a `switch`") {
            return;
        }
        let cases = match &switch.body {
            SwitchBody::BraceDelimited(body) => &body.cases,
            SwitchBody::ColonDelimited(body) => &body.cases,
        };
        if cases.iter().any(|case| matches!(case, SwitchCase::Expression(case) if !is_constant(case.expression))) {
            return self.switch_chain(statement, subject, cases);
        }
        let (cases, end) = match &switch.body {
            SwitchBody::BraceDelimited(body) => (&body.cases, body.right_brace.start.offset),
            SwitchBody::ColonDelimited(body) => {
                self.edits.replace(body.colon, " {");
                self.edits.replace(body.end_switch.span.join(body.terminator.span()), "}");
                (&body.cases, body.end_switch.span.start.offset)
            }
        };
        self.edits.replace(switch.switch.span, "match");

        let count = cases.len();
        let mut has_default = false;
        // The end of a case with no statements of its own, which joins the next case's arm.
        let mut joined: Option<u32> = None;
        // The constants of the arm being read, which become its patterns once the arm converts.
        let mut constants = Vec::new();
        for (index, case) in cases.iter().enumerate() {
            let is_last = index + 1 == count;
            let indentation = self.indentation(case.span().start.offset);
            let (separator, statements) = match case {
                SwitchCase::Expression(case) => {
                    // A case with no statements that joins `default`, through more such cases, is one `default` covers.
                    // Its value goes with it, so each value of the run is one that runs nothing.
                    let rest = &cases.as_slice()[index + 1..];
                    let run = rest.iter().take_while(
                        |later| matches!(later, SwitchCase::Expression(later) if later.statements.is_empty()),
                    );
                    let covered = case.statements.is_empty()
                        && runs_nothing(case.expression)
                        && run.clone().all(
                            |later| matches!(later, SwitchCase::Expression(later) if runs_nothing(later.expression)),
                        )
                        && matches!(rest.get(run.count()), Some(SwitchCase::Default(_)));
                    if covered && let Some(next) = rest.first() {
                        self.edits.delete(case.span().start.offset, next.span().start.offset);
                        continue;
                    }
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
                            "a `case` that joins `default` with a value that runs code has no PHP# form: a PHP# `default` arm stands alone, and PHP runs the value before `default` runs (spec section 21)",
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
            let line = end - u32::try_from(indentation.len()).unwrap_or(0);
            self.edits.insert(line, format!("{indentation}    default => {{}},\n"));
        }
    }

    /// A `switch` on `subject` with a `case` on a value that is no constant, which no pattern takes, as an `if … else if`
    /// chain of PHP's loose `==` tests, `if (code == limit) { … } else if (code == 0 || code == 1) { … } else { … }`,
    /// where each test passes [`Converter::loose`] and reading `subject` again runs nothing (spec sections 19 and 21).
    /// PHP tests the cases in order and runs the first that holds, as the chain does, and `default` where none does: a
    /// `default` last is the chain's `else`, and one before other cases is the branch where none of those holds, which
    /// reads their values a second time. Cases with no statements of their own join the next with `||`. Each branch
    /// keeps its statements in place, one level of indentation in from the `if`, and the `break` that ends one leaves.
    fn switch_chain(
        &mut self,
        statement: &Statement<'_>,
        subject: &Expression<'_>,
        cases: &Sequence<'_, SwitchCase<'_>>,
    ) {
        /// A branch of the chain: the cases it tests, by place, the place of the `default` it runs too, the end of the
        /// case that opens it, and the end of its statements.
        struct Branch {
            cases: Vec<usize>,
            default: Option<usize>,
            separator: Span,
            end: u32,
        }

        if !runs_nothing(subject) {
            let read = String::from_utf8_lossy(self.text(subject.span())).into_owned();
            return self.refuse(
                subject.span(),
                format!("a `switch` on `{read}` with a `case` on a value that is no constant has no PHP# form: a PHP# pattern takes types and constants only, and testing each `case` with `==` reads `{read}` again, which runs code (spec section 21)"),
            );
        }
        let read = self.render(subject);
        let indentation = self.indentation(statement.span().start.offset);
        let inner = format!("{indentation}    ");
        let count = cases.len();
        // Each case's test, and whether its value runs nothing, by the case's place, and none for `default`.
        let mut tests: Vec<Option<(String, bool)>> = Vec::new();
        let mut joined = Vec::new();
        let mut branches = Vec::new();
        for (index, case) in cases.iter().enumerate() {
            let is_last = index + 1 == count;
            let (separator, statements, default) = match case {
                SwitchCase::Expression(case) => {
                    let value = case.expression;
                    if !self.loose(subject, value) {
                        let reason = self.operands("==", subject, value, LOOSE);
                        return self.refuse(value.span(), reason);
                    }
                    self.expression(value, None);
                    let rendered = self.render(value);
                    let operand = if is_primary(value) { rendered } else { format!("({rendered})") };
                    tests.push(Some((format!("{read} == {operand}"), runs_nothing(value))));
                    joined.push(index);
                    if case.statements.is_empty() && !is_last {
                        continue;
                    }
                    (&case.separator, &case.statements, None)
                }
                SwitchCase::Default(case) => {
                    tests.push(None);
                    (&case.separator, &case.statements, Some(index))
                }
            };
            let separator = match separator {
                SwitchCaseSeparator::Colon(span) | SwitchCaseSeparator::SemiColon(span) => *span,
            };
            let Some(kept) = self.case_body(statements.as_slice(), separator, is_last) else {
                return;
            };
            self.case_statements(kept);
            self.outdent(kept, &inner);
            let end = kept.last().map_or(separator.end.offset, |last| last.span().end.offset);
            branches.push(Branch { cases: std::mem::take(&mut joined), default, separator, end });
        }

        // A `default` before other cases holds where none of them does, so where it fails one of them holds, and the
        // last branch is the chain's `else`.
        let early_default = branches.last().is_some_and(|last| last.default.is_none())
            && branches.iter().any(|branch| branch.default.is_some());
        let mut opening = statement.span().start.offset;
        for (position, branch) in branches.iter().enumerate() {
            let mut held: Vec<&str> =
                branch.cases.iter().filter_map(|case| tests[*case].as_ref()).map(|(test, _)| test.as_str()).collect();
            let test = match branch.default {
                None if early_default && position + 1 == branches.len() => None,
                None => Some(held.join(" || ")),
                Some(at) => {
                    let later: Vec<&(String, bool)> = tests[at + 1..].iter().flatten().collect();
                    let own_run = branch.cases.iter().filter_map(|case| tests[*case].as_ref()).any(|(_, read)| !read);
                    if later.is_empty() && own_run {
                        return self.refuse(
                            branch.separator,
                            "a `case` that joins a last `default` with a value that runs code has no PHP# form: the chain's `else` tests no value, and PHP runs the value before `default` runs (spec section 21)",
                        );
                    }
                    if later.iter().any(|(_, read)| !read) {
                        return self.refuse(
                            branch.separator,
                            "a `default` before a `case` on a value that runs code has no PHP# form: its branch holds where none of the later cases does, which reads their values again and runs their code twice (spec section 21)",
                        );
                    }
                    if later.is_empty() {
                        None
                    } else {
                        let none = format!(
                            "!({})",
                            later.iter().map(|(test, _)| test.as_str()).collect::<Vec<_>>().join(" || ")
                        );
                        held.push(&none);
                        Some(held.join(" || "))
                    }
                }
            };
            let branch_opening = match (position, test) {
                (0, Some(test)) => format!("if ({test}) {{"),
                (_, Some(test)) => format!("\n{indentation}}} else if ({test}) {{"),
                (_, None) => format!("\n{indentation}}} else {{"),
            };
            self.edits.replace_range(opening, branch.separator.end.offset, branch_opening);
            opening = branch.end;
        }
        self.edits.replace_range(opening, statement.span().end.offset, format!("\n{indentation}}}"));
    }

    /// Moves each line of `kept`, a case's statements, out to `indentation`, the branch's own, by the level a `case`
    /// adds, where no rewrite edits the line's start and no string runs over it, whose text the lines are.
    fn outdent(&mut self, kept: &[Statement<'_>], indentation: &str) {
        let (Some(first), Some(last)) = (kept.first(), kept.last()) else {
            return;
        };
        let own = self.indentation(first.span().start.offset);
        if !own.starts_with(indentation) || own.len() == indentation.len() {
            return;
        }
        let source = self.source;
        let width = own.len() - indentation.len();
        let mut strings = Multiline { source, spans: Vec::new() };
        for statement in kept {
            strings.walk_statement(statement, &mut ());
        }
        let (start, end) = (first.span().start.offset as usize - own.len(), last.span().end.offset as usize);
        let lines = std::iter::once(start).chain(
            source[start..end].iter().enumerate().filter(|(_, byte)| **byte == b'\n').map(|(at, _)| start + at + 1),
        );
        for line in lines {
            let indented =
                line + width <= end && source[line..line + width].iter().all(|byte| matches!(byte, b' ' | b'\t'));
            let (Ok(from), Ok(to)) = (u32::try_from(line), u32::try_from(line + width)) else {
                continue;
            };
            let in_string = strings.spans.iter().any(|span| span.start.offset < from && from < span.end.offset);
            if indented && !in_string && !self.edits.touches(from, to) {
                self.edits.delete(from, to);
            }
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

    /// A case's `statements` as a block arm, `=> { … },`, after its `separator`, with [`Converter::case_body`]'s rules.
    /// Gives whether it converted.
    fn arm(&mut self, statements: &[Statement<'_>], separator: Span, is_last: bool, indentation: &str) -> bool {
        let Some(kept) = self.case_body(statements, separator, is_last) else {
            return false;
        };
        if let (Some(Statement::Break(r#break)), true) = (statements.last(), kept.len() < statements.len()) {
            self.delete_lines(r#break.span());
        }

        let Some(last) = kept.last() else {
            self.edits.replace(separator, " => {},");
            return true;
        };
        self.edits.replace(separator, " => {");
        self.case_statements(kept);
        self.edits.insert(last.span().end.offset, format!("\n{indentation}}},"));

        true
    }

    /// The statements of a case, `statements`, that its branch keeps: all of them, or those before the `break` they end
    /// in, which leaves. A case whose statements run on into the next, and a `break` or `continue` that leaves the
    /// `switch` before the case ends, have no PHP# form, since a `match` arm and an `if` branch leave at their end, and
    /// refuse at `separator` or at the `break` (spec section 21).
    fn case_body<'statements, 'arena>(
        &mut self,
        statements: &'statements [Statement<'arena>],
        separator: Span,
        is_last: bool,
    ) -> Option<&'statements [Statement<'arena>]> {
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
            return None;
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
            return None;
        }

        Some(kept)
    }

    /// Converts `kept`, a case's statements, whose locals belong to its branch, as to a block.
    fn case_statements(&mut self, kept: &[Statement<'_>]) {
        let (Some(first), Some(last)) = (kept.first(), kept.last()) else {
            return;
        };
        self.scoped(first.span().join(last.span()), kept);
    }
    /// `match`, whose arms take blocks when it stands as a statement, as a PHP# `match` statement's do. One on an `int`
    /// with no `default` arm gains the one that throws PHP's `UnhandledMatchError`, whose message holds the `int`.
    pub(super) fn match_expression(&mut self, r#match: &Match<'_>, place: MatchPlace) {
        let statement = place == MatchPlace::Statement;
        let subject = r#match.expression;
        let unhandled = !r#match.arms.iter().any(MatchArm::is_default) && !self.is(subject, &[Kind::Enum]);
        if unhandled {
            let shown = self.shown(subject);
            if !self.is(subject, &[Kind::Int]) {
                return self.refuse(
                    r#match.r#match.span,
                    format!("a `match` on `{shown}` with no `default` arm has no PHP# form: PHP throws `UnhandledMatchError` where no arm fits, with a message that quotes a value other than an `int` as the `zend.exception_string_param_max_len` setting cuts it, and a PHP# `match` on a value that is no enum has a `default` arm (spec section 21)"),
                );
            }
            if !runs_nothing(subject) {
                let read = String::from_utf8_lossy(self.text(subject.span())).into_owned();
                return self.refuse(
                    r#match.r#match.span,
                    format!("a `match` on `{shown}` with no `default` arm has no PHP# form: its `default` arm throws PHP's `UnhandledMatchError`, whose message reads `{read}` again, which runs code (spec section 21)"),
                );
            }
        }

        let guarded = r#match
            .arms
            .iter()
            .any(|arm| matches!(arm, MatchArm::Expression(arm) if arm.conditions.iter().any(|condition| !is_constant(condition))));
        if guarded {
            return self.match_true(r#match, place);
        }

        self.expression(subject, None);
        if unhandled {
            self.unhandled_arm(r#match, subject, statement);
        }
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

    /// Adds to `r#match`, a `match` on the `int` `subject` with no `default` arm, the one PHP runs where no arm fits,
    /// which throws `UnhandledMatchError` with PHP's message, as `Unhandled match case 7`. It reads `subject` again, as
    /// a block where the `match` is a `statement`, and goes on the line after the last arm, or after it on its line.
    fn unhandled_arm(&mut self, r#match: &Match<'_>, subject: &Expression<'_>, statement: bool) {
        let Some(last) = r#match.arms.last() else {
            return;
        };
        // Rendering takes the subject's edits, so its text goes back in their place.
        let value = self.render(subject);
        self.edits.replace(subject.span(), value.clone());
        let thrown = format!("throw new UnhandledMatchError(`Unhandled match case ${{{value}}}`)");
        let arm = if statement { format!("default => {{ {thrown}; }}") } else { format!("default => {thrown}") };
        let trailing = r#match.arms.tokens.len() == r#match.arms.len();
        let end = last.span().end.offset;
        let after = r#match.arms.tokens.last().filter(|_| trailing).map_or(end, |comma| comma.start.offset + 1);
        let on_own_line = self.source[after as usize..r#match.right_brace.start.offset as usize].contains(&b'\n');
        let comma = if trailing { "" } else { "," };
        let text = if on_own_line {
            format!("{comma}\n{}{arm}", self.indentation(last.span().start.offset))
        } else {
            format!("{comma} {arm}")
        };
        self.edits.insert(after, text);
    }

    /// `match (true)`, which runs the arm of the first condition that is `true`, as a chain of ternaries, `a ? x : b ? y
    /// : z`, or as `if … else if … else` where it stands as a statement (spec section 21). PHP compares each condition
    /// with `true` by `===`, which holds for a `bool` just when it is `true`, and tries them in order, as the chain does.
    /// The chain ends in the `default` arm, or in PHP's `UnhandledMatchError` for a `match` on an `int` with none. The
    /// chain takes parentheses where an operator around it would bind tighter than `?:`, as `match (true) { … } +
    /// $extra` is `(express ? 5 : 0) + extra`.
    ///
    /// A `match` on another value with an arm on a value that is no constant, which no pattern takes, is the same
    /// chain of `==` tests, as `match ($code) { $limit => … }` is `code == limit ? … : …`, where `==` is PHP's `===`
    /// for the two values and reading the value again runs nothing (spec section 21).
    fn match_true(&mut self, r#match: &Match<'_>, place: MatchPlace) {
        let statement = place == MatchPlace::Statement;
        let subject = r#match.expression;
        let compared = !matches!(subject, Expression::Literal(Literal::True(_)));
        if compared && !runs_nothing(subject) {
            let read = String::from_utf8_lossy(self.text(subject.span())).into_owned();
            return self.refuse(
                r#match.r#match.span,
                format!("a `match` on `{read}` with an arm on a value that is no constant has no PHP# form: a PHP# pattern takes types and constants only, and testing each arm with `==` reads `{read}` again, which runs code (spec section 21)"),
            );
        }
        self.expression(subject, None);
        let subject_text = self.render(subject);
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
                        if compared {
                            if !matches!(identity(self.type_of(subject), self.type_of(condition)), Ok(true)) {
                                let (compared, constant) = (self.shown(subject), self.shown(condition));
                                return self.refuse(
                                    condition.span(),
                                    format!("a `match` arm compares `{compared}` with `{constant}` by PHP's `===`, which PHP#'s `==` gives only between scalars of a type they share, with no `int` beside a `float`, enums, or `null` (spec sections 19 and 21)"),
                                );
                            }
                            self.expression(condition, None);
                            let rendered = self.render(condition);
                            let operand = if is_primary(condition) { rendered } else { format!("({rendered})") };
                            tests.push(format!("{subject_text} == {operand}"));
                            continue;
                        }
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
        // The arms after `default` never run, as PHP's `default` runs only when no arm fits. A `match` with none is on an
        // `int`, whose value PHP's message holds.
        let otherwise = match arms.iter().position(|(test, _)| test.is_none()) {
            Some(default) => arms.remove(default).1,
            None => format!("throw new UnhandledMatchError(`Unhandled match case ${{{subject_text}}}`)"),
        };
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
                "a `switch` case on a value that is no constant is not converted yet: the converter writes a `switch` as a `match`, whose pattern takes types and constants only (spec section 21)",
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

/// Finds the strings the walked statements hold that run over several lines, whose lines are the strings' text.
struct Multiline<'source> {
    source: &'source [u8],
    spans: Vec<Span>,
}

impl Multiline<'_> {
    fn found(&mut self, span: Span) {
        if self.source[span.start.offset as usize..span.end.offset as usize].contains(&b'\n') {
            self.spans.push(span);
        }
    }
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for Multiline<'_> {
    fn walk_in_literal_string(&mut self, string: &'ast LiteralString<'arena>, _: &mut ()) {
        self.found(string.span());
    }

    fn walk_in_composite_string(&mut self, string: &'ast CompositeString<'arena>, _: &mut ()) {
        self.found(string.span());
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
