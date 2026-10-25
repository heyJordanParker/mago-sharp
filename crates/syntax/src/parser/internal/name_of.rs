use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::DottedIdentifier;
use crate::cst::cst::Identifier;
use crate::cst::cst::NameOf;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether PHP# `nameof(` starts here.
    pub(crate) fn is_at_name_of(&mut self) -> Result<bool, ParseError<'arena>> {
        Ok(self.dialect.is_sharp()
            && self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"nameof")
            && self.stream.peek_kind(1)? == Some(T!["("]))
    }

    /// Parses `nameof(Order.total)`. A class written as a full name, as in `nameof(App.Order.total)`, reads as one
    /// dotted name before the last `.`, which the analyzer refuses with the import to add, spec section 23.
    pub(crate) fn parse_name_of(&mut self) -> Result<NameOf<'arena>, ParseError<'arena>> {
        let nameof = self.expect_any_keyword()?;
        let left_parenthesis = self.stream.eat_span(T!["("])?;
        let class = if self.stream.peek_kind(1)? == Some(T!["."]) && self.stream.peek_kind(3)? == Some(T!["."]) {
            self.parse_dotted_class_before_member()?
        } else {
            self.parse_identifier()?
        };

        Ok(NameOf {
            nameof,
            left_parenthesis,
            class,
            dot: self.stream.eat_span(T!["."])?,
            property: self.parse_local_identifier()?,
            right_parenthesis: self.stream.eat_span(T![")"])?,
        })
    }

    /// Parses the names joined by `.` that start here, up to the `.` before the last one, as `App.Order` in
    /// `App.Order.total`.
    fn parse_dotted_class_before_member(&mut self) -> Result<Identifier<'arena>, ParseError<'arena>> {
        let first = self.parse_local_identifier()?;
        let mut value = std::vec::Vec::from(first.value);
        let mut span = first.span;
        while self.stream.peek_kind(0)? == Some(T!["."])
            && self.stream.peek_kind(1)?.is_some_and(|kind| kind.is_identifier_maybe_reserved())
            && self.stream.peek_kind(2)? == Some(T!["."])
        {
            self.stream.consume()?;
            let part = self.parse_local_identifier()?;
            value.push(b'.');
            value.extend_from_slice(part.value);
            span = span.join(part.span);
        }

        Ok(Identifier::Dotted(DottedIdentifier { span, value: self.bytes(&value) }))
    }
}
