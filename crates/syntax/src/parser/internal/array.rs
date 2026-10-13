use mago_allocator::prelude::*;
use mago_database::file::HasFileId;
use mago_span::HasSpan;

use crate::T;
use crate::cst::cst::Array;
use crate::cst::cst::ArrayElement;
use crate::cst::cst::Expression;
use crate::cst::cst::KeyValueArrayElement;
use crate::cst::cst::LegacyArray;
use crate::cst::cst::List;
use crate::cst::cst::MissingArrayElement;
use crate::cst::cst::UnaryPrefix;
use crate::cst::cst::UnaryPrefixOperator;
use crate::cst::cst::ValueArrayElement;
use crate::cst::cst::VariadicArrayElement;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::Precedence;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_array(&mut self) -> Result<Array<'arena>, ParseError> {
        if self.dialect.is_sharp() && self.stream.peek_kind(1)? == Some(T![":"]) {
            return Ok(Array {
                left_bracket: self.stream.eat_span(T!["["])?,
                elements: TokenSeparatedSequence::empty(),
                colon: Some(self.stream.eat_span(T![":"])?),
                right_bracket: self.stream.eat_span(T!["]"])?,
            });
        }

        let result = self.parse_comma_separated_sequence(T!["["], T!["]"], |p| p.parse_array_element())?;

        Ok(Array { left_bracket: result.open, elements: result.sequence, colon: None, right_bracket: result.close })
    }

    pub(crate) fn parse_list(&mut self) -> Result<List<'arena>, ParseError> {
        let list = self.expect_keyword(T!["list"])?;
        let result = self.parse_comma_separated_sequence(T!["("], T![")"], |p| p.parse_array_element())?;

        Ok(List { list, left_parenthesis: result.open, elements: result.sequence, right_parenthesis: result.close })
    }

    pub(crate) fn parse_legacy_array(&mut self) -> Result<LegacyArray<'arena>, ParseError> {
        let array = self.expect_keyword(T!["array"])?;
        let result = self.parse_comma_separated_sequence(T!["("], T![")"], |p| p.parse_array_element())?;

        Ok(LegacyArray {
            array,
            left_parenthesis: result.open,
            elements: result.sequence,
            right_parenthesis: result.close,
        })
    }

    pub(crate) fn parse_array_element(&mut self) -> Result<ArrayElement<'arena>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T!["..."]) => {
                let ellipsis = self.stream.consume_span()?;
                ArrayElement::Variadic(VariadicArrayElement {
                    ellipsis,
                    value: self.arena.alloc(self.parse_expression()?),
                })
            }
            Some(T![","]) => {
                let next = self.stream.lookahead(0)?.ok_or_else(|| self.stream.unexpected(None, &[]))?;
                ArrayElement::Missing(MissingArrayElement { comma: next.span_for(self.stream.file_id()) })
            }
            Some(T!["&"]) => {
                let ampersand_span = self.stream.eat_span(T!["&"])?;
                let referenced_expr = self.parse_expression_with_precedence(Precedence::Reference)?;
                let value = self.arena.alloc(Expression::UnaryPrefix(UnaryPrefix {
                    operator: UnaryPrefixOperator::Reference(ampersand_span),
                    operand: referenced_expr,
                }));

                ArrayElement::Value(ValueArrayElement { value })
            }
            _ => {
                let expr = self.arena.alloc(self.parse_expression()?);

                match self.stream.peek_kind(0)? {
                    Some(T![":"]) if self.dialect.is_sharp() => ArrayElement::KeyValue(KeyValueArrayElement {
                        key: expr,
                        double_arrow: self.stream.consume_span()?,
                        value: self.parse_expression()?,
                    }),
                    Some(T!["=>"]) => {
                        let double_arrow = self.stream.consume_span()?;
                        let refused = self.dialect.is_sharp();
                        if refused {
                            self.errors.push(ParseError::PhpSyntaxInSharp(T!["=>"], double_arrow));
                        }
                        let value = self.parse_possibly_referenced_expression()?;
                        let (key, value) = if refused {
                            (
                                &*self.arena.alloc(Expression::Error(expr.span())),
                                &*self.arena.alloc(Expression::Error(value.span())),
                            )
                        } else {
                            (*expr, value)
                        };

                        ArrayElement::KeyValue(KeyValueArrayElement { key, double_arrow, value })
                    }
                    _ => ArrayElement::Value(ValueArrayElement { value: expr }),
                }
            }
        })
    }
}
