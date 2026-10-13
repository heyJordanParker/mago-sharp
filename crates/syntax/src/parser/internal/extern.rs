use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::DottedIdentifier;
use crate::cst::cst::Extern;
use crate::cst::cst::Identifier;
use crate::cst::cst::Uses;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Parses a PHP# `extern` declaration: `extern` and its target, a bare name or `Class.member`, then an optional
    /// `uses` clause.
    pub(crate) fn parse_extern(&mut self) -> Result<Extern<'arena>, ParseError> {
        Ok(Extern {
            r#extern: self.expect_any_keyword()?,
            target: self.parse_extern_target()?,
            uses: self.parse_optional_uses()?,
            terminator: self.parse_terminator()?,
        })
    }

    /// A `.` joins the class and its member only when it touches both, as in a dotted name.
    fn parse_extern_target(&mut self) -> Result<Identifier<'arena>, ParseError> {
        let class = self.parse_local_identifier()?;

        let (Some(dot), Some(member)) = (self.stream.lookahead(0)?, self.stream.lookahead(1)?) else {
            return Ok(Identifier::Local(class));
        };
        if dot.kind != T!["."]
            || !member.kind.is_identifier_maybe_reserved()
            || dot.start != class.span.end
            || member.start.offset != dot.start.offset + 1
        {
            return Ok(Identifier::Local(class));
        }

        self.stream.consume()?;
        let member = self.parse_local_identifier()?;
        let value = [class.value, b".", member.value].concat();

        Ok(Identifier::Dotted(DottedIdentifier { span: class.span.join(member.span), value: self.bytes(&value) }))
    }

    /// Parses a `uses` clause when one starts here: `uses`, which stays a name everywhere else, and its effects.
    pub(crate) fn parse_optional_uses(&mut self) -> Result<Option<Uses<'arena>>, ParseError> {
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
