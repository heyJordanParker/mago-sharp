use crate::T;
use crate::cst::cst::Extends;
use crate::cst::cst::Hint;
use crate::cst::cst::Implements;
use crate::cst::cst::Inheritance;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;
use mago_span::Span;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_optional_implements(&mut self) -> Result<Option<Implements<'arena>>, ParseError> {
        if !self.is_at_php_keyword(T!["implements"])? {
            return Ok(None);
        }

        Ok(Some(Implements {
            implements: self.expect_any_keyword()?,
            types: {
                let mut types = self.new_vec();
                let mut commas = self.new_vec();
                loop {
                    types.push(self.parse_identifier()?);

                    match self.stream.peek_kind(0)? {
                        Some(T![","]) => {
                            commas.push(self.stream.consume()?);
                        }
                        _ => break,
                    }
                }

                TokenSeparatedSequence::new(types, commas)
            },
        }))
    }

    /// PHP#'s `: Base, Interface` header, as spec section 22 writes it. Each entry is a type, as in C#'s base list, so
    /// `: PaginatedList<Order>` names a generic base class.
    pub(crate) fn parse_optional_inheritance(&mut self) -> Result<Option<Inheritance<'arena>>, ParseError> {
        if !self.dialect.is_sharp() || !matches!(self.stream.peek_kind(0)?, Some(T![":"])) {
            return Ok(None);
        }

        let colon = self.stream.consume_span()?;
        let first = self.parse_type_hint()?;

        Ok(Some(self.parse_inheritance_from(colon, first)?))
    }

    /// A PHP# header whose `colon` and `first` type the parser already read, with each `,` and type after them.
    pub(crate) fn parse_inheritance_from(
        &mut self,
        colon: Span,
        first: Hint<'arena>,
    ) -> Result<Inheritance<'arena>, ParseError> {
        let mut types = self.new_vec();
        let mut commas = self.new_vec();
        types.push(first);
        while matches!(self.stream.peek_kind(0)?, Some(T![","])) {
            commas.push(self.stream.consume()?);
            types.push(self.parse_type_hint()?);
        }

        Ok(Inheritance { colon, types: TokenSeparatedSequence::new(types, commas) })
    }

    pub(crate) fn parse_optional_extends(&mut self) -> Result<Option<Extends<'arena>>, ParseError> {
        if !self.is_at_php_keyword(T!["extends"])? {
            return Ok(None);
        }

        Ok(Some(Extends {
            extends: self.expect_any_keyword()?,
            types: {
                let mut types = self.new_vec();
                let mut commas = self.new_vec();
                loop {
                    types.push(self.parse_identifier()?);

                    match self.stream.peek_kind(0)? {
                        Some(T![","]) => {
                            commas.push(self.stream.consume()?);
                        }
                        _ => break,
                    }
                }
                TokenSeparatedSequence::new(types, commas)
            },
        }))
    }
}
