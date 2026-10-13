use std::collections::BTreeMap;
use std::collections::BTreeSet;

use mago_names::ResolvedNames;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::Closure;
use mago_syntax::cst::Expression;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::MethodPartialApplication;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPostfix;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::Unset;
use mago_syntax::walker::MutWalker;
use mago_word::Word;

use crate::utils::expression::get_root_expression_id;

pub fn get_assignment_map<'ast, 'arena>(
    pre_conditions: &[&'ast Expression<'arena>],
    post_expressions: &[&'ast Expression<'arena>],
    statements: &'ast [Statement<'arena>],
    resolved_names: &ResolvedNames<'arena>,
) -> (BTreeMap<Word, BTreeSet<Word>>, Option<Word>) {
    let mut walker = AssignmentMapWalker::default();
    let mut resolved_names = resolved_names;

    for pre_condition in pre_conditions {
        walker.walk_expression(pre_condition, &mut resolved_names);
    }

    for statement in statements {
        walker.walk_statement(statement, &mut resolved_names);
    }

    for post_expression in post_expressions {
        walker.walk_expression(post_expression, &mut resolved_names);
    }

    let first_variable_id = walker.assignment_map.first_key_value().map(|(key, _)| *key);

    (walker.assignment_map, first_variable_id)
}

#[derive(Debug, Clone, Default)]
struct AssignmentMapWalker {
    assignment_map: BTreeMap<Word, BTreeSet<Word>>,
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, &ResolvedNames<'arena>> for AssignmentMapWalker {
    fn walk_unary_postfix(&mut self, unary_postfix: &'ast UnaryPostfix<'arena>, names: &mut &ResolvedNames<'arena>) {
        let root_expression_id = get_root_expression_id(unary_postfix.operand, names);

        if let Some(root_expression_id) = root_expression_id {
            self.assignment_map.entry(root_expression_id).or_default().insert(root_expression_id);
        }
    }

    fn walk_unary_prefix(&mut self, unary_prefix: &'ast UnaryPrefix<'arena>, names: &mut &ResolvedNames<'arena>) {
        if unary_prefix.operator.is_increment_or_decrement() {
            let root_expression_id = get_root_expression_id(unary_prefix.operand, names);

            if let Some(root_expression_id) = root_expression_id {
                self.assignment_map.entry(root_expression_id).or_default().insert(root_expression_id);
            }
        } else {
            self.walk_expression(unary_prefix.operand, names);
        }
    }

    fn walk_assignment(&mut self, assignment: &'ast Assignment<'arena>, names: &mut &ResolvedNames<'arena>) {
        let right_expression_id = get_root_expression_id(assignment.rhs, names).unwrap_or_else(|| Word::from("isset"));

        if let Some(array_elements) = assignment.lhs.get_array_like_elements() {
            for array_element in array_elements {
                if let Some(expression) = array_element.get_value() {
                    let left_expression_id = get_root_expression_id(expression, names);

                    if let Some(left_expression_id) = left_expression_id {
                        self.assignment_map.entry(left_expression_id).or_default().insert(right_expression_id);
                    }
                }
            }
        } else {
            let left_expression_id = get_root_expression_id(assignment.lhs, names);

            if let Some(left_expression_id) = left_expression_id {
                self.assignment_map.entry(left_expression_id).or_default().insert(right_expression_id);
            }
        }
    }

    fn walk_in_argument_list(&mut self, argument_list: &'ast ArgumentList<'arena>, names: &mut &ResolvedNames<'arena>) {
        for argument in &argument_list.arguments {
            let root_expression_id = get_root_expression_id(argument.value(), names);

            if let Some(root_expression_id) = root_expression_id {
                self.assignment_map.entry(root_expression_id).or_default().insert(root_expression_id);
            }
        }
    }

    fn walk_out_method_call(&mut self, method_call: &'ast MethodCall<'arena>, names: &mut &ResolvedNames<'arena>) {
        let root_expression_id = get_root_expression_id(method_call.object, names);

        if let Some(root_expression_id) = root_expression_id {
            self.assignment_map.entry(root_expression_id).or_default().insert(Word::from("isset"));
        }
    }

    fn walk_out_method_partial_application(
        &mut self,
        method_partial_application: &'ast MethodPartialApplication<'arena>,
        names: &mut &ResolvedNames<'arena>,
    ) {
        let root_expression_id = get_root_expression_id(method_partial_application.object, names);

        if let Some(root_expression_id) = root_expression_id {
            self.assignment_map.entry(root_expression_id).or_default().insert(Word::from("isset"));
        }
    }

    fn walk_in_unset(&mut self, unset: &'ast Unset<'arena>, names: &mut &ResolvedNames<'arena>) {
        for unset_value in &unset.values {
            let root_expression_id = get_root_expression_id(unset_value, names);

            if let Some(root_expression_id) = root_expression_id {
                self.assignment_map.entry(root_expression_id).or_default().insert(root_expression_id);
            }
        }
    }

    // Prevent walking into closure and arrow function bodies
    fn walk_closure(&mut self, _closure: &'ast Closure<'arena>, _names: &mut &ResolvedNames<'arena>) {}
    fn walk_arrow_function(
        &mut self,
        _arrow_function: &'ast ArrowFunction<'arena>,
        _names: &mut &ResolvedNames<'arena>,
    ) {
    }
}
