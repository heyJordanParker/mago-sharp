use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::attribute::AttributeList;
use crate::cst::cst::class_like::EnumBackingTypeHint;
use crate::cst::cst::class_like::inheritance::Inheritance;
use crate::cst::cst::class_like::member::ClassLikeMember;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::modifier::Modifier;
use crate::cst::sequence::Sequence;

/// Represents a PHP# struct, spec section 10: a value that never changes and compares by value.
///
/// Its header is an enum's: a backing type, then the interfaces it implements.
///
/// Example:
///
/// ```csharp
/// public struct Username : string, HasLabel
/// {
///     public Username(public string value { get; }) { }
/// }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Struct<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    pub modifiers: Sequence<'arena, Modifier<'arena>>,
    pub r#struct: Keyword<'arena>,
    pub name: LocalIdentifier<'arena>,
    pub backing_type_hint: Option<EnumBackingTypeHint<'arena>>,
    /// The interfaces in the `: string, Interface` header. After a backing type, its `colon` is the `,` that opens
    /// the interfaces.
    pub inheritance: Option<Inheritance<'arena>>,
    pub left_brace: Span,
    pub members: Sequence<'arena, ClassLikeMember<'arena>>,
    pub right_brace: Span,
}

impl HasSpan for Struct<'_> {
    fn span(&self) -> Span {
        let start = match (self.attribute_lists.first(), self.modifiers.first()) {
            (Some(attribute_list), _) => attribute_list.span(),
            (None, Some(modifier)) => modifier.span(),
            (None, None) => self.r#struct.span(),
        };

        start.join(self.right_brace)
    }
}
