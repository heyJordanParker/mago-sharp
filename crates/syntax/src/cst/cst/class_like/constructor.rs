use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::argument::ArgumentList;
use crate::cst::cst::attribute::AttributeList;
use crate::cst::cst::class_like::method::MethodBody;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::modifier::Modifier;
use crate::cst::sequence::Sequence;

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

/// A PHP# named constructor, spec section 9.1.
///
/// It passes control to the main constructor with `: this(…)`, then runs its body on the new object. Dart calls it a
/// named constructor.
///
/// ```csharp
/// public Artifact.fromJson(string json) : this(Manifest.parse(json)) { }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct NamedConstructor<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    pub modifiers: Sequence<'arena, Modifier<'arena>>,
    /// The class's name, written before the `.`.
    pub class: LocalIdentifier<'arena>,
    pub dot: Span,
    pub name: LocalIdentifier<'arena>,
    pub parameter_list: FunctionLikeParameterList<'arena>,
    pub initializer: Option<ConstructorInitializer<'arena>>,
    pub body: MethodBody<'arena>,
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

impl HasSpan for NamedConstructor<'_> {
    fn span(&self) -> Span {
        let start = match (self.attribute_lists.first(), self.modifiers.first()) {
            (Some(attribute_list), _) => attribute_list.span(),
            (None, Some(modifier)) => modifier.span(),
            (None, None) => self.class.span(),
        };

        Span::between(start, self.body.span())
    }
}
