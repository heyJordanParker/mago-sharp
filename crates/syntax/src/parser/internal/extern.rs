use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::AttributeList;
use crate::cst::cst::Extern;
use crate::cst::cst::ExternNamespaced;
use crate::cst::cst::Uses;
use crate::cst::sequence::Sequence;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Parses a PHP# `extern` declaration after its `attribute_lists`: `extern`, `event` when the class is an event, its
    /// target, a bare name or `Class.member`, then an optional `uses` clause. A stated signature writes its return type
    /// before the target and its parameters after it, and a generic class's type parameters between the class and the
    /// method, as in `extern Builder<TModel> Builder<TModel>.where(keyof TModel column);`.
    /// `extern namespace WordPress for WP_Post;` gives a library's class, function or constant of PHP's root namespace a
    /// namespace instead, spec section 23.
    pub(crate) fn parse_extern(
        &mut self,
        attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    ) -> Result<Extern<'arena>, ParseError<'arena>> {
        let r#extern = self.expect_any_keyword()?;
        let event = if self.stream.lookahead(0)?.is_some_and(|token| token.value == b"event")
            && self.stream.peek_kind(1)? == Some(T![Identifier])
        {
            Some(self.expect_any_keyword()?)
        } else {
            None
        };
        let namespace = if event.is_none() && self.stream.peek_kind(0)? == Some(T!["namespace"]) {
            Some(self.expect_any_keyword()?)
        } else {
            None
        };
        let return_type_hint = if event.is_none() && namespace.is_none() && self.is_at_extern_signature()? {
            Some(self.parse_type_hint()?)
        } else {
            None
        };
        // The target is a bare name or `Class.member`. A longer one is a full name, which the analyzer refuses with the
        // import to add, spec section 23. A namespace is dotted as any namespace.
        let target = self.parse_dotted_identifier()?;
        let (type_parameters, member, parameter_list) = if return_type_hint.is_some() {
            let type_parameters = self.parse_optional_type_parameter_list()?;
            let member = if type_parameters.is_some() {
                self.stream.eat_span(T!["."])?;

                Some(self.parse_local_identifier()?)
            } else {
                None
            };

            (type_parameters, member, Some(self.parse_function_like_parameter_list()?))
        } else {
            (None, None, None)
        };
        let (namespaced, uses) = if namespace.is_some() {
            let r#for = self.expect_keyword(T!["for"])?;

            (Some(ExternNamespaced { r#for, name: self.parse_dotted_identifier()? }), None)
        } else {
            (None, self.parse_optional_uses()?)
        };

        Ok(Extern {
            attribute_lists,
            r#extern,
            event,
            namespace,
            return_type_hint,
            target,
            type_parameters,
            member,
            parameter_list,
            namespaced,
            uses,
            terminator: self.parse_terminator()?,
        })
    }

    /// Whether the `extern` line ahead states a signature: a `(` comes before the line ends.
    fn is_at_extern_signature(&self) -> Result<bool, ParseError<'arena>> {
        for token in self.stream.peek_tokens() {
            match token?.kind {
                T!["("] => return Ok(true),
                T![";" | "{" | "}"] => return Ok(false),
                _ => {}
            }
        }

        Ok(false)
    }

    /// Parses a `uses` clause when one starts here: `uses`, which stays a name everywhere else, and its effects.
    pub(crate) fn parse_optional_uses(&mut self) -> Result<Option<Uses<'arena>>, ParseError<'arena>> {
        if !self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"uses") {
            return Ok(None);
        }

        let uses = self.expect_any_keyword()?;
        let mut names = self.new_vec();
        let mut commas = self.new_vec();
        loop {
            names.push(self.parse_local_identifier()?);

            if let Some(T![","]) = self.stream.peek_kind(0)? {
                commas.push(self.stream.consume()?);
            } else {
                break;
            }
        }

        Ok(Some(Uses { uses, names: TokenSeparatedSequence::new(names, commas) }))
    }
}
