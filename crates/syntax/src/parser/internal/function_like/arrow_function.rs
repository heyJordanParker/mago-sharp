use crate::T;
use crate::cst::cst::ArrowFunction;
use crate::cst::cst::AttributeList;
use crate::cst::cst::Closure;
use crate::cst::cst::Expression;
use crate::cst::cst::FunctionLikeParameter;
use crate::cst::cst::FunctionLikeParameterList;
use crate::cst::sequence::Sequence;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::Precedence;
use mago_allocator::prelude::*;
use mago_span::HasSpan;
use mago_span::Span;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Parses a PHP `fn` arrow function. A PHP# file refuses it, so there it stands as an error expression.
    pub(crate) fn parse_arrow_function_with_attributes(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
    ) -> Result<&'arena Expression<'arena>, ParseError> {
        let arrow_function = ArrowFunction {
            attribute_lists: attributes,
            r#static: self.maybe_expect_keyword(T!["static"])?,
            r#fn: Some(self.expect_php_lambda_keyword(T!["fn"])?),
            ampersand: if self.stream.is_at(T!["&"])? { Some(self.stream.eat_span(T!["&"])?) } else { None },
            parameter_list: self.parse_function_like_parameter_list()?,
            return_type_hint: self.parse_optional_function_like_return_type_hint()?,
            arrow: self.stream.eat_span(T!["=>"])?,
            expression: self.arena.alloc(self.parse_expression()?),
        };

        Ok(self.arena.alloc(if self.dialect.is_sharp() {
            Expression::Error(arrow_function.span())
        } else {
            Expression::ArrowFunction(arrow_function)
        }))
    }

    /// Whether a PHP# lambda starts here: a bare name, or a parenthesized list, followed by `=>`. A lambda binds as
    /// loosely as assignment, as in C#, so it starts only where a whole value, an assignment's value or a branch of
    /// `? :` goes. After any other operator, `=>` belongs to what holds the expression, such as a `match` arm.
    pub(crate) fn is_at_lambda(&mut self, precedence: Precedence) -> Result<bool, ParseError> {
        if precedence > Precedence::ElvisOrConditional {
            return Ok(false);
        }

        Ok(match self.stream.peek_kind(0)? {
            Some(T![Identifier]) => self.stream.peek_kind(1)? == Some(T!["=>"]),
            Some(T!["("]) => self.stream.peek_kind_after_parentheses()? == Some(T!["=>"]),
            _ => false,
        })
    }

    /// Reads a PHP# lambda written right after an operator, as in `handler ?? item => item.ready`: a `=>` after a
    /// whole expression whose last operand is a name or a parenthesized group. A lambda needs parentheses there, so
    /// this reports it, reads the lambda's body so the rest of the file parses on, and returns the expression and the
    /// lambda as one error. It returns `None` anywhere else.
    pub(crate) fn parse_lambda_after_an_operator(
        &mut self,
        expression: &'arena Expression<'arena>,
        precedence: Precedence,
    ) -> Result<Option<&'arena Expression<'arena>>, ParseError> {
        if precedence != Precedence::Lowest || !self.dialect.is_sharp() || !self.stream.is_at(T!["=>"])? {
            return Ok(None);
        }
        let Some(parameters) = last_operand_after_an_operator(expression) else {
            return Ok(None);
        };

        let arrow = self.stream.consume_span()?;
        self.errors.push(ParseError::LambdaAfterOperatorInSharp(parameters.join(arrow)));
        let body =
            if self.stream.is_at(T!["{"])? { self.parse_block()?.span() } else { self.parse_expression()?.span() };

        Ok(Some(self.arena.alloc(Expression::Error(expression.span().join(body)))))
    }

    /// Parses a PHP# lambda: an [`ArrowFunction`] without `fn` when its body is an expression, and a [`Closure`]
    /// without `function` or `use` when its body is a block. A lone parameter has no parentheses, so its list's
    /// parentheses are empty spans at its start and end.
    pub(crate) fn parse_lambda(&mut self) -> Result<&'arena Expression<'arena>, ParseError> {
        let parameter_list = if self.stream.is_at(T!["("])? {
            self.parse_lambda_parameter_list()?
        } else {
            let variable = self.parse_bare_variable()?;
            let span = variable.span;
            let parameter = FunctionLikeParameter {
                attribute_lists: Sequence::empty(),
                modifiers: Sequence::empty(),
                hint: None,
                ampersand: None,
                ellipsis: None,
                variable,
                default_value: None,
                hooks: None,
            };

            FunctionLikeParameterList {
                left_parenthesis: Span::new(span.file_id, span.start, span.start),
                parameters: TokenSeparatedSequence::new(self.new_vec_of(parameter), self.new_vec()),
                right_parenthesis: Span::new(span.file_id, span.end, span.end),
            }
        };
        let arrow = self.stream.eat_span(T!["=>"])?;

        Ok(self.arena.alloc(if self.stream.is_at(T!["{"])? {
            Expression::Closure(Closure {
                attribute_lists: Sequence::empty(),
                r#static: None,
                function: None,
                ampersand: None,
                parameter_list,
                use_clause: None,
                return_type_hint: None,
                arrow: Some(arrow),
                body: self.parse_block()?,
            })
        } else {
            Expression::ArrowFunction(ArrowFunction {
                attribute_lists: Sequence::empty(),
                r#static: None,
                r#fn: None,
                ampersand: None,
                parameter_list,
                return_type_hint: None,
                arrow,
                expression: self.parse_expression()?,
            })
        }))
    }
}

/// The name or parenthesized group that ends `expression` right after an operator, which a `=>` after it would make a
/// lambda's parameters.
fn last_operand_after_an_operator<'arena>(mut expression: &'arena Expression<'arena>) -> Option<Span> {
    loop {
        expression = match expression {
            Expression::Binary(binary) => binary.rhs,
            Expression::UnaryPrefix(prefix) => prefix.operand,
            Expression::Assignment(assignment) => assignment.rhs,
            Expression::Conditional(conditional) => conditional.r#else,
            _ => return None,
        };
        if matches!(expression, Expression::ConstantAccess(_) | Expression::Parenthesized(_)) {
            return Some(expression.span());
        }
    }
}
