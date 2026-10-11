use mago_allocator::prelude::*;
use mago_database::file::HasFileId;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax_core::stack::ensure_sufficient_stack;

use crate::T;
use crate::cst::cst::As;
use crate::cst::cst::BinaryOperator;
use crate::cst::cst::BinaryPattern;
use crate::cst::cst::ComparisonPattern;
use crate::cst::cst::Conditional;
use crate::cst::cst::Expression;
use crate::cst::cst::Is;
use crate::cst::cst::LocalIdentifier;
use crate::cst::cst::MatchGuard;
use crate::cst::cst::NotPattern;
use crate::cst::cst::ParenthesizedPattern;
use crate::cst::cst::Pattern;
use crate::cst::cst::PatternMatch;
use crate::cst::cst::PatternMatchArm;
use crate::cst::cst::PatternMatchArmBody;
use crate::cst::cst::PatternMatchDefaultArm;
use crate::cst::cst::PatternMatchPatternArm;
use crate::cst::cst::PropertiesPattern;
use crate::cst::cst::PropertyPattern;
use crate::cst::cst::TypePattern;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::Precedence;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether the next token is PHP#'s `is` or `as` after a value, which bind as tightly as `<`, as in C#.
    pub(crate) fn is_at_is_or_as(&mut self) -> Result<bool, ParseError> {
        Ok(self.dialect.is_sharp()
            && self
                .stream
                .lookahead(0)?
                .is_some_and(|token| token.kind == T!["as"] || (token.kind == T![Identifier] && token.value == b"is")))
    }

    /// Parses `is` and its pattern, or `as` and its type, after `value`.
    pub(crate) fn parse_is_or_as(
        &mut self,
        value: &'arena Expression<'arena>,
    ) -> Result<&'arena Expression<'arena>, ParseError> {
        let keyword = self.expect_any_keyword()?;

        Ok(self.arena.alloc(if keyword.value == b"as" {
            Expression::As(As { value, r#as: keyword, hint: self.arena.alloc(self.parse_type_hint_in_expression()?) })
        } else {
            Expression::Is(Is { value, is: keyword, pattern: self.parse_pattern()? })
        }))
    }

    /// Parses a pattern: patterns joined by `or`, each of them patterns joined by `and`, as in C#.
    pub(crate) fn parse_pattern(&mut self) -> Result<&'arena Pattern<'arena>, ParseError> {
        let mut left = self.parse_and_pattern()?;
        while self.stream.is_at(T!["or"])? {
            let operator = self.expect_any_keyword()?;
            let right = self.parse_and_pattern()?;
            left = self.arena.alloc(Pattern::Binary(BinaryPattern { left, operator, right }));
        }

        Ok(left)
    }

    fn parse_and_pattern(&mut self) -> Result<&'arena Pattern<'arena>, ParseError> {
        let mut left = self.parse_not_pattern()?;
        while self.stream.is_at(T!["and"])? {
            let operator = self.expect_any_keyword()?;
            let right = self.parse_not_pattern()?;
            left = self.arena.alloc(Pattern::Binary(BinaryPattern { left, operator, right }));
        }

        Ok(left)
    }

    fn parse_not_pattern(&mut self) -> Result<&'arena Pattern<'arena>, ParseError> {
        ensure_sufficient_stack(|| {
            if self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"not") {
                let not = self.expect_any_keyword()?;
                let pattern = self.parse_not_pattern()?;

                return Ok(&*self.arena.alloc(Pattern::Not(NotPattern { not, pattern })));
            }

            self.parse_primary_pattern()
        })
    }

    fn parse_primary_pattern(&mut self) -> Result<&'arena Pattern<'arena>, ParseError> {
        let token = self.stream.lookahead(0)?.ok_or_else(|| self.stream.unexpected(None, &[]))?;
        let pattern = match token.kind {
            T!["("] => Pattern::Parenthesized(ParenthesizedPattern {
                left_parenthesis: self.stream.consume_span()?,
                pattern: self.parse_pattern()?,
                right_parenthesis: self.stream.eat_span(T![")"])?,
            }),
            T!["{"] => {
                let properties = self.parse_comma_separated_sequence(T!["{"], T!["}"], |parser| {
                    Ok(PropertyPattern {
                        name: parser.parse_local_identifier()?,
                        colon: parser.stream.eat_span(T![":"])?,
                        pattern: parser.parse_pattern()?,
                    })
                })?;

                Pattern::Properties(PropertiesPattern {
                    left_brace: properties.open,
                    properties: properties.sequence,
                    right_brace: properties.close,
                })
            }
            T!["==" | "<" | "<=" | ">" | ">="] => {
                let span = self.stream.consume_span()?;
                let operator = match token.kind {
                    T!["=="] => BinaryOperator::Equal(span),
                    T!["<"] => BinaryOperator::LessThan(span),
                    T!["<="] => BinaryOperator::LessThanOrEqual(span),
                    T![">"] => BinaryOperator::GreaterThan(span),
                    _ => BinaryOperator::GreaterThanOrEqual(span),
                };

                Pattern::Comparison(ComparisonPattern {
                    operator,
                    value: self.parse_expression_with_precedence(Precedence::SharpComparison)?,
                })
            }
            T!["["] => {
                let span = self.skip_balanced()?;
                self.errors.push(ParseError::NotSupportedYetInSharp("A list pattern", span));

                Pattern::Value(self.arena.alloc(Expression::Error(span)))
            }
            T!["-" | "+"] => Pattern::Value(self.parse_expression_with_precedence(Precedence::SharpComparison)?),
            kind if kind.is_literal() => {
                Pattern::Value(self.parse_expression_with_precedence(Precedence::SharpComparison)?)
            }
            T![Identifier] if self.stream.peek_kind(1)? == Some(T!["."]) => {
                if self.is_at_case_with_data()? {
                    let span = self.skip_case_pattern()?;
                    self.errors.push(ParseError::NotSupportedYetInSharp("An enum case pattern", span));

                    Pattern::Value(self.arena.alloc(Expression::Error(span)))
                } else {
                    // A case without fields or a name, as in `Status.Open`, is the value `Class.y` reads.
                    Pattern::Value(self.parse_expression_with_precedence(Precedence::SharpComparison)?)
                }
            }
            _ => Pattern::Type(TypePattern {
                hint: self.parse_type_hint_in_expression()?,
                variable: self.parse_pattern_variable()?,
            }),
        };

        Ok(self.arena.alloc(pattern))
    }

    /// The name a type pattern declares, as in `int count`. `when`, which starts an arm's condition, and `is`, which
    /// starts another test, are no names.
    fn parse_pattern_variable(&mut self) -> Result<Option<LocalIdentifier<'arena>>, ParseError> {
        match self.stream.lookahead(0)? {
            Some(token) if token.kind == T![Identifier] && !matches!(token.value, b"when" | b"is") => {
                Ok(Some(self.parse_local_identifier()?))
            }
            _ => Ok(None),
        }
    }

    /// Whether `Case.Name` is followed by its fields in parentheses or a name, as an enum with data writes it.
    fn is_at_case_with_data(&mut self) -> Result<bool, ParseError> {
        Ok(match self.stream.lookahead(3)? {
            Some(token) if token.kind == T!["("] => true,
            Some(token) => token.kind == T![Identifier] && !matches!(token.value, b"when" | b"is"),
            None => false,
        })
    }

    /// Skips an enum case pattern, `Case.Name`, then its fields in parentheses or its name, and returns its span.
    fn skip_case_pattern(&mut self) -> Result<Span, ParseError> {
        let start = self.stream.consume_span()?;
        self.stream.consume()?;
        let mut end = self.parse_local_identifier()?.span;
        if self.stream.is_at(T!["("])? {
            end = self.skip_balanced()?;
        }
        if let Some(variable) = self.parse_pattern_variable()? {
            end = variable.span;
        }

        Ok(start.join(end))
    }

    /// Skips a bracketed group and everything nested in it, and returns its span.
    fn skip_balanced(&mut self) -> Result<Span, ParseError> {
        let open = self.stream.consume_span()?;
        let mut depth = 1usize;
        let mut end = open;
        while depth > 0 {
            let token = self.stream.consume()?;
            match token.kind {
                T!["(" | "[" | "{"] => depth += 1,
                T![")" | "]" | "}"] => depth -= 1,
                _ => {}
            }
            end = token.span_for(self.stream.file_id());
        }

        Ok(open.join(end))
    }

    /// Parses a PHP# `match`. Its arms are written `pattern => value`, `pattern when condition => value` and
    /// `default => value`, and an arm's value may be a block. A `when` condition holds operators that bind as tightly
    /// as `??` or tighter, as in C#, so the arm's `=>` ends it, and a `? :` or a lambda in it needs parentheses.
    pub(crate) fn parse_pattern_match(&mut self) -> Result<PatternMatch<'arena>, ParseError> {
        let r#match = self.expect_keyword(T!["match"])?;
        let left_parenthesis = self.stream.eat_span(T!["("])?;
        let expression = self.parse_expression()?;
        let right_parenthesis = self.stream.eat_span(T![")"])?;
        let arms = self.parse_comma_separated_sequence(T!["{"], T!["}"], Parser::parse_pattern_match_arm)?;

        Ok(PatternMatch {
            r#match,
            left_parenthesis,
            expression,
            right_parenthesis,
            left_brace: arms.open,
            arms: arms.sequence,
            right_brace: arms.close,
        })
    }

    fn parse_pattern_match_arm(&mut self) -> Result<PatternMatchArm<'arena>, ParseError> {
        if self.stream.is_at(T!["default"])? {
            return Ok(PatternMatchArm::Default(PatternMatchDefaultArm {
                default: self.expect_keyword(T!["default"])?,
                arrow: self.stream.eat_span(T!["=>"])?,
                body: self.parse_pattern_match_arm_body()?,
            }));
        }

        let pattern = self.parse_pattern()?;
        let guard = match self.stream.lookahead(0)? {
            Some(token) if token.kind == T![Identifier] && token.value == b"when" => {
                let when = self.expect_any_keyword()?;
                let condition = self.parse_expression_with_precedence(Precedence::NullCoalesce)?;

                Some(MatchGuard { when, condition: self.parse_conditional_in_guard(condition)? })
            }
            _ => None,
        };

        Ok(PatternMatchArm::Pattern(PatternMatchPatternArm {
            pattern,
            guard,
            arrow: self.stream.eat_span(T!["=>"])?,
            body: self.parse_pattern_match_arm_body()?,
        }))
    }

    /// Reads a `? :` written at the top of a `when` condition, as in `when strict ? forced : ready =>`. A `? :` needs
    /// parentheses there, so this reports it and reads both branches as operands of `??`, so the arm's `=>` still ends
    /// the condition and the rest of the file parses on, and returns the whole condition as an error expression. It
    /// returns `condition` itself when no `?` follows it.
    fn parse_conditional_in_guard(
        &mut self,
        condition: &'arena Expression<'arena>,
    ) -> Result<&'arena Expression<'arena>, ParseError> {
        if !self.stream.is_at(T!["?"])? {
            return Ok(condition);
        }

        let conditional = Conditional {
            condition,
            question_mark: self.stream.consume_span()?,
            then: Some(self.parse_expression_with_precedence(Precedence::NullCoalesce)?),
            colon: self.stream.eat_span(T![":"])?,
            r#else: self.parse_expression_with_precedence(Precedence::NullCoalesce)?,
        };
        self.errors.push(ParseError::ConditionalInGuardInSharp(conditional.span()));

        Ok(self.arena.alloc(Expression::Error(conditional.span())))
    }

    fn parse_pattern_match_arm_body(&mut self) -> Result<PatternMatchArmBody<'arena>, ParseError> {
        Ok(if self.stream.is_at(T!["{"])? {
            PatternMatchArmBody::Block(self.parse_block()?)
        } else {
            PatternMatchArmBody::Expression(self.parse_expression()?)
        })
    }
}
