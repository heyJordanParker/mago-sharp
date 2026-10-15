use mago_codex::metadata::CodebaseMetadata;
use mago_names::ResolvedNames;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Parenthesized;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax_core::stack::ensure_sufficient_stack;
use mago_word::Word;
use mago_word::ascii_lowercase_constant_name_word;
use mago_word::ascii_lowercase_word;

use crate::artifacts::AnalysisArtifacts;
use crate::context::block::BlockContext;
use crate::plugin::hook::NamedCall;

/// Extracts `function_exists` and `defined` calls from a condition expression.
pub fn extract_function_constant_existence(
    resolved_names: &ResolvedNames<'_>,
    codebase: &CodebaseMetadata,
    expression: &Expression<'_>,
    artifacts: &AnalysisArtifacts,
    block_context: &mut BlockContext<'_>,
    negated: bool,
) {
    let extract = |expression, block_context: &mut BlockContext<'_>, negated| {
        extract_function_constant_existence(resolved_names, codebase, expression, artifacts, block_context, negated);
    };

    ensure_sufficient_stack(|| match expression {
        Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::Not(_), operand }) => {
            extract(operand, block_context, !negated);
        }
        Expression::Binary(Binary { lhs, operator: BinaryOperator::And(_) | BinaryOperator::LowAnd(_), rhs })
            if !negated =>
        {
            extract(lhs, block_context, negated);
            extract(rhs, block_context, negated);
        }
        Expression::Binary(Binary { lhs, operator: BinaryOperator::Or(_) | BinaryOperator::LowOr(_), rhs })
            if negated =>
        {
            extract(lhs, block_context, negated);
            extract(rhs, block_context, negated);
        }
        Expression::Call(call) if !negated => {
            let Some(call) = NamedCall::from_call(resolved_names, codebase, call) else {
                return;
            };

            if call.calls(&[b"function_exists"]) {
                if let Some(name) = get_first_literal_string_arg(call.argument_list, artifacts, false) {
                    block_context.known_functions.insert(name);
                }
            } else if call.calls(&[b"defined"])
                && let Some(name) = get_first_literal_string_arg(call.argument_list, artifacts, true)
            {
                block_context.known_constants.insert(name);
            }
        }
        Expression::Parenthesized(Parenthesized { expression, .. }) => {
            extract(expression, block_context, negated);
        }
        _ => {}
    });
}

fn get_first_literal_string_arg(
    argument_list: &ArgumentList,
    artifacts: &AnalysisArtifacts,
    constant: bool,
) -> Option<Word> {
    argument_list
        .arguments
        .first()
        .map(mago_syntax::cst::Argument::value)
        .and_then(|expr| artifacts.get_expression_type(expr))
        .and_then(|ty| ty.get_single_literal_string_value())
        .map(|s| if constant { ascii_lowercase_constant_name_word(s) } else { ascii_lowercase_word(s) })
}
