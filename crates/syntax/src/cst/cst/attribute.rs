use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::argument::PartialArgumentList;
use crate::cst::cst::identifier::Identifier;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::sequence::TokenSeparatedSequence;

/// Represents a list of attributes.
///
/// Example: `#[Foo, Bar(1)]` in `#[Foo, Bar(1)] class Foo {}`
///
/// PHP# writes it `[Foo, Bar(1)]`, and `hash_left_bracket` is then the `[`. A PHP# list may open with a target, as
/// `return:` in `[return: NotNull]`, which names where all of its attributes go, as in C#.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AttributeList<'arena> {
    pub hash_left_bracket: Span,
    pub target: Option<AttributeTarget<'arena>>,
    pub attributes: TokenSeparatedSequence<'arena, Attribute<'arena>>,
    pub right_bracket: Span,
}

/// The target a PHP# attribute list opens with: `return:` in `[return: NotNull]`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AttributeTarget<'arena> {
    pub name: LocalIdentifier<'arena>,
    pub colon: Span,
}

/// Represents a single attribute.
///
/// Example: `Foo` in `#[Foo]`, `Bar(1)` in `#[Bar(1)]`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Attribute<'arena> {
    pub name: Identifier<'arena>,
    pub argument_list: Option<PartialArgumentList<'arena>>,
}

impl HasSpan for AttributeList<'_> {
    fn span(&self) -> Span {
        Span::between(self.hash_left_bracket, self.right_bracket)
    }
}

impl HasSpan for AttributeTarget<'_> {
    fn span(&self) -> Span {
        self.name.span.join(self.colon)
    }
}

impl HasSpan for Attribute<'_> {
    fn span(&self) -> Span {
        if let Some(arguments) = &self.argument_list {
            Span::between(self.name.span(), arguments.span())
        } else {
            self.name.span()
        }
    }
}
