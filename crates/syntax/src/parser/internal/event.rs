use mago_allocator::prelude::*;
use mago_span::HasSpan;

use crate::T;
use crate::cst::cst::AttributeList;
use crate::cst::cst::Event;
use crate::cst::cst::Modifier;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether a PHP# event starts here: `event`, a name and `(`. The name may be a reserved word, as a class's may, and
    /// the checks refuse it there. Everywhere else `event` is a name, such as a local `event`, a call `event(x)` or an
    /// operand `event and (x)`.
    pub(crate) fn is_at_event(&mut self) -> Result<bool, ParseError> {
        let starts_with_event =
            self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"event");
        let names_an_event =
            self.stream.peek_kind(1)?.is_some_and(|kind| kind.is_identifier_maybe_reserved() && !kind.is_infix());

        Ok(self.dialect.is_sharp() && starts_with_event && names_an_event && self.stream.peek_kind(2)? == Some(T!["("]))
    }

    /// Parses a PHP# event, spec section 15: `event`, its name, its parameters and the interfaces after `:`. An event
    /// written with a body parses whole and is left out, as a named constructor is.
    pub(crate) fn parse_event_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<Event<'arena>, ParseError> {
        let event = self.expect_any_keyword()?;
        let name = self.parse_local_identifier()?;
        let parameter_list = self.parse_function_like_parameter_list()?;
        let inheritance = self.parse_optional_inheritance()?;
        if self.stream.is_at(T!["{"])? {
            return Err(ParseError::EventBodyInSharp(self.parse_block()?.span()));
        }

        Ok(Event {
            attribute_lists: attributes,
            modifiers,
            event,
            name,
            parameter_list,
            inheritance,
            terminator: self.parse_terminator()?,
        })
    }
}
