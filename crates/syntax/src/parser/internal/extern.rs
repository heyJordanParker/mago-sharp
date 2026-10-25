use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::Extern;
use crate::cst::cst::Uses;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Parses a PHP# `extern` declaration: `extern`, `event` when the class is an event, its target, a bare name or
    /// `Class.member`, then an optional `uses` clause.
    pub(crate) fn parse_extern(&mut self) -> Result<Extern<'arena>, ParseError<'arena>> {
        let r#extern = self.expect_any_keyword()?;
        let event = if self.stream.lookahead(0)?.is_some_and(|token| token.value == b"event")
            && self.stream.peek_kind(1)? == Some(T![Identifier])
        {
            Some(self.expect_any_keyword()?)
        } else {
            None
        };

        Ok(Extern {
            r#extern,
            event,
            // The target is a bare name or `Class.member`. A longer one is a full name, which the analyzer refuses with
            // the import to add, spec section 23.
            target: self.parse_dotted_identifier()?,
            uses: self.parse_optional_uses()?,
            terminator: self.parse_terminator()?,
        })
    }

    /// Parses a `uses` clause when one starts here: `uses`, which stays a name everywhere else, and its effects.
    pub(crate) fn parse_optional_uses(&mut self) -> Result<Option<Uses<'arena>>, ParseError<'arena>> {
        if !self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"uses") {
            return Ok(None);
        }

        let uses = self.expect_any_keyword()?;
        let mut names = self.new_vec();
        let mut commas = self.new_vec();
        loop {
            names.push(self.parse_local_identifier()?);

            if let Some(T![","]) = self.stream.peek_kind(0)? {
                commas.push(self.stream.consume()?);
            } else {
                break;
            }
        }

        Ok(Some(Uses { uses, names: TokenSeparatedSequence::new(names, commas) }))
    }
}
