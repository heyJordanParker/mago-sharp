use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::argument::ArgumentList;
use crate::cst::cst::keyword::Keyword;

/// How a PHP# constructor passes control before its body runs, spec section 9.1.
///
/// `: super(…)` calls the base class's constructor, and `: this(…)` the class's main constructor. Roslyn calls it
/// `ConstructorInitializerSyntax`.
///
/// ```csharp
/// public Order(Row row) : super(row) { }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ConstructorInitializer<'arena> {
    pub colon: Span,
    /// `super` or `this`.
    pub keyword: Keyword<'arena>,
    pub argument_list: ArgumentList<'arena>,
}

impl ConstructorInitializer<'_> {
    /// Whether it is `: super(…)`, which calls the base class's constructor.
    #[must_use]
    pub fn is_super(&self) -> bool {
        self.keyword.value == b"super"
    }
}

impl HasSpan for ConstructorInitializer<'_> {
    fn span(&self) -> Span {
        Span::between(self.colon, self.argument_list.span())
    }
}
