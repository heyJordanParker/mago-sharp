use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::block::Block;
use crate::cst::cst::expression::Expression;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;

/// Represents a PHP# law, spec section 28: a claim about every value of its parameters, or of those that meet its
/// `when` condition. The checker proves a law alone, or by its body, and the law never runs.
///
/// Example:
///
/// ```csharp
/// law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;
/// law timesKeepsCurrency(Money m, int n) => m.times(n).currency == m.currency
/// {
///     if (n <= 0) {
///         return;
///     }
///     timesKeepsCurrency(m, n - 1);
///     addKeepsCurrency(m, m.times(n - 1));
/// }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Law<'arena> {
    pub law: Keyword<'arena>,
    pub name: LocalIdentifier<'arena>,
    pub parameter_list: FunctionLikeParameterList<'arena>,
    pub condition: Option<LawCondition<'arena>>,
    pub arrow: Span,
    pub claim: &'arena Expression<'arena>,
    pub proof: LawProof<'arena>,
}

/// Represents `when` and the condition a law's parameters meet, as in `when paid >= 0 && refunded >= 0`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct LawCondition<'arena> {
    pub when: Keyword<'arena>,
    pub expression: &'arena Expression<'arena>,
}

/// Represents what proves a law: the checker alone, after the `;` that ends its claim, or the body after its claim.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum LawProof<'arena> {
    Checker(Span),
    Body(Block<'arena>),
}

impl HasSpan for Law<'_> {
    fn span(&self) -> Span {
        Span::between(self.law.span(), self.proof.span())
    }
}

impl HasSpan for LawCondition<'_> {
    fn span(&self) -> Span {
        Span::between(self.when.span(), self.expression.span())
    }
}

impl HasSpan for LawProof<'_> {
    fn span(&self) -> Span {
        match self {
            LawProof::Checker(semicolon) => *semicolon,
            LawProof::Body(block) => block.span(),
        }
    }
}
