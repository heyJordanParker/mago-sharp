use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::expression::Expression;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::terminator::Terminator;
use crate::cst::cst::type_hint::Hint;
use crate::cst::sequence::TokenSeparatedSequence;

/// Represents a PHP# local declaration, which `let` makes reassignable and `const` does not.
///
/// A local declared with its type written has no `let`, and is reassignable unless it is `const`.
///
/// Example: `let total = 0;`, `const currency = this.currency;`, `Money? total = null;`, `const Plan plan = …;` or
/// `const [total, count] = summary;`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct LocalDeclaration<'arena> {
    pub keyword: Option<Keyword<'arena>>,
    pub hint: Option<&'arena Hint<'arena>>,
    pub target: LocalTarget<'arena>,
    pub equals: Span,
    pub value: &'arena Expression<'arena>,
    pub terminator: Terminator<'arena>,
}

/// Represents what a PHP# local declaration declares.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum LocalTarget<'arena> {
    /// One local, as in `const total`.
    Name(LocalIdentifier<'arena>),
    /// One local for each element of a tuple, as in `const [total, count]`, spec section 30.
    Unpack(LocalUnpack<'arena>),
}

/// Represents the locals a PHP# local declaration unpacks a tuple into, in the order of its elements, as in
/// `[total, count]`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct LocalUnpack<'arena> {
    pub left_bracket: Span,
    pub names: TokenSeparatedSequence<'arena, LocalIdentifier<'arena>>,
    pub right_bracket: Span,
}

impl LocalDeclaration<'_> {
    /// Returns `true` when the local is declared with `const`.
    #[inline]
    #[must_use]
    pub fn is_const(&self) -> bool {
        self.keyword.as_ref().is_some_and(|keyword| keyword.value.eq_ignore_ascii_case(b"const"))
    }
}

impl<'arena> LocalTarget<'arena> {
    /// The names the declaration declares, in their written order.
    #[must_use]
    pub fn names(&self) -> Vec<&LocalIdentifier<'arena>> {
        match self {
            LocalTarget::Name(name) => vec![name],
            LocalTarget::Unpack(unpack) => unpack.names.iter().collect(),
        }
    }
}

impl HasSpan for LocalDeclaration<'_> {
    fn span(&self) -> Span {
        let start = match (&self.keyword, &self.hint) {
            (Some(keyword), _) => keyword.span(),
            (None, Some(hint)) => hint.span(),
            (None, None) => self.target.span(),
        };

        start.join(self.terminator.span())
    }
}

impl HasSpan for LocalTarget<'_> {
    fn span(&self) -> Span {
        match self {
            LocalTarget::Name(name) => name.span,
            LocalTarget::Unpack(unpack) => unpack.span(),
        }
    }
}

impl HasSpan for LocalUnpack<'_> {
    fn span(&self) -> Span {
        self.left_bracket.join(self.right_bracket)
    }
}
