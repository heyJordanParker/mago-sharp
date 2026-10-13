use strum::Display;

use mago_database::file::File;
use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::Sequence;
use crate::cst::cst::class_like::law::Law;
use crate::cst::cst::keyword::Keyword;

/// Represents a PHP# module, spec section 32: the `module` of its folder's `Module.sharp`.
///
/// It has no name, because the folder names it. It states the laws that span its namespace's classes, spec section 28.
///
/// Example:
///
/// ```csharp
/// module
/// {
///     law receiptMatchesRefunds(int paid, int refunded) => Receipts.refundable(paid, refunded) == Refunds.remaining(paid, refunded);
/// }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Module<'arena> {
    pub module: Keyword<'arena>,
    pub left_brace: Span,
    pub members: Sequence<'arena, ModuleMember<'arena>>,
    pub right_brace: Span,
}

/// A member of a module.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum ModuleMember<'arena> {
    Law(Law<'arena>),
}

impl Module<'_> {
    /// Whether `file` is where a module is declared, spec section 32: its folder's `Module.sharp`.
    #[inline]
    #[must_use]
    pub fn is_declared_in(file: &File) -> bool {
        file.name.rsplit(|&byte| byte == b'/').next() == Some(b"Module.sharp".as_slice())
    }
}

impl HasSpan for Module<'_> {
    fn span(&self) -> Span {
        Span::between(self.module.span(), self.right_brace)
    }
}

impl HasSpan for ModuleMember<'_> {
    fn span(&self) -> Span {
        match self {
            ModuleMember::Law(law) => law.span(),
        }
    }
}
