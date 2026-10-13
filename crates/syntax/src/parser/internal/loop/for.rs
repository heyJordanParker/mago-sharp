use mago_span::HasSpan;

use crate::T;
use crate::cst::cst::For;
use crate::cst::cst::ForBody;
use crate::cst::cst::ForColonDelimitedBody;
use crate::cst::cst::ForOfTarget;
use crate::cst::cst::Statement;
use crate::cst::sequence::Sequence;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Parses a `for` loop, or a PHP# `for … of` loop when its header declares a loop variable followed by `of`.
    pub(crate) fn parse_for(&mut self) -> Result<Statement<'arena>, ParseError> {
        let r#for = self.expect_keyword(T!["for"])?;
        let left_parenthesis = self.stream.eat_span(T!["("])?;
        let declaration = if self.is_at_loop_variable_keyword()? {
            let keyword = self.expect_any_keyword()?;
            let is_const = keyword.value.eq_ignore_ascii_case(b"const");
            if matches!(self.stream.peek_kind(0)?, Some(T!["["])) {
                let target = ForOfTarget::KeyValue(self.parse_for_of_key_value_target(is_const)?);

                return Ok(Statement::ForOf(self.parse_for_of(r#for, left_parenthesis, keyword, target)?));
            }

            let variable = self.parse_for_of_variable(is_const)?;
            if self.is_at_of_keyword()? {
                let target = ForOfTarget::Value(variable);

                return Ok(Statement::ForOf(self.parse_for_of(r#for, left_parenthesis, keyword, target)?));
            }

            Some(self.parse_local_declaration_value(Some(keyword), variable.hint, variable.name)?)
        } else if self.is_at_local_declaration()? {
            Some(self.parse_local_declaration()?)
        } else {
            None
        };

        Ok(Statement::For(For {
            r#for,
            left_parenthesis,
            initializations: {
                let mut initializations = self.new_vec();
                let mut commas = self.new_vec();
                loop {
                    if declaration.is_some() || matches!(self.stream.peek_kind(0)?, Some(T![";"])) {
                        break;
                    }

                    initializations.push(self.parse_expression()?);

                    match self.stream.peek_kind(0)? {
                        Some(T![","]) => {
                            commas.push(self.stream.consume()?);
                        }
                        _ => {
                            break;
                        }
                    }
                }

                TokenSeparatedSequence::new(initializations, commas)
            },
            initializations_semicolon: match &declaration {
                Some(declaration) => declaration.terminator.span(),
                None => self.stream.eat_span(T![";"])?,
            },
            declaration,
            conditions: {
                let mut conditions = self.new_vec();
                let mut commas = self.new_vec();
                loop {
                    if matches!(self.stream.peek_kind(0)?, Some(T![";"])) {
                        break;
                    }

                    conditions.push(self.parse_expression()?);

                    match self.stream.peek_kind(0)? {
                        Some(T![","]) => {
                            commas.push(self.stream.consume()?);
                        }
                        _ => {
                            break;
                        }
                    }
                }

                TokenSeparatedSequence::new(conditions, commas)
            },
            conditions_semicolon: self.stream.eat_span(T![";"])?,
            increments: {
                let mut increments = self.new_vec();
                let mut commas = self.new_vec();
                loop {
                    if matches!(self.stream.peek_kind(0)?, Some(T![")"])) {
                        break;
                    }

                    increments.push(self.parse_expression()?);

                    match self.stream.peek_kind(0)? {
                        Some(T![","]) => {
                            commas.push(self.stream.consume()?);
                        }
                        _ => {
                            break;
                        }
                    }
                }

                TokenSeparatedSequence::new(increments, commas)
            },
            right_parenthesis: self.stream.eat_span(T![")"])?,
            body: self.parse_for_body()?,
        }))
    }

    fn parse_for_body(&mut self) -> Result<ForBody<'arena>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T![":"]) => ForBody::ColonDelimited(self.parse_for_colon_delimited_body()?),
            _ => ForBody::Statement(self.arena.alloc(self.parse_statement()?)),
        })
    }

    fn parse_for_colon_delimited_body(&mut self) -> Result<ForColonDelimitedBody<'arena>, ParseError> {
        Ok(ForColonDelimitedBody {
            colon: self.stream.eat_span(T![":"])?,
            statements: {
                let mut statements = self.new_vec();
                loop {
                    if matches!(self.stream.peek_kind(0)?, Some(T!["endfor"])) {
                        break;
                    }

                    let position_before = self.stream.current_position();
                    statements.push(self.parse_statement()?);
                    if self.stream.current_position() == position_before {
                        if let Ok(Some(token)) = self.stream.lookahead(0) {
                            self.errors.push(self.stream.unexpected(Some(token), &[]));
                            let _ = self.stream.consume();
                        } else {
                            break;
                        }
                    }
                }

                Sequence::new(statements)
            },
            end_for: self.expect_keyword(T!["endfor"])?,
            terminator: self.parse_terminator()?,
        })
    }
}
