use crate::T;
use crate::cst::cst::Block;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_block(&mut self) -> Result<Block<'arena>, ParseError> {
        let within_block = std::mem::replace(&mut self.state.within_block, true);
        let block = self.parse_block_statements();
        self.state.within_block = within_block;

        block
    }

    fn parse_block_statements(&mut self) -> Result<Block<'arena>, ParseError> {
        let left_brace = self.stream.eat_span(T!["{"])?;
        let mut statements = self.new_vec();
        // Whether the loop is skipping the tokens after an error, which report nothing until a statement parses again.
        // PHP# recovers so, and PHP keeps upstream's error per skipped token.
        let mut recovering = false;

        loop {
            match self.stream.peek_kind(0)? {
                Some(T!["}"]) => break,
                Some(_) => {
                    let position_before = self.stream.current_position();
                    let errors_before = self.errors.len();
                    match self.parse_statement() {
                        Ok(statement) => {
                            statements.push(statement);
                            recovering = false;
                        }
                        Err(_) if recovering => self.errors.truncate(errors_before),
                        Err(err) => {
                            self.errors.push(err);
                            recovering = self.dialect.is_sharp();
                        }
                    }
                    // Forward-progress guard: prevent an infinite loop if statement parsing didn't advance.
                    if self.stream.current_position() == position_before
                        && let Ok(Some(token)) = self.stream.lookahead(0)
                    {
                        if token.kind == T!["}"] {
                            break;
                        }
                        if !recovering {
                            self.errors.push(self.stream.unexpected(Some(token), &[]));
                            recovering = self.dialect.is_sharp();
                        }
                        let _ = self.stream.consume();
                    }
                }
                None => {
                    // EOF without closing brace
                    return Err(self.stream.unexpected(None, &[T!["}"]]));
                }
            }
        }

        let right_brace = self.stream.eat_span(T!["}"])?;

        Ok(Block { left_brace, statements: Sequence::new(statements), right_brace })
    }
}
