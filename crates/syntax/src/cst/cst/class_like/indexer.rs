use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::attribute::AttributeList;
use crate::cst::cst::class_like::method::MethodBody;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::modifier::Modifier;
use crate::cst::cst::type_hint::Hint;
use crate::cst::sequence::Sequence;

/// A PHP# indexer, spec section 6: a property that takes parameters, read and written as `cart[0]`, as C#'s.
///
/// ```csharp
/// public Line this[int index]
/// {
///     get => this.lines[index];
///     set => this.lines[index] = value;
/// }
/// ```
///
/// Its parameters stand between `[` and `]`, which `parameter_list`'s parenthesis spans hold. It runs as PHP's
/// `ArrayAccess`, so plain PHP's `$cart[0]` reaches it too.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Indexer<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    pub modifiers: Sequence<'arena, Modifier<'arena>>,
    pub hint: Hint<'arena>,
    pub this: Keyword<'arena>,
    pub parameter_list: FunctionLikeParameterList<'arena>,
    pub left_brace: Span,
    pub accessors: Sequence<'arena, IndexerAccessor<'arena>>,
    pub right_brace: Span,
}

/// An indexer's `get` or `set`, with a method's body: `get => this.lines[index];`, a block, or `get;` in an
/// interface. `set` writes its parameter `value`, of the indexer's type.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct IndexerAccessor<'arena> {
    pub modifiers: Sequence<'arena, Modifier<'arena>>,
    pub name: LocalIdentifier<'arena>,
    pub body: MethodBody<'arena>,
}

impl<'arena> Indexer<'arena> {
    /// The indexer's `get`, when it declares one.
    #[must_use]
    pub fn get(&self) -> Option<&IndexerAccessor<'arena>> {
        self.accessors.iter().find(|accessor| accessor.name.value == b"get")
    }

    /// The indexer's `set`, when it declares one.
    #[must_use]
    pub fn set(&self) -> Option<&IndexerAccessor<'arena>> {
        self.accessors.iter().find(|accessor| accessor.name.value == b"set")
    }
}

impl HasSpan for Indexer<'_> {
    fn span(&self) -> Span {
        let start = match (self.attribute_lists.first(), self.modifiers.first()) {
            (Some(attribute_list), _) => attribute_list.span(),
            (None, Some(modifier)) => modifier.span(),
            (None, None) => self.hint.span(),
        };

        Span::between(start, self.right_brace)
    }
}

impl HasSpan for IndexerAccessor<'_> {
    fn span(&self) -> Span {
        let start = self.modifiers.first().map_or(self.name.span, |modifier| modifier.span());

        Span::between(start, self.body.span())
    }
}
