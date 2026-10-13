use mago_allocator::prelude::*;
use mago_database::file::HasFileId;

use crate::T;
use crate::cst::cst::Keyword;
use crate::error::ParseError;
use crate::lexer::lookup_keyword;
use crate::parser::Parser;
use crate::token::SHARP_KEYWORDS;
use crate::token::Token;
use crate::token::TokenKind;
use crate::token::is_sharp_keyword;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Expects and consumes a keyword token. Only the keyword can stand here, so a `.sharp` file's name that spells it
    /// in another case reads as it, as [`Self::read_miscased_keyword`] reads it.
    #[inline]
    pub(crate) fn expect_keyword(&mut self, kind: TokenKind) -> Result<Keyword<'arena>, ParseError> {
        if let Some(token) = self.stream.lookahead(0)?
            && let Some(word) = self.miscased_keyword(token)
            && lookup_keyword(word.as_bytes()) == Some(kind)
        {
            self.read_miscased_keyword(token, word);
        }
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
    /// construct starts, and its own error names the PHP# form. A modifier or a type's keyword written in another case
    /// reads as it there, as `Static` in `public Static int run()` does.
    pub(crate) fn read_php_construct(&mut self) -> Result<(), ParseError> {
        self.read_php_keyword(Head::Declaration)
    }

    /// Reads the name at the head of an expression as [`Self::read_php_construct`] does, and also a keyword written
    /// in another case, as `New` in `New Cart()`.
    pub(crate) fn read_php_expression(&mut self) -> Result<(), ParseError> {
        self.read_php_keyword(Head::Expression)
    }

    /// Reads the name at the head of a statement as [`Self::read_php_expression`] does, and also as the statement PHP
    /// writes with a colon body or a braceless body, as in `foreach (items as item):`, and as `declare(…)`. The token
    /// after an `if` statement's body is a statement's head too, where `elseif` or `else` may follow, and so is the
    /// token after a `try` statement's block or a `catch` clause, where `catch` or `finally` may follow.
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
    /// A PHP# keyword written in another case reads as the keyword where its construct follows and the name could not
    /// stand, as [`Self::miscased_construct_follows`] decides.
    fn read_php_keyword(&mut self, head: Head) -> Result<(), ParseError> {
        let n = usize::from(self.stream.is_at(T!["static"])?);
        let Some(token) = self.stream.lookahead(n)? else {
            return Ok(());
        };
        if let Some(keyword) = self.php_keyword(token) {
            if self.construct_follows(keyword, n, head == Head::Statement)? {
                self.stream.read_as(n, keyword);
            }
        } else if n == 0 {
            self.read_keyword_in_another_case(token, head)?;
        }

        Ok(())
    }

    /// Reads `token`, a name at the head of the stream that spells a keyword in another case, as that keyword where
    /// its construct follows. A PHP# keyword reads with one error, as [`Self::miscased_construct_follows`] decides. A
    /// PHP keyword PHP# doesn't use reads as its construct with no other error, as `FUNCTION f() { }` does, so the
    /// construct's own error names the PHP# form.
    fn read_keyword_in_another_case(&mut self, token: Token<'_>, head: Head) -> Result<(), ParseError> {
        if let Some(word) = self.miscased_keyword(token) {
            if self.miscased_construct_follows(word, head)? {
                self.read_miscased_keyword(token, word);
            }
        } else if head != Head::Declaration
            && token.kind == T![Identifier]
            && let Some(keyword) = lookup_keyword(token.value).filter(|_| self.dialect.is_sharp())
            && self.construct_follows(keyword, 0, head == Head::Statement)?
            && !self.is_at_local_declaration()?
        {
            self.stream.read_as(0, keyword);
        }

        Ok(())
    }

    /// Whether the construct `keyword` starts follows the name at `n`, as [`Self::read_php_keyword`] describes.
    fn construct_follows(&mut self, keyword: TokenKind, n: usize, at_statement: bool) -> Result<bool, ParseError> {
        let starts_lambda = matches!(keyword, T!["fn" | "function"]);
        let starts_body = at_statement && matches!(keyword, T!["foreach" | "switch" | "elseif"]);

        Ok(keyword.is_magic_constant()
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
            })
    }

    /// Whether the construct of the PHP# keyword `word`, written in another case at the head of the stream, follows
    /// it where the name could not stand. At a statement's or an expression's head, that is PHP's rule above, a block
    /// after `try`, `do`, `else` and `finally`, `if` after `else`, and `;` after `break`, `continue` and `return`,
    /// unless the name starts a local whose type it names, as in `Return refund = new Return();`. At a declaration's
    /// head, it is a modifier followed by another modifier or by a type and a name, as `Static` in
    /// `public Static int run()`, and a type's keyword followed by its name, as `Class` in `public Class Cart {`. So
    /// `public Static run()` keeps `Static` a return type.
    fn miscased_construct_follows(&mut self, word: &'static str, head: Head) -> Result<bool, ParseError> {
        let next = self.stream.lookahead(1)?;
        let next_kind = next.map(|next| next.kind);
        let after_name = match next_kind {
            Some(kind) if kind.is_identifier_maybe_reserved() => self.stream.peek_kind(2)?,
            _ => None,
        };

        if head == Head::Declaration {
            return Ok(match word {
                "class" | "interface" | "enum" | "struct" => matches!(after_name, Some(T!["{" | ":" | "<"])),
                _ if is_modifier_word(word) => {
                    next.is_some_and(|next| {
                        next.kind.is_modifier() || self.miscased_keyword(next).is_some_and(is_modifier_word)
                    }) || after_name.is_some_and(|kind| !matches!(kind, T!["(" | ")" | ";" | "=" | "{" | "=>" | ","]))
                }
                _ => false,
            });
        }

        let follows = match word {
            "try" | "do" | "finally" => next_kind == Some(T!["{"]),
            "else" => next_kind == Some(T!["{"]) || next.is_some_and(|next| next.value.eq_ignore_ascii_case(b"if")),
            "break" | "continue" | "return" if next_kind == Some(T![";"]) => true,
            _ => {
                let keyword = lookup_keyword(word.as_bytes()).unwrap_or(T![Identifier]);

                self.construct_follows(keyword, 0, head == Head::Statement)?
            }
        };

        Ok(follows && !self.is_at_local_declaration()?)
    }

    /// The PHP# keyword a name in a `.sharp` file spells in another case, as `return` for `Return`, or `None` for a name
    /// in its own case. `true`, `false`, `null`, `this`, `super` and `Self` are values, which the semantic checks
    /// report.
    fn miscased_keyword(&self, token: Token<'_>) -> Option<&'static str> {
        if !self.dialect.is_sharp() || token.kind != T![Identifier] || is_sharp_keyword(token.value) {
            return None;
        }

        SHARP_KEYWORDS
            .into_iter()
            .find(|keyword| keyword.as_bytes().eq_ignore_ascii_case(token.value))
            .filter(|keyword| !matches!(*keyword, "true" | "false" | "null" | "this" | "super" | "Self"))
    }

    /// Reads `token`, at the head of the stream, as the PHP# keyword `word` it spells in another case, with one error,
    /// as rustc's `eat_keyword_case` reads `Fn` as `fn`.
    fn read_miscased_keyword(&mut self, token: Token<'_>, word: &'static str) {
        self.stream.read_as_keyword(0, lookup_keyword(word.as_bytes()).unwrap_or(T![Identifier]), word);
        self.errors.push(ParseError::MiscasedKeywordInSharp(word, token.span_for(self.stream.file_id())));
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

/// Whether `word`, one of the [`SHARP_KEYWORDS`], is a modifier of a declaration or a member.
fn is_modifier_word(word: &str) -> bool {
    matches!(
        word,
        "static"
            | "public"
            | "protected"
            | "private"
            | "internal"
            | "abstract"
            | "final"
            | "sealed"
            | "override"
            | "virtual"
            | "required"
            | "readonly"
            | "lazy"
            | "extern"
    )
}

/// Whether `token` is a PHP# infix word, which follows an operand and never starts one.
fn is_infix_word(token: Token<'_>) -> bool {
    token.kind == T![Identifier] && matches!(token.value, b"is" | b"with" | b"when" | b"and" | b"or" | b"not")
}
