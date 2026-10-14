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
    /// The matched list's elements, each tested against a pattern, and the rest, as in `[Line first, ...List<Line> rest]`.
    List(ListPattern<'arena>),
}

/// Represents a type pattern: a type, its fields, its properties and the name it assigns the matched value to.
///
/// The fields are a case's that carries data, by their position. A bare name without a variable is a type, never a
/// local's value: a pattern takes types and constants only.
///
/// Example: `int`, `Paid paid`, `Paid { amount: > 1000 } big`, `PaymentResult.Paid(string transactionId)`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TypePattern<'arena> {
    pub hint: Hint<'arena>,
    pub fields: Option<&'arena FieldsPattern<'arena>>,
    pub properties: Option<&'arena PropertiesPattern<'arena>>,
    pub variable: Option<LocalIdentifier<'arena>>,
}

/// Represents the fields of a case that carries data, spec section 20, in the order the case declares them, each with
/// the pattern its value matches.
///
/// Example: `(string transactionId)`, `(> 1000, _)`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct FieldsPattern<'arena> {
    pub left_parenthesis: Span,
    pub fields: TokenSeparatedSequence<'arena, &'arena Pattern<'arena>>,
    pub right_parenthesis: Span,
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

/// Represents a list pattern, which matches a list of exactly its elements, or of at least them when a rest ends it.
///
/// Example: `[]`, `[Line only]`, `[Line first, ...List<Line> rest]`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ListPattern<'arena> {
    pub left_bracket: Span,
    pub elements: TokenSeparatedSequence<'arena, ListPatternElement<'arena>>,
    pub right_bracket: Span,
}

/// Represents one element of a list pattern: a pattern the element at its place matches, or the rest.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum ListPatternElement<'arena> {
    Pattern(&'arena Pattern<'arena>),
    Rest(RestPattern<'arena>),
}

/// Represents the rest of a list pattern: the elements the others leave, at its place among them.
///
/// A named rest holds them in a list of its type, and a bare one skips them. The parser gives a rest both a type and a
/// name, or neither.
///
/// Example: `...List<Line> rest`, `...`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct RestPattern<'arena> {
    pub ellipsis: Span,
    pub hint: Option<&'arena Hint<'arena>>,
    pub variable: Option<LocalIdentifier<'arena>>,
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
            Pattern::List(pattern) => pattern.span(),
        }
    }
}

impl HasSpan for ListPattern<'_> {
    fn span(&self) -> Span {
        self.left_bracket.join(self.right_bracket)
    }
}

impl HasSpan for ListPatternElement<'_> {
    fn span(&self) -> Span {
        match self {
            ListPatternElement::Pattern(pattern) => pattern.span(),
            ListPatternElement::Rest(rest) => rest.span(),
        }
    }
}

impl HasSpan for RestPattern<'_> {
    fn span(&self) -> Span {
        self.variable.map_or(self.ellipsis, |variable| self.ellipsis.join(variable.span))
    }
}

impl HasSpan for TypePattern<'_> {
    fn span(&self) -> Span {
        match (&self.fields, &self.properties, &self.variable) {
            (_, _, Some(variable)) => self.hint.span().join(variable.span),
            (_, Some(properties), None) => self.hint.span().join(properties.span()),
            (Some(fields), None, None) => self.hint.span().join(fields.span()),
            (None, None, None) => self.hint.span(),
        }
    }
}

impl HasSpan for FieldsPattern<'_> {
    fn span(&self) -> Span {
        self.left_parenthesis.join(self.right_parenthesis)
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
