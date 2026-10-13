use crate::T;
use crate::cst::cst::ConstantAccess;
use crate::cst::cst::Expression;
use crate::cst::cst::Identifier;
use crate::cst::cst::Instantiation;
use crate::cst::cst::NamedValue;
use crate::cst::cst::ObjectInitializer;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::Precedence;
use mago_allocator::prelude::*;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_instantiation(&mut self) -> Result<Instantiation<'arena>, ParseError> {
        Ok(Instantiation {
            new: self.expect_keyword(T!["new"])?,
            class: self.arena.alloc(self.parse_expression_with_precedence(Precedence::New)?),
            constructor: None,
            argument_list: self.parse_optional_argument_list()?,
            initializer: self.parse_optional_object_initializer()?,
        })
    }

    /// Parses the PHP# object initializer after `new X(…)`, spec section 16, when one follows. A `{` after `new X`
    /// opens one only where a name and `:`, `,` or `}` follow it, or `}` closes it at once, so PHP's `{` never does.
    pub(crate) fn parse_optional_object_initializer(
        &mut self,
    ) -> Result<Option<ObjectInitializer<'arena>>, ParseError> {
        let opens = self.dialect.is_sharp()
            && self.stream.is_at(T!["{"])?
            && match self.stream.peek_kind(1)? {
                Some(T!["}"]) => true,
                Some(kind) if kind.is_identifier_maybe_reserved() => {
                    matches!(self.stream.peek_kind(2)?, Some(T![":" | "," | "}"]))
                }
                _ => false,
            };

        if opens { Ok(Some(self.parse_object_initializer()?)) } else { Ok(None) }
    }

    /// Parses `{ name: value, … }`, the values `new` and `with` set by name, spec section 16. The shorthand `name`
    /// reads the local `name`.
    pub(crate) fn parse_object_initializer(&mut self) -> Result<ObjectInitializer<'arena>, ParseError> {
        let result = self.parse_comma_separated_sequence(T!["{"], T!["}"], |parser| {
            let name = parser.parse_local_identifier()?;
            if parser.stream.is_at(T![":"])? {
                return Ok(NamedValue {
                    name,
                    colon: Some(parser.stream.eat_span(T![":"])?),
                    value: parser.parse_expression()?,
                });
            }

            let local = Expression::ConstantAccess(ConstantAccess { name: Identifier::Local(name) });

            Ok(NamedValue { name, colon: None, value: parser.arena.alloc(local) })
        })?;

        Ok(ObjectInitializer { left_brace: result.open, values: result.sequence, right_brace: result.close })
    }
}
