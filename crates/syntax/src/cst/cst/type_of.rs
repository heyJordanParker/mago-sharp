use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::identifier::Identifier;
use crate::cst::cst::keyword::Keyword;

/// Represents PHP# `typeof(X)`, the class `X` written by its short name, as spec section 25 gives it.
///
/// Example: `typeof(Order)`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TypeOf<'arena> {
    pub r#typeof: Keyword<'arena>,
    pub left_parenthesis: Span,
    pub class: Identifier<'arena>,
    pub right_parenthesis: Span,
}

impl HasSpan for TypeOf<'_> {
    fn span(&self) -> Span {
        self.r#typeof.span().join(self.right_parenthesis)
    }
}
