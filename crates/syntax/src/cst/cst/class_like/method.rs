use strum::Display;

use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::Terminator;
use crate::cst::cst::attribute::AttributeList;
use crate::cst::cst::block::Block;
use crate::cst::cst::expression::Expression;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::function_like::r#return::FunctionLikeReturnTypeHint;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::modifier::Modifier;
use crate::cst::cst::type_hint::Hint;
use crate::cst::sequence::Sequence;

/// Represents a method statement in PHP.
///
/// Example:
///
/// ```php
/// class Foo {
///    public function bar() {
///       return 'baz';
///    }
/// }
/// ```
///
/// A PHP# method has no `function` keyword, and its return type, which has no colon, comes before its name:
///
/// ```csharp
/// public string bar() { return "baz"; }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Method<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    pub modifiers: Sequence<'arena, Modifier<'arena>>,
    pub function: Option<Keyword<'arena>>,
    pub ampersand: Option<Span>,
    pub name: LocalIdentifier<'arena>,
    pub parameter_list: FunctionLikeParameterList<'arena>,
    pub return_type_hint: Option<FunctionLikeReturnTypeHint<'arena>>,
    pub body: MethodBody<'arena>,
}

/// Represents the body of a method statement in PHP.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum MethodBody<'arena> {
    Abstract(MethodAbstractBody<'arena>),
    Concrete(Block<'arena>),
    Expression(MethodExpressionBody<'arena>),
}

/// The expression body of a PHP# method, which returns the expression, or runs it in a `void` method and the
/// constructor, as in C#:
///
/// ```csharp
/// public int total() => this.a + this.b;
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MethodExpressionBody<'arena> {
    pub arrow: Span,
    pub expression: &'arena Expression<'arena>,
    pub semicolon: Span,
}

/// Represents the abstract body of a method statement in PHP.
///
/// Example:
///
/// ```php
/// <?php
///
/// abstract class Foo {
///   abstract public function bar();
/// }
///
/// ```
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MethodAbstractBody<'arena> {
    pub terminator: Terminator<'arena>,
}

impl Method<'_> {
    /// Returns `true` if the method contains any promoted properties.
    pub fn has_promoted_properties(&self) -> bool {
        self.parameter_list
            .parameters
            .iter()
            .any(super::super::function_like::parameter::FunctionLikeParameter::is_promoted_property)
    }

    /// Returns `true` if the method is abstract.
    #[inline]
    #[must_use]
    pub const fn is_abstract(&self) -> bool {
        matches!(self.body, MethodBody::Abstract(_))
    }

    /// Returns `true` if the method returns a value: it has a return type other than `void` and `never`. A PHP#
    /// expression body returns its expression in such a method, and runs it as a statement in any other.
    #[inline]
    #[must_use]
    pub const fn returns_value(&self) -> bool {
        matches!(&self.return_type_hint, Some(hint) if !matches!(hint.hint, Hint::Void(_) | Hint::Never(_)))
    }

    /// Returns `true` if the method is static.
    #[inline]
    pub fn is_static(&self) -> bool {
        self.modifiers.iter().any(Modifier::is_static)
    }
}

impl HasSpan for Method<'_> {
    fn span(&self) -> Span {
        if let Some(attribute_list) = self.attribute_lists.first() {
            return Span::between(attribute_list.span(), self.body.span());
        }

        if let Some(modifier) = self.modifiers.first() {
            return Span::between(modifier.span(), self.body.span());
        }

        if let Some(function) = self.function {
            return Span::between(function.span, self.body.span());
        }

        if let Some(return_type_hint) = &self.return_type_hint {
            return Span::between(return_type_hint.span(), self.body.span());
        }

        Span::between(self.name.span, self.body.span())
    }
}

impl HasSpan for MethodBody<'_> {
    fn span(&self) -> Span {
        match self {
            MethodBody::Abstract(body) => body.span(),
            MethodBody::Concrete(body) => body.span(),
            MethodBody::Expression(body) => body.span(),
        }
    }
}

impl HasSpan for MethodExpressionBody<'_> {
    fn span(&self) -> Span {
        Span::between(self.arrow, self.semicolon)
    }
}

impl HasSpan for MethodAbstractBody<'_> {
    fn span(&self) -> Span {
        self.terminator.span()
    }
}
