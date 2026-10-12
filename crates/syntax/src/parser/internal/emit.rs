use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::Emit;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether a PHP# `emit` statement starts here: `emit` and then `new`, a name or a literal, as in
    /// `emit new OrderPaid(…);` and `emit paid;`. Everywhere else `emit` is a name, such as a local `emit`, a call
    /// `emit(x)` or a test `emit is Paid`.
    pub(crate) fn is_at_emit(&mut self) -> Result<bool, ParseError> {
        let starts_with_emit =
            self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"emit");

        Ok(self.dialect.is_sharp()
            && starts_with_emit
            && self.stream.lookahead(1)?.is_some_and(|token| match token.kind {
                T!["new"] => true,
                T![Identifier] => token.value != b"is",
                kind => kind.is_literal(),
            }))
    }

    /// Parses a PHP# `emit` statement, spec section 15: `emit`, the event it raises, and its terminator.
    pub(crate) fn parse_emit(&mut self) -> Result<Emit<'arena>, ParseError> {
        Ok(Emit {
            emit: self.expect_any_keyword()?,
            value: self.parse_expression()?,
            terminator: self.parse_terminator()?,
        })
    }
}
