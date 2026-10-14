use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::identifier::Identifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::type_hint::Hint;
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
/// Spec section 22 writes it. Each entry is a type, as in C#'s base list: a name, or a generic type with its type
/// arguments. The checker and the engine tell the class from the interfaces. An enum's header holds only interfaces.
///
/// # Example
///
/// ```csharp
/// public class OrderPage : PaginatedList<Order>, Linkable {}
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Inheritance<'arena> {
    pub colon: Span,
    pub types: TokenSeparatedSequence<'arena, Hint<'arena>>,
}

impl<'arena> Inheritance<'arena> {
    /// The name each entry is written with, in written order: a name as written, and a generic type by its name before
    /// its type arguments. Any other type, which the checker refuses in a header, names nothing.
    pub fn names(&self) -> impl Iterator<Item = Identifier<'arena>> {
        self.types.iter().filter_map(|hint| match hint {
            Hint::Identifier(identifier) => Some(*identifier),
            Hint::Generic(generic) => Some(Identifier::Local(generic.name)),
            _ => None,
        })
    }
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
