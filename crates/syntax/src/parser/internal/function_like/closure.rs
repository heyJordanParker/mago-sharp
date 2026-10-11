use crate::T;
use crate::cst::cst::AttributeList;
use crate::cst::cst::Closure;
use crate::cst::cst::ClosureUseClause;
use crate::cst::cst::ClosureUseClauseVariable;
use crate::cst::cst::Expression;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;
use mago_span::HasSpan;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Parses a PHP `function` closure. A PHP# file refuses it, so there it stands as an error expression.
    pub(crate) fn parse_closure_with_attributes(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
    ) -> Result<&'arena Expression<'arena>, ParseError> {
        let closure = Closure {
            attribute_lists: attributes,
            r#static: self.maybe_expect_keyword(T!["static"])?,
            function: Some(self.expect_php_lambda_keyword(T!["function"])?),
            ampersand: if self.stream.is_at(T!["&"])? { Some(self.stream.eat_span(T!["&"])?) } else { None },
            parameter_list: self.parse_function_like_parameter_list()?,
            use_clause: self.parse_optional_closure_use_clause()?,
            return_type_hint: self.parse_optional_function_like_return_type_hint()?,
            arrow: None,
            body: self.parse_block()?,
        };

        Ok(self.arena.alloc(if self.dialect.is_sharp() {
            Expression::Error(closure.span())
        } else {
            Expression::Closure(closure)
        }))
    }

    fn parse_optional_closure_use_clause(&mut self) -> Result<Option<ClosureUseClause<'arena>>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T!["use"]) => Some(self.parse_closure_use_clause()?),
            _ => None,
        })
    }

    fn parse_closure_use_clause(&mut self) -> Result<ClosureUseClause<'arena>, ParseError> {
        let r#use = self.expect_php_keyword(T!["use"])?;
        let result =
            self.parse_comma_separated_sequence(T!["("], T![")"], |p| p.parse_closure_use_clause_variable())?;

        Ok(ClosureUseClause {
            r#use,
            left_parenthesis: result.open,
            variables: result.sequence,
            right_parenthesis: result.close,
        })
    }

    fn parse_closure_use_clause_variable(&mut self) -> Result<ClosureUseClauseVariable<'arena>, ParseError> {
        Ok(ClosureUseClauseVariable {
            ampersand: if self.stream.is_at(T!["&"])? { Some(self.stream.eat_span(T!["&"])?) } else { None },
            variable: self.parse_direct_variable()?,
        })
    }
}
