use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::TypeOf;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether PHP# `typeof(` starts here.
    pub(crate) fn is_at_type_of(&mut self) -> Result<bool, ParseError> {
        Ok(self.dialect.is_sharp()
            && self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"typeof")
            && self.stream.peek_kind(1)? == Some(T!["("]))
    }

    pub(crate) fn parse_type_of(&mut self) -> Result<TypeOf<'arena>, ParseError> {
        Ok(TypeOf {
            r#typeof: self.expect_any_keyword()?,
            left_parenthesis: self.stream.eat_span(T!["("])?,
            class: self.parse_identifier()?,
            right_parenthesis: self.stream.eat_span(T![")"])?,
        })
    }
}
