//! Spec section 28's smaller value, one rule for the analyzer and the checker.
//!
//! The analyzer refuses a law's call of itself that passes none, and the checker measures a law's or a method's
//! recursion by it: a list's rest from a list pattern, or an int one step towards a bound a condition on the path
//! tested. The bound is an int literal, tested with `<`, `<=`, `>` or `>=`, or another int parameter the call passes
//! on unchanged, tested with `<` or `>`.

use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Literal;
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

/// What the rule reads of the code it judges: the caller's parameters, and the rests its list patterns bound.
pub trait Parameters {
    /// The place of the caller's parameter `expression` names, and whether it is an `int`.
    fn parameter(&self, expression: &Expression<'_>) -> Option<(usize, bool)>;

    /// The place of the list parameter whose rest, from a list pattern on the path, `expression` names.
    fn rest_of(&self, expression: &Expression<'_>) -> Option<usize>;
}

/// What `condition` shows about the int parameters where it is `holds`: a comparison of one with an int literal or
/// another int parameter, under `!`, `&&` where it holds, and `||` where it fails.
pub fn facts(parameters: &impl Parameters, condition: &Expression<'_>, holds: bool) -> Vec<Fact> {
    match unparenthesized(condition) {
        Expression::UnaryPrefix(prefix) if matches!(prefix.operator, UnaryPrefixOperator::Not(_)) => {
            facts(parameters, prefix.operand, !holds)
        }
        Expression::Binary(binary) => match binary.operator {
            BinaryOperator::And(_) | BinaryOperator::LowAnd(_) if holds => {
                [facts(parameters, binary.lhs, true), facts(parameters, binary.rhs, true)].concat()
            }
            BinaryOperator::Or(_) if !holds => {
                [facts(parameters, binary.lhs, false), facts(parameters, binary.rhs, false)].concat()
            }
            _ => {
                let fact = match (int_parameter(parameters, binary.lhs), int_parameter(parameters, binary.rhs)) {
                    (Some(parameter), _) => {
                        comparison(parameters, parameter, &binary.operator, binary.rhs, holds, false)
                    }
                    (None, Some(parameter)) => {
                        comparison(parameters, parameter, &binary.operator, binary.lhs, holds, true)
                    }
                    (None, None) => None,
                };
                fact.into_iter().collect()
            }
        },
        _ => Vec::new(),
    }
}

/// What `parameter OPERATOR other`, or `other OPERATOR parameter` when `swapped`, shows where it is `holds`.
///
/// The int parameter at `parameter` is above or below `other`, an int literal or another int parameter. A literal
/// bound takes `>=` and `<=` as one step past it, and a parameter only `>` and `<`, since a parameter may be the end of
/// the range, where one step further leaves it. A comparison pattern is the comparison of the value it tests.
pub fn comparison(
    parameters: &impl Parameters,
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
        (BinaryOperator::LessThanOrEqual(_), false) | (BinaryOperator::GreaterThanOrEqual(_), true) => (false, true),
        _ => return None,
    };
    // Where the comparison fails, the opposite one holds: not `>` is `<=`.
    let (above, inclusive) = if holds { (above, inclusive) } else { (!above, !inclusive) };
    let bound = match (int_parameter(parameters, other), literal(other)) {
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

/// The argument of `arguments` that passes a smaller value of one of the caller's parameters, where `facts` hold.
///
/// It is a list parameter's rest, or an int parameter one step towards a bound a fact names, which a parameter bound's
/// own argument passes on unchanged. The first that passes it in the parameter's own place comes before any other, as
/// a call of the caller itself passes it. With an int step, also the parameter it steps from, as the argument reads it.
pub fn smaller<'expression, 'arena>(
    parameters: &impl Parameters,
    facts: &[Fact],
    arguments: &[&'expression Expression<'arena>],
) -> Option<(Smaller, Option<&'expression Expression<'arena>>)> {
    let mut passed = arguments.iter().enumerate().filter_map(|(argument, value)| {
        let value = unparenthesized(value);
        if let Some(parameter) = parameters.rest_of(value) {
            return Some((Smaller { parameter, argument, step: Step::Rest }, None));
        }
        let (operand, step) = step(parameters, facts, arguments, value)?;

        Some((Smaller { parameter: step.parameter, argument, step: step.step }, Some(operand)))
    });
    let first = passed.next()?;
    if first.0.parameter == first.0.argument {
        return Some(first);
    }

    Some(passed.find(|(smaller, _)| smaller.parameter == smaller.argument).unwrap_or(first))
}

/// The int parameter `value` steps from, one towards a bound a fact of `facts` names, and the fact.
fn step<'expression, 'arena>(
    parameters: &impl Parameters,
    facts: &[Fact],
    arguments: &[&Expression<'_>],
    value: &'expression Expression<'arena>,
) -> Option<(&'expression Expression<'arena>, Fact)> {
    let Expression::Binary(binary) = value else {
        return None;
    };
    let (operand, down) = match binary.operator {
        BinaryOperator::Subtraction(_) if literal(binary.rhs) == Some(1) => (binary.lhs, true),
        BinaryOperator::Addition(_) if literal(binary.rhs) == Some(1) => (binary.lhs, false),
        BinaryOperator::Addition(_) if literal(binary.lhs) == Some(1) => (binary.rhs, false),
        _ => return None,
    };
    let parameter = int_parameter(parameters, operand)?;
    let passes_on = |bound: Bound| match bound {
        Bound::Literal(_) => true,
        Bound::Parameter(index) => {
            arguments.get(index).is_some_and(|value| int_parameter(parameters, value) == Some(index))
        }
    };
    let fact = facts.iter().rev().find(|fact| {
        fact.parameter == parameter
            && match fact.step {
                Step::Down(bound) => down && passes_on(bound),
                Step::Up(bound) => !down && passes_on(bound),
                Step::Rest => unreachable!("a condition shows an int parameter's bound, never a list's rest"),
            }
    })?;

    Some((unparenthesized(operand), *fact))
}

fn int_parameter(parameters: &impl Parameters, expression: &Expression<'_>) -> Option<usize> {
    parameters.parameter(expression).and_then(|(place, int)| int.then_some(place))
}

/// The value of `expression` when it is an int literal, negated or not.
fn literal(expression: &Expression<'_>) -> Option<i64> {
    match unparenthesized(expression) {
        Expression::Literal(Literal::Integer(integer)) => integer.value.and_then(|value| i64::try_from(value).ok()),
        Expression::UnaryPrefix(prefix) if matches!(prefix.operator, UnaryPrefixOperator::Negation(_)) => {
            literal(prefix.operand)?.checked_neg()
        }
        _ => None,
    }
}

fn unparenthesized<'expression, 'arena>(
    expression: &'expression Expression<'arena>,
) -> &'expression Expression<'arena> {
    match expression {
        Expression::Parenthesized(parenthesized) => unparenthesized(parenthesized.expression),
        expression => expression,
    }
}
