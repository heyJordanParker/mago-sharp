//! Spec section 28's smaller value, one rule for the analyzer and the checker.
//!
//! The analyzer refuses a law's call of itself that passes none, and the checker measures a law's or a method's
//! recursion by it: a list's rest from a list pattern, or an int one step towards a bound a condition on the path
//! tested. The bound is an int literal, tested with `<`, `<=`, `>` or `>=`, or another int parameter the call passes
//! on unchanged, tested with `<` or `>`. Both walk a body's paths as a [`Path`], which each `if`, `?:`, `&&`, `||` and
//! `match` arm on the way extends.

use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Hint;
use mago_syntax::cst::ListPatternElement;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::PatternMatchArm;
use mago_syntax::cst::UnaryPrefixOperator;

/// The value an int parameter steps towards: an int literal, or the parameter at a place, which every call that steps
/// towards it passes on unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    Literal(i64),
    Parameter(usize),
}

/// How a parameter's value gets smaller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A list parameter's rest from a list pattern that holds an element besides the rest.
    Rest,
    /// An int parameter minus one, where a condition on the path to the call showed it above the bound.
    Down(Bound),
    /// An int parameter plus one, where a condition on the path to the call showed it below the bound.
    Up(Bound),
}

/// What a condition on the path shows about an int parameter: a call may pass it one step towards `step`'s bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fact {
    pub parameter: usize,
    pub step: Step,
}

/// An argument of a call that passes a smaller value of one of the caller's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Smaller {
    /// The caller's parameter, by place.
    pub parameter: usize,
    /// The argument that passes the smaller value, by place.
    pub argument: usize,
    pub step: Step,
}

/// What a parameter's type lets the rule read of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Int,
    /// A `List`, never null.
    List,
    Other,
}

/// One path through the body of a method or a law: its parameters, as the code reads them through `names`, and what
/// the conditions and patterns on the way show about them.
#[derive(Debug, Clone)]
pub struct Path<'names, 'arena> {
    names: &'names ResolvedNames<'arena>,
    /// Where each parameter's name starts, and its shape, in order.
    parameters: Vec<(u32, Shape)>,
    /// What the conditions on the path show about the int parameters.
    pub facts: Vec<Fact>,
    /// Each local a list pattern on the path binds to the rest of a list parameter: its name, and the parameter's
    /// place.
    pub rests: Vec<(Span, usize)>,
}

impl<'names, 'arena> Path<'names, 'arena> {
    /// The path at the start of a body with `parameters`, which knows nothing yet.
    pub fn new<'parameter>(
        names: &'names ResolvedNames<'arena>,
        parameters: impl IntoIterator<Item = &'parameter FunctionLikeParameter<'parameter>>,
    ) -> Self {
        let parameters = parameters
            .into_iter()
            .map(|parameter| {
                let shape = match &parameter.hint {
                    Some(Hint::Integer(_)) => Shape::Int,
                    Some(Hint::Generic(generic))
                        if mago_names::short_name(names.resolve(&generic.name).unwrap_or(generic.name.value))
                            == "List" =>
                    {
                        Shape::List
                    }
                    _ => Shape::Other,
                };

                (parameter.variable.span.start.offset, shape)
            })
            .collect();

        Self { names, parameters, facts: Vec::new(), rests: Vec::new() }
    }

    /// The place of the parameter `expression` reads, and whether it is an `int`.
    #[must_use]
    pub fn parameter(&self, expression: &Expression<'_>) -> Option<(usize, bool)> {
        let (place, shape) = self.shaped(expression)?;

        Some((place, shape == Shape::Int))
    }

    /// The path into the branch of `condition` where it is `holds`: it knows what the condition shows about the int
    /// parameters, and, where `x is [first, ...rest]` holds of a list parameter `x`, that `rest` is its rest.
    #[must_use]
    pub fn branch(&self, condition: &Expression<'_>, holds: bool) -> Self {
        let mut path = self.clone();
        path.facts.extend(self.facts(condition, holds));
        if holds && let Expression::Is(is) = condition.unparenthesized() {
            path.rests.extend(self.rest(is.value, is.pattern));
        }

        path
    }

    /// The paths into the arms of a `match` on `subject`, in order, and the path where no arm matched. Each arm knows
    /// what its pattern shows, a comparison pattern about an int parameter and a list pattern's rest of a list one,
    /// and that each arm before it without a `when` guard failed.
    #[must_use]
    pub fn arms(&self, subject: &Expression<'_>, arms: &[PatternMatchArm<'_>]) -> (Vec<Self>, Self) {
        let mut missed = self.clone();
        let mut paths = Vec::new();
        for arm in arms {
            let PatternMatchArm::Pattern(arm) = arm else {
                paths.push(missed.clone());
                continue;
            };
            let mut path = missed.clone();
            path.facts.extend(self.pattern_facts(subject, arm.pattern, true));
            path.rests.extend(self.rest(subject, arm.pattern));
            paths.push(path);
            if arm.guard.is_none() {
                missed.facts.extend(self.pattern_facts(subject, arm.pattern, false));
            }
        }

        (paths, missed)
    }

    /// What `condition` shows about the int parameters where it is `holds`: a comparison of one with an int literal or
    /// another int parameter, under `!`, `&&` where it holds, and `||` where it fails.
    fn facts(&self, condition: &Expression<'_>, holds: bool) -> Vec<Fact> {
        match condition.unparenthesized() {
            Expression::UnaryPrefix(prefix) if matches!(prefix.operator, UnaryPrefixOperator::Not(_)) => {
                self.facts(prefix.operand, !holds)
            }
            Expression::Binary(binary) => match binary.operator {
                BinaryOperator::And(_) | BinaryOperator::LowAnd(_) if holds => {
                    [self.facts(binary.lhs, true), self.facts(binary.rhs, true)].concat()
                }
                BinaryOperator::Or(_) if !holds => {
                    [self.facts(binary.lhs, false), self.facts(binary.rhs, false)].concat()
                }
                _ => {
                    let fact = match (self.int_parameter(binary.lhs), self.int_parameter(binary.rhs)) {
                        (Some(parameter), _) => self.comparison(parameter, &binary.operator, binary.rhs, holds, false),
                        (None, Some(parameter)) => {
                            self.comparison(parameter, &binary.operator, binary.lhs, holds, true)
                        }
                        (None, None) => None,
                    };
                    fact.into_iter().collect()
                }
            },
            _ => Vec::new(),
        }
    }

    /// What the `match` arm's `pattern` shows about `subject` where it is `holds`: a comparison pattern of an int
    /// parameter is the comparison of the parameter.
    fn pattern_facts(&self, subject: &Expression<'_>, pattern: &Pattern<'_>, holds: bool) -> Vec<Fact> {
        match (self.int_parameter(subject), pattern) {
            (Some(parameter), Pattern::Comparison(comparison)) => {
                self.comparison(parameter, &comparison.operator, comparison.value, holds, false).into_iter().collect()
            }
            _ => Vec::new(),
        }
    }

    /// The rest `pattern` binds of the list parameter `subject` reads, when the pattern holds an element besides it,
    /// so the rest is smaller than the list.
    fn rest(&self, subject: &Expression<'_>, pattern: &Pattern<'_>) -> Option<(Span, usize)> {
        let (parameter, Shape::List) = self.shaped(subject)? else {
            return None;
        };
        let mut pattern = pattern;
        while let Pattern::Parenthesized(parenthesized) = pattern {
            pattern = parenthesized.pattern;
        }
        let Pattern::List(list) = pattern else {
            return None;
        };
        let rest = list.elements.iter().find_map(|element| match element {
            ListPatternElement::Rest(rest) => rest.variable.as_ref(),
            ListPatternElement::Pattern(_) => None,
        })?;

        (list.elements.len() > 1).then_some((rest.span(), parameter))
    }

    /// What `parameter OPERATOR other`, or `other OPERATOR parameter` when `swapped`, shows where it is `holds`.
    ///
    /// The int parameter at `parameter` is above or below `other`, an int literal or another int parameter. A literal
    /// bound takes `>=` and `<=` as one step past it, and a parameter only `>` and `<`, since a parameter may be the
    /// end of the range, where one step further leaves it.
    fn comparison(
        &self,
        parameter: usize,
        operator: &BinaryOperator,
        other: &Expression<'_>,
        holds: bool,
        swapped: bool,
    ) -> Option<Fact> {
        let (above, inclusive) = match (operator, swapped) {
            (BinaryOperator::GreaterThan(_), false) | (BinaryOperator::LessThan(_), true) => (true, false),
            (BinaryOperator::GreaterThanOrEqual(_), false) | (BinaryOperator::LessThanOrEqual(_), true) => (true, true),
            (BinaryOperator::LessThan(_), false) | (BinaryOperator::GreaterThan(_), true) => (false, false),
            (BinaryOperator::LessThanOrEqual(_), false) | (BinaryOperator::GreaterThanOrEqual(_), true) => {
                (false, true)
            }
            _ => return None,
        };
        // Where the comparison fails, the opposite one holds: not `>` is `<=`.
        let (above, inclusive) = if holds { (above, inclusive) } else { (!above, !inclusive) };
        let bound = match (self.int_parameter(other), literal(other)) {
            (Some(bound), _) if !inclusive && bound != parameter => Bound::Parameter(bound),
            (_, Some(value)) => Bound::Literal(match (inclusive, above) {
                (false, _) => value,
                (true, true) => value.checked_sub(1)?,
                (true, false) => value.checked_add(1)?,
            }),
            _ => return None,
        };

        Some(Fact { parameter, step: if above { Step::Down(bound) } else { Step::Up(bound) } })
    }

    /// The argument of `arguments` that passes a smaller value of one of the parameters where the path's facts hold.
    ///
    /// It is a list parameter's rest, or an int parameter one step towards a bound a fact names, which a parameter
    /// bound's own argument passes on unchanged. The first that passes it in the parameter's own place comes before any
    /// other, as a call of the caller itself passes it. With an int step, also the parameter it steps from, as the
    /// argument reads it.
    #[must_use]
    pub fn smaller<'expression, 'value>(
        &self,
        arguments: &[&'expression Expression<'value>],
    ) -> Option<(Smaller, Option<&'expression Expression<'value>>)> {
        let mut passed = arguments.iter().enumerate().filter_map(|(argument, value)| {
            let value = value.unparenthesized();
            if let Some(parameter) = self.rest_of(value) {
                return Some((Smaller { parameter, argument, step: Step::Rest }, None));
            }
            let (operand, step) = self.step(arguments, value)?;

            Some((Smaller { parameter: step.parameter, argument, step: step.step }, Some(operand)))
        });
        let first = passed.next()?;
        if first.0.parameter == first.0.argument {
            return Some(first);
        }

        Some(passed.find(|(smaller, _)| smaller.parameter == smaller.argument).unwrap_or(first))
    }

    /// The int parameter `value` steps from, one towards a bound a fact of the path names, and the fact.
    fn step<'expression, 'value>(
        &self,
        arguments: &[&Expression<'_>],
        value: &'expression Expression<'value>,
    ) -> Option<(&'expression Expression<'value>, Fact)> {
        let Expression::Binary(binary) = value else {
            return None;
        };
        let (operand, down) = match binary.operator {
            BinaryOperator::Subtraction(_) if literal(binary.rhs) == Some(1) => (binary.lhs, true),
            BinaryOperator::Addition(_) if literal(binary.rhs) == Some(1) => (binary.lhs, false),
            BinaryOperator::Addition(_) if literal(binary.lhs) == Some(1) => (binary.rhs, false),
            _ => return None,
        };
        let parameter = self.int_parameter(operand)?;
        let passes_on = |bound: Bound| match bound {
            Bound::Literal(_) => true,
            Bound::Parameter(index) => {
                arguments.get(index).is_some_and(|value| self.int_parameter(value) == Some(index))
            }
        };
        let fact = self.facts.iter().rev().find(|fact| {
            fact.parameter == parameter
                && match fact.step {
                    Step::Down(bound) => down && passes_on(bound),
                    Step::Up(bound) => !down && passes_on(bound),
                    Step::Rest => unreachable!("a condition shows an int parameter's bound, never a list's rest"),
                }
        })?;

        Some((operand.unparenthesized(), *fact))
    }

    /// The place and the shape of the parameter `expression` reads.
    fn shaped(&self, expression: &Expression<'_>) -> Option<(usize, Shape)> {
        let declared = self.declared(expression)?;
        let place = self.parameters.iter().position(|(offset, _)| *offset == declared)?;

        Some((place, self.parameters[place].1))
    }

    /// Where the local that `expression`, a bare name, reads is declared, or `None` when it reads no local.
    fn declared(&self, expression: &Expression<'_>) -> Option<u32> {
        let Expression::ConstantAccess(name) = expression.unparenthesized() else {
            return None;
        };
        let Some(Binding::Local(local)) = self.names.binding(&name.name) else {
            return None;
        };

        Some(local.declaration.start.offset)
    }

    fn int_parameter(&self, expression: &Expression<'_>) -> Option<usize> {
        self.parameter(expression).and_then(|(place, int)| int.then_some(place))
    }

    /// The place of the list parameter whose rest, from a list pattern on the path, `expression` reads.
    fn rest_of(&self, expression: &Expression<'_>) -> Option<usize> {
        let declared = self.declared(expression)?;

        self.rests.iter().rev().find(|(rest, _)| rest.start.offset == declared).map(|(_, parameter)| *parameter)
    }
}

/// The value of `expression` when it is an int literal, negated or not.
#[must_use]
pub fn literal(expression: &Expression<'_>) -> Option<i64> {
    match expression.unparenthesized() {
        Expression::Literal(Literal::Integer(integer)) => integer.value.and_then(|value| i64::try_from(value).ok()),
        Expression::UnaryPrefix(prefix) if matches!(prefix.operator, UnaryPrefixOperator::Negation(_)) => {
            literal(prefix.operand)?.checked_neg()
        }
        _ => None,
    }
}
