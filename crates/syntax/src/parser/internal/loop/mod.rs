use crate::T;
use crate::cst::cst::Break;
use crate::cst::cst::Continue;
use crate::cst::cst::DoWhile;
use crate::cst::cst::Expression;
use crate::cst::cst::For;
use crate::cst::cst::ForOf;
use crate::cst::cst::LocalIdentifier;
use crate::cst::cst::Statement;
use crate::cst::cst::While;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;

pub mod do_while;
pub mod r#for;
pub mod for_of;
pub mod foreach;
pub mod r#while;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Parses a PHP# loop written with its name, as `orders: for (const order of this.orders) { … }`, spec section 17.
    pub(crate) fn parse_named_loop(&mut self) -> Result<Statement<'arena>, ParseError<'arena>> {
        let label = Some(self.parse_label()?);

        Ok(match self.stream.peek_kind(0)? {
            Some(T!["while"]) => Statement::While(While { label, ..self.parse_while()? }),
            Some(T!["do"]) => Statement::DoWhile(DoWhile { label, ..self.parse_do_while()? }),
            _ => match self.parse_for()? {
                Statement::For(r#for) => Statement::For(For { label, ..r#for }),
                Statement::ForOf(for_of) => Statement::ForOf(ForOf { label, ..for_of }),
                // TypeScript's `for (x in y)` is a parse error already, and parses to its body alone.
                body => body,
            },
        })
    }

    pub(crate) fn parse_continue(&mut self) -> Result<Continue<'arena>, ParseError<'arena>> {
        let r#continue = self.expect_keyword(T!["continue"])?;
        let label = self.parse_loop_name()?;

        Ok(Continue { r#continue, label, level: self.parse_level(label)?, terminator: self.parse_terminator()? })
    }

    pub(crate) fn parse_break(&mut self) -> Result<Break<'arena>, ParseError<'arena>> {
        let r#break = self.expect_keyword(T!["break"])?;
        let label = self.parse_loop_name()?;

        Ok(Break { r#break, label, level: self.parse_level(label)?, terminator: self.parse_terminator()? })
    }

    /// The name of the loop a PHP# `break` or `continue` leaves or continues, as `orders` in `break orders;`. PHP
    /// writes a level there instead, as in `break 2;`.
    fn parse_loop_name(&mut self) -> Result<Option<LocalIdentifier<'arena>>, ParseError<'arena>> {
        if !self.dialect.is_sharp()
            || !self.stream.peek_kind(0)?.is_some_and(|kind| kind.is_identifier_maybe_reserved())
        {
            return Ok(None);
        }

        Ok(Some(self.parse_local_identifier()?))
    }

    /// The level PHP writes after `break` or `continue`, which a jump written with a loop's name has none of.
    fn parse_level(
        &mut self,
        label: Option<LocalIdentifier<'arena>>,
    ) -> Result<Option<&'arena Expression<'arena>>, ParseError<'arena>> {
        if label.is_some() || matches!(self.stream.peek_kind(0)?, Some(T![";" | "?>"])) {
            return Ok(None);
        }

        Ok(Some(self.parse_expression()?))
    }
}
