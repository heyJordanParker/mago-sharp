use mago_allocator::prelude::*;
use mago_span::HasSpan;

use crate::T;
use crate::cst::cst::Module;
use crate::cst::cst::ModuleMember;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Parses a PHP# module, spec section 32: `module` and its members in braces. A member is a law, spec section 28.
    /// The parser reads section 32's parent header and service bindings, and reports each as not supported yet where
    /// it starts.
    pub(crate) fn parse_module(&mut self) -> Result<Module<'arena>, ParseError> {
        let module = self.expect_any_keyword()?;
        if self.stream.is_at(T![":"])? {
            let colon = self.stream.consume_span()?;
            let parent = self.parse_dotted_identifier()?;
            self.errors
                .push(ParseError::NotSupportedYetInSharp("A module that overrides another", colon.join(parent.span())));
        }

        let left_brace = self.stream.eat_span(T!["{"])?;
        let mut members = self.new_vec();
        while let Some(token) = self.stream.lookahead(0)?
            && token.kind != T!["}"]
        {
            if self.skip_service_binding()? {
                continue;
            }

            if self.is_at_law()? {
                match self.parse_law() {
                    Ok(law) => members.push(ModuleMember::Law(law)),
                    Err(error) => self.errors.push(error),
                }
                continue;
            }

            self.errors.push(self.stream.unexpected(Some(token), &[T![Identifier], T!["}"]]));
            self.stream.consume()?;
        }

        Ok(Module { module, left_brace, members: Sequence::new(members), right_brace: self.stream.eat_span(T!["}"])? })
    }

    /// Skips a service binding of spec section 32 when one starts here, as in `singleton Mailer for SendReceipt =
    /// QueueMailer;`: its lifetime, the service's type, an optional `for` and the class it serves, `=`, the value and
    /// `;`. It reports the binding once, at its lifetime.
    fn skip_service_binding(&mut self) -> Result<bool, ParseError> {
        let Some(lifetime) = self.stream.lookahead(0)? else {
            return Ok(false);
        };
        let construct = match lifetime.value {
            b"singleton" if lifetime.kind == T![Identifier] => "A `singleton` service",
            b"scoped" if lifetime.kind == T![Identifier] => "A `scoped` service",
            b"transient" if lifetime.kind == T![Identifier] => "A `transient` service",
            _ => return Ok(false),
        };
        if !self.stream.peek_kind(1)?.is_some_and(|kind| kind.is_identifier_maybe_reserved()) {
            return Ok(false);
        }

        let lifetime = self.stream.consume_span()?;
        self.errors.push(ParseError::NotSupportedYetInSharp(construct, lifetime));
        self.parse_type_hint()?;
        if self.stream.is_at(T!["for"])? {
            self.stream.consume()?;
            self.parse_identifier()?;
        }
        self.stream.eat_span(T!["="])?;
        self.parse_expression()?;
        self.stream.eat_span(T![";"])?;

        Ok(true)
    }
}
