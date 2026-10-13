use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::class_like::method::MethodExpressionBody;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;

/// Represents a PHP# law, spec section 28: a fact about a class's values, stated over parameters that range over every
/// value. A law has only an expression body, and it never runs.
///
/// Example:
///
/// ```csharp
/// law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Law<'arena> {
    pub law: Keyword<'arena>,
    pub name: LocalIdentifier<'arena>,
    pub parameter_list: FunctionLikeParameterList<'arena>,
    pub body: MethodExpressionBody<'arena>,
}

impl HasSpan for Law<'_> {
    fn span(&self) -> Span {
        Span::between(self.law.span(), self.body.span())
    }
}
