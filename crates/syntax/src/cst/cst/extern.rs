use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::identifier::Identifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::terminator::Terminator;
use crate::cst::cst::uses::Uses;

/// Represents a PHP# `extern` declaration, which states the effects of a plain PHP class, method or function.
///
/// The target is a bare name, a class or a function, or `Class.member`, a dotted identifier.
///
/// Example:
///
/// ```csharp
/// extern StripeClient uses Http;
/// extern Carbon.now uses Clock;
/// extern BigDecimal;
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Extern<'arena> {
    pub r#extern: Keyword<'arena>,
    pub target: Identifier<'arena>,
    pub uses: Option<Uses<'arena>>,
    pub terminator: Terminator<'arena>,
}

impl HasSpan for Extern<'_> {
    fn span(&self) -> Span {
        Span::between(self.r#extern.span(), self.terminator.span())
    }
}
