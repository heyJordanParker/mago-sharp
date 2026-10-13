use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::argument::ArgumentList;
use crate::cst::cst::class_like::constructor::ConstructorName;
use crate::cst::cst::expression::Expression;
use crate::cst::cst::keyword::Keyword;

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Instantiation<'arena> {
    pub new: Keyword<'arena>,
    pub class: &'arena Expression<'arena>,
    /// The PHP# named constructor `new` calls, as the `.fromJson` of `new Artifact.fromJson(json)`. PHP never sets it.
    pub constructor: Option<ConstructorName<'arena>>,
    pub argument_list: Option<ArgumentList<'arena>>,
}

impl HasSpan for Instantiation<'_> {
    fn span(&self) -> Span {
        match (&self.argument_list, &self.constructor) {
            (Some(argument_list), _) => self.new.span().join(argument_list.span()),
            (None, Some(constructor)) => self.new.span().join(constructor.span()),
            (None, None) => self.new.span().join(self.class.span()),
        }
    }
}
