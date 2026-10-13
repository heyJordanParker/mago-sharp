use mago_span::Span;
use mago_word::Word;
use mago_word::ascii_lowercase_word;

use crate::metadata::ttype::TypeMetadata;
use crate::ttype::TType;
use crate::ttype::union::TUnion;

/// A PHP# module, spec section 32: the services it binds for the classes of one namespace and the namespaces below it.
///
/// Modules order by their file's name, then by where the file declares them, so the order is the same whatever order
/// the files are scanned in.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct ModuleMetadata {
    /// The name of the file that declares it.
    pub file: Word,
    /// The declaration, from `module` to its closing brace.
    pub span: Span,
    /// The full name of the namespace whose classes it serves, in lowercase: its own namespace for a base module, and
    /// the namespace its header names for an environment or test module.
    pub namespace: Word,
    pub kind: ModuleKind,
    /// Its bindings, in the order it declares them.
    pub bindings: Vec<BindingMetadata>,
}

/// Which set of services a module belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ModuleKind {
    /// A folder's `Module.sharp`, which every environment starts from.
    Base,
    /// A `Module.<Environment>.sharp`, which `SHARP_ENV` picks. It holds the environment's name, as in `Production`.
    Environment(Word),
    /// A module declared in a test file, which overrides for the tests of that file.
    Test,
}

/// One service binding of a module, spec section 32.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct BindingMetadata {
    /// The binding, from its lifetime to its semicolon.
    pub span: Span,
    pub lifetime: Lifetime,
    /// The service it binds, as its type.
    pub service: TypeMetadata,
    /// The full name of the one class it serves, when it says `for`.
    pub target: Option<Word>,
    pub value: BindingValueMetadata,
}

/// How long the container keeps a service, spec section 32.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Lifetime {
    /// Built each time a class needs one.
    Transient,
    /// Kept for one scope, which the host starts and ends, such as one request, job or command.
    Scoped,
    /// Kept as long as the host runs the app.
    Singleton,
}

/// What a binding builds its service from.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BindingValueMetadata {
    /// A class, built through its main constructor: its full name, and where the binding writes it.
    Class(Word, Span),
    /// A list of classes, for a `List` service.
    Classes(Vec<(Word, Span)>),
    /// One expression over the binding's parameters. It is scanned as a lambda that takes the parameters and returns
    /// the service, and this is that lambda's name.
    Expression(Word),
}

/// The id the container knows the service of `type` by: its type in lowercase, as PHP compares class names.
///
/// A binding serves it, the analyzer resolves a parameter by it, and a compiled module's table and a parameter whose
/// PHP type drops its type arguments carry it to the engine.
#[must_use]
pub fn service_id(r#type: &TUnion) -> Word {
    ascii_lowercase_word(r#type.get_id().as_bytes())
}

impl Lifetime {
    /// The lifetime a binding's keyword names: `singleton`, `scoped` or `transient`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Lifetime::Transient => "transient",
            Lifetime::Scoped => "scoped",
            Lifetime::Singleton => "singleton",
        }
    }
}
