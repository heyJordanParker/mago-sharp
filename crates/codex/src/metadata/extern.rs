use mago_span::Span;
use mago_word::Word;

/// A PHP# `extern` declaration, which states the effects of one plain PHP class, method or function, spec section 29.
///
/// Declarations order by their file's name, then by where the file declares them, so the first declaration of a
/// target is the same whatever order the files are scanned in.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct ExternMetadata {
    /// The name of the file that declares it.
    pub file: Word,
    /// The declaration, from `extern` to its semicolon.
    pub span: Span,
    /// What it declares, keyed as `CodebaseMetadata::function_likes` keys its entries, in lowercase: `(class, "")` for
    /// a class, `(class, method)` for a method, and `("", function)` for a function.
    pub target: (Word, Word),
    /// The fully qualified class names of the effects its `uses` names. Empty when it declares its target pure.
    pub effects: Vec<Word>,
}
