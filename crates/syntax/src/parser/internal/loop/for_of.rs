use mago_allocator::prelude::*;
use mago_database::file::HasFileId;
use mago_span::Span;

use crate::T;
use crate::cst::cst::ForOf;
use crate::cst::cst::ForOfKeyValueTarget;
use crate::cst::cst::ForOfTarget;
use crate::cst::cst::ForOfVariable;
use crate::cst::cst::Keyword;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether the `for` header after `(` declares its variables with `let` or `const`, as a PHP# `for … of` loop and
    /// a PHP# counter both do.
    pub(crate) fn is_at_loop_variable_keyword(&mut self) -> Result<bool, ParseError> {
        if !self.dialect.is_sharp() {
            return Ok(false);
        }

        Ok(match self.stream.lookahead(0)? {
            Some(token) if token.kind == T!["const"] => true,
            Some(token) if token.kind == T![Identifier] && token.value == b"let" => {
                self.stream.peek_kind(1)?.is_some_and(|kind| kind == T!["["] || kind.is_identifier_maybe_reserved())
            }
            _ => false,
        })
    }

    /// Whether `of`, or TypeScript's `in`, comes next.
    pub(crate) fn is_at_of_keyword(&mut self) -> Result<bool, ParseError> {
        Ok(self
            .stream
            .lookahead(0)?
            .is_some_and(|token| token.kind == T![Identifier] && matches!(token.value, b"of" | b"in")))
    }

    /// Parses the rest of a PHP# `for … of` loop after its loop variables.
    pub(crate) fn parse_for_of(
        &mut self,
        r#for: Keyword<'arena>,
        left_parenthesis: Span,
        keyword: Keyword<'arena>,
        target: ForOfTarget<'arena>,
    ) -> Result<ForOf<'arena>, ParseError> {
        Ok(ForOf {
            r#for,
            left_parenthesis,
            keyword,
            target,
            of: self.parse_of_keyword()?,
            expression: self.parse_expression()?,
            right_parenthesis: self.stream.eat_span(T![")"])?,
            body: self.arena.alloc(self.parse_statement()?),
        })
    }

    pub(crate) fn parse_for_of_key_value_target(
        &mut self,
        is_const: bool,
    ) -> Result<ForOfKeyValueTarget<'arena>, ParseError> {
        Ok(ForOfKeyValueTarget {
            left_bracket: self.stream.eat_span(T!["["])?,
            key: self.parse_for_of_variable(is_const)?,
            comma: self.stream.eat_span(T![","])?,
            value: self.parse_for_of_variable(is_const)?,
            right_bracket: self.stream.eat_span(T!["]"])?,
        })
    }

    /// Parses a variable that `let` or `const` declares in a `for` header, and its type when one is written before its
    /// name. A `let` variable has none, as a typed local has no `let`.
    ///
    /// A written type can be longer than the parser looks ahead, as `Map<string, List<int>>` is, so the header reads
    /// the variable first and only then decides between `for … of` and a counter by the `of` or `=` after it.
    pub(crate) fn parse_for_of_variable(&mut self, is_const: bool) -> Result<ForOfVariable<'arena>, ParseError> {
        let name_follows = match self.stream.lookahead(1)? {
            Some(token) => {
                matches!(token.kind, T![","] | T!["]"] | T!["="])
                    || (token.kind == T![Identifier] && matches!(token.value, b"of" | b"in"))
            }
            None => true,
        };
        let hint = if is_const && !name_follows { Some(&*self.arena.alloc(self.parse_type_hint()?)) } else { None };

        Ok(ForOfVariable { hint, name: self.parse_local_identifier()? })
    }

    /// Consumes `of`. TypeScript's `in` is reported with the `of` to write, and the loop still parses.
    fn parse_of_keyword(&mut self) -> Result<Keyword<'arena>, ParseError> {
        let token = self.stream.consume()?;
        let keyword = Keyword { span: token.span_for(self.stream.file_id()), value: token.value };
        if token.kind != T![Identifier] || !matches!(token.value, b"of" | b"in") {
            return Err(self.stream.unexpected(Some(token), &[]));
        }

        if token.value == b"in" {
            self.errors.push(ParseError::ForInInSharp(keyword.span));
        }

        Ok(keyword)
    }
}
