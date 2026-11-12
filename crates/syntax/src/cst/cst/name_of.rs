use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::identifier::Identifier;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;

/// Represents PHP# `nameof(X.y)`, the name of the member `y` of the class `X`, and `nameof(X)`, the class's own name,
/// as spec sections 6.3 and 25 give them.
///
/// Example: `nameof(Order.total)`, `nameof(OrdersController.index)` or `nameof(Counted)`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct NameOf<'arena> {
    pub nameof: Keyword<'arena>,
    pub left_parenthesis: Span,
    pub class: Identifier<'arena>,
    pub member: Option<NameOfMember<'arena>>,
    pub right_parenthesis: Span,
}

/// Represents the `.y` of `nameof(X.y)`: a property, a field or a method of `X`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct NameOfMember<'arena> {
    pub dot: Span,
    pub name: LocalIdentifier<'arena>,
}

impl<'arena> NameOf<'arena> {
    /// The name the expression gives: the member's, or the class's own name, the part after its last `.`.
    #[must_use]
    pub fn named(&self) -> &'arena [u8] {
        match &self.member {
            Some(member) => member.name.value,
            None => {
                let class = self.class.value();

                class.rsplit(|byte| *byte == b'.' || *byte == b'\\').next().unwrap_or(class)
            }
        }
    }
}

impl HasSpan for NameOf<'_> {
    fn span(&self) -> Span {
        self.nameof.span().join(self.right_parenthesis)
    }
}
