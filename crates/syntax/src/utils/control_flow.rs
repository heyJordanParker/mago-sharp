use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax_core::stack::ensure_sufficient_stack;

use crate::cst::Access;
use crate::cst::Array;
use crate::cst::ArrayElement;
use crate::cst::Block;
use crate::cst::Break;
use crate::cst::Call;
use crate::cst::ClassLikeConstantSelector;
use crate::cst::ClassLikeMemberSelector;
use crate::cst::Construct;
use crate::cst::Continue;
use crate::cst::Expression;
use crate::cst::ForBody;
use crate::cst::ForeachBody;
use crate::cst::ForeachTarget;
use crate::cst::IfBody;
use crate::cst::LegacyArray;
use crate::cst::List;
use crate::cst::Literal;
use crate::cst::LiteralInteger;
use crate::cst::MatchArm;
use crate::cst::PartialApplication;
use crate::cst::PatternMatch;
use crate::cst::PatternMatchArm;
use crate::cst::PatternMatchArmBody;
use crate::cst::PatternMatchPatternArm;
use crate::cst::Return;
use crate::cst::Statement;
use crate::cst::StringPart;
use crate::cst::SwitchBody;
use crate::cst::SwitchCase;
use crate::cst::Throw;
use crate::cst::Variable;
use crate::cst::WhileBody;
use crate::cst::Yield;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlFlow<'arena> {
    Return(&'arena Return<'arena>),
    Throw(&'arena Throw<'arena>),
    Continue(&'arena Continue<'arena>),
    Break(&'arena Break<'arena>),
}

impl HasSpan for ControlFlow<'_> {
    fn span(&self) -> Span {
        match self {
            ControlFlow::Return(r#return) => r#return.span(),
            ControlFlow::Throw(throw) => throw.span(),
            ControlFlow::Continue(r#continue) => r#continue.span(),
            ControlFlow::Break(r#break) => r#break.span(),
        }
    }
}

#[inline]
#[must_use]
pub fn find_control_flows_in_block<'arena>(block: &'arena Block<'arena>) -> Vec<ControlFlow<'arena>> {
    let mut controls = vec![];
    block_control_flows(block, &mut controls);

    controls
}

#[inline]
#[must_use]
pub fn find_control_flows_in_statement<'arena>(statement: &'arena Statement<'arena>) -> Vec<ControlFlow<'arena>> {
    let mut controls = vec![];
    statement_control_flows(statement, &mut controls);

    controls
}

#[inline]
#[must_use]
pub fn find_control_flows_in_expression<'arena>(expression: &'arena Expression<'arena>) -> Vec<ControlFlow<'arena>> {
    let mut controls = vec![];
    expression_control_flows(expression, &mut controls);

    controls
}

fn block_control_flows<'arena>(block: &'arena Block<'arena>, controls: &mut Vec<ControlFlow<'arena>>) {
    for statement in &block.statements {
        statement_control_flows(statement, controls);
    }
}

fn statement_control_flows<'arena>(statement: &'arena Statement<'arena>, controls: &mut Vec<ControlFlow<'arena>>) {
    ensure_sufficient_stack(|| match statement {
        Statement::Namespace(namespace) => {
            for statement in namespace.statements() {
                statement_control_flows(statement, controls);
            }
        }
        Statement::Block(block) => {
            block_control_flows(block, controls);
        }
        Statement::Try(r#try) => {
            block_control_flows(&r#try.block, controls);

            for catch in &r#try.catch_clauses {
                block_control_flows(&catch.block, controls);
            }

            if let Some(finally) = &r#try.finally_clause {
                block_control_flows(&finally.block, controls);
            }
        }
        Statement::Foreach(foreach) => {
            expression_control_flows(foreach.expression, controls);
            match &foreach.target {
                ForeachTarget::Value(foreach_value_target) => {
                    expression_control_flows(foreach_value_target.value, controls);
                }
                ForeachTarget::KeyValue(foreach_key_value_target) => {
                    expression_control_flows(foreach_key_value_target.key, controls);
                    expression_control_flows(foreach_key_value_target.value, controls);
                }
            }

            match &foreach.body {
                ForeachBody::Statement(statement) => {
                    statement_control_flows(statement, controls);
                }
                ForeachBody::ColonDelimited(foreach_colon_delimited_body) => {
                    for statement in &foreach_colon_delimited_body.statements {
                        statement_control_flows(statement, controls);
                    }
                }
            }
        }
        Statement::For(r#for) => {
            if let Some(declaration) = &r#for.declaration {
                expression_control_flows(declaration.value, controls);
            }

            for initialization in &r#for.initializations {
                expression_control_flows(initialization, controls);
            }

            for condition in &r#for.conditions {
                expression_control_flows(condition, controls);
            }

            for increment in &r#for.increments {
                expression_control_flows(increment, controls);
            }

            match &r#for.body {
                ForBody::Statement(statement) => {
                    statement_control_flows(statement, controls);
                }
                ForBody::ColonDelimited(foreach_colon_delimited_body) => {
                    for statement in &foreach_colon_delimited_body.statements {
                        statement_control_flows(statement, controls);
                    }
                }
            }
        }
        Statement::ForOf(for_of) => {
            expression_control_flows(for_of.expression, controls);
            statement_control_flows(for_of.body, controls);
        }
        Statement::While(r#while) => {
            expression_control_flows(r#while.condition, controls);

            match &r#while.body {
                WhileBody::Statement(statement) => {
                    statement_control_flows(statement, controls);
                }
                WhileBody::ColonDelimited(foreach_colon_delimited_body) => {
                    for statement in &foreach_colon_delimited_body.statements {
                        statement_control_flows(statement, controls);
                    }
                }
            }
        }
        Statement::DoWhile(do_while) => {
            expression_control_flows(do_while.condition, controls);
            statement_control_flows(do_while.statement, controls);
        }
        Statement::Switch(switch) => {
            expression_control_flows(switch.expression, controls);

            let cases = match &switch.body {
                SwitchBody::BraceDelimited(switch_brace_delimited_body) => &switch_brace_delimited_body.cases,
                SwitchBody::ColonDelimited(switch_colon_delimited_body) => &switch_colon_delimited_body.cases,
            };

            let mut switch_controls = vec![];
            for case in cases {
                match &case {
                    SwitchCase::Expression(switch_expression_case) => {
                        expression_control_flows(switch_expression_case.expression, &mut switch_controls);

                        for statement in &switch_expression_case.statements {
                            statement_control_flows(statement, &mut switch_controls);
                        }
                    }
                    SwitchCase::Default(switch_default_case) => {
                        for statement in &switch_default_case.statements {
                            statement_control_flows(statement, &mut switch_controls);
                        }
                    }
                }
            }

            for control in switch_controls {
                match control {
                    ControlFlow::Break(r#break) => {
                        if !matches!(
                            r#break.level,
                            Some(Expression::Literal(Literal::Integer(LiteralInteger { value: Some(1), .. }))) | None
                        ) {
                            controls.push(control);
                        }
                    }
                    _ => controls.push(control),
                }
            }
        }
        Statement::If(r#if) => {
            expression_control_flows(r#if.condition, controls);

            match &r#if.body {
                IfBody::Statement(if_statement_body) => {
                    statement_control_flows(if_statement_body.statement, controls);

                    for else_if in &if_statement_body.else_if_clauses {
                        expression_control_flows(else_if.condition, controls);
                        statement_control_flows(else_if.statement, controls);
                    }

                    if let Some(else_clause) = &if_statement_body.else_clause {
                        statement_control_flows(else_clause.statement, controls);
                    }
                }
                IfBody::ColonDelimited(if_colon_delimited_body) => {
                    for statement in &if_colon_delimited_body.statements {
                        statement_control_flows(statement, controls);
                    }

                    for else_if in &if_colon_delimited_body.else_if_clauses {
                        expression_control_flows(else_if.condition, controls);
                        for statement in &else_if.statements {
                            statement_control_flows(statement, controls);
                        }
                    }

                    if let Some(else_clause) = &if_colon_delimited_body.else_clause {
                        for statement in &else_clause.statements {
                            statement_control_flows(statement, controls);
                        }
                    }
                }
            }
        }
        Statement::Return(r#return) => {
            controls.push(ControlFlow::Return(r#return));
            if let Some(value) = &r#return.value {
                expression_control_flows(value, controls);
            }
        }
        Statement::Continue(r#continue) => {
            controls.push(ControlFlow::Continue(r#continue));
            if let Some(level) = &r#continue.level {
                expression_control_flows(level, controls);
            }
        }
        Statement::Break(r#break) => {
            controls.push(ControlFlow::Break(r#break));
            if let Some(level) = &r#break.level {
                expression_control_flows(level, controls);
            }
        }
        Statement::Expression(expression_statement) => {
            expression_control_flows(expression_statement.expression, controls);
        }
        Statement::Echo(echo) => {
            for expression in &echo.values {
                expression_control_flows(expression, controls);
            }
        }
        Statement::Unset(unset) => {
            for value in &unset.values {
                expression_control_flows(value, controls);
            }
        }
        Statement::PatternMatch(pattern_match) => pattern_match_control_flows(pattern_match, controls),
        _ => {}
    });
}

fn pattern_match_control_flows<'arena>(
    pattern_match: &'arena PatternMatch<'arena>,
    controls: &mut Vec<ControlFlow<'arena>>,
) {
    expression_control_flows(pattern_match.expression, controls);
    for arm in &pattern_match.arms {
        if let PatternMatchArm::Pattern(PatternMatchPatternArm { guard: Some(guard), .. }) = arm {
            expression_control_flows(guard.condition, controls);
        }

        match arm.body() {
            PatternMatchArmBody::Expression(expression) => expression_control_flows(expression, controls),
            PatternMatchArmBody::Block(block) => block_control_flows(block, controls),
        }
    }
}

fn expression_control_flows<'arena>(expression: &'arena Expression<'arena>, controls: &mut Vec<ControlFlow<'arena>>) {
    ensure_sufficient_stack(|| match expression {
        Expression::Binary(binary) => {
            expression_control_flows(binary.lhs, controls);
            expression_control_flows(binary.rhs, controls);
        }
        Expression::UnaryPrefix(unary_prefix) => {
            expression_control_flows(unary_prefix.operand, controls);
        }
        Expression::UnaryPostfix(unary_postfix) => {
            expression_control_flows(unary_postfix.operand, controls);
        }
        Expression::Parenthesized(parenthesized) => {
            expression_control_flows(parenthesized.expression, controls);
        }
        Expression::CompositeString(composite_string) => {
            for part in composite_string.parts() {
                match part {
                    StringPart::Expression(expression) => {
                        expression_control_flows(expression, controls);
                    }
                    StringPart::BracedExpression(braced_expression_string_part) => {
                        expression_control_flows(braced_expression_string_part.expression, controls);
                    }
                    StringPart::Literal(_) => {}
                }
            }
        }
        Expression::Assignment(assignment) => {
            expression_control_flows(assignment.lhs, controls);
            expression_control_flows(assignment.rhs, controls);
        }
        Expression::Conditional(conditional) => {
            expression_control_flows(conditional.condition, controls);
            if let Some(then) = &conditional.then {
                expression_control_flows(then, controls);
            }

            expression_control_flows(conditional.r#else, controls);
        }
        Expression::Array(Array { elements, .. })
        | Expression::LegacyArray(LegacyArray { elements, .. })
        | Expression::List(List { elements, .. }) => {
            for element in elements {
                match element {
                    ArrayElement::KeyValue(key_value_array_element) => {
                        expression_control_flows(key_value_array_element.key, controls);
                        expression_control_flows(key_value_array_element.value, controls);
                    }
                    ArrayElement::Value(value_array_element) => {
                        expression_control_flows(value_array_element.value, controls);
                    }
                    ArrayElement::Variadic(variadic_array_element) => {
                        expression_control_flows(variadic_array_element.value, controls);
                    }
                    ArrayElement::Missing(_) => {}
                }
            }
        }
        Expression::ArrayAccess(array_access) => {
            expression_control_flows(array_access.array, controls);
            expression_control_flows(array_access.index, controls);
        }
        Expression::ArrayAppend(array_append) => {
            expression_control_flows(array_append.array, controls);
        }
        Expression::AnonymousClass(anonymous_class) => {
            if let Some(arguments) = &anonymous_class.argument_list {
                for argument in &arguments.arguments {
                    let Some(value) = argument.value() else {
                        continue;
                    };

                    expression_control_flows(value, controls);
                }
            }
        }
        Expression::Match(r#match) => {
            expression_control_flows(r#match.expression, controls);
            for arm in &r#match.arms {
                match arm {
                    MatchArm::Expression(match_expression_arm) => {
                        for condition in &match_expression_arm.conditions {
                            expression_control_flows(condition, controls);
                        }

                        expression_control_flows(match_expression_arm.expression, controls);
                    }
                    MatchArm::Default(match_default_arm) => {
                        expression_control_flows(match_default_arm.expression, controls);
                    }
                }
            }
        }
        Expression::PatternMatch(pattern_match) => pattern_match_control_flows(pattern_match, controls),
        Expression::Is(is) => expression_control_flows(is.value, controls),
        Expression::As(r#as) => expression_control_flows(r#as.value, controls),
        Expression::Yield(r#yield) => match r#yield {
            Yield::Value(yield_value) => {
                if let Some(value) = &yield_value.value {
                    expression_control_flows(value, controls);
                }
            }
            Yield::Pair(yield_pair) => {
                expression_control_flows(yield_pair.key, controls);
                expression_control_flows(yield_pair.value, controls);
            }
            Yield::From(yield_from) => {
                expression_control_flows(yield_from.iterator, controls);
            }
        },
        Expression::Construct(construct) => match construct {
            Construct::Isset(isset_construct) => {
                for expression in &isset_construct.values {
                    expression_control_flows(expression, controls);
                }
            }
            Construct::Empty(empty_construct) => {
                expression_control_flows(empty_construct.value, controls);
            }
            Construct::Eval(eval_construct) => {
                expression_control_flows(eval_construct.value, controls);
            }
            Construct::Include(include_construct) => {
                expression_control_flows(include_construct.value, controls);
            }
            Construct::IncludeOnce(include_once_construct) => {
                expression_control_flows(include_once_construct.value, controls);
            }
            Construct::Require(require_construct) => {
                expression_control_flows(require_construct.value, controls);
            }
            Construct::RequireOnce(require_once_construct) => {
                expression_control_flows(require_once_construct.value, controls);
            }
            Construct::Print(print_construct) => {
                expression_control_flows(print_construct.value, controls);
            }
            Construct::Exit(exit_construct) => {
                if let Some(arguments) = &exit_construct.arguments {
                    for argument in &arguments.arguments {
                        expression_control_flows(argument.value(), controls);
                    }
                }
            }
            Construct::Die(die_construct) => {
                if let Some(arguments) = &die_construct.arguments {
                    for argument in &arguments.arguments {
                        expression_control_flows(argument.value(), controls);
                    }
                }
            }
        },
        Expression::Throw(throw) => {
            controls.push(ControlFlow::Throw(throw));
        }
        Expression::Clone(clone) => {
            expression_control_flows(clone.object, controls);
        }
        Expression::Call(call) => match call {
            Call::Function(function_call) => {
                expression_control_flows(function_call.function, controls);
                for argument in &function_call.argument_list.arguments {
                    expression_control_flows(argument.value(), controls);
                }
            }
            Call::Method(method_call) => {
                expression_control_flows(method_call.object, controls);
                match &method_call.method {
                    ClassLikeMemberSelector::Variable(variable) => {
                        variable_control_flows(variable, controls);
                    }
                    ClassLikeMemberSelector::Expression(class_like_member_expression_selector) => {
                        expression_control_flows(class_like_member_expression_selector.expression, controls);
                    }
                    _ => {}
                }

                for argument in &method_call.argument_list.arguments {
                    expression_control_flows(argument.value(), controls);
                }
            }
            Call::NullSafeMethod(null_safe_method_call) => {
                expression_control_flows(null_safe_method_call.object, controls);
                match &null_safe_method_call.method {
                    ClassLikeMemberSelector::Variable(variable) => {
                        variable_control_flows(variable, controls);
                    }
                    ClassLikeMemberSelector::Expression(class_like_member_expression_selector) => {
                        expression_control_flows(class_like_member_expression_selector.expression, controls);
                    }
                    _ => {}
                }

                for argument in &null_safe_method_call.argument_list.arguments {
                    expression_control_flows(argument.value(), controls);
                }
            }
            Call::StaticMethod(static_method_call) => {
                expression_control_flows(static_method_call.class, controls);
                match &static_method_call.method {
                    ClassLikeMemberSelector::Variable(variable) => {
                        variable_control_flows(variable, controls);
                    }
                    ClassLikeMemberSelector::Expression(class_like_member_expression_selector) => {
                        expression_control_flows(class_like_member_expression_selector.expression, controls);
                    }
                    _ => {}
                }

                for argument in &static_method_call.argument_list.arguments {
                    expression_control_flows(argument.value(), controls);
                }
            }
        },
        Expression::Access(access) => match access {
            Access::Property(property_access) => {
                expression_control_flows(property_access.object, controls);
                match &property_access.property {
                    ClassLikeMemberSelector::Variable(variable) => {
                        variable_control_flows(variable, controls);
                    }
                    ClassLikeMemberSelector::Expression(class_like_member_expression_selector) => {
                        expression_control_flows(class_like_member_expression_selector.expression, controls);
                    }
                    _ => {}
                }
            }
            Access::NullSafeProperty(null_safe_property_access) => {
                expression_control_flows(null_safe_property_access.object, controls);
                match &null_safe_property_access.property {
                    ClassLikeMemberSelector::Variable(variable) => {
                        variable_control_flows(variable, controls);
                    }
                    ClassLikeMemberSelector::Expression(class_like_member_expression_selector) => {
                        expression_control_flows(class_like_member_expression_selector.expression, controls);
                    }
                    _ => {}
                }
            }
            Access::StaticProperty(static_property_access) => {
                expression_control_flows(static_property_access.class, controls);
                variable_control_flows(&static_property_access.property, controls);
            }
            Access::ClassConstant(class_constant_access) => {
                expression_control_flows(class_constant_access.class, controls);
                if let ClassLikeConstantSelector::Expression(class_like_member_expression_selector) =
                    &class_constant_access.constant
                {
                    expression_control_flows(class_like_member_expression_selector.expression, controls);
                }
            }
        },
        Expression::Variable(variable) => {
            variable_control_flows(variable, controls);
        }
        Expression::PartialApplication(partial_application) => match partial_application {
            PartialApplication::Function(function_partial_application) => {
                expression_control_flows(function_partial_application.function, controls);
            }
            PartialApplication::Method(method_partial_application) => {
                expression_control_flows(method_partial_application.object, controls);
                match &method_partial_application.method {
                    ClassLikeMemberSelector::Variable(variable) => {
                        variable_control_flows(variable, controls);
                    }
                    ClassLikeMemberSelector::Expression(class_like_member_expression_selector) => {
                        expression_control_flows(class_like_member_expression_selector.expression, controls);
                    }
                    _ => {}
                }
            }
            PartialApplication::StaticMethod(static_method_partial_application) => {
                expression_control_flows(static_method_partial_application.class, controls);
                match &static_method_partial_application.method {
                    ClassLikeMemberSelector::Variable(variable) => {
                        variable_control_flows(variable, controls);
                    }
                    ClassLikeMemberSelector::Expression(class_like_member_expression_selector) => {
                        expression_control_flows(class_like_member_expression_selector.expression, controls);
                    }
                    _ => {}
                }
            }
        },
        Expression::Instantiation(instantiation) => {
            expression_control_flows(instantiation.class, controls);
            if let Some(argument_list) = &instantiation.argument_list {
                for argument in &argument_list.arguments {
                    expression_control_flows(argument.value(), controls);
                }
            }
        }
        _ => {}
    });
}

fn variable_control_flows<'arena>(variable: &'arena Variable<'arena>, controls: &mut Vec<ControlFlow<'arena>>) {
    match variable {
        Variable::Indirect(indirect_variable) => expression_control_flows(indirect_variable.expression, controls),
        Variable::Nested(nested_variable) => variable_control_flows(nested_variable.variable, controls),
        Variable::Direct(_) => {}
    }
}
