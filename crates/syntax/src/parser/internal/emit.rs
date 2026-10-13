use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::Emit;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether a PHP# `emit` statement starts here. Spec section 2 reserves `emit`, so in a `.sharp` file it always
    /// starts one at the start of a statement, as in `emit new OrderPaid(…);` and `emit(paid);`. Plain PHP keeps
    /// `emit` a name.
    pub(crate) fn is_at_emit(&mut self) -> Result<bool, ParseError> {
        Ok(self.dialect.is_sharp()
            && self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"emit"))
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
