use std::collections::HashSet;

use foldhash::HashMap;
use mago_span::Span;

use mago_span::HasPosition;
use mago_span::Position;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Closure;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::PropertyHookList;
use mago_syntax::walker::MutWalker;
use mago_syntax::walker::walk_property_hook_list_mut;

use crate::binding::Binding;
use crate::binding::BindingError;
use crate::binding::Local;

pub mod binding;
pub mod kind;
pub mod resolver;
pub mod scope;

mod internal;

/// The methods of a PHP# `List` or `Map` that change it.
///
/// Spec section 12 runs them on the collection a local holds, so a call of one on a local writes the local, as
/// [`ResolvedNames::is_written`] reports. The binder knows no types, so a call of a method of these names on an object
/// writes its local too, which captures it by reference, as harmless.
pub const CHANGING_COLLECTION_METHODS: [&str; 3] = ["add", "set", "delete"];

/// Stores the results of a name resolution pass over a PHP program.
///
/// Maps the start byte offset of every identifier in the source to a tuple of
/// `(end offset, resolved fully qualified name, was-imported flag)`. Storing the end
/// offset alongside the start lets callers answer "what name is at this cursor offset?"
/// without re-scanning the source for identifier boundaries.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ResolvedNames<'arena> {
    /// Internal map: start offset -> (end offset, (resolved FQN, imported flag)).
    ///
    /// The inner pair is kept as a nested tuple (not flattened) so that [`all`](Self::all)
    /// can return `&(&'arena [u8], bool)` references — preserving the original signature
    /// for backward compatibility.
    names: HashMap<u32, (u32, (&'arena [u8], bool))>,

    /// Start offset of every bare PHP# name -> what it refers to. Empty for PHP.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "HashMap::is_empty"))]
    bindings: HashMap<u32, Binding>,

    /// The PHP# scope rules the bare names break, in source order. Empty for PHP.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty"))]
    binding_errors: Vec<BindingError>,

    /// Start offset of every PHP# lambda -> the names and locals declared outside it that it uses, in first-use order.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "HashMap::is_empty"))]
    captures: HashMap<u32, Vec<(&'arena [u8], Local)>>,

    /// Declaration start offset of every PHP# local that code writes after its declaration.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "foldhash::HashSet::is_empty"))]
    written_locals: foldhash::HashSet<u32>,
}

impl<'arena> ResolvedNames<'arena> {
    /// Returns the total number of resolved names stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Returns `true` if no resolved names are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Checks if a resolved name exists at the given source `Position`.
    #[must_use]
    pub fn contains(&self, position: &Position) -> bool {
        self.names.contains_key(&position.offset)
    }

    /// Gets the resolved name identifier for the given source position.
    ///
    /// # Panics
    ///
    /// Panics if no resolved name is found at the specified `position`.
    /// Use `contains` first if unsure.
    #[allow(clippy::expect_used)]
    pub fn get<T>(&self, position: &T) -> &'arena [u8]
    where
        T: HasPosition,
    {
        self.names.get(&position.offset()).map(|(_, (name, _))| *name).expect("resolved name not found at position")
    }

    /// Attempts to resolve the name at the given source position.
    ///
    /// Returns `Some(&str)` if a resolved name exists at the position, or `None` otherwise.
    pub fn resolve<T>(&self, position: &T) -> Option<&'arena [u8]>
    where
        T: HasPosition,
    {
        self.names.get(&position.offset()).map(|(_, (name, _))| *name)
    }

    /// Checks if the name resolved at the given position originated from an explicit
    /// `use` alias or construct.
    ///
    /// Returns `false` if the name was resolved relative to the namespace, is a
    /// definition, or if no name is found at the position.
    pub fn is_imported<T>(&self, position: &T) -> bool
    where
        T: HasPosition,
    {
        self.names.get(&position.offset()).is_some_and(|(_, (_, imported))| *imported)
    }

    /// Returns the resolved entry whose source range covers the given byte offset.
    ///
    /// Identifier ranges in `ResolvedNames` never overlap, so at most one entry can
    /// match. Returns `Some((start, end, fqn, imported))` for the covering entry, or
    /// `None` if the offset falls outside every tracked identifier.
    #[must_use]
    pub fn at_offset(&self, offset: u32) -> Option<(u32, u32, &'arena [u8], bool)> {
        self.names.iter().find_map(|(&start, &(end, (name, imported)))| {
            if start <= offset && offset < end { Some((start, end, name, imported)) } else { None }
        })
    }

    /// Iterates over every resolved entry as `(start, end, fqn, imported)`.
    pub fn iter(&self) -> impl Iterator<Item = (u32, u32, &'arena [u8], bool)> + '_ {
        self.names.iter().map(|(&start, &(end, (name, imported)))| (start, end, name, imported))
    }

    /// Byte ranges `(start, end)` of every entry whose resolved name matches
    /// `fqcn`, compared case-insensitively (PHP name resolution is). This
    /// matches aliased uses too: `use Bar as Qux; Qux\G` resolves to `Bar\G`, so
    /// searching for `Bar\G` finds it where a raw text scan would not.
    ///
    /// `exclude_offset` drops the entry starting at that offset, e.g. to omit a
    /// declaration from its own reference list; pass `None` to keep every match.
    #[must_use]
    pub fn references_to(&self, fqcn: &[u8], exclude_offset: Option<u32>) -> Vec<(u32, u32)> {
        self.iter()
            .filter(|(start, _, _, _)| exclude_offset.is_none_or(|offset| offset != *start))
            .filter_map(
                |(start, end, name, _)| if eq_ignore_ascii_case(name, fqcn) { Some((start, end)) } else { None },
            )
            .collect()
    }

    /// Returns what the bare PHP# name starting at the given position refers to.
    ///
    /// Returns `None` for every name in a PHP file.
    pub fn binding<T>(&self, position: &T) -> Option<Binding>
    where
        T: HasPosition,
    {
        self.bindings.get(&position.offset()).copied()
    }

    /// Returns the class of a PHP# static call, `Class.method()`: the object of `call` when the binder bound it to a
    /// class. Returns `None` for an instance call and for every call in a PHP file.
    ///
    /// The checker and the engine both read this, so they never disagree on a static call.
    #[must_use]
    pub fn static_call_class<'ast>(&self, call: &MethodCall<'ast>) -> Option<&'ast ConstantAccess<'ast>> {
        self.class_object(call.object)
    }

    /// Returns the class of a PHP# static property access, `Class.name`: the object of `access` when the binder bound
    /// it to a class. Returns `None` for an instance property and for every access in a PHP file.
    ///
    /// The checker, the analyzer and the engine all read this, so they never disagree on a static property.
    #[must_use]
    pub fn static_property_class<'ast>(&self, access: &PropertyAccess<'ast>) -> Option<&'ast ConstantAccess<'ast>> {
        self.class_object(access.object)
    }

    /// Returns whether an accessor body of a PHP# property names `field` outside a lambda, as php-src finds
    /// `$this->name` in a hook outside a closure, nested blocks included. A lambda is a function of its own, where PHP
    /// calls the accessor again. Returns `false` for every property in a PHP file.
    ///
    /// The checker, the analyzer and the engine all read this, so they never disagree on a backed property.
    #[must_use]
    pub fn uses_field(&self, accessors: &PropertyHookList<'_>) -> bool {
        let mut field_use = FieldUse { names: self, lambdas: 0, found: false };
        walk_property_hook_list_mut(&mut field_use, accessors, &mut ());

        field_use.found
    }

    /// Returns whether a PHP# property has storage: an auto accessor, `get;` or `set;`, or a body that
    /// [uses `field`](Self::uses_field). A property without storage runs as PHP's virtual property.
    ///
    /// The checker and the analyzer both read this, so they never disagree on a virtual property.
    #[must_use]
    pub fn has_storage(&self, accessors: &PropertyHookList<'_>) -> bool {
        accessors.hooks.iter().any(|accessor| matches!(accessor.body, PropertyHookBody::Abstract(_)))
            || self.uses_field(accessors)
    }

    fn class_object<'ast>(&self, object: &'ast Expression<'ast>) -> Option<&'ast ConstantAccess<'ast>> {
        match object {
            Expression::ConstantAccess(access) if self.binding(&access.name) == Some(Binding::Class) => Some(access),
            _ => None,
        }
    }

    /// Returns the PHP# scope rules the bare names break, in source order.
    #[must_use]
    pub fn binding_errors(&self) -> &[BindingError] {
        &self.binding_errors
    }

    /// Returns the name and local of each local declared outside the PHP# lambda starting at the given position that
    /// the lambda uses, in the order it first uses them. A lambda inside another captures what it uses for both.
    pub fn captures<T>(&self, lambda: &T) -> &[(&'arena [u8], Local)]
    where
        T: HasPosition,
    {
        self.captures.get(&lambda.offset()).map_or(&[], Vec::as_slice)
    }

    /// Returns whether code writes the PHP# local after its declaration: with `=`, a compound assignment, `++` or
    /// `--` on it or an index of it, or a call of one of the [`CHANGING_COLLECTION_METHODS`] on it.
    #[must_use]
    pub fn is_written(&self, local: &Local) -> bool {
        self.written_locals.contains(&local.declaration.start.offset)
    }

    pub(crate) fn bind(&mut self, span: Span, binding: Binding) {
        self.bindings.insert(span.start.offset, binding);
    }

    pub(crate) fn capture(&mut self, lambda: u32, name: &'arena [u8], local: Local) {
        let captures = self.captures.entry(lambda).or_default();
        if !captures.iter().any(|(_, captured)| *captured == local) {
            captures.push((name, local));
        }
    }

    pub(crate) fn write(&mut self, local: Local) {
        self.written_locals.insert(local.declaration.start.offset);
    }

    pub(crate) fn report_binding_error(&mut self, error: BindingError) {
        self.binding_errors.push(error);
    }

    /// Inserts a resolution result into the map (intended for internal use).
    ///
    /// The full source span of the identifier is stored, so [`at_offset`](Self::at_offset)
    /// and other range-based lookups work without re-scanning the source.
    pub(crate) fn insert_at(&mut self, span: Span, name: &'arena [u8], imported: bool) {
        self.names.insert(span.start.offset, (span.end.offset, (name, imported)));
    }

    /// Returns a `HashSet` containing every resolution result as `(&start, (fqn, imported))`.
    #[deprecated(
        note = "Allocates a HashSet for no good reason. Prefer `iter()` for allocation-free iteration, or `at_offset()` for cursor lookups."
    )]
    #[must_use]
    pub fn all(&self) -> HashSet<(&u32, &(&'arena [u8], bool))> {
        self.names.iter().map(|(k, (_, inner))| (k, inner)).collect()
    }
}

/// Finds `field` in accessor bodies outside the lambdas they hold, for [`ResolvedNames::uses_field`].
struct FieldUse<'names, 'arena> {
    names: &'names ResolvedNames<'arena>,
    /// How many lambdas hold the node being walked.
    lambdas: u32,
    found: bool,
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for FieldUse<'_, '_> {
    fn walk_in_arrow_function(&mut self, _arrow_function: &'ast ArrowFunction<'arena>, _context: &mut ()) {
        self.lambdas += 1;
    }

    fn walk_out_arrow_function(&mut self, _arrow_function: &'ast ArrowFunction<'arena>, _context: &mut ()) {
        self.lambdas -= 1;
    }

    fn walk_in_closure(&mut self, _closure: &'ast Closure<'arena>, _context: &mut ()) {
        self.lambdas += 1;
    }

    fn walk_out_closure(&mut self, _closure: &'ast Closure<'arena>, _context: &mut ()) {
        self.lambdas -= 1;
    }

    fn walk_in_constant_access(&mut self, constant_access: &'ast ConstantAccess<'arena>, _context: &mut ()) {
        self.found |= self.lambdas == 0 && self.names.binding(&constant_access.name) == Some(Binding::Field);
    }
}

/// Case-insensitive byte equality, routing equal-length inputs through
/// `mago_word`'s SIMD prefix comparison.
#[inline]
fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && mago_word::starts_with_ignore_case(a, b)
}
