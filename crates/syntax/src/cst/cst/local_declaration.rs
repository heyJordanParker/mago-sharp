use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::expression::Expression;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::terminator::Terminator;
use crate::cst::cst::type_hint::Hint;

/// Represents a PHP# local declaration, which `let` makes reassignable and `const` does not.
///
/// A local declared with its type written has no `let`, and is reassignable unless it is `const`.
///
/// Example: `let total = 0;`, `const currency = this.currency;`, `Money? total = null;` or `const Plan plan = …;`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct LocalDeclaration<'arena> {
    pub keyword: Option<Keyword<'arena>>,
    pub hint: Option<&'arena Hint<'arena>>,
    pub name: LocalIdentifier<'arena>,
    pub equals: Span,
    pub value: &'arena Expression<'arena>,
    pub terminator: Terminator<'arena>,
}

impl LocalDeclaration<'_> {
    /// Returns `true` when the local is declared with `const`.
    #[inline]
    #[must_use]
    pub fn is_const(&self) -> bool {
        self.keyword.as_ref().is_some_and(|keyword| keyword.value.eq_ignore_ascii_case(b"const"))
    }
}

impl HasSpan for LocalDeclaration<'_> {
    fn span(&self) -> Span {
        let start = match (&self.keyword, &self.hint) {
            (Some(keyword), _) => keyword.span(),
            (None, Some(hint)) => hint.span(),
            (None, None) => self.name.span,
        };

        start.join(self.terminator.span())
    }
}
