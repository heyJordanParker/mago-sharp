use mago_allocator::prelude::*;
use mago_database::file::HasFileId;

use crate::cst::cst::Keyword;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::TokenKind;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Expects and consumes a keyword token.
    #[inline]
    pub(crate) fn expect_keyword(&mut self, kind: TokenKind) -> Result<Keyword<'arena>, ParseError> {
        let token = self.stream.eat(kind)?;
        Ok(Keyword { span: token.span_for(self.stream.file_id()), value: token.value })
    }

    /// Expects and consumes a PHP keyword that PHP# does not have, such as `use`, and reports it in a PHP# file.
    ///
    /// The error stands, and the PHP construct still parses so the rest of the file does.
    pub(crate) fn expect_php_keyword(&mut self, kind: TokenKind) -> Result<Keyword<'arena>, ParseError> {
        let keyword = self.expect_keyword(kind)?;
        if self.dialect.is_sharp() {
            self.errors.push(ParseError::PhpSyntaxInSharp(kind, keyword.span));
        }

        Ok(keyword)
    }

    /// Expects and consumes the `fn` or `function` that starts a PHP lambda, and reports it in a PHP# file, which
    /// writes a lambda as a bare arrow. The error stands, and the PHP lambda still parses.
    pub(crate) fn expect_php_lambda_keyword(&mut self, kind: TokenKind) -> Result<Keyword<'arena>, ParseError> {
        let keyword = self.expect_keyword(kind)?;
        if self.dialect.is_sharp() {
            self.errors.push(ParseError::PhpLambdaInSharp(keyword.span));
        }

        Ok(keyword)
    }

    /// Optionally consumes a keyword token if present.
    #[inline]
    pub(crate) fn maybe_expect_keyword(&mut self, kind: TokenKind) -> Result<Option<Keyword<'arena>>, ParseError> {
        if self.stream.is_at(kind)? { Ok(Some(self.expect_keyword(kind)?)) } else { Ok(None) }
    }

    /// Consumes any token and returns it as a keyword.
    #[inline]
    pub(crate) fn expect_any_keyword(&mut self) -> Result<Keyword<'arena>, ParseError> {
        let token = self.stream.consume()?;
        Ok(Keyword { span: token.span_for(self.stream.file_id()), value: token.value })
    }

    /// Expects and consumes one of the given token kinds.
    #[inline]
    pub(crate) fn expect_one_of_keyword(&mut self, kinds: &'static [TokenKind]) -> Result<Keyword<'arena>, ParseError> {
        let token = self.stream.consume()?;
        if kinds.contains(&token.kind) {
            Ok(Keyword { span: token.span_for(self.stream.file_id()), value: token.value })
        } else {
            Err(self.stream.unexpected(Some(token), kinds))
        }
    }
}
