use mago_span::Span;
use mago_syntax::cst::Method;
use mago_word::Word;
use mago_word::concat_word;
use mago_word::word;

/// Returns the PHP variable a PHP# local, parameter or `this` runs as: `total` runs as `$total`.
///
/// A name already written with `$`, which is a PHP# error of its own, is kept as written.
#[inline]
#[must_use]
pub fn php_variable_name(name: &[u8]) -> Word {
    if name.starts_with(b"$") { word(name) } else { concat_word!(b"$", name) }
}

/// Returns the PHP method a method runs as. The PHP# constructor, the one method without `function` or a return type,
/// runs as `__construct`, and every other method keeps its name.
#[inline]
#[must_use]
pub fn php_method_name<'arena>(method: &Method<'arena>) -> &'arena [u8] {
    if method.function.is_none() && method.return_type_hint.is_none() { b"__construct" } else { method.name.value }
}

/// What a bare PHP# name refers to.
///
/// The parser reads `x`, `x.y` and `x.y()` the same way whatever `x` is. The binder then decides,
/// from the scopes of the file alone, what each bare name means.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum Binding {
    /// A parameter, or a local declared with `let` or `const`, whose block is still open.
    Local(Local),
    /// `this`, the object the method runs on.
    This,
    /// A class, written before `.`. Its fully qualified name is resolved like any class name.
    Class,
    /// A constant. Its fully qualified name is resolved like any constant name.
    Constant,
    /// A member of the enclosing class, written without `this.`.
    Member,
    /// `field` in an accessor body: the storage of the property the accessor belongs to.
    Field,
}

/// A PHP# scope rule a bare name breaks. The name still has its [`Binding`].
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum BindingError {
    /// A local used after the block that declares it closed. The name still binds as that local, so the scope
    /// error is the only one it causes.
    OutOfScope {
        /// The name where it is used.
        name: Span,
        /// The local, whose block has closed.
        local: Local,
    },
    /// A local declared with a name that an enclosing block of the same method already declares (C# CS0136).
    Redeclared {
        /// The name in the second declaration.
        name: Span,
        /// The declaration still open in an enclosing block.
        earlier: Local,
    },
}

/// A local of a PHP# method: where it is declared, and how.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Local {
    /// The name in the local's declaration.
    pub declaration: Span,
    pub kind: LocalKind,
}

/// How a PHP# local is declared.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum LocalKind {
    Parameter,
    /// Declared with `let`, so it can be reassigned.
    Let,
    /// Declared with `const`, so it cannot be reassigned.
    Const,
    /// Declared by a pattern, so it exists only where `test` is true, or false when `negated` (`x is not T name`).
    Pattern {
        /// The `is` that declares it, or the pattern of the `match` arm that declares it.
        test: Span,
        negated: bool,
    },
}
