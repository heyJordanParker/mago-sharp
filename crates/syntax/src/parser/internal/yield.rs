use crate::T;
use crate::cst::cst::Expression;
use crate::cst::cst::UnaryPrefix;
use crate::cst::cst::UnaryPrefixOperator;
use crate::cst::cst::Yield;
use crate::cst::cst::YieldFrom;
use crate::cst::cst::YieldPair;
use crate::cst::cst::YieldSpread;
use crate::cst::cst::YieldValue;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::Precedence;
use mago_allocator::prelude::*;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_yield(&mut self) -> Result<Yield<'arena>, ParseError> {
        let r#yield = self.expect_keyword(T!["yield"])?;
        let Some(next) = self.stream.lookahead(0)? else {
            return Ok(Yield::Value(YieldValue { r#yield, value: None }));
        };

        // A `.sharp` file reads `from` as a name, spec section 2, so PHP's `yield from other` is that name before an
        // expression, which the checker refuses by naming `yield ...other`.
        let is_from = T!["from"] == next.kind
            || (self.dialect.is_sharp()
                && next.kind == T![Identifier]
                && next.value == b"from"
                && self.stream.lookahead(1)?.is_some_and(|after| Self::is_at_start_of_expression(after.kind)));
        if is_from {
            return Ok(Yield::From(YieldFrom {
                r#yield,
                from: self.expect_any_keyword()?,
                iterator: self.arena.alloc(self.parse_expression_with_precedence(Precedence::YieldFrom)?),
            }));
        }

        // `yield ...other` produces every element of `other`, spec section 12, as PHP's `yield from other` does.
        if self.dialect.is_sharp() && T!["..."] == next.kind {
            return Ok(Yield::Spread(YieldSpread {
                r#yield,
                ellipsis: self.stream.eat_span(T!["..."])?,
                iterator: self.arena.alloc(self.parse_expression_with_precedence(Precedence::YieldFrom)?),
            }));
        }

        if T!["&"] == next.kind {
            let ampersand_span = self.stream.eat_span(T!["&"])?;
            let referenced_expr = self.parse_expression_with_precedence(Precedence::Reference)?;
            let value = self.arena.alloc(Expression::UnaryPrefix(UnaryPrefix {
                operator: UnaryPrefixOperator::Reference(ampersand_span),
                operand: referenced_expr,
            }));

            return Ok(Yield::Value(YieldValue { r#yield, value: Some(value) }));
        }

        if !Self::is_at_start_of_expression(next.kind) {
            return Ok(Yield::Value(YieldValue { r#yield, value: None }));
        }

        let key_or_value = self.parse_expression_with_precedence(Precedence::Yield)?;

        Ok(if matches!(self.stream.peek_kind(0)?, Some(T!["=>"])) {
            Yield::Pair(YieldPair {
                r#yield,
                key: self.arena.alloc(key_or_value),
                arrow: self.stream.eat_span(T!["=>"])?,
                value: self.arena.alloc(self.parse_expression_with_precedence(Precedence::Yield)?),
            })
        } else {
            Yield::Value(YieldValue { r#yield, value: Some(self.arena.alloc(key_or_value)) })
        })
    }
}
