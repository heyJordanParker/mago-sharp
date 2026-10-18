use strum::Display;

use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::expression::Expression;
use crate::cst::cst::goto::Label;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::statement::Statement;
use crate::cst::cst::terminator::Terminator;
use crate::cst::sequence::Sequence;

/// Represents a while statement in PHP.
///
/// Example:
///
/// ```php
/// <?php
///
/// $i = 0;
/// while ($i < 10) {
///   echo $i;
///   $i++;
/// }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct While<'arena> {
    /// The PHP# name the loop is written with, as `polling:` in `polling: while (running)`, which `break polling;`
    /// and `continue polling;` name. Always `None` in PHP.
    pub label: Option<Label<'arena>>,
    pub r#while: Keyword<'arena>,
    pub left_parenthesis: Span,
    pub condition: &'arena Expression<'arena>,
    pub right_parenthesis: Span,
    pub body: WhileBody<'arena>,
}

/// Represents the body of a while statement.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum WhileBody<'arena> {
    Statement(&'arena Statement<'arena>),
    ColonDelimited(WhileColonDelimitedBody<'arena>),
}

/// Represents a colon-delimited body of a while statement.
///
/// Example:
///
/// ```php
/// <?php
///
/// $i = 0;
/// while ($i < 10):
///   echo $i;
///   $i++;
/// endwhile;
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct WhileColonDelimitedBody<'arena> {
    pub colon: Span,
    pub statements: Sequence<'arena, Statement<'arena>>,
    pub end_while: Keyword<'arena>,
    pub terminator: Terminator<'arena>,
}

impl<'arena> WhileBody<'arena> {
    #[inline]
    #[must_use]
    pub fn statements(&self) -> &[Statement<'arena>] {
        match self {
            WhileBody::Statement(statement) => std::slice::from_ref(statement),
            WhileBody::ColonDelimited(body) => body.statements.as_slice(),
        }
    }
}

impl HasSpan for While<'_> {
    fn span(&self) -> Span {
        self.label.map_or_else(|| self.r#while.span(), |label| label.span()).join(self.body.span())
    }
}

impl HasSpan for WhileBody<'_> {
    fn span(&self) -> Span {
        match self {
            WhileBody::Statement(statement) => statement.span(),
            WhileBody::ColonDelimited(body) => body.span(),
        }
    }
}

impl HasSpan for WhileColonDelimitedBody<'_> {
    fn span(&self) -> Span {
        self.colon.join(self.terminator.span())
    }
}
