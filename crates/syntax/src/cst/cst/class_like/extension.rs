use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::class_like::member::ClassLikeMember;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::keyword::Keyword;
use crate::cst::sequence::Sequence;

/// Represents a PHP# extension block, spec section 26.
///
/// It holds the members a static class adds to the type of its receiver, which the parentheses declare as one
/// parameter. Each member runs as a static method of the class, with the receiver as its first parameter.
///
/// Example:
///
/// ```csharp
/// extension(string value)
/// {
///     public string slug() => Str.slug(value);
///     public bool isBlank => value.trim() == "";
/// }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Extension<'arena> {
    pub extension: Keyword<'arena>,
    pub parameter_list: FunctionLikeParameterList<'arena>,
    pub left_brace: Span,
    pub members: Sequence<'arena, ClassLikeMember<'arena>>,
    pub right_brace: Span,
}

impl HasSpan for Extension<'_> {
    fn span(&self) -> Span {
        Span::between(self.extension.span(), self.right_brace)
    }
}
