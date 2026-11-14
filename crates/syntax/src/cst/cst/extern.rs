use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::attribute::AttributeList;
use crate::cst::cst::identifier::Identifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::terminator::Terminator;
use crate::cst::cst::uses::Uses;
use crate::cst::sequence::Sequence;

/// Represents a PHP# `extern` declaration, which states the effects of a plain PHP class, method or function.
///
/// The target is a bare name, a class or a function, or `Class.member`, a dotted identifier. Attributes before it are
/// the plain PHP class's own, spec section 29.
///
/// Example:
///
/// ```csharp
/// extern StripeClient uses Http;
/// extern Carbon.now uses Clock;
/// extern BigDecimal;
/// extern event JobFailed;
/// extern namespace WordPress for WP_Post;
/// extern namespace Foundation for abort;
/// [DynamicallyAccessed]
/// extern AsEventListener;
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Extern<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    pub r#extern: Keyword<'arena>,
    /// `event`, which marks the plain PHP class as an event, spec section 15.
    pub event: Option<Keyword<'arena>>,
    /// `namespace`, which makes the target the PHP# namespace of the class, function or constant `for` names, spec
    /// section 23.
    pub namespace: Option<Keyword<'arena>>,
    pub target: Identifier<'arena>,
    /// `for WP_Post` or `for abort` after a namespace target.
    pub namespaced: Option<ExternNamespaced<'arena>>,
    pub uses: Option<Uses<'arena>>,
    pub terminator: Terminator<'arena>,
}

/// The class, function or constant of PHP's root namespace that `extern namespace WordPress for WP_Post;` or
/// `extern namespace Foundation for abort;` gives a namespace: `for WP_Post`, `for abort`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ExternNamespaced<'arena> {
    pub r#for: Keyword<'arena>,
    pub name: Identifier<'arena>,
}

impl HasSpan for ExternNamespaced<'_> {
    fn span(&self) -> Span {
        Span::between(self.r#for.span(), self.name.span())
    }
}

impl HasSpan for Extern<'_> {
    fn span(&self) -> Span {
        let start = self.attribute_lists.first().map_or_else(|| self.r#extern.span(), HasSpan::span);

        Span::between(start, self.terminator.span())
    }
}
