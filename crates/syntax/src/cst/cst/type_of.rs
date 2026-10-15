use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::type_hint::Hint;

/// Represents PHP# `typeof(X)`, the class value of the type `X`, as spec section 25 gives it. A class is written by its
/// short name.
///
/// Example: `typeof(Order)`, `typeof(PaginatedList<Order>)`, `typeof(int)`, `typeof(List<Order>)`, `typeof(Self)`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TypeOf<'arena> {
    pub r#typeof: Keyword<'arena>,
    pub left_parenthesis: Span,
    pub r#type: Hint<'arena>,
    pub right_parenthesis: Span,
}

impl HasSpan for TypeOf<'_> {
    fn span(&self) -> Span {
        self.r#typeof.span().join(self.right_parenthesis)
    }
}
