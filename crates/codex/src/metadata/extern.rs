use std::cmp::Ordering;

use mago_span::Span;
use mago_word::Word;

/// A PHP# `extern` declaration, which states the effects of one plain PHP class, method or function, spec section 29.
///
/// The standard library's declarations order first, then the others by their file's name and where the file declares
/// them, so the first declaration of a target is the same whatever order the files are scanned in, and a project's
/// declaration of what the library declares is always the second.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct ExternMetadata {
    /// The name of the file that declares it.
    pub file: Word,
    /// Whether the standard library's file declares it.
    pub is_standard_library: bool,
    /// The declaration, from `extern` to its semicolon.
    pub span: Span,
    /// What it declares, keyed as `CodebaseMetadata::function_likes` keys its entries, in lowercase: `(class, "")` for
    /// a class, `(class, method)` for a method, and `("", function)` for a function.
    pub target: (Word, Word),
    /// The fully qualified class names of the effects its `uses` names. Empty when it declares its target pure.
    pub effects: Vec<Word>,
    /// Whether it is an `extern event`, which marks its plain PHP class as an event, spec section 15.
    pub event: bool,
}

impl Ord for ExternMetadata {
    fn cmp(&self, other: &Self) -> Ordering {
        let key = |declaration: &Self| {
            (!declaration.is_standard_library, declaration.file, declaration.span, declaration.target)
        };

        key(self)
            .cmp(&key(other))
            .then_with(|| self.effects.cmp(&other.effects))
            .then_with(|| self.event.cmp(&other.event))
    }
}

impl PartialOrd for ExternMetadata {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
