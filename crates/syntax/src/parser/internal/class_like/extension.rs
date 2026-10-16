use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::Extension;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether a PHP# class member starts an extension block: `extension` and `(`, or `<` for its type parameters.
    /// Everywhere else `extension` is a name, such as a return type after a modifier, a member's type before its name,
    /// or a call `extension(x)`. A lowercase name is a value, spec section 24, so `extension<` is never a generic type.
    pub(crate) fn is_at_extension(&mut self) -> Result<bool, ParseError> {
        let starts_with_extension =
            self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"extension");

        Ok(starts_with_extension && matches!(self.stream.peek_kind(1)?, Some(T!["(" | "<"])))
    }

    /// Parses a PHP# extension block, spec section 26: `extension`, its type parameters, its receiver as a parameter
    /// list, and the members it adds in braces.
    pub(crate) fn parse_extension(&mut self) -> Result<Extension<'arena>, ParseError> {
        let extension = self.expect_any_keyword()?;
        let type_parameters = self.parse_optional_type_parameter_list()?.map(|list| &*self.arena.alloc(list));
        let parameter_list = self.parse_function_like_parameter_list()?;
        let left_brace = self.stream.eat_span(T!["{"])?;
        let mut members = self.new_vec();
        while let Some(token) = self.stream.lookahead(0)?
            && token.kind != T!["}"]
        {
            let position_before = self.stream.current_position();
            self.state.members_of = "An extension";
            self.state.members_name = None;
            match self.parse_classlike_member() {
                Ok(member) => members.push(member),
                Err(error) => self.errors.push(error),
            }
            if self.stream.current_position() == position_before {
                self.errors.push(self.stream.unexpected(Some(token), &[]));
                self.stream.consume()?;
            }
        }

        Ok(Extension {
            extension,
            type_parameters,
            parameter_list,
            left_brace,
            members: Sequence::new(members),
            right_brace: self.stream.eat_span(T!["}"])?,
        })
    }
}
