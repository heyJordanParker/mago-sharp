use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::argument::ArgumentList;
use crate::cst::cst::class_like::constructor::ConstructorName;
use crate::cst::cst::expression::Expression;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::type_hint::TypeArgumentList;

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Instantiation<'arena> {
    pub new: Keyword<'arena>,
    pub class: &'arena Expression<'arena>,
    /// PHP#'s type arguments, as in `new PaginatedList<Order>(rows)`. Always `None` in PHP.
    pub type_arguments: Option<TypeArgumentList<'arena>>,
    /// The PHP# named constructor `new` calls, as the `.fromJson` of `new Artifact.fromJson(json)`. PHP never sets it.
    pub constructor: Option<ConstructorName<'arena>>,
    pub argument_list: Option<ArgumentList<'arena>>,
}

/// A PHP# class with its type arguments, read as an expression: the `Counter<Order>` of `Counter<Order>.count`, spec
/// section 11. PHP never builds one.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct GenericClass<'arena> {
    pub class: &'arena Expression<'arena>,
    pub type_arguments: TypeArgumentList<'arena>,
}

impl HasSpan for GenericClass<'_> {
    fn span(&self) -> Span {
        self.class.span().join(self.type_arguments.span())
    }
}

impl HasSpan for Instantiation<'_> {
    fn span(&self) -> Span {
        if let Some(argument_list) = &self.argument_list {
            self.new.span().join(argument_list.span())
        } else if let Some(constructor) = &self.constructor {
            self.new.span().join(constructor.span())
        } else if let Some(type_arguments) = &self.type_arguments {
            self.new.span().join(type_arguments.span())
        } else {
            self.new.span().join(self.class.span())
        }
    }
}
