use mago_allocator::prelude::*;
use mago_database::file::HasFileId;

use crate::T;
use crate::cst::cst::Keyword;
use crate::error::ParseError;
use crate::lexer::lookup_keyword;
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

    /// Reads the name at the head of the stream, or after `static` there, as the PHP keyword it spells where PHP's
    /// construct follows it: an operand, as in `echo total;` or `function helper()`, or parentheses followed by `{`,
    /// `=>`, `:`, `=` or a name, as in `foreach (items as item) {` or `list(first) = pair;`. A `.sharp` file reads a PHP keyword that PHP# doesn't
    /// use as a plain name, spec section 2, so PHP's construct parses only where a statement, a member, an expression
    /// or a part of a PHP construct starts, and its own error names the PHP# form.
    pub(crate) fn read_php_construct(&mut self) -> Result<(), ParseError> {
        let n = usize::from(self.stream.is_at(T!["static"])?);
        let Some(token) = self.stream.lookahead(n)? else {
            return Ok(());
        };
        let Some(keyword) =
            lookup_keyword(token.value).filter(|_| self.dialect.is_sharp() && token.kind == T![Identifier])
        else {
            return Ok(());
        };
        let construct_follows = keyword.is_magic_constant()
            || match self.stream.peek_kind(n + 1)? {
                Some(T!["("]) => {
                    matches!(self.stream.peek_kind_after_parentheses()?, Some(T!["{" | "=>" | ":" | "=" | Identifier]))
                }
                Some(kind) => {
                    Self::is_at_start_of_expression(kind) && !matches!(kind, T!["[" | "#[" | "-" | "+" | "++" | "--"])
                }
                None => false,
            };
        if construct_follows {
            self.stream.read_as(n, keyword);
        }

        Ok(())
    }

    /// Whether the head of the stream is PHP's keyword `kind` where a PHP construct expects it, as `endif` ends
    /// `if (…): … endif;`. A `.sharp` file lexes a PHP keyword that PHP# doesn't use as a name, which reads as the
    /// keyword from now on.
    pub(crate) fn is_at_php_keyword(&mut self, kind: TokenKind) -> Result<bool, ParseError> {
        let Some(token) = self.stream.lookahead(0)? else {
            return Ok(false);
        };
        if self.dialect.is_sharp() && token.kind == T![Identifier] && lookup_keyword(token.value) == Some(kind) {
            self.stream.read_as(0, kind);

            return Ok(true);
        }

        Ok(token.kind == kind)
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
