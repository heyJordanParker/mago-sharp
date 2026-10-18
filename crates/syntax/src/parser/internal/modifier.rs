use crate::T;
use crate::cst::cst::Modifier;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::Token;
use mago_allocator::prelude::*;
use mago_database::file::HasFileId;
use mago_span::Span;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether `token` is spec section 5's `internal`, which starts a declaration's or a member's modifiers, as `public`
    /// does.
    pub(crate) fn is_internal(&self, token: Token<'_>) -> bool {
        self.dialect.is_sharp() && token.kind == T![Identifier] && token.value == b"internal"
    }

    /// Whether `token` is spec section 29's `foreign`, which starts a class's modifiers, as `public` does.
    pub(crate) fn is_foreign(&self, token: Token<'_>) -> bool {
        self.dialect.is_sharp() && token.kind == T![Identifier] && token.value == b"foreign"
    }

    /// Whether C#'s `partial` starts here, before a type's keyword or its other modifiers, as in `partial class Order`
    /// or `partial static class Texts`. PHP# has no partial types, so there it fails as a typo does, and everywhere
    /// else `partial` is a name.
    pub(crate) fn is_at_partial(&mut self) -> Result<bool, ParseError> {
        if !self.dialect.is_sharp()
            || !self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"partial")
        {
            return Ok(false);
        }

        Ok(self.stream.lookahead(1)?.is_some_and(|next| {
            matches!(next.kind, T!["class" | "interface" | "enum"])
                || next.kind.is_modifier()
                || (next.kind == T![Identifier] && matches!(next.value, b"struct" | b"sealed" | b"event"))
                || self.is_internal(next)
                || self.is_foreign(next)
        }))
    }

    /// Whether the token `offset` ahead is spec section 5's `internal`.
    fn is_internal_token_at(&mut self, offset: usize) -> Result<bool, ParseError> {
        Ok(self.stream.lookahead(offset)?.is_some_and(|token| self.is_internal(token)))
    }

    /// Consumes a `(` and every token up to its matching `)`, and returns their span.
    fn skip_parenthesized(&mut self) -> Result<Span, ParseError> {
        let start = self.stream.eat_span(T!["("])?;
        let mut depth: usize = 1;
        loop {
            let token = self.stream.consume()?;
            match token.kind {
                T!["("] => depth += 1,
                T![")"] if depth == 1 => return Ok(start.join(token.span_for(self.stream.file_id()))),
                T![")"] => depth -= 1,
                _ => {}
            }
        }
    }

    pub(crate) fn parse_modifier_sequence(&mut self) -> Result<Sequence<'arena, Modifier<'arena>>, ParseError> {
        let mut modifiers = self.new_vec();
        while let Some(modifier) = self.parse_optional_modifier()? {
            modifiers.push(modifier);
        }

        Ok(Sequence::new(modifiers))
    }

    pub(crate) fn parse_optional_modifier(&mut self) -> Result<Option<Modifier<'arena>>, ParseError> {
        self.read_php_construct()?;
        Ok(Some(match self.stream.peek_kind(0)? {
            Some(T!["public"]) => Modifier::Public(self.expect_any_keyword()?),
            Some(T!["protected"]) => Modifier::Protected(self.expect_any_keyword()?),
            Some(T!["private"]) => Modifier::Private(self.expect_any_keyword()?),
            Some(T!["static"]) => Modifier::Static(self.expect_any_keyword()?),
            Some(T!["final"]) => Modifier::Final(self.expect_any_keyword()?),
            Some(T!["abstract"]) => Modifier::Abstract(self.expect_any_keyword()?),
            Some(T!["readonly"]) if !self.is_at_readonly_hint()? => Modifier::Readonly(self.expect_any_keyword()?),
            Some(T!["private(set)"]) => Modifier::PrivateSet(self.expect_any_keyword()?),
            Some(T!["protected(set)"]) => Modifier::ProtectedSet(self.expect_any_keyword()?),
            Some(T!["public(set)"]) => Modifier::PublicSet(self.expect_any_keyword()?),
            // Spec section 5 has no named reach, so `internal(App.Tenant)` fails as a typo does at its parentheses, and
            // the declaration parses on as `internal`.
            Some(T![Identifier]) if self.is_internal_token_at(0)? && self.stream.peek_kind(1)? == Some(T!["("]) => {
                let internal = self.expect_any_keyword()?;
                let reach = self.skip_parenthesized()?;
                self.report_unexpected_in_sharp(reach);

                Modifier::Internal(internal)
            }
            // A class lives in one file, so C#'s `partial` fails as a typo does, and the declaration parses on without
            // it.
            Some(T![Identifier]) if self.is_at_partial()? => {
                let partial = self.stream.consume()?.span_for(self.stream.file_id());
                self.report_unexpected_in_sharp(partial);

                return self.parse_optional_modifier();
            }
            // A word before `(`, `)`, `,` or `=` is a name, as a call or an untyped lambda parameter, which
            // `parse_parameter` reads the same way.
            Some(T![Identifier])
                if self.dialect.is_sharp() && !matches!(self.stream.peek_kind(1)?, Some(T!["(" | ")" | "," | "="])) =>
            {
                match self.stream.lookahead(0)?.map(|token| token.value) {
                    Some(b"virtual") => Modifier::Virtual(self.expect_any_keyword()?),
                    Some(b"override") => Modifier::Override(self.expect_any_keyword()?),
                    Some(b"required") => Modifier::Required(self.expect_any_keyword()?),
                    Some(b"extern") => Modifier::Extern(self.expect_any_keyword()?),
                    Some(b"lazy") => Modifier::Lazy(self.expect_any_keyword()?),
                    Some(b"internal") => Modifier::Internal(self.expect_any_keyword()?),
                    Some(b"foreign") => Modifier::Foreign(self.expect_any_keyword()?),
                    _ => return Ok(None),
                }
            }
            _ => return Ok(None),
        }))
    }
}
