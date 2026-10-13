use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::Hint;
use crate::cst::cst::Keyword;
use crate::cst::cst::LocalDeclaration;
use crate::cst::cst::LocalIdentifier;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::Token;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether a PHP# local declaration starts here: `const`, `let` followed by a name, or a type, as
    /// `is_at_typed_local` reads it.
    pub(crate) fn is_at_local_declaration(&mut self) -> Result<bool, ParseError> {
        if !self.dialect.is_sharp() {
            return Ok(false);
        }

        match self.stream.lookahead(0)? {
            Some(token) if token.kind == T!["const"] => Ok(true),
            Some(token)
                if token.kind == T![Identifier]
                    && token.value == b"let"
                    && self.stream.peek_kind(1)?.is_some_and(|kind| kind.is_identifier_maybe_reserved()) =>
            {
                Ok(true)
            }
            _ => self.is_at_typed_local(),
        }
    }

    /// Parses a PHP# local: `let` or `const`, then its type when it is written, its name, `=` and its value. A local
    /// with its type written has no `let`, as in `Money? total = null;`.
    pub(crate) fn parse_local_declaration(&mut self) -> Result<LocalDeclaration<'arena>, ParseError> {
        let keyword = match self.stream.lookahead(0)? {
            Some(token) if token.kind == T!["const"] || token.value == b"let" => Some(self.expect_any_keyword()?),
            _ => None,
        };
        let is_let = keyword.as_ref().is_some_and(|keyword| keyword.value == b"let");
        let hint =
            if !is_let && self.is_at_typed_local()? { Some(&*self.arena.alloc(self.parse_type_hint()?)) } else { None };
        let name = self.parse_local_identifier()?;

        self.parse_local_declaration_value(keyword, hint, name)
    }

    /// Parses what follows a PHP# local's name: `=`, its value and the terminator.
    pub(crate) fn parse_local_declaration_value(
        &mut self,
        keyword: Option<Keyword<'arena>>,
        hint: Option<&'arena Hint<'arena>>,
        name: LocalIdentifier<'arena>,
    ) -> Result<LocalDeclaration<'arena>, ParseError> {
        Ok(LocalDeclaration {
            keyword,
            hint,
            name,
            equals: self.stream.eat_span(T!["="])?,
            value: self.parse_expression()?,
            terminator: self.parse_terminator()?,
        })
    }

    /// Returns `true` when the next tokens read as a type: a name and an optional `?` written right after it, then
    /// another name and `=`, or a union type that a name and `=` follow, as in `int|string key = 1;` or
    /// `(int|string)? key = null;`, or `(` and a type with type arguments, which starts a nullable union as in
    /// `(List<int>|string)? items = null;`. A spaced `?` is the conditional operator, and a union without a name after
    /// it is the bitwise `|`, as in `flags | MASK;` or `(flags | 1);`.
    ///
    /// A capitalized name followed by `<` starts a type with type arguments, as in `List<Line> lines = [];`, because
    /// spec section 24 capitalizes every type but the built-in ones. Its arguments can be longer than the parser
    /// looks ahead.
    fn is_at_typed_local(&mut self) -> Result<bool, ParseError> {
        let Some(hint) = self.stream.lookahead(0)? else {
            return Ok(false);
        };
        if hint.kind == T!["("] {
            let Some(first) = self.stream.lookahead(1)? else {
                return Ok(false);
            };

            return Ok(match self.stream.peek_kind(2)? {
                Some(T!["|"]) => first.kind == T![Identifier] && self.is_at_union_typed_local()?,
                Some(T!["<"]) => {
                    (matches!(first.kind, T![Identifier | "list"])
                        && first.value.first().is_some_and(u8::is_ascii_uppercase))
                        || (first.kind == T!["function"] && first.value == b"Function")
                }
                _ => false,
            });
        }
        if self.is_at_generic_hint()? {
            return Ok(hint.value.first().is_some_and(u8::is_ascii_uppercase));
        }
        if self.is_at_function_hint()? {
            return Ok(true);
        }

        if hint.kind != T![Identifier] {
            return Ok(false);
        }

        let name = match self.stream.lookahead(1)? {
            Some(question_mark)
                if question_mark.kind == T!["?"]
                    && question_mark.start.offset == hint.start.offset + hint.value.len() as u32 =>
            {
                2
            }
            _ => 1,
        };

        match self.stream.peek_kind(name)? {
            Some(T!["|"]) => self.is_at_union_typed_local(),
            Some(kind) if kind.is_identifier_maybe_reserved() => {
                Ok(matches!(self.stream.peek_kind(name + 1)?, Some(T!["="])))
            }
            _ => Ok(false),
        }
    }

    /// Returns `true` when the union type at the head of the stream is followed by a local's name and `=`. A type
    /// holds names, `|`, a `?` written right after the token before it, and anything inside `<…>` or `(…)`, so the
    /// first other token ends it. It reads past the token buffer, because a union can be longer than the buffer holds.
    fn is_at_union_typed_local(&self) -> Result<bool, ParseError> {
        let ends_type = |token: &Token<'_>| {
            token.kind.is_identifier_maybe_reserved() || matches!(token.kind, T![">" | ">>" | ")" | "?"])
        };
        let mut depth = 0usize;
        let mut before: [Option<Token<'_>>; 2] = [None, None];
        for token in self.stream.peek_tokens() {
            let token = token?;
            let previous = before[1];
            match token.kind {
                T![";" | "{" | "}"] => return Ok(false),
                T!["<" | "("] => depth += 1,
                T![">" | ")" | ">>"] => {
                    let closed = if token.kind == T![">>"] { 2 } else { 1 };
                    let Some(outer) = depth.checked_sub(closed) else {
                        return Ok(false);
                    };
                    depth = outer;
                }
                _ if depth > 0 => {}
                T!["="] => {
                    return Ok(matches!(before, [Some(type_end), Some(name)]
                        if ends_type(&type_end) && name.kind.is_identifier_maybe_reserved()));
                }
                T!["?"]
                    if previous.is_some_and(|previous| {
                        token.start.offset == previous.start.offset + previous.value.len() as u32
                    }) => {}
                T!["|"] => {}
                kind if kind.is_identifier_maybe_reserved() => {}
                _ => return Ok(false),
            }
            before = [previous, Some(token)];
        }

        Ok(false)
    }
}
