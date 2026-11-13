use foldhash::HashSet;
use mago_allocator::prelude::*;
use mago_syntax::cst::Program;
use mago_syntax::walker::MutWalker;

use crate::ResolvedNames;
use crate::default_class;
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
        self.resolve_in(
            program,
            &NamespaceClasses::default(),
            &ChangingMembers::default(),
            &ExternNamespaces::default(),
        )
    }

    /// Resolves names within `program` as [`resolve`](Self::resolve) does, where a bare PHP# name of a class-like
    /// `Sharp` imports by default names the class-like of `namespace_classes` in the file's namespace instead, spec
    /// section 23, a call of one of the `changing_members` on a local writes it, and a PHP# name of a class in one of
    /// the `extern_namespaces` names the class of PHP's root namespace.
    #[must_use]
    pub fn resolve_in<'ast>(
        &self,
        program: &'ast Program<'arena>,
        namespace_classes: &NamespaceClasses,
        changing_members: &ChangingMembers,
        extern_namespaces: &ExternNamespaces,
    ) -> ResolvedNames<'arena> {
        let mut context = NameResolutionContext::new(self.arena);
        let mut walker = NameWalker::new(program, namespace_classes, changing_members, extern_namespaces);

        walker.walk_program(program, &mut context);

        walker.resolved_names
    }
}

/// The class-likes of a project whose short name `Sharp` imports by default.
///
/// Spec section 23: a class-like in a file's own namespace wins over the default one, as a class of the same package
/// does in Kotlin, C# and Java. With `App\Math` among them, a bare `Math` in the namespace `App` is `App\Math`, and in
/// any other namespace it stays `Sharp\Math`.
///
/// Each is named by the engine's rule, spec section 24: a PHP# class-like only as written, so `App\MATH` wins over no
/// `Math`, and a plain PHP one in any case, as PHP compares class names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NamespaceClasses {
    sharp: HashSet<std::vec::Vec<u8>>,
    plain: HashSet<std::vec::Vec<u8>>,
}

impl NamespaceClasses {
    /// The class-likes among `names`, each the full name of a project's class-like and whether it is PHP#, whose short
    /// name `Sharp` imports by default.
    pub fn new<N>(names: impl IntoIterator<Item = (N, bool)>) -> Self
    where
        N: AsRef<[u8]>,
    {
        let mut classes = Self::default();
        for (name, sharp) in names {
            let name = trim_start_byte(name.as_ref(), b'\\');
            let short = name.rsplit(|byte| *byte == b'\\').next().unwrap_or(name);
            if sharp && default_class(short).is_some() {
                classes.sharp.insert(name.to_vec());
            } else if !sharp && shadows_a_default_class(short) {
                classes.plain.insert(name.to_ascii_lowercase());
            }
        }

        classes
    }

    /// Whether `name`, a full name, is one of these class-likes.
    #[must_use]
    pub fn contains(&self, name: &[u8]) -> bool {
        let name = trim_start_byte(name, b'\\');

        self.sharp.contains(name) || self.plain.contains(&name.to_ascii_lowercase())
    }
}

/// The namespaces a project gives libraries whose classes have none, spec section 23, as
/// `extern namespace WordPress for WP_Post;` gives `WordPress`.
///
/// A class named in one, as `WordPress\WP_Post`, is the class of PHP's root namespace, `WP_Post`, which the code runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternNamespaces(HashSet<std::vec::Vec<u8>>);

impl ExternNamespaces {
    /// The namespaces `names`, full names as `WordPress`. PHP compares namespaces ignoring case, and so does
    /// [`root_class`](Self::root_class).
    pub fn new<N>(names: impl IntoIterator<Item = N>) -> Self
    where
        N: AsRef<[u8]>,
    {
        Self(names.into_iter().map(|name| trim_start_byte(name.as_ref(), b'\\').to_ascii_lowercase()).collect())
    }

    /// Whether the project gives no library a namespace.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The class of PHP's root namespace the full name `name` names, when it is a class directly in one of these
    /// namespaces, as `WP_Post` for `WordPress\WP_Post`.
    #[must_use]
    pub fn root_class<'name>(&self, name: &'name [u8]) -> Option<&'name [u8]> {
        let name = trim_start_byte(name, b'\\');
        let separator = name.iter().rposition(|byte| *byte == b'\\')?;

        self.0.contains(&name[..separator].to_ascii_lowercase()).then(|| &name[separator + 1..])
    }
}

/// The names of the members of a project that change the value they are called on.
///
/// They are the standard library's members that take their receiver by `ref`, spec section 26, as a `List`'s `add`,
/// which the project's scan finds. The binder counts a call of one on a local as a write to the local.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangingMembers(HashSet<std::vec::Vec<u8>>);

impl ChangingMembers {
    /// The members named `names`. PHP compares method names ignoring case, and so does [`contains`](Self::contains).
    pub fn new<N>(names: impl IntoIterator<Item = N>) -> Self
    where
        N: AsRef<[u8]>,
    {
        Self(names.into_iter().map(|name| name.as_ref().to_ascii_lowercase()).collect())
    }

    /// Whether `name` is one of these members.
    #[must_use]
    pub fn contains(&self, name: &[u8]) -> bool {
        self.0.contains(&name.to_ascii_lowercase())
    }
}
