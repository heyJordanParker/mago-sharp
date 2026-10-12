use crate::T;
use crate::cst::cst::Instantiation;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::Precedence;
use mago_allocator::prelude::*;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_instantiation(&mut self) -> Result<Instantiation<'arena>, ParseError> {
        Ok(Instantiation {
            new: self.expect_keyword(T!["new"])?,
            class: self.arena.alloc(self.parse_expression_with_precedence(Precedence::New)?),
            type_arguments: self.parse_optional_type_argument_list()?,
            constructor: None,
            argument_list: self.parse_optional_argument_list()?,
        })
    }
}
