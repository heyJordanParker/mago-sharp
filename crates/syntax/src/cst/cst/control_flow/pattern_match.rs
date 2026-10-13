use strum::Display;

use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::block::Block;
use crate::cst::cst::expression::Expression;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::pattern::Pattern;
use crate::cst::sequence::TokenSeparatedSequence;

/// Represents a PHP# `match`, which tests its value against each arm's pattern from top to bottom.
///
/// A `match` in an expression gives the value of the arm that matches. A `match` that starts a statement runs the
/// arm that matches, and its arms may be blocks.
///
/// Example: `match (status) { 200 => "ok", < 500 => "client", default => "server" }`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PatternMatch<'arena> {
    pub r#match: Keyword<'arena>,
    pub left_parenthesis: Span,
    pub expression: &'arena Expression<'arena>,
    pub right_parenthesis: Span,
    pub left_brace: Span,
    pub arms: TokenSeparatedSequence<'arena, PatternMatchArm<'arena>>,
    pub right_brace: Span,
}

/// Represents one arm of a PHP# `match`: a pattern with an optional `when` condition, or `default`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum PatternMatchArm<'arena> {
    Pattern(PatternMatchPatternArm<'arena>),
    Default(PatternMatchDefaultArm<'arena>),
}

/// Represents an arm that runs when the value matches its pattern and its `when` condition is `true`.
///
/// Example: `int n when n > 100 => "many"`
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PatternMatchPatternArm<'arena> {
    pub pattern: &'arena Pattern<'arena>,
    pub guard: Option<MatchGuard<'arena>>,
    pub arrow: Span,
    pub body: PatternMatchArmBody<'arena>,
}

/// Represents the arm that runs when no other arm matches.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PatternMatchDefaultArm<'arena> {
    pub default: Keyword<'arena>,
    pub arrow: Span,
    pub body: PatternMatchArmBody<'arena>,
}

/// Represents `when` and the condition an arm adds to its pattern.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MatchGuard<'arena> {
    pub when: Keyword<'arena>,
    pub condition: &'arena Expression<'arena>,
}

/// Represents what an arm runs: an expression, or a block in a `match` that starts a statement.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum PatternMatchArmBody<'arena> {
    Expression(&'arena Expression<'arena>),
    Block(Block<'arena>),
}

impl<'arena> PatternMatchArm<'arena> {
    #[inline]
    #[must_use]
    pub const fn is_default(&self) -> bool {
        matches!(self, PatternMatchArm::Default(_))
    }

    #[inline]
    #[must_use]
    pub const fn body(&self) -> &PatternMatchArmBody<'arena> {
        match self {
            PatternMatchArm::Pattern(arm) => &arm.body,
            PatternMatchArm::Default(arm) => &arm.body,
        }
    }
}

impl HasSpan for PatternMatch<'_> {
    fn span(&self) -> Span {
        self.r#match.span.join(self.right_brace)
    }
}

impl HasSpan for PatternMatchArm<'_> {
    fn span(&self) -> Span {
        match self {
            PatternMatchArm::Pattern(arm) => arm.span(),
            PatternMatchArm::Default(arm) => arm.span(),
        }
    }
}

impl HasSpan for PatternMatchPatternArm<'_> {
    fn span(&self) -> Span {
        self.pattern.span().join(self.body.span())
    }
}

impl HasSpan for PatternMatchDefaultArm<'_> {
    fn span(&self) -> Span {
        self.default.span.join(self.body.span())
    }
}

impl HasSpan for MatchGuard<'_> {
    fn span(&self) -> Span {
        self.when.span.join(self.condition.span())
    }
}

impl HasSpan for PatternMatchArmBody<'_> {
    fn span(&self) -> Span {
        match self {
            PatternMatchArmBody::Expression(expression) => expression.span(),
            PatternMatchArmBody::Block(block) => block.span(),
        }
    }
}
