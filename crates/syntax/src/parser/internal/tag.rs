use mago_allocator::prelude::*;
use mago_database::file::HasFileId;

use crate::T;
use crate::cst::cst::ClosingTag;
use crate::cst::cst::FullOpeningTag;
use crate::cst::cst::OpeningTag;
use crate::cst::cst::ShortOpeningTag;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Parses a PHP opening tag. A `.sharp` file has none, spec section 27, so there the tag is an error, and the file
    /// parses on.
    pub(crate) fn parse_opening_tag(&mut self) -> Result<OpeningTag<'arena>, ParseError> {
        let token = self.stream.consume()?;

        Ok(match token.kind {
            T!["<?php"] => {
                let span = token.span_for(self.stream.file_id());
                if self.dialect.is_sharp() {
                    self.errors.push(ParseError::PhpSyntaxInSharp(T!["<?php"], span));
                }
                OpeningTag::Full(FullOpeningTag { span, value: token.value })
            }
            T!["<?"] => OpeningTag::Short(ShortOpeningTag { span: token.span_for(self.stream.file_id()) }),
            _ => return Err(self.stream.unexpected(Some(token), &[T!["<?php"], T!["<?="]])),
        })
    }

    pub(crate) fn parse_closing_tag(&mut self) -> Result<ClosingTag, ParseError> {
        let span = self.stream.eat_span(T!["?>"])?;

        Ok(ClosingTag { span })
    }
}
