use crate::T;
use crate::cst::cst::Echo;
use crate::cst::cst::EchoTag;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Parses a PHP echo tag. A `.sharp` file has none, spec section 27, so there the tag is an error, and the file
    /// parses on.
    pub(crate) fn parse_echo_tag(&mut self) -> Result<EchoTag<'arena>, ParseError> {
        let tag = self.stream.eat_span(T!["<?="])?;
        if self.dialect.is_sharp() {
            self.report_unexpected_in_sharp(tag);
        }

        Ok(EchoTag {
            tag,
            values: {
                let mut values = self.new_vec();
                let mut commas = self.new_vec();

                loop {
                    if matches!(self.stream.peek_kind(0)?, Some(T!["?>" | ";"])) {
                        break;
                    }

                    values.push(self.parse_expression()?);

                    if let Some(T![","]) = self.stream.peek_kind(0)? {
                        commas.push(self.stream.consume()?);
                    } else {
                        break;
                    }
                }

                TokenSeparatedSequence::new(values, commas)
            },
            terminator: self.parse_terminator()?,
        })
    }

    pub(crate) fn parse_echo(&mut self) -> Result<Echo<'arena>, ParseError> {
        Ok(Echo {
            echo: self.expect_keyword(T!["echo"])?,
            values: {
                let mut values = self.new_vec();
                let mut commas = self.new_vec();

                loop {
                    if matches!(self.stream.peek_kind(0)?, Some(T!["?>" | ";"])) {
                        break;
                    }

                    values.push(self.parse_expression()?);

                    if let Some(T![","]) = self.stream.peek_kind(0)? {
                        commas.push(self.stream.consume()?);
                    } else {
                        break;
                    }
                }

                TokenSeparatedSequence::new(values, commas)
            },
            terminator: self.parse_terminator()?,
        })
    }
}
