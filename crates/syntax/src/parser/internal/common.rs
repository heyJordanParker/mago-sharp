use mago_allocator::prelude::*;
use mago_database::file::HasFileId;

use crate::T;
use crate::cst::cst::Keyword;
use crate::error::ParseError;
use crate::lexer::lookup_keyword;
use crate::parser::Parser;
use crate::token::Token;
use crate::token::TokenKind;
use crate::token::is_sharp_keyword;

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
    /// construct follows it, as [`Self::read_php_keyword`] decides. A `.sharp` file reads a PHP keyword that PHP#
    /// doesn't use as a plain name, spec section 2, so PHP's construct parses only where a member or a part of a PHP
    /// construct starts, and its own error names the PHP# form.
    pub(crate) fn read_php_construct(&mut self) -> Result<(), ParseError> {
        self.read_php_keyword(Head::Declaration)
    }

    /// Reads the name at the head of an expression as [`Self::read_php_construct`] does, and also a keyword written
    /// in another case, as `New` in `New Cart()`, as [`Self::miscased_keyword`] decides.
    pub(crate) fn read_php_expression(&mut self) -> Result<(), ParseError> {
        self.read_php_keyword(Head::Expression)
    }

    /// Reads the name at the head of a statement as [`Self::read_php_expression`] does, and also as the statement PHP
    /// writes with a colon body or a braceless body, as in `foreach (items as item):`, and as `declare(…)`. The token
    /// after an `if` statement's body is a statement's head too, where `elseif` may follow.
    pub(crate) fn read_php_statement(&mut self) -> Result<(), ParseError> {
        self.read_php_keyword(Head::Statement)
    }

    /// Reads the PHP keyword a name spells where its construct follows it: an operand, as in `echo total;` or
    /// `function helper()`, or parentheses followed by `{`, `=>` or `=`, as in `foreach (items as item) {` or
    /// `list(first) = pair;`. Parentheses followed by `:` or a name are a lambda's return type or `use` only after
    /// `fn` and `function`, and a colon or braceless body, which may start with a keyword, only after `foreach`,
    /// `switch` and `elseif` at a statement's start, where `declare` always starts PHP's construct. Anywhere else
    /// they are a call, as in `flag ? print(1) : 0`. A PHP# infix word never starts an operand, so `empty is bool`
    /// reads `empty` as a name.
    ///
    /// A PHP# keyword written in another case reads as the keyword with one error that writes its line in lowercase,
    /// as rustc reads `Fn`. A PHP keyword PHP# doesn't use reads as its construct, whose own error names the PHP# form.
    fn read_php_keyword(&mut self, head: Head) -> Result<(), ParseError> {
        let at_statement = head == Head::Statement;
        let n = usize::from(self.stream.is_at(T!["static"])?);
        let Some(token) = self.stream.lookahead(n)? else {
            return Ok(());
        };
        let (keyword, miscased) = match self.php_keyword(token) {
            Some(keyword) => (keyword, false),
            None if head != Head::Declaration && n == 0 => match self.miscased_keyword(token)? {
                Some(keyword) => (keyword, true),
                None => return Ok(()),
            },
            None => return Ok(()),
        };
        let starts_lambda = matches!(keyword, T!["fn" | "function"]);
        let starts_body = at_statement && matches!(keyword, T!["foreach" | "switch" | "elseif"]);
        let construct_follows = keyword.is_magic_constant()
            || match self.stream.lookahead(n + 1)? {
                Some(next) if next.kind == T!["("] => match self.stream.peek_after_parentheses()? {
                    _ if at_statement && keyword == T!["declare"] => true,
                    Some(after) if is_infix_word(after) => false,
                    Some(after) => match after.kind {
                        T!["{" | "=>" | "="] => true,
                        T![":" | Identifier] => starts_lambda || starts_body,
                        kind => starts_body && kind.is_identifier_maybe_reserved(),
                    },
                    None => false,
                },
                Some(next) => {
                    !is_infix_word(next)
                        && Self::is_at_start_of_expression(next.kind)
                        && !matches!(next.kind, T!["[" | "#[" | "-" | "+" | "++" | "--"])
                }
                None => false,
            };
        if construct_follows {
            self.stream.read_as(n, keyword);
            let lowercase = token.value.to_ascii_lowercase();
            if miscased && is_sharp_keyword(&lowercase) {
                let (line_start, line) = self.stream.line_at(token.start.offset);
                let start = (token.start.offset - line_start) as usize;
                let mut fixed = line.to_vec();
                fixed[start..start + token.value.len()].make_ascii_lowercase();

                self.errors.push(ParseError::MiscasedKeywordInSharp(
                    String::from_utf8_lossy(&lowercase).into(),
                    String::from_utf8_lossy(&fixed).trim().into(),
                    token.span_for(self.stream.file_id()),
                ));
            }
        }

        Ok(())
    }

    /// The keyword a name in a `.sharp` file spells in another case, as `Return` or `IF`, unless it starts a local
    /// whose type it names, as in `Return refund = new Return();`.
    fn miscased_keyword(&mut self, token: Token<'_>) -> Result<Option<TokenKind>, ParseError> {
        let Some(keyword) = lookup_keyword(token.value)
            .filter(|keyword| self.dialect.is_sharp() && token.kind == T![Identifier] && !keyword.is_magic_constant())
        else {
            return Ok(None);
        };

        Ok((!self.is_at_local_declaration()?).then_some(keyword))
    }

    /// Whether the head of the stream is PHP's keyword `kind` where a PHP construct expects it, as `endif` ends
    /// `if (…): … endif;`. A `.sharp` file lexes a PHP keyword that PHP# doesn't use as a name, which reads as the
    /// keyword from now on.
    pub(crate) fn is_at_php_keyword(&mut self, kind: TokenKind) -> Result<bool, ParseError> {
        let Some(token) = self.stream.lookahead(0)? else {
            return Ok(false);
        };
        if self.php_keyword(token) == Some(kind) {
            self.stream.read_as(0, kind);

            return Ok(true);
        }

        Ok(token.kind == kind)
    }

    /// The PHP keyword a name in a `.sharp` file spells, as PHP code writes it: lowercase, or a magic constant such as
    /// `__CLASS__`. PHP#'s names are case-sensitive, so `Print` names a class and never reads as `print`.
    pub(crate) fn php_keyword(&self, token: Token<'_>) -> Option<TokenKind> {
        lookup_keyword(token.value).filter(|keyword| {
            self.dialect.is_sharp()
                && token.kind == T![Identifier]
                && (keyword.is_magic_constant() || !token.value.iter().any(u8::is_ascii_uppercase))
        })
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

/// Where the parser reads a name that may spell a PHP keyword: in a declaration, as a member's or an import's head,
/// at an expression's head, or at a statement's head.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Head {
    Declaration,
    Expression,
    Statement,
}

/// Whether `token` is a PHP# infix word, which follows an operand and never starts one.
fn is_infix_word(token: Token<'_>) -> bool {
    token.kind == T![Identifier] && matches!(token.value, b"is" | b"with" | b"when" | b"and" | b"or" | b"not")
}
