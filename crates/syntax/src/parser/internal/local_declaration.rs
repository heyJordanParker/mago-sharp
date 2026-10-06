use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::Hint;
use crate::cst::cst::Keyword;
use crate::cst::cst::LocalDeclaration;
use crate::cst::cst::LocalIdentifier;
use crate::error::ParseError;
use crate::parser::Parser;

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
    /// another name and `=`, or `|`, which starts a union type as in `int|string key = 1;`, or `(` and a name with
    /// `|` after it, or a type with type arguments, which start a nullable union as in `(int|string)? key = null;` or
    /// `(List<int>|string)? items = null;`. A spaced `?` is the conditional operator. The token buffer cannot look past
    /// a union, so a statement of the bitwise `|`, such as `flags | 1;` or `(flags | 1);`, is a parse error while the
    /// slice refuses bitwise `|`.
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
                Some(T!["|"]) => first.kind == T![Identifier],
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
            Some(T!["|"]) => Ok(true),
            Some(kind) if kind.is_identifier_maybe_reserved() => {
                Ok(matches!(self.stream.peek_kind(name + 1)?, Some(T!["="])))
            }
            _ => Ok(false),
        }
    }
}
