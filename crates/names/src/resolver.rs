use foldhash::HashSet;
use mago_allocator::prelude::*;
use mago_syntax::cst::Program;
use mago_syntax::walker::MutWalker;

use crate::ResolvedNames;
use crate::internal::context::NameResolutionContext;
use crate::internal::walker::NameWalker;
use crate::scope::trim_start_byte;
use crate::shadows_a_default_class;

/// Orchestrates the process of resolving names within a PHP Concrete Syntax Tree (CST).
///
/// This struct acts as the main entry point for the name resolution pass.
/// It requires an arena to store resolved names.
#[derive(Debug, Clone)]
#[repr(transparent)]
pub struct NameResolver<'arena, A>
where
    A: Arena,
{
    arena: &'arena A,
}

impl<'arena, A> NameResolver<'arena, A>
where
    A: Arena,
{
    /// Creates a new `NameResolver` instance.
    pub fn new(arena: &'arena A) -> Self {
        NameResolver { arena }
    }

    /// Resolves names within the provided PHP CST `Program`.
    ///
    /// # Arguments
    ///
    /// * `program` - A reference to the root `Program` CST node. The lifetime `'ast`
    ///   ensures the CST outlives the borrowing done within this method.
    ///
    /// # Returns
    ///
    /// A `ResolvedNames` struct containing the mapping of original names/nodes
    /// to their resolved fully qualified names.
    #[must_use]
    pub fn resolve<'ast>(&self, program: &'ast Program<'arena>) -> ResolvedNames<'arena> {
        self.resolve_in(program, &NamespaceClasses::default())
    }

    /// Resolves names within `program` as [`resolve`](Self::resolve) does, where a bare PHP# name of a class-like
    /// `Sharp` imports by default names the class-like of `namespace_classes` in the file's namespace instead, spec
    /// section 23.
    #[must_use]
    pub fn resolve_in<'ast>(
        &self,
        program: &'ast Program<'arena>,
        namespace_classes: &NamespaceClasses,
    ) -> ResolvedNames<'arena> {
        let mut context = NameResolutionContext::new(self.arena);
        let mut walker = NameWalker::new(program, namespace_classes);

        walker.walk_program(program, &mut context);

        walker.resolved_names
    }
}

/// The class-likes of a project whose short name `Sharp` imports by default.
///
/// Spec section 23: a class-like in a file's own namespace wins over the default one, as a class of the same package
/// does in Kotlin, C# and Java. With `App\Cache` among them, a bare `Cache` in the namespace `App` is `App\Cache`, and in
/// any other namespace it stays `Sharp\Cache`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NamespaceClasses(HashSet<std::vec::Vec<u8>>);

impl NamespaceClasses {
    /// The class-likes among `names`, the full names of a project's class-likes, whose short name `Sharp` imports by
    /// default. PHP compares class names ignoring case, and so does [`contains`](Self::contains).
    pub fn new<N>(names: impl IntoIterator<Item = N>) -> Self
    where
        N: AsRef<[u8]>,
    {
        Self(
            names
                .into_iter()
                .map(|name| trim_start_byte(name.as_ref(), b'\\').to_ascii_lowercase())
                .filter(|name| shadows_a_default_class(name.rsplit(|byte| *byte == b'\\').next().unwrap_or(name)))
                .collect(),
        )
    }

    /// Whether `name`, a full name, is one of these class-likes.
    #[must_use]
    pub fn contains(&self, name: &[u8]) -> bool {
        self.0.contains(&trim_start_byte(name, b'\\').to_ascii_lowercase())
    }
}
