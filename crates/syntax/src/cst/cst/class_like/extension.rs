use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::class_like::member::ClassLikeMember;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::type_hint::TypeParameterList;
use crate::cst::sequence::Sequence;

/// Represents a PHP# extension block, spec section 26.
///
/// It holds the members a static class adds to the type of its receiver, which the parentheses declare as one
/// parameter. Each member runs as a static method of the class, with the receiver as its first parameter. The block's
/// type parameters, as in C# 14, come before each member's own, and a call infers them from its receiver.
///
/// Example:
///
/// ```csharp
/// extension(string value)
/// {
///     public string slug() => Str.slug(value);
///     public bool isBlank => value.trim() == "";
/// }
///
/// extension<T>(List<T> items)
/// {
///     public bool isEmpty => items.count() == 0;
/// }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Extension<'arena> {
    pub extension: Keyword<'arena>,
    /// The block's type parameters, as in `extension<T>(List<T> items)`. They live in the arena, as a method's do.
    pub type_parameters: Option<&'arena TypeParameterList<'arena>>,
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
