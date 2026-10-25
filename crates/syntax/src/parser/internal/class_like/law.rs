use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::Law;
use crate::cst::cst::LawCondition;
use crate::cst::cst::LawProof;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::Precedence;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether a PHP# class member starts a law: `law`, a name and `(`. Everywhere else `law` is a name, such as a
    /// return type after a modifier or a call `law(x)`.
    pub(crate) fn is_at_law(&mut self) -> Result<bool, ParseError<'arena>> {
        let starts_with_law =
            self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"law");
        let named = self.stream.lookahead(1)?.is_some_and(|token| token.kind.is_identifier_maybe_reserved());

        Ok(starts_with_law && named && self.stream.peek_kind(2)? == Some(T!["("]))
    }

    /// Parses a PHP# law, spec section 28: `law`, its name, its parameters, an optional `when` condition, `=>` and its
    /// claim, then `;` or the body that proves it. The `when` condition reads as a `match` arm's does, so the `=>`
    /// after it ends it.
    pub(crate) fn parse_law(&mut self) -> Result<Law<'arena>, ParseError<'arena>> {
        let law = self.expect_any_keyword()?;
        let name = self.parse_local_identifier()?;
        let parameter_list = self.parse_function_like_parameter_list()?;
        let condition = match self.stream.lookahead(0)? {
            Some(token) if token.kind == T![Identifier] && token.value == b"when" => {
                let when = self.expect_any_keyword()?;
                let condition = self.parse_expression_with_precedence(Precedence::NullCoalesce)?;

                Some(LawCondition { when, expression: self.parse_conditional_in_guard(condition)? })
            }
            _ => None,
        };
        let arrow = self.stream.eat_span(T!["=>"])?;
        let claim = self.parse_expression()?;
        let proof = if self.stream.is_at(T!["{"])? {
            LawProof::Body(self.parse_block()?)
        } else {
            LawProof::Checker(self.stream.eat_span(T![";"])?)
        };

        Ok(Law { law, name, parameter_list, condition, arrow, claim, proof })
    }
}
