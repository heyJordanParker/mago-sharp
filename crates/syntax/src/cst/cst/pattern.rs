use strum::Display;

use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::binary::BinaryOperator;
use crate::cst::cst::expression::Expression;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::type_hint::Hint;
use crate::cst::sequence::TokenSeparatedSequence;

/// Represents a PHP# test of a value against a pattern, which is `true` when the value matches.
///
/// Example: `entity is HasDesign`, `result is Paid paid` or `payload is not int orderId`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Is<'arena> {
    pub value: &'arena Expression<'arena>,
    pub is: Keyword<'arena>,
    pub pattern: &'arena Pattern<'arena>,
}

/// Represents a PHP# conversion of a value to a type, which gives null when the value is not of the type.
///
/// Example: `result as Paid`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct As<'arena> {
    pub value: &'arena Expression<'arena>,
    pub r#as: Keyword<'arena>,
    pub hint: &'arena Hint<'arena>,
}

/// Represents a PHP# pattern, which `is` and the arms of a `match` test a value against.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum Pattern<'arena> {
    /// A type, which may name the matched value, as in `int` or `Paid paid`.
    Type(TypePattern<'arena>),
    /// A value the matched value is identical to, as in `200`, `"draft"` or `null`.
    Value(&'arena Expression<'arena>),
    /// A comparison of the matched value with a value, as in `< 1000`.
    Comparison(ComparisonPattern<'arena>),
    /// A pattern that does not match, as in `not null`.
    Not(NotPattern<'arena>),
    /// Two patterns joined by `and` or `or`.
    Binary(BinaryPattern<'arena>),
    Parenthesized(ParenthesizedPattern<'arena>),
    /// The matched object's properties, each tested against a pattern, as in `{ status: 200, body: string body }`.
    Properties(PropertiesPattern<'arena>),
}

/// Represents a type pattern: a type, and the name of a variable the matched value is assigned to.
///
/// A bare name without a variable is a local's value when a local of that name is in scope, which the binder
/// decides.
///
/// Example: `int`, `Paid paid`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TypePattern<'arena> {
    pub hint: Hint<'arena>,
    pub variable: Option<LocalIdentifier<'arena>>,
}

/// Represents a comparison pattern: `==`, `<`, `<=`, `>` or `>=` and the value the matched value is compared with.
///
/// Example: `< 1000`, or `== LIMIT` for a constant, whose bare name alone is a type
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ComparisonPattern<'arena> {
    pub operator: BinaryOperator<'arena>,
    pub value: &'arena Expression<'arena>,
}

/// Represents `not` and the pattern it negates.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct NotPattern<'arena> {
    pub not: Keyword<'arena>,
    pub pattern: &'arena Pattern<'arena>,
}

/// Represents two patterns joined by `and`, which matches when both match, or `or`, which matches when either does.
///
/// Example: `>= 1000 and < 10000`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct BinaryPattern<'arena> {
    pub left: &'arena Pattern<'arena>,
    pub operator: Keyword<'arena>,
    pub right: &'arena Pattern<'arena>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ParenthesizedPattern<'arena> {
    pub left_parenthesis: Span,
    pub pattern: &'arena Pattern<'arena>,
    pub right_parenthesis: Span,
}

/// Represents a properties pattern, which matches an object whose properties each match their pattern.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PropertiesPattern<'arena> {
    pub left_brace: Span,
    pub properties: TokenSeparatedSequence<'arena, PropertyPattern<'arena>>,
    pub right_brace: Span,
}

/// Represents one property of a properties pattern and the pattern its value matches, as in `status: 200`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PropertyPattern<'arena> {
    pub name: LocalIdentifier<'arena>,
    pub colon: Span,
    pub pattern: &'arena Pattern<'arena>,
}

impl BinaryPattern<'_> {
    /// Returns `true` for `and`, and `false` for `or`.
    #[inline]
    #[must_use]
    pub fn is_and(&self) -> bool {
        self.operator.value == b"and"
    }
}

impl HasSpan for Is<'_> {
    fn span(&self) -> Span {
        self.value.span().join(self.pattern.span())
    }
}

impl HasSpan for As<'_> {
    fn span(&self) -> Span {
        self.value.span().join(self.hint.span())
    }
}

impl HasSpan for Pattern<'_> {
    fn span(&self) -> Span {
        match self {
            Pattern::Type(pattern) => pattern.span(),
            Pattern::Value(value) => value.span(),
            Pattern::Comparison(pattern) => pattern.span(),
            Pattern::Not(pattern) => pattern.span(),
            Pattern::Binary(pattern) => pattern.span(),
            Pattern::Parenthesized(pattern) => pattern.span(),
            Pattern::Properties(pattern) => pattern.span(),
        }
    }
}

impl HasSpan for TypePattern<'_> {
    fn span(&self) -> Span {
        match &self.variable {
            Some(variable) => self.hint.span().join(variable.span),
            None => self.hint.span(),
        }
    }
}

impl HasSpan for ComparisonPattern<'_> {
    fn span(&self) -> Span {
        self.operator.span().join(self.value.span())
    }
}

impl HasSpan for NotPattern<'_> {
    fn span(&self) -> Span {
        self.not.span.join(self.pattern.span())
    }
}

impl HasSpan for BinaryPattern<'_> {
    fn span(&self) -> Span {
        self.left.span().join(self.right.span())
    }
}

impl HasSpan for ParenthesizedPattern<'_> {
    fn span(&self) -> Span {
        self.left_parenthesis.join(self.right_parenthesis)
    }
}

impl HasSpan for PropertiesPattern<'_> {
    fn span(&self) -> Span {
        self.left_brace.join(self.right_brace)
    }
}

impl HasSpan for PropertyPattern<'_> {
    fn span(&self) -> Span {
        self.name.span.join(self.pattern.span())
    }
}
