//! A law's proof: the Lean theorem of its statement, proven by its body, or by Lean's automatic steps alone for a law
//! without one.
//!
//! Each `if` is a case split on its condition and each `match` arm a case of its subject, each call of a law is a fact
//! in scope, the theorem of the law it calls, and each case ends in `sharp_close`, which `Sharp/Lean/Proof.lean`
//! defines. A call of the law itself is Lean's recursion on the theorem.

use mago_sharp_bridge::CheckedProgram;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Argument;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Block;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Law as LawNode;
use mago_syntax::cst::LawProof;
use mago_syntax::cst::ListPatternElement;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::PatternMatch;
use mago_syntax::cst::PatternMatchArm;
use mago_syntax::cst::PatternMatchArmBody;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPrefixOperator;

use crate::issues;
use crate::reach::Reach;
use crate::translate::Unmodeled;
use crate::translate::Use;
use crate::translate::body::Body;
use crate::translate::names;

/// The theorem of a law, and what its errors name.
#[derive(Debug, Clone, Default)]
pub(crate) struct Proof {
    /// The Lean theorem, `<statement>.proof`.
    pub(crate) text: String,
    /// Each case the proof closes, in the order of its index, from 1.
    pub(crate) cases: Vec<Case>,
    /// Each call of a law in the body, in the order of its index, from 1.
    pub(crate) calls: Vec<LawCall>,
    /// The statement of each law the body calls, the law itself apart, with where it is first called.
    pub(crate) laws: Vec<(String, Span)>,
    pub(crate) uses: Vec<Use>,
    pub(crate) unmodeled: Vec<Unmodeled>,
}

/// One case of a law's proof: the end of the body, or a `return;`, on one path through its conditions.
#[derive(Debug, Clone, Default)]
pub(crate) struct Case {
    /// What holds in the case, in PHP#: the law's `when` condition, then each condition and pattern on the way.
    pub(crate) conditions: Vec<String>,
    /// The call of the law itself on a smaller value the case knows of, when it makes none.
    pub(crate) smaller: Option<(String, String)>,
}

/// A call of a law in a body, whose `when` condition, when the law has one, the case it is in must show.
#[derive(Debug, Clone)]
pub(crate) struct LawCall {
    pub(crate) span: Span,
    /// As written: `neverAbovePaid(p, r)`.
    pub(crate) written: String,
    /// Each argument as written, which stands for the binder `«$argN»` of a call whose arguments compute.
    pub(crate) arguments: Vec<String>,
    /// What holds where the call is, in PHP#, as a case's conditions.
    pub(crate) conditions: Vec<String>,
}

/// A smaller value of a parameter that the conditions on the way to a statement show.
#[derive(Debug, Clone, Copy)]
enum Smaller<'arena> {
    /// The rest of the list parameter at this place, from a list pattern with an element before the rest.
    Rest(usize, &'arena [u8]),
    /// The int parameter at this place is above a bound, so one less is nearer it.
    Above(usize),
    /// The int parameter at this place is below a bound, so one more is nearer it.
    Below(usize),
}

/// One arm of a `match` on a list, as Lean matches the list.
struct ListArm<'ast, 'arena> {
    /// Its Lean pattern: `[]`, `first :: rest`, or `_` for every list the arms before it miss.
    lean: String,
    /// Its PHP# pattern, or `None` for `default`.
    pattern: Option<String>,
    /// What holds in it, in PHP#: `items is []`, or for `default`, that each pattern before it does not match.
    condition: String,
    block: Option<&'ast Block<'arena>>,
    smaller: Option<Smaller<'arena>>,
}

/// What holds on one path through a body.
#[derive(Clone, Default)]
struct Scope<'ast, 'arena> {
    locals: Vec<&'ast Statement<'arena>>,
    /// Each case hypothesis, by its Lean name, with the condition it states in PHP#.
    hypotheses: Vec<(String, String)>,
    smaller: Vec<Smaller<'arena>>,
    /// Whether the path calls the law itself.
    recurses: bool,
}

/// What writes the proof of one law.
pub(crate) struct Prover<'prover, 'program, 'ast, 'arena> {
    checked: &'prover CheckedProgram<'program>,
    reach: &'prover Reach,
    /// The fully qualified PHP name of the class-like that states the law.
    full: &'prover [u8],
    /// Its Lean name.
    lean: &'prover str,
    place: String,
    law: &'ast LawNode<'arena>,
    statement: String,
    proof: Proof,
    facts: u32,
    hypotheses: u32,
}

impl<'prover, 'program, 'ast, 'arena> Prover<'prover, 'program, 'ast, 'arena> {
    pub(crate) fn new(
        checked: &'prover CheckedProgram<'program>,
        reach: &'prover Reach,
        full: &'prover [u8],
        lean: &'prover str,
        place: String,
        law: &'ast LawNode<'arena>,
        statement: String,
    ) -> Self {
        Self { checked, reach, full, lean, place, law, statement, proof: Proof::default(), facts: 0, hypotheses: 0 }
    }

    /// The law's theorem: `theorem <statement>.proof (parameters) : <statement> parameters := by …`.
    pub(crate) fn prove(mut self, binders: &[String], parameters: &[String]) -> Proof {
        let mut lines = vec![format!("unfold {}", self.statement)];
        let mut scope = Scope::default();
        if let Some(condition) = &self.law.condition {
            lines.push("intro «when»".to_owned());
            scope.hypotheses.push(("«when»".to_owned(), self.source(condition.expression.span())));
        }
        match &self.law.proof {
            LawProof::Checker(_) => lines.push(self.close(&scope, "sharp_prove")),
            LawProof::Body(body) => {
                let statements: Vec<&Statement> = body.statements.iter().collect();
                self.statements(&statements, scope, &mut lines);
            }
        }

        let applied = std::iter::once(self.statement.clone()).chain(parameters.iter().cloned()).collect::<Vec<_>>();
        self.proof.text = format!(
            "theorem {}.proof {} : {} := by\n{}",
            self.statement,
            binders.join(" "),
            applied.join(" "),
            lines.iter().map(|line| format!("  {line}")).collect::<Vec<_>>().join("\n")
        );

        self.proof
    }

    /// The tactics of `statements`, the rest of a path through the body, which `scope` holds on.
    fn statements(
        &mut self,
        statements: &[&'ast Statement<'arena>],
        mut scope: Scope<'ast, 'arena>,
        lines: &mut Vec<String>,
    ) {
        let Some((statement, rest)) = statements.split_first() else {
            lines.push(self.close(&scope, "sharp_close"));
            return;
        };

        match statement {
            Statement::Return(_) => lines.push(self.close(&scope, "sharp_close")),
            Statement::Block(block) => {
                let inner: Vec<&Statement> = block.statements.iter().chain(rest.iter().copied()).collect();
                self.statements(&inner, scope, lines);
            }
            Statement::LocalDeclaration(_) => {
                scope.locals.push(statement);
                self.statements(rest, scope, lines);
            }
            Statement::If(r#if) => {
                let IfBody::Statement(body) = &r#if.body else {
                    return;
                };
                let condition = self.condition(&scope, r#if.condition);
                let written = self.source(r#if.condition.span());
                self.hypotheses += 1;
                let hypothesis = format!("«$c{}»", self.hypotheses);
                lines.push(format!("by_cases {hypothesis} : {condition}"));

                let mut then = scope.clone();
                then.hypotheses.push((hypothesis.clone(), written.clone()));
                learn(self.law, r#if.condition, false, &mut then.smaller);
                let branch: Vec<&Statement> = std::iter::once(body.statement).chain(rest.iter().copied()).collect();
                self.branch(&branch, then, lines);

                let mut otherwise = scope;
                otherwise.hypotheses.push((hypothesis, negated(self, r#if.condition)));
                learn(self.law, r#if.condition, true, &mut otherwise.smaller);
                let branch: Vec<&Statement> =
                    body.else_clause.iter().map(|clause| clause.statement).chain(rest.iter().copied()).collect();
                self.branch(&branch, otherwise, lines);
            }
            Statement::PatternMatch(r#match) => self.r#match(r#match, rest, scope, lines),
            Statement::Expression(expression) => {
                if let Expression::Call(call) = expression.expression {
                    self.call(call, &mut scope, lines);
                }
                self.statements(rest, scope, lines);
            }
            _ => self.statements(rest, scope, lines),
        }
    }

    /// One branch of a case split: `·` and its tactics.
    fn branch(&mut self, statements: &[&'ast Statement<'arena>], scope: Scope<'ast, 'arena>, lines: &mut Vec<String>) {
        let mut branch = Vec::new();
        self.statements(statements, scope, &mut branch);
        for (index, line) in branch.into_iter().enumerate() {
            lines.push(if index == 0 { format!("· {line}") } else { format!("  {line}") });
        }
    }

    /// A `match` on a list parameter: each arm is the case of the list it matches, `[]` or an element before the rest.
    fn r#match(
        &mut self,
        r#match: &'ast PatternMatch<'arena>,
        rest: &[&'ast Statement<'arena>],
        scope: Scope<'ast, 'arena>,
        lines: &mut Vec<String>,
    ) {
        let Some(subject) = parameter(self.law, r#match.expression) else {
            self.unmodeled(r#match.span());
            return;
        };
        let written = self.source(r#match.expression.span());
        let mut arms: Vec<ListArm<'ast, 'arena>> = Vec::new();
        for arm in r#match.arms.iter() {
            let PatternMatchArmBody::Block(block) = arm.body() else {
                continue;
            };
            let PatternMatchArm::Pattern(arm) = arm else {
                // A `default` after both shapes of a list never runs.
                let missed: Vec<String> = arms
                    .iter()
                    .filter_map(|arm| arm.pattern.as_ref())
                    .map(|pattern| format!("{written} is not {pattern}"))
                    .collect();
                if !(arms.iter().any(|arm| arm.lean == "[]") && arms.iter().any(|arm| arm.lean != "[]")) {
                    arms.push(ListArm {
                        lean: "_".to_owned(),
                        pattern: None,
                        condition: missed.join(" && "),
                        block: Some(block),
                        smaller: None,
                    });
                }
                continue;
            };
            let (Pattern::List(list), None) = (arm.pattern, &arm.guard) else {
                self.unmodeled(arm.span());
                return;
            };
            let pattern = self.source(arm.pattern.span());
            let condition = format!("{written} is {pattern}");
            let elements: Vec<&ListPatternElement> = list.elements.iter().collect();
            let (lean, smaller) = match elements.as_slice() {
                [] => ("[]".to_owned(), None),
                [ListPatternElement::Pattern(first), ListPatternElement::Rest(tail)] => {
                    let Some(head) = element(first) else {
                        self.unmodeled(arm.span());
                        return;
                    };
                    let tail = tail.variable.as_ref().map(|variable| variable.value);
                    (
                        format!("{head} :: {}", tail.map_or_else(|| "_".to_owned(), names::identifier)),
                        tail.map(|tail| Smaller::Rest(subject, tail)),
                    )
                }
                _ => {
                    self.unmodeled(arm.span());
                    return;
                }
            };
            arms.push(ListArm { lean, pattern: Some(pattern), condition, block: Some(block), smaller });
        }
        let covers = |shape: fn(&str) -> bool| arms.iter().any(|arm| arm.lean == "_" || shape(&arm.lean));
        if !covers(|lean| lean == "[]") || !covers(|lean| lean != "[]") {
            arms.push(ListArm {
                lean: "_".to_owned(),
                pattern: None,
                condition: String::new(),
                block: None,
                smaller: None,
            });
        }

        lines.push(format!("match {} with", self.parameter_name(subject)));
        for ListArm { lean, condition, block, smaller, .. } in arms {
            let mut arm = scope.clone();
            if !condition.is_empty() {
                arm.hypotheses.push((String::new(), condition));
            }
            arm.smaller.extend(smaller);
            let statements: Vec<&Statement> =
                block.into_iter().flat_map(|block| block.statements.iter()).chain(rest.iter().copied()).collect();
            let mut tactics = Vec::new();
            self.statements(&statements, arm, &mut tactics);
            lines.push(format!("| {lean} =>"));
            lines.extend(tactics.into_iter().map(|line| format!("  {line}")));
        }
    }

    /// A call statement of a law: the fact `have «$fN» := sharp_call% N (<law>.proof <arguments>)`, which proves the
    /// called law's `when` condition first. Arguments that compute are bound to `«$argN»`, which the fact holds for
    /// each value they compute.
    fn call(&mut self, call: &'ast Call<'arena>, scope: &mut Scope<'ast, 'arena>, lines: &mut Vec<String>) {
        let Some((key, arguments)) = self.called(call) else {
            return;
        };
        if key == self.statement {
            scope.recurses = true;
        } else if !self.proof.laws.iter().any(|(law, _)| *law == key) {
            self.proof.laws.push((key.clone(), call.span()));
        }

        let values: Vec<&Expression> = arguments
            .iter()
            .map(|argument| match argument {
                Argument::Positional(argument) => argument.value,
                Argument::Named(argument) => argument.value,
            })
            .collect();
        let (computed, terms, uses, unmodeled) =
            Body::new(self.checked, self.reach, self.full, self.place.clone()).terms(&scope.locals, &values);
        self.proof.uses.extend(uses);
        self.proof.unmodeled.extend(unmodeled);
        self.proof.calls.push(LawCall {
            span: call.span(),
            written: self.source(call.span()),
            arguments: values.iter().map(|value| self.source(value.span())).collect(),
            conditions: scope.hypotheses.iter().map(|(_, written)| written.clone()).collect(),
        });
        let index = self.proof.calls.len();
        self.facts += 1;
        let fact = format!("«$f{}»", self.facts);
        if computed.is_empty() {
            let applied: Vec<String> = terms.iter().map(|term| format!("({term})")).collect();
            lines.push(format!("have {fact} := sharp_call% {index} ({key}.proof {})", applied.join(" ")));
            return;
        }

        let binders: Vec<String> = (1..=terms.len()).map(|index| format!("«$arg{index}»")).collect();
        lines.push(format!("have {fact} := fun {} (_ : ((do", binders.join(" ")));
        lines.extend(computed.iter().map(|line| format!("      {line}")));
        lines.push(format!("      return ({})) : M _) = pure ({})) =>", terms.join(", "), binders.join(", ")));
        lines.push(format!("    sharp_call% {index} ({key}.proof {})", binders.join(" ")));
    }

    /// The Lean name of the law `call` calls, and its arguments: a bare name calls a law of the class-like the law is in,
    /// and `Class.law(…)` a law of `Class`.
    fn called(&self, call: &'ast Call<'arena>) -> Option<(String, &'ast [Argument<'arena>])> {
        match call {
            Call::Function(call) => {
                let Expression::Identifier(name) = call.function else {
                    return None;
                };
                Some((names::member(self.lean, name.value()), call.argument_list.arguments.as_slice()))
            }
            Call::Method(call) => {
                let class = self.checked.names().static_call_class(call)?;
                let ClassLikeMemberSelector::Identifier(name) = &call.method else {
                    return None;
                };
                let full = self.checked.names().get(&class.name);
                Some((names::member(&names::full_name(full), name.value), call.argument_list.arguments.as_slice()))
            }
            _ => None,
        }
    }

    /// `(do … return (c)) = pure true`, the condition `expression` as a case hypothesis.
    fn condition(&mut self, scope: &Scope<'ast, 'arena>, expression: &'ast Expression<'arena>) -> String {
        let (lines, terms, uses, unmodeled) =
            Body::new(self.checked, self.reach, self.full, self.place.clone()).terms(&scope.locals, &[expression]);
        self.proof.uses.extend(uses);
        self.proof.unmodeled.extend(unmodeled);
        let mut text = String::from("((do\n");
        for line in &lines {
            text.push_str(&format!("      {line}\n"));
        }
        text.push_str(&format!("      return ({})) : M Bool) = pure true", terms[0]));

        text
    }

    /// The step that closes a case: `sharp_close` with its index and its hypotheses.
    fn close(&mut self, scope: &Scope<'ast, 'arena>, step: &str) -> String {
        let hypotheses: Vec<&str> =
            scope.hypotheses.iter().map(|(name, _)| name.as_str()).filter(|name| !name.is_empty()).collect();
        let smaller = (!scope.recurses).then(|| self.smaller_call(&scope.smaller)).flatten();
        self.proof
            .cases
            .push(Case { conditions: scope.hypotheses.iter().map(|(_, written)| written.clone()).collect(), smaller });

        format!("{step} {} [{}]", self.proof.cases.len(), hypotheses.join(", "))
    }

    /// The call of the law itself on the first smaller value `smaller` holds, and the parameter it makes smaller.
    fn smaller_call(&self, smaller: &[Smaller<'arena>]) -> Option<(String, String)> {
        let known = smaller.first()?;
        let (place, value) = match known {
            Smaller::Rest(place, rest) => (*place, String::from_utf8_lossy(rest).into_owned()),
            Smaller::Above(place) => (*place, format!("{} - 1", self.parameter_written(*place))),
            Smaller::Below(place) => (*place, format!("{} + 1", self.parameter_written(*place))),
        };
        let arguments: Vec<String> = (0..self.law.parameter_list.parameters.len())
            .map(|index| if index == place { value.clone() } else { self.parameter_written(index) })
            .collect();

        Some((
            self.parameter_written(place),
            format!("{}({})", String::from_utf8_lossy(self.law.name.value), arguments.join(", ")),
        ))
    }

    fn parameter_written(&self, place: usize) -> String {
        let parameter = &self.law.parameter_list.parameters.as_slice()[place];

        String::from_utf8_lossy(parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name))
            .into_owned()
    }

    fn parameter_name(&self, place: usize) -> String {
        let parameter = &self.law.parameter_list.parameters.as_slice()[place];

        names::identifier(parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name))
    }

    fn unmodeled(&mut self, span: Span) {
        self.proof.unmodeled.push(Unmodeled {
            span,
            what: format!("the statement `{}`", self.source(span)),
            place: self.place.clone(),
            reason: issues::NOT_TRANSLATED,
        });
    }

    fn source(&self, span: Span) -> String {
        let contents = &self.checked.file().contents;
        let text = contents.get(span.start.offset as usize..span.end.offset as usize).unwrap_or_default();

        String::from_utf8_lossy(text).into_owned()
    }
}

/// The name of a list pattern's element, `_` for a type pattern without one, or `None` for any other pattern.
fn element(pattern: &Pattern) -> Option<String> {
    match pattern {
        Pattern::Type(r#type) if r#type.fields.is_none() && r#type.properties.is_none() => {
            Some(r#type.variable.as_ref().map_or_else(|| "_".to_owned(), |variable| names::identifier(variable.value)))
        }
        _ => None,
    }
}

/// The place of the law's parameter `expression` names.
fn parameter(law: &LawNode, expression: &Expression) -> Option<usize> {
    let Expression::ConstantAccess(name) = expression else {
        return None;
    };

    law.parameter_list.parameters.iter().position(|parameter| {
        parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name) == name.name.value()
    })
}

/// `condition`'s negation as PHP# writes it: a comparison flipped, a `!` removed, or `!(…)` around anything else.
fn negated(prover: &Prover, condition: &Expression) -> String {
    match condition {
        Expression::Parenthesized(parenthesized) => negated(prover, parenthesized.expression),
        Expression::UnaryPrefix(prefix) if matches!(prefix.operator, UnaryPrefixOperator::Not(_)) => {
            prover.source(prefix.operand.span())
        }
        Expression::Binary(binary) => {
            let flipped = match binary.operator {
                BinaryOperator::LessThan(_) => ">=",
                BinaryOperator::LessThanOrEqual(_) => ">",
                BinaryOperator::GreaterThan(_) => "<=",
                BinaryOperator::GreaterThanOrEqual(_) => "<",
                BinaryOperator::Equal(_) => "!=",
                BinaryOperator::NotEqual(_) => "==",
                _ => return format!("!({})", prover.source(condition.span())),
            };
            format!("{} {flipped} {}", prover.source(binary.lhs.span()), prover.source(binary.rhs.span()))
        }
        _ => format!("!{}", prover.source(condition.span())),
    }
}

/// Learns what `condition`, or its negation when `negated`, shows of the law's int parameters: a strict comparison with
/// an int literal or another parameter holds the parameter above or below it.
fn learn<'arena>(
    law: &LawNode<'arena>,
    condition: &Expression<'arena>,
    negated: bool,
    smaller: &mut Vec<Smaller<'arena>>,
) {
    match condition {
        Expression::Parenthesized(parenthesized) => learn(law, parenthesized.expression, negated, smaller),
        Expression::UnaryPrefix(prefix) if matches!(prefix.operator, UnaryPrefixOperator::Not(_)) => {
            learn(law, prefix.operand, !negated, smaller);
        }
        Expression::Binary(binary) => match binary.operator {
            BinaryOperator::And(_) if !negated => {
                learn(law, binary.lhs, false, smaller);
                learn(law, binary.rhs, false, smaller);
            }
            BinaryOperator::Or(_) if negated => {
                learn(law, binary.lhs, true, smaller);
                learn(law, binary.rhs, true, smaller);
            }
            BinaryOperator::LessThan(_)
            | BinaryOperator::LessThanOrEqual(_)
            | BinaryOperator::GreaterThan(_)
            | BinaryOperator::GreaterThanOrEqual(_) => {
                let below = matches!(binary.operator, BinaryOperator::LessThan(_) | BinaryOperator::LessThanOrEqual(_));
                let strict = matches!(binary.operator, BinaryOperator::LessThan(_) | BinaryOperator::GreaterThan(_));
                if strict == negated {
                    return;
                }
                let (left, right) = (parameter(law, binary.lhs), parameter(law, binary.rhs));
                let held = [
                    left.filter(|_| right.is_some() || is_literal(binary.rhs)).map(|place| (place, below)),
                    right.filter(|_| left.is_some() || is_literal(binary.lhs)).map(|place| (place, !below)),
                ];
                for (place, below) in held.into_iter().flatten() {
                    smaller.push(if below != negated { Smaller::Below(place) } else { Smaller::Above(place) });
                }
            }
            _ => {}
        },
        _ => {}
    }
}

fn is_literal(expression: &Expression) -> bool {
    match expression {
        Expression::Literal(_) => true,
        Expression::UnaryPrefix(prefix) => {
            matches!(prefix.operator, UnaryPrefixOperator::Negation(_)) && is_literal(prefix.operand)
        }
        _ => false,
    }
}
