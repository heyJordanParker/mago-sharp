use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::expression::Expression;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::sequence::TokenSeparatedSequence;

/// Represents PHP# `with`, spec section 16: a copy of the object with the values it names set.
///
/// It sets them through their `init` or `set` accessors, and the original is unchanged. Roslyn calls it
/// `WithExpressionSyntax`, and gives it the initializer an object creation takes.
///
/// Example: `point with { y: 9 }`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct With<'arena> {
    pub object: &'arena Expression<'arena>,
    pub with: Keyword<'arena>,
    pub initializer: ObjectInitializer<'arena>,
}

/// Represents a PHP# object initializer, spec section 16: the properties set after the constructor runs, through
/// their `init` or `set` accessors. `new` and `with` take it.
///
/// Example: `{ x: 1, y: 2 }` in `new Point { x: 1, y: 2 }`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ObjectInitializer<'arena> {
    pub left_brace: Span,
    pub values: TokenSeparatedSequence<'arena, NamedValue<'arena>>,
    pub right_brace: Span,
}

/// Represents a value named after the property it sets, spec section 16.
///
/// The shorthand `currency` names the local `currency`: it has no colon, and its value is that local, read where the
/// name is written.
///
/// Example: `amount: 500` and `currency` in `new Amount { amount: 500, currency }`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct NamedValue<'arena> {
    pub name: LocalIdentifier<'arena>,
    pub colon: Option<Span>,
    pub value: &'arena Expression<'arena>,
}

impl NamedValue<'_> {
    /// Whether it is the shorthand `name`, which reads the local of its name.
    #[must_use]
    pub const fn is_shorthand(&self) -> bool {
        self.colon.is_none()
    }
}

impl HasSpan for With<'_> {
    fn span(&self) -> Span {
        self.object.span().join(self.initializer.span())
    }
}

impl HasSpan for ObjectInitializer<'_> {
    fn span(&self) -> Span {
        self.left_brace.join(self.right_brace)
    }
}

impl HasSpan for NamedValue<'_> {
    fn span(&self) -> Span {
        self.name.span().join(self.value.span())
    }
}
