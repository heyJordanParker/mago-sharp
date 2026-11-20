use mago_span::Span;
use mago_word::Word;

/// A PHP# machine, spec section 20.2: its cases and the moves between them, which the analyzer checks as a graph.
/// Each case is named as declared, as `Paid`, without its machine.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct MachineMetadata {
    /// Each case, in the order the machine declares them.
    pub cases: Vec<MachineCase>,
    /// Each static method of the machine that gives a case: a value of the machine starts as a case one gives.
    pub entry_points: Vec<MachineMove>,
    /// Each method that gives a case: one in a case's block moves from that case, and one in the machine's body from
    /// each case that isn't `final`.
    pub moves: Vec<MachineMove>,
}

/// A case of a PHP# machine, spec section 20.2.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct MachineCase {
    pub name: Word,
    /// The span of its name.
    pub span: Span,
    /// Whether it is written `final`: no move leaves it.
    pub is_final: bool,
}

/// An entry point or a move of a PHP# machine, spec section 20.2.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct MachineMove {
    pub name: Word,
    /// The span of its name.
    pub span: Span,
    /// The cases it moves from, none for an entry point.
    pub from: Vec<Word>,
    /// The cases it gives: each case its return type names, and every case where it names the machine.
    pub to: Vec<Word>,
}

impl MachineCase {
    #[must_use]
    pub fn new(name: Word, span: Span, is_final: bool) -> Self {
        Self { name, span, is_final }
    }
}

impl MachineMove {
    #[must_use]
    pub fn new(name: Word, span: Span, from: Vec<Word>, to: Vec<Word>) -> Self {
        Self { name, span, from, to }
    }
}
