use mago_allocator::prelude::*;
use mago_span::HasSpan;

use crate::T;
use crate::cst::cst::TypeOf;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether PHP# `typeof(` starts here.
    pub(crate) fn is_at_type_of(&mut self) -> Result<bool, ParseError> {
        Ok(self.dialect.is_sharp()
            && self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"typeof")
            && self.stream.peek_kind(1)? == Some(T!["("]))
    }

    /// Parses `typeof(Class)`. `typeof(PaginatedList<Order>)` is a class value with type arguments, spec section 25,
    /// which the engine does not keep yet. Its type arguments parse whole as one error, and the class value without
    /// them adds nothing.
    pub(crate) fn parse_type_of(&mut self) -> Result<TypeOf<'arena>, ParseError> {
        let r#typeof = self.expect_any_keyword()?;
        let left_parenthesis = self.stream.eat_span(T!["("])?;
        let class = self.parse_identifier()?;
        if self.stream.peek_kind(0)? == Some(T!["<"]) {
            let span = class.span().join(self.parse_type_argument_list()?.span());
            self.errors.push(ParseError::NotSupportedYetInSharp("A class value with type arguments", span));
        }

        Ok(TypeOf { r#typeof, left_parenthesis, class, right_parenthesis: self.stream.eat_span(T![")"])? })
    }
}
