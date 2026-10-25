use mago_allocator::prelude::*;
use mago_database::file::HasFileId;
use mago_span::HasSpan;

use crate::T;
use crate::cst::cst::DottedIdentifier;
use crate::cst::cst::FullyQualifiedIdentifier;
use crate::cst::cst::Identifier;
use crate::cst::cst::LocalIdentifier;
use crate::cst::cst::QualifiedIdentifier;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_identifier(&mut self) -> Result<Identifier<'arena>, ParseError<'arena>> {
        let identifier = self.parse_php_identifier()?;

        // PHP# writes a full name only in an `import` line. The error stands, and the name still parses so the rest
        // of the file does.
        if self.dialect.is_sharp() && !identifier.is_local() {
            self.report_unexpected_in_sharp(identifier.span());
        }

        Ok(identifier)
    }

    /// Parses a local, qualified or fully qualified name as PHP writes it. A `use` line, an import or a trait use,
    /// reads its names with this, because PHP# reports the whole line once, at its `use` keyword.
    pub(crate) fn parse_php_identifier(&mut self) -> Result<Identifier<'arena>, ParseError<'arena>> {
        let token = self.stream.lookahead(0)?.ok_or_else(|| self.stream.unexpected(None, &[]))?;

        Ok(match &token.kind {
            T![QualifiedIdentifier] => Identifier::Qualified(self.parse_qualified_identifier()?),
            T![FullyQualifiedIdentifier] => Identifier::FullyQualified(self.parse_fully_qualified_identifier()?),
            _ => Identifier::Local(self.parse_local_identifier()?),
        })
    }

    /// Parses a PHP# name for a `namespace` or `import` line, such as `App.Tenant.Store`.
    ///
    /// A `.` joins two parts only when it touches both of them, so the name's value is its source text.
    pub(crate) fn parse_dotted_identifier(&mut self) -> Result<Identifier<'arena>, ParseError<'arena>> {
        let first = self.parse_local_identifier()?;
        let mut value = std::vec::Vec::from(first.value);
        let mut span = first.span;

        while let (Some(dot), Some(next)) = (self.stream.lookahead(0)?, self.stream.lookahead(1)?)
            && dot.kind == T!["."]
            && next.kind.is_identifier_maybe_reserved()
            && dot.start == span.end
            && next.start.offset == dot.start.offset + 1
        {
            self.stream.consume()?;
            let part = self.parse_local_identifier()?;
            value.push(b'.');
            value.extend_from_slice(part.value);
            span = span.join(part.span);
        }

        if span == first.span {
            return Ok(Identifier::Local(first));
        }

        Ok(Identifier::Dotted(DottedIdentifier { span, value: self.bytes(&value) }))
    }

    /// Parses a name, a keyword included. A name written with `@`, spec section 2, spans the `@`, and its value is the
    /// word after it, so `@event` is the name `event`. `@this` keeps its `@`, because a bare `this` is the object the
    /// method runs on, so the name `@this` differs from it wherever a name is read.
    pub(crate) fn parse_local_identifier(&mut self) -> Result<LocalIdentifier<'arena>, ParseError<'arena>> {
        let token = self.stream.consume()?;

        if !token.kind.is_identifier_maybe_reserved() {
            return Err(self.stream.unexpected(Some(token), &[T![Identifier]]));
        }

        let value = match token.value {
            [b'@', word @ ..] if word != b"this" => word,
            value => value,
        };

        Ok(LocalIdentifier { span: token.span_for(self.stream.file_id()), value })
    }

    pub(crate) fn parse_qualified_identifier(&mut self) -> Result<QualifiedIdentifier<'arena>, ParseError<'arena>> {
        let token = self.stream.eat(T![QualifiedIdentifier])?;

        Ok(QualifiedIdentifier { span: token.span_for(self.stream.file_id()), value: token.value })
    }

    pub(crate) fn parse_fully_qualified_identifier(
        &mut self,
    ) -> Result<FullyQualifiedIdentifier<'arena>, ParseError<'arena>> {
        let token = self.stream.eat(T![FullyQualifiedIdentifier])?;

        Ok(FullyQualifiedIdentifier { span: token.span_for(self.stream.file_id()), value: token.value })
    }
}
