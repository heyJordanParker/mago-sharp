use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::expression::Expression;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::terminator::Terminator;

/// Represents a PHP# `emit` statement, spec section 15: it raises the event its expression gives.
///
/// Example:
///
/// ```csharp
/// emit new Order.Paid(order.id, order.total);
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Emit<'arena> {
    pub emit: Keyword<'arena>,
    pub value: &'arena Expression<'arena>,
    pub terminator: Terminator<'arena>,
}

impl HasSpan for Emit<'_> {
    fn span(&self) -> Span {
        self.emit.span().join(self.terminator.span())
    }
}
