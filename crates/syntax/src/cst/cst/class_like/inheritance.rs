use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::identifier::Identifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::cst::sequence::TokenSeparatedSequenceExt;

/// Represents `implements` keyword with one or more types.
///
/// # Example
///
/// ```php
/// <?php
///
/// final class Foo implements Bar, Baz {}
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Implements<'arena> {
    pub implements: Keyword<'arena>,
    pub types: TokenSeparatedSequence<'arena, Identifier<'arena>>,
}

/// Represents `extends` keyword with one or more types.
///
/// # Example
///
/// ```php
/// <?php
///
/// interface Foo extends Bar, Baz {}
/// ```
///
/// ```php
/// <?php
///
/// class Foo extends Bar {}
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Extends<'arena> {
    pub extends: Keyword<'arena>,
    pub types: TokenSeparatedSequence<'arena, Identifier<'arena>>,
}

/// Represents PHP#'s class, interface or enum header: `:` and the base class and interfaces.
///
/// Spec section 22 writes it. The checker and the engine tell the class from the interfaces. An enum's header holds
/// only interfaces.
///
/// # Example
///
/// ```csharp
/// public class Page : DatabaseEntity, Linkable {}
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Inheritance<'arena> {
    pub colon: Span,
    pub types: TokenSeparatedSequence<'arena, Identifier<'arena>>,
}

impl HasSpan for Inheritance<'_> {
    fn span(&self) -> Span {
        Span::between(self.colon, self.types.span(self.colon.file_id, self.colon.end))
    }
}

impl HasSpan for Implements<'_> {
    fn span(&self) -> Span {
        let span = self.implements.span();

        Span::between(span, self.types.span(span.file_id, span.end))
    }
}

impl HasSpan for Extends<'_> {
    fn span(&self) -> Span {
        let span = self.extends.span();

        Span::between(span, self.types.span(span.file_id, span.end))
    }
}
