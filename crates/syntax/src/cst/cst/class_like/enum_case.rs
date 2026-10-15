use strum::Display;

use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::attribute::AttributeList;
use crate::cst::cst::expression::Expression;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::terminator::Terminator;
use crate::cst::sequence::Sequence;

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EnumCase<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    pub case: Keyword<'arena>,
    pub item: EnumCaseItem<'arena>,
    pub terminator: Terminator<'arena>,
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
        if let Some(attribute_list) = self.attribute_lists.first() {
            return attribute_list.span().join(self.terminator.span());
        }

        self.case.span().join(self.terminator.span())
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
        self.name.span().join(self.parameter_list.span())
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
