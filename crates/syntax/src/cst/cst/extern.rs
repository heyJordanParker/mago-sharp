use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::attribute::AttributeList;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::identifier::Identifier;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::terminator::Terminator;
use crate::cst::cst::type_hint::Hint;
use crate::cst::cst::type_hint::TypeParameterList;
use crate::cst::cst::uses::Uses;
use crate::cst::sequence::Sequence;

/// Represents a PHP# `extern` declaration, which states the effects of a plain PHP class, method or function, and may
/// state a method's or a function's signature, spec section 23.
///
/// The target is a bare name, a class or a function, or `Class.member`, a dotted identifier. A stated signature writes
/// the return type before the target and the parameters after it, in PHP# method order, and a method of a generic class
/// writes the class's type parameters between the class and the method, as `Builder<TModel>.where`. Attributes before
/// it are the plain PHP class's own, spec section 29.
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
/// extern string Foundation.route(RouteName name);
/// extern Builder<TModel> Builder<TModel>.where(keyof TModel column, Any? value);
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
    /// The return type of a stated signature, as `string` in `extern string Foundation.route(RouteName name);`.
    pub return_type_hint: Option<Hint<'arena>>,
    pub target: Identifier<'arena>,
    /// The type parameters of the generic class a stated method signature names, as `<TModel>` in
    /// `Builder<TModel>.where(…)`, where the target is the class alone.
    pub type_parameters: Option<TypeParameterList<'arena>>,
    /// The method after the class's type parameters, as `where` in `Builder<TModel>.where(…)`.
    pub member: Option<LocalIdentifier<'arena>>,
    /// The parameters of a stated signature, in PHP# method order.
    pub parameter_list: Option<FunctionLikeParameterList<'arena>>,
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

impl<'arena> Extern<'arena> {
    /// The member the line declares, as written: `now` in `Carbon.now`, `where` in `Builder<TModel>.where`, or none for a
    /// class or a function.
    #[must_use]
    pub fn member_name(&self) -> Option<&'arena [u8]> {
        match &self.member {
            Some(member) => Some(member.value),
            None => self.target.is_dotted().then(|| self.target.last_segment()),
        }
    }
}

impl HasSpan for Extern<'_> {
    fn span(&self) -> Span {
        let start = self.attribute_lists.first().map_or_else(|| self.r#extern.span(), HasSpan::span);

        Span::between(start, self.terminator.span())
    }
}
