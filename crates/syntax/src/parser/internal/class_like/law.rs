use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::Law;
use crate::cst::cst::MethodExpressionBody;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether a PHP# class member starts a law: `law`, a name and `(`. Everywhere else `law` is a name, such as a
    /// return type after a modifier or a call `law(x)`.
    pub(crate) fn is_at_law(&mut self) -> Result<bool, ParseError> {
        let starts_with_law =
            self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"law");
        let named = self.stream.lookahead(1)?.is_some_and(|token| token.kind.is_identifier_maybe_reserved());

        Ok(starts_with_law && named && self.stream.peek_kind(2)? == Some(T!["("]))
    }

    /// Parses a PHP# law, spec section 28: `law`, its name, its parameters and an expression body.
    pub(crate) fn parse_law(&mut self) -> Result<Law<'arena>, ParseError> {
        Ok(Law {
            law: self.expect_any_keyword()?,
            name: self.parse_local_identifier()?,
            parameter_list: self.parse_function_like_parameter_list()?,
            body: MethodExpressionBody {
                arrow: self.stream.eat_span(T!["=>"])?,
                expression: self.parse_expression()?,
                semicolon: self.stream.eat_span(T![";"])?,
            },
        })
    }
}
