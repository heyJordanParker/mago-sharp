use mago_allocator::Arena;
use std::rc::Rc;

use foldhash::HashMap;

use mago_algebra::AlgebraThresholds;
use mago_algebra::clause::Clause;
use mago_algebra::negate_formula;
use mago_codex::assertion::Assertion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::Span;
use mago_syntax::cst::Expression;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;

use crate::code::IssueCode;
use crate::context::Context;
use crate::utils::names::display_atomic;
use crate::utils::names::display_bool;
use crate::utils::names::display_variable_name;

/// Checks for two types of logical issues between a set of existing assertions (`formula_1`)
/// and a new set of assertions (`formula_2`) from a conditional expression.
pub fn check_for_paradox<A>(
    context: &mut Context<'_, '_, A>,
    formula_1: &[Rc<Clause>],
    formula_2: &[Clause],
    span: Span,
) where
    A: Arena,
{
    let algebra_thresholds = context.settings.algebra_thresholds();
    let formula_1_hashes: HashMap<u32, Span> = formula_1.iter().map(|c| (c.hash, c.condition_span)).collect();
    let mut formula_2_hashes: HashMap<u32, Span> = HashMap::default();

    for formula_2_clause in formula_2 {
        if !formula_2_clause.generated
            && !formula_2_clause.wedge
            && formula_2_clause.reconcilable
            && let Some(original_span) =
                formula_1_hashes.get(&formula_2_clause.hash).or_else(|| formula_2_hashes.get(&formula_2_clause.hash))
            && *original_span != span
        {
            report_redundant_condition(context, formula_2_clause, span, *original_span);
        }

        formula_2_hashes.entry(formula_2_clause.hash).or_insert(formula_2_clause.condition_span);
    }

    let Some(negated_formula_2) = negate_formula(formula_2.to_vec(), &algebra_thresholds) else {
        return;
    };

    let formula_1_clauses = formula_1.iter().map(|c| &**c).collect::<Vec<_>>();
    for negated_clause_2 in &negated_formula_2 {
        if !negated_clause_2.reconcilable || negated_clause_2.wedge {
            continue;
        }

        for clause_1 in &formula_1_clauses {
            if negated_clause_2 == *clause_1 || !clause_1.reconcilable || clause_1.wedge {
                continue;
            }

            let is_subset = clause_1.possibilities.iter().all(|(key, clause_1_possibilities)| {
                if let Some(clause_2_possibilities) = negated_clause_2.possibilities.get(key) {
                    clause_1_possibilities == clause_2_possibilities
                } else {
                    false
                }
            });

            if is_subset && !clause_1.possibilities.is_empty() {
                report_paradoxical_condition(context, clause_1, negated_clause_2, span, &algebra_thresholds);

                return;
            }
        }
    }
}

fn report_redundant_condition<A>(
    context: &mut Context<'_, '_, A>,
    redundant_clause: &Clause,
    redundant_span: Span,
    original_span: Span,
) where
    A: Arena,
{
    let (kind, title) = if redundant_clause.to_string() == "isset" {
        (IssueCode::RedundantIssetCheck, "Redundant `isset` check")
    } else {
        (IssueCode::RedundantCondition, "Redundant condition")
    };
    let clause_string = display_clause(context, redundant_clause);
    let holds = display_bool(context, true);

    context.collector.report_with_code(
        kind,
        Issue::warning(title)
            .with_annotation(
                Annotation::primary(redundant_span)
                    .with_message(format!("This condition (`{clause_string}`) is always {holds} here")),
            )
            .with_annotation(
                Annotation::secondary(original_span)
                    .with_message(format!("This was already established as {holds} by a previous condition here")),
            )
            .with_note(format!("The analyzer determined this condition is guaranteed to be {holds} based on preceding logic, making this check unnecessary."))
            .with_help("Consider removing this redundant conditional check to simplify the code.")
    );
}

fn report_paradoxical_condition<A>(
    context: &mut Context<'_, '_, A>,
    original_clause: &Clause,
    negated_conflicting_clause: &Clause,
    paradox_span: Span,
    algebra_thresholds: &AlgebraThresholds,
) where
    A: Arena,
{
    let Some(conflicting_clause) = negate_formula(vec![negated_conflicting_clause.clone()], algebra_thresholds) else {
        return;
    };

    let new_condition_str =
        conflicting_clause.iter().map(|clause| display_clause(context, clause)).collect::<Vec<_>>().join(" && ");
    let established_fact_str = display_clause(context, original_clause);
    let holds = display_bool(context, true);

    context.collector.report_with_code(
        IssueCode::ParadoxicalCondition,
        Issue::error("Paradoxical condition")
            .with_annotation(
                Annotation::primary(paradox_span)
                    .with_message(format!("This condition (`{new_condition_str}`) can never be {holds} here")),
            )
            .with_annotation(
                Annotation::secondary(original_clause.condition_span)
                    .with_message("Because of this preceding condition..."),
            )
            .with_note(format!(
                "...the analyzer knows that `{established_fact_str}` must be {holds} for this code path to be taken."
            ))
            .with_note(format!(
                "Therefore, this new condition (`{new_condition_str}`) directly contradicts that established fact."
            ))
            .with_note("As a result, the code this condition guards is unreachable.")
            .with_help("Remove the unreachable code or refactor the conditional logic."),
    );
}

/// Returns `clause` as the analyzed file writes it. In a `.sharp` file each variable and type is written as PHP#
/// writes it, and the clause reads as the `||` of its parts, as `count is 2 || done`. PHP gets the clause's own text.
fn display_clause<A>(context: &Context<'_, '_, A>, clause: &Clause) -> String
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return clause.to_string();
    }

    let mut parts = Vec::new();
    for (variable, assertions) in &clause.possibilities {
        let variable = if variable.as_bytes().starts_with(b"*") {
            "<expr>".to_owned()
        } else {
            display_variable_name(context, variable.as_bytes())
        };

        for assertion in assertions.values() {
            parts.push(match assertion {
                Assertion::Truthy => variable.clone(),
                Assertion::Falsy => format!("!{variable}"),
                Assertion::IsType(atomic) | Assertion::IsIdentical(atomic) | Assertion::IsEqual(atomic) => {
                    format!("{variable} is {}", display_atomic(context, atomic))
                }
                Assertion::IsNotType(atomic) | Assertion::IsNotIdentical(atomic) | Assertion::IsNotEqual(atomic) => {
                    format!("{variable} is not {}", display_atomic(context, atomic))
                }
                assertion => assertion.to_atom().to_string(),
            });
        }
    }

    parts.join(" || ")
}

#[inline]
pub const fn unwrap_expression<'ast, 'arena>(expression: &'ast Expression<'arena>) -> &'ast Expression<'arena> {
    match expression {
        Expression::Parenthesized(parenthesized) => unwrap_expression(parenthesized.expression),
        // `@` only suppresses diagnostics; it yields the operand's value unchanged.
        Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::ErrorControl(_), operand }) => {
            unwrap_expression(operand)
        }
        _ => expression,
    }
}
