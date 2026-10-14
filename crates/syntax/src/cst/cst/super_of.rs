use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::identifier::Identifier;
use crate::cst::cst::keyword::Keyword;

/// Represents PHP# `super<HasLabel>`, the object of a call of the interface `HasLabel`'s default body, as spec section
/// 22 gives it to a class that gets one method's body from two places.
///
/// Example: `super<HasLabel>.label()`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SuperOf<'arena> {
    pub super_: Keyword<'arena>,
    pub left_angle_bracket: Span,
    pub interface: Identifier<'arena>,
    pub right_angle_bracket: Span,
}

impl HasSpan for SuperOf<'_> {
    fn span(&self) -> Span {
        self.super_.span().join(self.right_angle_bracket)
    }
}
