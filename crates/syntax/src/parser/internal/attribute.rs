use crate::T;
use crate::cst::cst::Attribute;
use crate::cst::cst::AttributeList;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::TokenKind;
use mago_allocator::prelude::*;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_attribute_list_sequence(
        &mut self,
    ) -> Result<Sequence<'arena, AttributeList<'arena>>, ParseError> {
        let mut inner = self.new_vec();
        while let Some(open) = self.attribute_list_open()? {
            inner.push(self.parse_attribute_list(open)?);
        }

        Ok(Sequence::new(inner))
    }

    /// Whether a PHP# statement that starts with `[` starts with an attribute list. Outside a block, a statement
    /// declares, so `[` opens its attributes. Inside a block, `[` opens a list, as C# decides by the same place.
    pub(crate) const fn is_at_sharp_attribute_list(&self) -> bool {
        self.dialect.is_sharp() && !self.state.within_block
    }

    /// The token that opens the next attribute list: `#[`, or `[` in PHP#.
    fn attribute_list_open(&mut self) -> Result<Option<TokenKind>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T!["#["]) => Some(T!["#["]),
            Some(T!["["]) if self.dialect.is_sharp() => Some(T!["["]),
            _ => None,
        })
    }

    /// PHP# writes an attribute list in square brackets, as in `[Searchable]`. PHP's `#[` stands in a PHP# file as
    /// an error, and the list still parses.
    fn parse_attribute_list(&mut self, open: TokenKind) -> Result<AttributeList<'arena>, ParseError> {
        let result = self.parse_comma_separated_sequence(open, T!["]"], |p| p.parse_attribute())?;
        if self.dialect.is_sharp() && open == T!["#["] {
            self.errors.push(ParseError::PhpSyntaxInSharp(open, result.open));
        }

        Ok(AttributeList { hash_left_bracket: result.open, attributes: result.sequence, right_bracket: result.close })
    }

    /// A PHP# attribute target, such as `return:` in `[return: NotNull]`, is spec syntax the engine cannot run yet.
    pub(crate) fn parse_attribute(&mut self) -> Result<Attribute<'arena>, ParseError> {
        if self.dialect.is_sharp()
            && self.stream.peek_kind(0)?.is_some_and(|kind| kind.is_identifier_maybe_reserved())
            && self.stream.peek_kind(1)? == Some(T![":"])
        {
            let target = self.stream.consume_span()?;
            let colon = self.stream.eat_span(T![":"])?;
            self.errors.push(ParseError::NotSupportedYetInSharp("An attribute target", target.join(colon)));
        }

        Ok(Attribute { name: self.parse_identifier()?, argument_list: self.parse_optional_partial_argument_list()? })
    }
}
