use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::cst::sequence::TokenSeparatedSequenceExt;

/// Represents a PHP# `uses` clause, the comma-separated effects an `extern` declaration or a function type names.
///
/// Example:
///
/// ```csharp
/// extern Mailer uses Http, Mail;
/// Function<Charge(Cart) uses Http, Mail>
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Uses<'arena> {
    pub uses: Keyword<'arena>,
    pub names: TokenSeparatedSequence<'arena, LocalIdentifier<'arena>>,
}

impl HasSpan for Uses<'_> {
    fn span(&self) -> Span {
        let keyword = self.uses.span();

        keyword.join(self.names.span(keyword.file_id, keyword.end))
    }
}
