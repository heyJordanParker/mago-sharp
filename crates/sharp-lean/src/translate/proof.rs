//! A law's proof: the Lean theorem of its statement, proven by its body, or by Lean's automatic steps alone for a law
//! without one.
//!
//! Each `if` is a case split on its condition and each `match` arm a case of its subject, each call of a law is a fact
//! in scope, the theorem of the law it calls, and each case ends in `sharp_close`, which `Sharp/Lean/Proof.lean`
//! defines. A call of the law itself is Lean's recursion on the theorem.

use mago_analyzer::smaller::Path;
use mago_analyzer::smaller::Step;
use mago_names::binding::Binding;
use mago_sharp_bridge::CheckedProgram;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Block;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Law as LawNode;
use mago_syntax::cst::LawProof;
use mago_syntax::cst::PatternMatch;
use mago_syntax::cst::PatternMatchArm;
use mago_syntax::cst::PatternMatchArmBody;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPrefixOperator;

use crate::issues;
use crate::reach::Reach;
use crate::translate::Measure;
use crate::translate::Unmodeled;
use crate::translate::Use;
use crate::translate::body::Body;
use crate::translate::body::Kind;
use crate::translate::body::indented;
use crate::translate::names;
use crate::translate::pattern::Choice;
use crate::translate::pattern::choice;

/// The theorem of a law, and what its errors name.
#[derive(Debug, Clone, Default)]
pub(crate) struct Proof {
    /// The Lean theorem, `<statement>.proof`.
    pub(crate) text: String,
    /// The law's claim as PHP# writes it, which a case's goal prints as when it holds a part PHP# writes no other way.
    pub(crate) claim: String,
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

/// What holds on one path through a body.
#[derive(Clone)]
struct Scope<'prover, 'program, 'ast, 'arena> {
    locals: Vec<&'ast Statement<'arena>>,
    /// Each case hypothesis, by its Lean name, with the condition it states in PHP#.
    hypotheses: Vec<(String, String)>,
    /// What the conditions and patterns on the path show about the law's parameters.
    path: Path<'prover, 'program>,
    /// Whether the path calls the law itself.
    recurses: bool,
}

/// What writes the proof of one law.
pub(crate) struct Prover<'prover, 'program, 'ast, 'arena> {
    checked: &'prover CheckedProgram<'program>,
    reach: &'prover Reach,
    /// The fully qualified PHP name of the class-like that states the law.
    full: &'prover [u8],
    /// The law as messages name it: `Money.timesKeepsCurrency`.
    place: String,
    law: &'ast LawNode<'arena>,
    statement: String,
    proof: Proof,
    facts: u32,
    hypotheses: u32,
    /// The parameter the body's first call of the law itself makes smaller, and how, which Lean's recursion on the
    /// theorem ends by.
    step: Option<(usize, Step)>,
}

impl<'prover, 'program, 'ast, 'arena> Prover<'prover, 'program, 'ast, 'arena> {
    pub(crate) fn new(
        checked: &'prover CheckedProgram<'program>,
        reach: &'prover Reach,
        full: &'prover [u8],
        place: String,
        law: &'ast LawNode<'arena>,
        statement: String,
    ) -> Self {
        Self {
            checked,
            reach,
            full,
            place,
            law,
            statement,
            proof: Proof::default(),
            facts: 0,
            hypotheses: 0,
            step: None,
        }
    }

    /// A translation of the law's code on the path `scope`, whose calls of the law's class pass the smaller values
    /// the path shows.
    fn body(&self, scope: &Scope<'prover, 'program, 'ast, 'arena>) -> Body<'prover, 'program> {
        let mut body = Body::for_law(self.checked, self.reach, self.full, self.place.clone(), self.law);
        body.on_path(scope.path.clone());

        body
    }

    /// The law's theorem: `theorem <statement>.proof (parameters) : <statement> parameters := by …`.
    pub(crate) fn prove(mut self, binders: &[String], parameters: &[String]) -> Proof {
        let mut lines = vec![format!("unfold {}", self.statement)];
        let mut scope = Scope {
            locals: Vec::new(),
            hypotheses: Vec::new(),
            path: Path::new(self.checked.names(), self.law.parameter_list.parameters.iter()),
            recurses: false,
        };
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

        self.proof.claim = self.source(self.law.claim.span());
        let applied = std::iter::once(self.statement.clone()).chain(parameters.iter().cloned()).collect::<Vec<_>>();
        self.proof.text = format!(
            "theorem {}.proof {} : {} := by\n{}",
            self.statement,
            binders.join(" "),
            applied.join(" "),
            lines.iter().map(|line| format!("  {line}")).collect::<Vec<_>>().join("\n")
        );
        if let Some((place, step)) = self.step {
            self.proof.text.push('\n');
            self.proof.text.push_str(&Measure::new(place, step, parameters).termination);
        }

        self.proof
    }

    /// The tactics of `statements`, the rest of a path through the body, which `scope` holds on.
    fn statements(
        &mut self,
        statements: &[&'ast Statement<'arena>],
        mut scope: Scope<'prover, 'program, 'ast, 'arena>,
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
                then.hypotheses.push((hypothesis.clone(), written));
                then.path = scope.path.branch(r#if.condition, true);
                let branch: Vec<&Statement> = std::iter::once(body.statement).chain(rest.iter().copied()).collect();
                self.branch(&branch, then, lines);

                let mut otherwise = scope;
                otherwise.hypotheses.push((hypothesis, negated(self, r#if.condition)));
                otherwise.path = otherwise.path.branch(r#if.condition, false);
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
    fn branch(
        &mut self,
        statements: &[&'ast Statement<'arena>],
        scope: Scope<'prover, 'program, 'ast, 'arena>,
        lines: &mut Vec<String>,
    ) {
        let mut branch = Vec::new();
        self.statements(statements, scope, &mut branch);
        for (index, line) in branch.into_iter().enumerate() {
            lines.push(if index == 0 { format!("· {line}") } else { format!("  {line}") });
        }
    }

    /// A `match` statement: each arm is a case that knows its pattern and its `when` guard, as the pattern tests of a
    /// method's `match` test them, and `default` is the case where no arm matches. The subject is a value the tactics
    /// name, so each case split rewrites it where the claim reads it.
    fn r#match(
        &mut self,
        r#match: &'ast PatternMatch<'arena>,
        rest: &[&'ast Statement<'arena>],
        scope: Scope<'prover, 'program, 'ast, 'arena>,
        lines: &mut Vec<String>,
    ) {
        let mut body = self.body(&scope);
        let (computed, subject) = body.nested(|body| body.value(r#match.expression));
        if !computed.is_empty() || !self.names_value(&scope, r#match.expression) {
            self.proof.unmodeled.push(Unmodeled {
                span: r#match.expression.span(),
                what: format!("a match on `{}`", self.source(r#match.expression.span())),
                place: self.place.clone(),
                reason: issues::LAW_MATCH,
            });
            return;
        }

        let r#type = self.checked.types().expression_type(r#match.expression);
        let written = self.source(r#match.expression.span());
        let statements = |block: Option<&'ast Block<'arena>>| -> Vec<&'ast Statement<'arena>> {
            block.into_iter().flat_map(|block| block.statements.iter()).chain(rest.iter().copied()).collect()
        };
        let (paths, missed_path) = scope.path.arms(r#match.expression, r#match.arms.as_slice());
        let mut arms = Vec::new();
        // What holds where no arm before matched: each arm's pattern failed, or its guard did.
        let mut missed = Vec::new();
        let mut default = None;
        let mut tested = true;
        for (arm, path) in r#match.arms.iter().zip(paths) {
            let PatternMatchArmBody::Block(block) = arm.body() else {
                continue;
            };
            let PatternMatchArm::Pattern(arm) = arm else {
                default = Some(block);
                continue;
            };
            body.names_elements_of(&written);
            let Some(mut test) = body.test(&subject, r#type, arm.pattern) else {
                tested = false;
                break;
            };
            let pattern = self.source(arm.pattern.span());
            let (holds, fails_here) = match &arm.guard {
                Some(guard) => {
                    let Some(condition) = body.pure(guard.condition, "a guard that can throw", issues::GUARD) else {
                        tested = false;
                        break;
                    };
                    test.conditions.push(condition);
                    let guarded = format!("{written} is {pattern} && {}", self.source(guard.condition.span()));
                    (guarded.clone(), format!("!({guarded})"))
                }
                None => (format!("{written} is {pattern}"), format!("{written} is not {pattern}")),
            };

            let mut case = scope.clone();
            case.hypotheses.push((String::new(), holds));
            case.path = path;
            let mut tactics = Vec::new();
            self.statements(&statements(Some(block)), case, &mut tactics);
            arms.push((test, tactics));

            missed.push(fails_here);
        }
        self.proof.uses.append(&mut body.uses);
        self.proof.unmodeled.append(&mut body.unmodeled);
        if !tested {
            return;
        }

        let mut otherwise = scope;
        if !missed.is_empty() {
            otherwise.hypotheses.push((String::new(), missed.join(" && ")));
        }
        otherwise.path = missed_path;
        let mut tactics = Vec::new();
        self.statements(&statements(default), otherwise, &mut tactics);
        // Where every arm failed on an enum, whose arms cover its cases, the cases show that no value is left.
        let otherwise = if body.kind(r#type) == Kind::Enum {
            let mut covered = vec![format!("try (cases {subject} <;> simp_all; done)"), "all_goals".to_owned()];
            covered.extend(tactics.into_iter().map(|line| format!("  {line}")));
            covered
        } else {
            tactics
        };

        lines.extend(self.tactics(&choice(arms, otherwise)));
    }

    /// The tactics of `choice`, the case split of a `match`'s arms: a named `match` for each Lean pattern an arm tests,
    /// so the claim reads the value of each case, `by_cases` for each condition, and `obtain` for each name an arm gives
    /// its value. A `_` after patterns that cover every value is left out, since Lean refuses an arm that never runs.
    fn tactics(&mut self, choice: &Choice) -> Vec<String> {
        self.hypotheses += 1;
        let index = self.hypotheses;
        match choice {
            Choice::Branch(tactics) => tactics.clone(),
            Choice::If(condition, then, otherwise) => {
                let mut lines = vec![format!("by_cases «$c{index}» : ({condition}) = true")];
                let then = self.tactics(then);
                lines.extend(dotted(then));
                let otherwise = self.tactics(otherwise);
                lines.extend(dotted(otherwise));
                lines
            }
            Choice::Match(term, alternatives) => {
                let mut lines = vec![format!("match «$m{index}» : {term} with")];
                let patterns: Vec<&str> = alternatives.iter().map(|(pattern, _)| pattern.as_str()).collect();
                for (place, (pattern, alternative)) in alternatives.iter().enumerate() {
                    if pattern == "_" && covers(&patterns[..place]) {
                        continue;
                    }
                    lines.push(format!("| {pattern} =>"));
                    let tactics = self.tactics(alternative);
                    lines.extend(tactics.into_iter().map(|line| format!("  {line}")));
                }
                lines
            }
            Choice::Let(name, value, inner) => {
                let mut lines = vec![format!("obtain ⟨{name}, «$l{index}»⟩ : ∃ value, value = {value} := ⟨_, rfl⟩")];
                lines.extend(self.tactics(inner));
                lines
            }
        }
    }

    /// A call statement of a law: the fact `have «$fN» := sharp_call% N (<law>.proof <arguments>)`, which proves the
    /// called law's `when` condition first. Arguments that compute are bound to `«$argN»`, which the fact holds for
    /// each value they compute.
    fn call(
        &mut self,
        call: &'ast Call<'arena>,
        scope: &mut Scope<'prover, 'program, 'ast, 'arena>,
        lines: &mut Vec<String>,
    ) {
        let Some((key, arguments)) = self.called(call) else {
            return;
        };
        // A call of the law itself passes the smaller value the path shows as its pure term, which Lean's recursion on
        // the theorem ends by, as a method's recursion does.
        let mut stepped = None;
        if key == self.statement {
            scope.recurses = true;
            stepped = self
                .body(scope)
                .smaller(call.get_argument_list())
                .filter(|(smaller, _)| smaller.parameter == smaller.argument);
            if let Some((smaller, _)) = &stepped {
                self.step.get_or_insert((smaller.parameter, smaller.step));
            }
        } else if !self.proof.laws.iter().any(|(law, _)| *law == key) {
            self.proof.laws.push((key.clone(), call.span()));
        }

        let values: Vec<&Expression> = arguments.iter().map(Argument::value).collect();
        // The smaller value as its pure term: an int one step from the parameter, or the rest of a list pattern.
        let stepped = stepped.and_then(|(smaller, term)| {
            let term = term.or_else(|| match values.get(smaller.argument) {
                Some(Expression::ConstantAccess(rest)) => Some(names::identifier(rest.name.value())),
                _ => None,
            })?;
            Some((smaller.argument, term))
        });
        // Each other argument, by its place, is computed in the call's `do` block.
        let places: Vec<usize> =
            (0..values.len()).filter(|place| stepped.as_ref().is_none_or(|(argument, _)| argument != place)).collect();
        let computed_values: Vec<&Expression> = places.iter().map(|place| values[*place]).collect();
        let (computed, terms, uses, unmodeled) = self.body(scope).terms(&scope.locals, &computed_values);
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
        // The argument at each place: its pure term when nothing computes, and else the binder `«$argN»` of the value
        // the `do` block computes, named by the place, apart from the smaller value, which is passed as it is.
        let applied = |computes: bool| -> Vec<String> {
            (0..values.len())
                .map(|place| match &stepped {
                    Some((argument, term)) if *argument == place => format!("({term})"),
                    _ if computes => format!("«$arg{}»", place + 1),
                    // The terms skip the smaller value's place.
                    Some((argument, _)) if *argument < place => format!("({})", terms[place - 1]),
                    _ => format!("({})", terms[place]),
                })
                .collect()
        };
        if computed.is_empty() {
            lines.push(format!("have {fact} := sharp_call% {index} ({key}.proof {})", applied(false).join(" ")));
            return;
        }

        let binders: Vec<String> = places.iter().map(|place| format!("«$arg{}»", place + 1)).collect();
        lines.push(format!("have {fact} := fun {} (_ : ((do", binders.join(" ")));
        lines.extend(computed.iter().map(|line| format!("      {line}")));
        lines.push(format!("      return ({})) : M _) = pure ({})) =>", terms.join(", "), binders.join(", ")));
        lines.push(format!("    sharp_call% {index} ({key}.proof {})", applied(true).join(" ")));
    }

    /// The Lean name of the law `call` calls, `Class.law(…)` a law of `Class`, and its arguments.
    fn called(&self, call: &'ast Call<'arena>) -> Option<(String, &'ast [Argument<'arena>])> {
        let Call::Method(call) = call else {
            return None;
        };
        let class = self.checked.names().static_call_class(call)?;
        let ClassLikeMemberSelector::Identifier(name) = &call.method else {
            return None;
        };
        let full = self.checked.names().get(&class.name);

        Some((names::member(&names::full_name(full), name.value), call.argument_list.arguments.as_slice()))
    }

    /// `(do … return (c)) = pure true`, the condition `expression` as a case hypothesis.
    fn condition(
        &mut self,
        scope: &Scope<'prover, 'program, 'ast, 'arena>,
        expression: &'ast Expression<'arena>,
    ) -> String {
        let (lines, terms, uses, unmodeled) = self.body(scope).terms(&scope.locals, &[expression]);
        self.proof.uses.extend(uses);
        self.proof.unmodeled.extend(unmodeled);
        let computed: Vec<String> = indented(&lines, 6).chain([format!("      return ({}))", terms[0])]).collect();

        format!("((do\n{} : M Bool) = pure true", computed.join("\n"))
    }

    /// The step that closes a case: `sharp_close` with its index and its hypotheses.
    fn close(&mut self, scope: &Scope<'prover, 'program, 'ast, 'arena>, step: &str) -> String {
        let hypotheses: Vec<&str> =
            scope.hypotheses.iter().map(|(name, _)| name.as_str()).filter(|name| !name.is_empty()).collect();
        let smaller = (!scope.recurses).then(|| self.smaller_call(scope)).flatten();
        self.proof
            .cases
            .push(Case { conditions: scope.hypotheses.iter().map(|(_, written)| written.clone()).collect(), smaller });

        format!("{step} {} [{}]", self.proof.cases.len(), hypotheses.join(", "))
    }

    /// The call of the law itself on the first smaller value the path `scope` shows, a list pattern's rest before an
    /// int one step towards its bound, and the parameter it makes smaller.
    fn smaller_call(&self, scope: &Scope<'prover, 'program, 'ast, 'arena>) -> Option<(String, String)> {
        let rest = scope.path.rests.first().map(|(rest, parameter)| (*parameter, self.source(*rest)));
        let (place, value) = rest.or_else(|| {
            let fact = scope.path.facts.first()?;
            let operator = match fact.step {
                Step::Down(_) => "-",
                Step::Up(_) => "+",
                Step::Rest => return None,
            };
            Some((fact.parameter, format!("{} {operator} 1", self.parameter_written(fact.parameter))))
        })?;
        let arguments: Vec<String> = (0..self.law.parameter_list.parameters.len())
            .map(|index| if index == place { value.clone() } else { self.parameter_written(index) })
            .collect();

        Some((self.parameter_written(place), format!("{}({})", self.place, arguments.join(", "))))
    }

    fn parameter_written(&self, place: usize) -> String {
        let parameter = &self.law.parameter_list.parameters.as_slice()[place];

        String::from_utf8_lossy(parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name))
            .into_owned()
    }

    /// Whether `expression` names a value the proof's tactics name: a parameter of the law, a value a pattern of its
    /// body names, or a property of one. A local of the body is none of them, since only the code that computes a law
    /// call's arguments declares it.
    fn names_value(&self, scope: &Scope<'prover, 'program, 'ast, 'arena>, expression: &Expression) -> bool {
        match expression {
            Expression::Parenthesized(parenthesized) => self.names_value(scope, parenthesized.expression),
            Expression::Access(Access::Property(access)) => self.names_value(scope, access.object),
            Expression::ConstantAccess(name) => match self.checked.names().binding(&name.name) {
                Some(Binding::Local(local)) => !scope.locals.iter().any(|statement| {
                    let Statement::LocalDeclaration(declaration) = statement else {
                        return false;
                    };
                    declaration
                        .target
                        .names()
                        .iter()
                        .any(|name| name.span.start.offset == local.declaration.start.offset)
                }),
                _ => false,
            },
            _ => false,
        }
    }

    fn source(&self, span: Span) -> String {
        let contents = &self.checked.file().contents;
        let text = contents.get(span.start.offset as usize..span.end.offset as usize).unwrap_or_default();

        String::from_utf8_lossy(text).into_owned()
    }
}

/// Whether the Lean patterns `patterns` of one term cover every value of it: `_` or a name, or a list's `[]` beside an
/// element before a rest that is a name or `_`.
fn covers(patterns: &[&str]) -> bool {
    let name =
        |pattern: &str| pattern.chars().all(|character| character.is_alphanumeric() || "_«»$'".contains(character));

    patterns.iter().any(|pattern| name(pattern))
        || (patterns.contains(&"[]")
            && patterns.iter().any(|pattern| pattern.split_once(" :: ").is_some_and(|(_, tail)| name(tail))))
}

/// `tactics` as one branch of a case split: `·` before the first.
fn dotted(tactics: Vec<String>) -> impl Iterator<Item = String> {
    tactics
        .into_iter()
        .enumerate()
        .map(|(index, line)| if index == 0 { format!("· {line}") } else { format!("  {line}") })
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
