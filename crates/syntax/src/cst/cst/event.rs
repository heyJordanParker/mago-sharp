use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::attribute::AttributeList;
use crate::cst::cst::class_like::inheritance::Inheritance;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::modifier::Modifier;
use crate::cst::cst::terminator::Terminator;
use crate::cst::sequence::Sequence;

/// Represents a PHP# event type, spec section 15: a class whose parameters become get-only properties.
///
/// An event is declared on its own in its file or inside the class that raises it, beside an interface its events
/// share.
///
/// Example:
///
/// ```csharp
/// public event Paid(int orderId, Money amount) : Event;
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Event<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    pub modifiers: Sequence<'arena, Modifier<'arena>>,
    pub event: Keyword<'arena>,
    pub name: LocalIdentifier<'arena>,
    pub parameter_list: FunctionLikeParameterList<'arena>,
    /// The `: Interface` header, which lists the interfaces the event implements.
    pub inheritance: Option<Inheritance<'arena>>,
    pub terminator: Terminator<'arena>,
}

impl HasSpan for Event<'_> {
    fn span(&self) -> Span {
        let start = match (self.attribute_lists.first(), self.modifiers.first()) {
            (Some(attribute_list), _) => attribute_list.span(),
            (None, Some(modifier)) => modifier.span(),
            (None, None) => self.event.span(),
        };

        Span::between(start, self.terminator.span())
    }
}
