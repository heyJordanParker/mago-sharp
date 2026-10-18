use crate::T;
use crate::cst::cst::Attribute;
use crate::cst::cst::AttributeList;
use crate::cst::cst::AttributeTarget;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::TokenKind;
use mago_allocator::prelude::*;
use mago_span::HasSpan;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_attribute_list_sequence(
        &mut self,
    ) -> Result<Sequence<'arena, AttributeList<'arena>>, ParseError> {
        self.parse_attribute_lists(true)
    }

    /// The attributes of a PHP# accessor or type parameter, where no type can start, so `[` always opens a list, as
    /// in `{ [Pure] get; }`.
    pub(crate) fn parse_typeless_attribute_list_sequence(
        &mut self,
    ) -> Result<Sequence<'arena, AttributeList<'arena>>, ParseError> {
        self.parse_attribute_lists(false)
    }

    fn parse_attribute_lists(
        &mut self,
        types_may_start: bool,
    ) -> Result<Sequence<'arena, AttributeList<'arena>>, ParseError> {
        let mut inner = self.new_vec();
        while let Some(open) = self.attribute_list_open(types_may_start)? {
            inner.push(self.parse_attribute_list(open)?);
        }

        Ok(Sequence::new(inner))
    }

    /// Whether a PHP# statement that starts with `[` starts with an attribute list. Outside a block, a statement
    /// declares, so `[` opens its attributes. Inside a block, `[` opens a list, as C# decides by the same place.
    pub(crate) const fn is_at_sharp_attribute_list(&self) -> bool {
        self.dialect.is_sharp() && !self.state.within_block
    }

    /// The token that opens the next attribute list: `#[`, or in PHP# a `[` that opens no tuple type where
    /// `types_may_start`.
    fn attribute_list_open(&mut self, types_may_start: bool) -> Result<Option<TokenKind>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T!["#["]) => Some(T!["#["]),
            Some(T!["["]) if self.dialect.is_sharp() && !(types_may_start && self.is_at_tuple_hint()?) => Some(T!["["]),
            _ => None,
        })
    }

    /// PHP# writes an attribute list in square brackets, as in `[Searchable]`, which may open with a target, as in
    /// `[return: NotNull]`, spec section 1. PHP's `#[` stands in a PHP# file as an error, and the list still parses.
    fn parse_attribute_list(&mut self, open: TokenKind) -> Result<AttributeList<'arena>, ParseError> {
        let mut target = None;
        let mut first = true;
        let result = self.parse_comma_separated_sequence(open, T!["]"], |parser| {
            let attribute_target = parser.parse_optional_attribute_target()?;
            if first {
                target = attribute_target;
            } else if let Some(late) = attribute_target {
                parser.errors.push(ParseError::LateAttributeTargetInSharp(late.span()));
            }
            first = false;

            parser.parse_attribute()
        })?;
        if self.dialect.is_sharp() && open == T!["#["] {
            self.report_unexpected_in_sharp(result.open);
        }

        Ok(AttributeList {
            hash_left_bracket: result.open,
            target,
            attributes: result.sequence,
            right_bracket: result.close,
        })
    }

    /// A PHP# attribute target, as `return:` in `[return: NotNull]`: a name and a colon before an attribute.
    fn parse_optional_attribute_target(&mut self) -> Result<Option<AttributeTarget<'arena>>, ParseError> {
        if !self.dialect.is_sharp()
            || !self.stream.peek_kind(0)?.is_some_and(|kind| kind.is_identifier_maybe_reserved())
            || self.stream.peek_kind(1)? != Some(T![":"])
        {
            return Ok(None);
        }

        Ok(Some(AttributeTarget { name: self.parse_local_identifier()?, colon: self.stream.eat_span(T![":"])? }))
    }

    fn parse_attribute(&mut self) -> Result<Attribute<'arena>, ParseError> {
        // A dotted name is a full name, which the analyzer refuses with the import to add, spec section 23.
        let name = if self.dialect.is_sharp() && self.stream.peek_kind(1)? == Some(T!["."]) {
            self.parse_dotted_identifier()?
        } else {
            self.parse_identifier()?
        };

        Ok(Attribute { name, argument_list: self.parse_optional_partial_argument_list()? })
    }
}
