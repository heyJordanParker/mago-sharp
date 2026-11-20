use strum::Display;

use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::attribute::AttributeList;
use crate::cst::cst::class_like::member::ClassLikeMember;
use crate::cst::cst::expression::Expression;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::modifier::Modifier;
use crate::cst::cst::modifier::ModifierSequenceExt;
use crate::cst::cst::terminator::Terminator;
use crate::cst::sequence::Sequence;

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EnumCase<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    /// PHP#'s `final` before a machine's case, spec section 20.2: no move leaves the case. Always empty in PHP.
    pub modifiers: Sequence<'arena, Modifier<'arena>>,
    pub case: Keyword<'arena>,
    pub item: EnumCaseItem<'arena>,
    pub body: EnumCaseBody<'arena>,
}

/// What ends a case: its `;`, or PHP#'s block of a machine case's members, spec section 20.2.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum EnumCaseBody<'arena> {
    Terminator(Terminator<'arena>),
    Members(EnumCaseMembers<'arena>),
}

/// The members of a machine's case in braces, spec section 20.2: its moves, each a method that gives another case of
/// the machine, and any other member of the class the case runs as.
///
/// Example: `{ public OrderStatus.Paid pay(int cents) => OrderStatus.Paid(cents); }`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EnumCaseMembers<'arena> {
    pub left_brace: Span,
    pub members: Sequence<'arena, ClassLikeMember<'arena>>,
    pub right_brace: Span,
}

impl<'arena> EnumCase<'arena> {
    /// Whether the case is a machine's `final` case, spec section 20.2.
    #[must_use]
    pub fn is_final(&self) -> bool {
        self.modifiers.contains_final()
    }

    /// The members of a machine's case, spec section 20.2, and none for a case that ends at its `;`.
    #[must_use]
    pub fn members(&self) -> &[ClassLikeMember<'arena>] {
        match &self.body {
            EnumCaseBody::Terminator(_) => &[],
            EnumCaseBody::Members(members) => members.members.as_slice(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum EnumCaseItem<'arena> {
    Unit(EnumCaseUnitItem<'arena>),
    Backed(EnumCaseBackedItem<'arena>),
    /// A PHP# case that carries data, spec section 20, as in `case Paid(string transactionId);`.
    Data(EnumCaseDataItem<'arena>),
}

/// Represents a PHP# enum case that carries data: its name and the fields it holds, which make its constructor.
///
/// Example: `Paid(string transactionId)`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EnumCaseDataItem<'arena> {
    pub name: LocalIdentifier<'arena>,
    pub parameter_list: FunctionLikeParameterList<'arena>,
    /// The backing value of a machine's case that carries data, spec section 20.2, as `= "paid"` in
    /// `case Paid(int cents) = "paid"`. Always `None` in an enum.
    pub backing: Option<EnumCaseBacking<'arena>>,
}

/// The `=` and the value that back a machine's case that carries data, spec section 20.2.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EnumCaseBacking<'arena> {
    pub equals: Span,
    pub value: &'arena Expression<'arena>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EnumCaseUnitItem<'arena> {
    pub name: LocalIdentifier<'arena>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EnumCaseBackedItem<'arena> {
    pub name: LocalIdentifier<'arena>,
    pub equals: Span,
    pub value: &'arena Expression<'arena>,
}

impl<'arena> EnumCaseItem<'arena> {
    #[must_use]
    pub fn name(&self) -> &LocalIdentifier<'arena> {
        match &self {
            EnumCaseItem::Unit(enum_case_unit_item) => &enum_case_unit_item.name,
            EnumCaseItem::Backed(enum_case_backed_item) => &enum_case_backed_item.name,
            EnumCaseItem::Data(enum_case_data_item) => &enum_case_data_item.name,
        }
    }
}

impl HasSpan for EnumCase<'_> {
    fn span(&self) -> Span {
        let start = match (self.attribute_lists.first(), self.modifiers.first()) {
            (Some(attribute_list), _) => attribute_list.span(),
            (None, Some(modifier)) => modifier.span(),
            (None, None) => self.case.span(),
        };

        start.join(self.body.span())
    }
}

impl HasSpan for EnumCaseBody<'_> {
    fn span(&self) -> Span {
        match self {
            EnumCaseBody::Terminator(terminator) => terminator.span(),
            EnumCaseBody::Members(members) => members.span(),
        }
    }
}

impl HasSpan for EnumCaseMembers<'_> {
    fn span(&self) -> Span {
        Span::between(self.left_brace, self.right_brace)
    }
}

impl HasSpan for EnumCaseItem<'_> {
    fn span(&self) -> Span {
        match self {
            EnumCaseItem::Unit(item) => item.span(),
            EnumCaseItem::Backed(item) => item.span(),
            EnumCaseItem::Data(item) => item.span(),
        }
    }
}

impl HasSpan for EnumCaseDataItem<'_> {
    fn span(&self) -> Span {
        match &self.backing {
            Some(backing) => self.name.span().join(backing.value.span()),
            None => self.name.span().join(self.parameter_list.span()),
        }
    }
}

impl HasSpan for EnumCaseUnitItem<'_> {
    fn span(&self) -> Span {
        self.name.span()
    }
}

impl HasSpan for EnumCaseBackedItem<'_> {
    fn span(&self) -> Span {
        Span::between(self.name.span(), self.value.span())
    }
}
