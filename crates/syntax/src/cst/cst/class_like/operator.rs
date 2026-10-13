use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::attribute::AttributeList;
use crate::cst::cst::binary::BinaryOperator;
use crate::cst::cst::class_like::method::MethodBody;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::function_like::r#return::FunctionLikeReturnTypeHint;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::modifier::Modifier;
use crate::cst::sequence::Sequence;

/// A PHP# operator a class declares, spec section 19: its return type, `operator`, the operator's symbol, its
/// parameters and a PHP# method's body.
///
/// ```csharp
/// public static bool operator ==(Money a, Money b) => a.cents == b.cents;
/// ```
///
/// The symbol is the binary operator written after `operator`. `-` with one parameter is unary `-`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Operator<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    pub modifiers: Sequence<'arena, Modifier<'arena>>,
    pub return_type_hint: FunctionLikeReturnTypeHint<'arena>,
    pub operator: Keyword<'arena>,
    pub symbol: BinaryOperator<'arena>,
    pub parameter_list: FunctionLikeParameterList<'arena>,
    pub body: MethodBody<'arena>,
}

impl Operator<'_> {
    /// The operators a PHP# class declares, as the parse error and the semantic error that refuse every other symbol
    /// name them.
    pub const DECLARABLE: &'static str = "`+ - * / % **`, unary `-`, `==` and `<=>`";
}

impl HasSpan for Operator<'_> {
    fn span(&self) -> Span {
        let start = match (self.attribute_lists.first(), self.modifiers.first()) {
            (Some(attribute_list), _) => attribute_list.span(),
            (None, Some(modifier)) => modifier.span(),
            (None, None) => self.return_type_hint.span(),
        };

        Span::between(start, self.body.span())
    }
}
