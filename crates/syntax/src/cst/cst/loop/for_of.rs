use strum::Display;

use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::expression::Expression;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::statement::Statement;
use crate::cst::cst::type_hint::Hint;

/// Represents a PHP# `for … of` loop over a collection, whose loop variable `let` or `const` declares.
///
/// Example: `for (const line of lines) { … }` or `for (const [key, plan] of plans) { … }`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ForOf<'arena> {
    pub r#for: Keyword<'arena>,
    pub left_parenthesis: Span,
    pub keyword: Keyword<'arena>,
    pub target: ForOfTarget<'arena>,
    pub of: Keyword<'arena>,
    pub expression: &'arena Expression<'arena>,
    pub right_parenthesis: Span,
    pub body: &'arena Statement<'arena>,
}

/// Represents the loop variables of a PHP# `for … of` loop.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum ForOfTarget<'arena> {
    /// Each value, as in `const line`.
    Value(ForOfVariable<'arena>),
    /// Each key and value, as in `const [key, plan]`.
    KeyValue(ForOfKeyValueTarget<'arena>),
}

/// Represents the key and value of a PHP# `for … of` loop, as in `[key, plan]`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ForOfKeyValueTarget<'arena> {
    pub left_bracket: Span,
    pub key: ForOfVariable<'arena>,
    pub comma: Span,
    pub value: ForOfVariable<'arena>,
    pub right_bracket: Span,
}

/// Represents one loop variable of a PHP# `for … of` loop and its type when it is written, as in `Status status`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ForOfVariable<'arena> {
    pub hint: Option<&'arena Hint<'arena>>,
    pub name: LocalIdentifier<'arena>,
}

impl ForOf<'_> {
    /// Returns `true` when the loop variables are declared with `const`.
    #[inline]
    #[must_use]
    pub fn is_const(&self) -> bool {
        self.keyword.value.eq_ignore_ascii_case(b"const")
    }
}

impl<'arena> ForOfTarget<'arena> {
    /// The variables the loop declares, the key first.
    #[must_use]
    pub fn variables(&self) -> Vec<&ForOfVariable<'arena>> {
        match self {
            ForOfTarget::Value(value) => vec![value],
            ForOfTarget::KeyValue(key_value) => vec![&key_value.key, &key_value.value],
        }
    }

    /// The names the loop declares, the key first.
    #[must_use]
    pub fn names(&self) -> Vec<&LocalIdentifier<'arena>> {
        self.variables().into_iter().map(|variable| &variable.name).collect()
    }
}

impl HasSpan for ForOf<'_> {
    fn span(&self) -> Span {
        self.r#for.span().join(self.body.span())
    }
}

impl HasSpan for ForOfTarget<'_> {
    fn span(&self) -> Span {
        match self {
            ForOfTarget::Value(value) => value.span(),
            ForOfTarget::KeyValue(key_value) => key_value.span(),
        }
    }
}

impl HasSpan for ForOfKeyValueTarget<'_> {
    fn span(&self) -> Span {
        self.left_bracket.join(self.right_bracket)
    }
}

impl HasSpan for ForOfVariable<'_> {
    fn span(&self) -> Span {
        self.hint.map_or(self.name.span, |hint| hint.span().join(self.name.span))
    }
}
