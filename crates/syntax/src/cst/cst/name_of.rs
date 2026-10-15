use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::identifier::Identifier;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;

/// Represents PHP# `nameof(X.y)`, the name of the property `y` of the class `X`, as spec section 6.3 gives it.
///
/// Example: `nameof(Order.total)`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct NameOf<'arena> {
    pub nameof: Keyword<'arena>,
    pub left_parenthesis: Span,
    pub class: Identifier<'arena>,
    pub dot: Span,
    pub property: LocalIdentifier<'arena>,
    pub right_parenthesis: Span,
}

impl HasSpan for NameOf<'_> {
    fn span(&self) -> Span {
        self.nameof.span().join(self.right_parenthesis)
    }
}
