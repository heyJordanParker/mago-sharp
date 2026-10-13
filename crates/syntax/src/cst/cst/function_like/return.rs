use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::type_hint::Hint;

/// Represents a function-like return type hint in PHP.
///
/// A PHP# method writes its return type before its name, with no colon.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct FunctionLikeReturnTypeHint<'arena> {
    pub colon: Option<Span>,
    pub hint: Hint<'arena>,
}

impl HasSpan for FunctionLikeReturnTypeHint<'_> {
    fn span(&self) -> Span {
        match self.colon {
            Some(colon) => Span::between(colon, self.hint.span()),
            None => self.hint.span(),
        }
    }
}
