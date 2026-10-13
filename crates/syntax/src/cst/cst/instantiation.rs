use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::argument::ArgumentList;
use crate::cst::cst::class_like::constructor::ConstructorName;
use crate::cst::cst::expression::Expression;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::with::ObjectInitializer;

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Instantiation<'arena> {
    pub new: Keyword<'arena>,
    pub class: &'arena Expression<'arena>,
    /// The PHP# named constructor `new` calls, as the `.fromJson` of `new Artifact.fromJson(json)`. PHP never sets it.
    pub constructor: Option<ConstructorName<'arena>>,
    pub argument_list: Option<ArgumentList<'arena>>,
    /// The PHP# object initializer that sets properties after the constructor runs, as the `{ x: 1 }` of
    /// `new Point { x: 1 }`, spec section 16. PHP never sets it.
    pub initializer: Option<ObjectInitializer<'arena>>,
}

impl HasSpan for Instantiation<'_> {
    fn span(&self) -> Span {
        match (&self.initializer, &self.argument_list, &self.constructor) {
            (Some(initializer), _, _) => self.new.span().join(initializer.span()),
            (None, Some(argument_list), _) => self.new.span().join(argument_list.span()),
            (None, None, Some(constructor)) => self.new.span().join(constructor.span()),
            (None, None, None) => self.new.span().join(self.class.span()),
        }
    }
}
