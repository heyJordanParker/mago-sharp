use crate::T;
use crate::cst::cst::ConstructorName;
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
        let new = self.expect_keyword(T!["new"])?;
        let class = self.arena.alloc(self.parse_expression_with_precedence(Precedence::New)?);
        let type_arguments = self.parse_optional_type_argument_list()?;
        // `new Box<int>.of(…)` calls a named constructor of a generic class, spec sections 9.1 and 11: a `.` after the
        // type arguments names the constructor, as one after the class does in `new Report.fromJson(…)`.
        let constructor = if self.dialect.is_sharp()
            && type_arguments.is_some()
            && self.stream.peek_kind(0)? == Some(T!["."])
            && self.stream.peek_kind(1)?.is_some_and(|kind| kind.is_identifier_maybe_reserved())
        {
            Some(ConstructorName { dot: self.stream.eat_span(T!["."])?, name: self.parse_local_identifier()? })
        } else {
            None
        };

        Ok(Instantiation {
            new,
            class,
            type_arguments,
            constructor,
            argument_list: self.parse_optional_argument_list()?,
        })
    }
}
