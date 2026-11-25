use foldhash::HashMap;
use foldhash::HashSet;
use mago_word::ascii_lowercase_word;
use mago_word::empty_word;

use mago_word::Word;
use mago_word::WordSet;

use crate::context::ScopeContext;
use crate::diff::CodebaseDiff;
use crate::identifier::function_like::FunctionLikeIdentifier;
use crate::identifier::method::MethodIdentifier;
use crate::metadata::CodebaseMetadata;
use crate::symbol::SymbolIdentifier;

/// Represents the source of a reference, distinguishing between top-level symbols
/// and members within a class-like structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ReferenceSource {
    /// A reference from a top-level symbol (function, class, enum, trait, interface, constant).
    /// The bool indicates if the reference occurs within a signature context (true) or body (false).
    /// The Word is the name (FQCN or FQN) of the referencing symbol.
    Symbol(bool, Word),
    /// A reference from a member within a class-like structure (method, property, class constant, enum case).
    /// The bool indicates if the reference occurs within a signature context (true) or body (false).
    /// The first Word is the FQCN of the class-like structure.
    /// The second Word is the name of the member.
    ClassLikeMember(bool, Word, Word),
    /// A reference from top-level code in a source file.
    /// The bool indicates if the reference occurs within a signature context.
    File(bool, Word),
}

/// Identifies where a recorded symbol reference originates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReferenceOrigin {
    /// A top-level symbol or class-like member.
    Symbol(SymbolIdentifier),
    /// Top-level code in a source file.
    File(Word),
}

/// An edge the invalidation cascade follows. A change at the edge's target can invalidate its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CascadeEdge {
    /// The signature of `from` references `to`: a change to `to`'s signature changes `from`'s signature.
    Signature { from: SymbolIdentifier, to: SymbolIdentifier },
    /// The body of `from` references `to`.
    Body { from: SymbolIdentifier, to: SymbolIdentifier },
    /// An attribute `from` carries references `to`, its class or its constructor, in `from`'s signature or body. The
    /// cascade reads it as the signature or the body edge it stands in for. The attribute's constructor never runs as
    /// part of `from`, so a walk of what code runs skips it.
    Attribute { from: SymbolIdentifier, to: SymbolIdentifier, signature: bool },
    /// The top-level code of `file` references `to`.
    File { file: Word, to: SymbolIdentifier },
    /// `class` directly extends, implements or uses `parent`.
    Inherits { class: Word, parent: Word },
}

/// What a recorded reference starts from: a symbol or member, which belongs to a class-like or to none, or a file.
pub trait Referrer: Copy + Eq + std::hash::Hash {
    /// The class-like a member belongs to, or the symbol itself, for a symbol or member, and none for a file.
    fn class(self) -> Option<Word>;
}

impl Referrer for SymbolIdentifier {
    fn class(self) -> Option<Word> {
        Some(self.0)
    }
}

impl Referrer for Word {
    fn class(self) -> Option<Word> {
        None
    }
}

/// The references each symbol, member or file makes, with the sources of each target and of each class-like beside them.
///
/// The invalidation cascade walks back from a change through the edges that reach it alone, in time that grows with
/// what it invalidates, never with the whole graph. Every change goes through these methods, which keep the three in
/// step. Only the references themselves are serialized: the indexes are made again from them on load.
#[derive(Debug, Clone)]
pub struct References<K: Referrer> {
    /// Each source's targets.
    to: HashMap<K, HashSet<SymbolIdentifier>>,
    /// Each target's sources.
    referrers: HashMap<SymbolIdentifier, HashSet<K>>,
    /// The sources that are a class-like or one of its members, by the class-like.
    by_class: HashMap<Word, HashSet<K>>,
}

impl<K: Referrer> Default for References<K> {
    fn default() -> Self {
        Self { to: HashMap::default(), referrers: HashMap::default(), by_class: HashMap::default() }
    }
}

impl<K: Referrer> PartialEq for References<K> {
    fn eq(&self, other: &Self) -> bool {
        self.to == other.to
    }
}

impl<K: Referrer> Eq for References<K> {}

#[cfg(feature = "serde")]
impl<K> serde::Serialize for References<K>
where
    K: Referrer + serde::Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to.serialize(serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de, K> serde::Deserialize<'de> for References<K>
where
    K: Referrer + serde::Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut references = Self::default();
        for (source, targets) in HashMap::<K, HashSet<SymbolIdentifier>>::deserialize(deserializer)? {
            references.extend_from(source, targets);
        }

        Ok(references)
    }
}

impl<K: Referrer> References<K> {
    /// Records that `source` references `target`, and whether it was new.
    pub fn insert(&mut self, source: K, target: SymbolIdentifier) -> bool {
        if !self.to.entry(source).or_default().insert(target) {
            return false;
        }
        self.referrers.entry(target).or_default().insert(source);
        if let Some(class) = source.class() {
            self.by_class.entry(class).or_default().insert(source);
        }

        true
    }

    /// Records that `source` references each of `targets`.
    pub fn extend_from(&mut self, source: K, targets: impl IntoIterator<Item = SymbolIdentifier>) {
        for target in targets {
            self.insert(source, target);
        }
    }

    /// Records that `source` references each of `targets`, as a set of them was recorded.
    pub fn insert_set(&mut self, source: K, targets: HashSet<SymbolIdentifier>) {
        self.remove(&source);
        self.extend_from(source, targets);
    }

    /// What `source` references.
    #[must_use]
    pub fn get(&self, source: &K) -> Option<&HashSet<SymbolIdentifier>> {
        self.to.get(source)
    }

    #[must_use]
    pub fn contains_key(&self, source: &K) -> bool {
        self.to.contains_key(source)
    }

    /// Forgets every reference `source` makes, and gives them.
    pub fn remove(&mut self, source: &K) -> Option<HashSet<SymbolIdentifier>> {
        let targets = self.to.remove(source)?;
        for target in &targets {
            if let Some(sources) = self.referrers.get_mut(target) {
                sources.remove(source);
                if sources.is_empty() {
                    self.referrers.remove(target);
                }
            }
        }
        if let Some(class) = source.class()
            && let Some(sources) = self.by_class.get_mut(&class)
        {
            sources.remove(source);
            if sources.is_empty() {
                self.by_class.remove(&class);
            }
        }

        Some(targets)
    }

    /// Keeps only the sources `keep` accepts, with their references.
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &HashSet<SymbolIdentifier>) -> bool) {
        let dropped: Vec<K> =
            self.to.iter().filter(|(source, targets)| !keep(source, targets)).map(|(source, _)| *source).collect();
        for source in dropped {
            self.remove(&source);
        }
    }

    /// The sources that reference `target`.
    pub fn referrers(&self, target: &SymbolIdentifier) -> impl Iterator<Item = &K> {
        self.referrers.get(target).into_iter().flatten()
    }

    /// The sources that are the class-like `class` or one of its members.
    pub fn sources_of_class(&self, class: &Word) -> impl Iterator<Item = &K> {
        self.by_class.get(class).into_iter().flatten()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &HashSet<SymbolIdentifier>)> {
        self.to.iter()
    }

    pub fn values(&self) -> impl Iterator<Item = &HashSet<SymbolIdentifier>> {
        self.to.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.to.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.to.is_empty()
    }
}

impl<K: Referrer> std::ops::Index<&K> for References<K> {
    type Output = HashSet<SymbolIdentifier>;

    fn index(&self, source: &K) -> &Self::Output {
        &self.to[source]
    }
}

impl<'references, K: Referrer> IntoIterator for &'references References<K> {
    type Item = (&'references K, &'references HashSet<SymbolIdentifier>);
    type IntoIter = std::collections::hash_map::Iter<'references, K, HashSet<SymbolIdentifier>>;

    fn into_iter(self) -> Self::IntoIter {
        self.to.iter()
    }
}

impl<K: Referrer> IntoIterator for References<K> {
    type Item = (K, HashSet<SymbolIdentifier>);
    type IntoIter = std::collections::hash_map::IntoIter<K, HashSet<SymbolIdentifier>>;

    fn into_iter(self) -> Self::IntoIter {
        self.to.into_iter()
    }
}

/// Describes the semantic role of a recorded symbol reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SymbolReferenceKind {
    /// A reference from executable code.
    Body,
    /// A reference from a declaration signature.
    Signature,
    /// A reference to an overridden member.
    OverriddenMember,
    /// A use of another function-like's return value.
    FunctionLikeReturn,
    /// A property value read.
    PropertyRead,
    /// A property write.
    PropertyWrite,
}

/// Stores various maps tracking references between symbols (classes, functions, etc.)
/// and class-like members (methods, properties, constants, etc.) within the codebase.
///
/// This is primarily used for dependency analysis, understanding code structure,
/// and potentially for tasks like dead code detection or impact analysis.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[allow(clippy::struct_field_names)]
pub struct SymbolReferences {
    /// Maps a referencing symbol/member `(RefSymbol, RefMember)` to a set of referenced symbols/members `(Symbol, Member)`
    /// found within the *body* of the referencing context.
    /// `RefMember` or `Member` being empty usually signifies the symbol itself.
    symbol_references_to_symbols: References<SymbolIdentifier>,

    /// Maps a referencing symbol/member `(RefSymbol, RefMember)` to a set of referenced symbols/members `(Symbol, Member)`
    /// found within the *signature* (e.g., type hints) of the referencing context.
    symbol_references_to_symbols_in_signature: References<SymbolIdentifier>,

    /// The references an attribute a symbol/member carries makes in its *body*: to the attribute's constructor, and to
    /// its class. Every reader but [`Self::cascade_edges`] reads them as body references.
    attribute_references: References<SymbolIdentifier>,

    /// The references an attribute a symbol/member carries makes in its *signature*: to the attribute's class. Every
    /// reader but [`Self::cascade_edges`] reads them as signature references.
    attribute_references_in_signature: References<SymbolIdentifier>,

    /// Maps a referencing symbol/member `(RefSymbol, RefMember)` to a set of *overridden* members `(ParentSymbol, Member)`
    /// that it directly references (e.g., via `parent::method()`).
    symbol_references_to_overridden_members: HashMap<SymbolIdentifier, HashSet<SymbolIdentifier>>,

    /// Maps a referencing function/method (`FunctionLikeIdentifier`) to a set of functions/methods (`FunctionLikeIdentifier`)
    /// whose return values it references/uses. Used for dead code analysis on return values.
    functionlike_references_to_functionlike_returns: HashMap<FunctionLikeIdentifier, HashSet<FunctionLikeIdentifier>>,

    /// Maps a logical file name to a set of referenced symbols/members `(Symbol, Member)`
    /// found within the file's global scope (outside any symbol). This tracks references from top-level code.
    /// Used for incremental analysis to determine which files need re-analysis when a symbol changes.
    file_references_to_symbols: References<Word>,

    /// Maps a logical file name to a set of referenced symbols/members `(Symbol, Member)`
    /// found within the file's global scope signatures (e.g., top-level type declarations).
    file_references_to_symbols_in_signature: References<Word>,

    /// Maps a referencing symbol/member to a set of properties that are *written* (assigned to).
    /// This is separate from read references to enable detection of write-only properties.
    /// The key is the referencing symbol/member, the value is the set of properties being written.
    property_write_references: HashMap<SymbolIdentifier, HashSet<SymbolIdentifier>>,

    /// Maps a referencing symbol/member to a set of properties that are *read* (accessed for value).
    /// This is separate from write references to enable accurate read/write tracking.
    /// The key is the referencing symbol/member, the value is the set of properties being read.
    property_read_references: HashMap<SymbolIdentifier, HashSet<SymbolIdentifier>>,

    /// Maps a logical file name to properties written from top-level code or a file-scoped closure.
    file_property_write_references: HashMap<Word, HashSet<SymbolIdentifier>>,

    /// Maps a logical file name to properties read from top-level code or a file-scoped closure.
    file_property_read_references: HashMap<Word, HashSet<SymbolIdentifier>>,
}

impl SymbolReferences {
    /// Creates a new, empty `SymbolReferences` collection.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether no references of any kind are recorded.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.symbol_references_to_symbols.is_empty()
            && self.symbol_references_to_symbols_in_signature.is_empty()
            && self.attribute_references.is_empty()
            && self.attribute_references_in_signature.is_empty()
            && self.symbol_references_to_overridden_members.is_empty()
            && self.functionlike_references_to_functionlike_returns.is_empty()
            && self.file_references_to_symbols.is_empty()
            && self.file_references_to_symbols_in_signature.is_empty()
            && self.property_write_references.is_empty()
            && self.property_read_references.is_empty()
            && self.file_property_write_references.is_empty()
            && self.file_property_read_references.is_empty()
    }

    /// Counts the total number of body references from symbols and files.
    #[inline]
    pub fn count_body_references(&self) -> usize {
        self.symbol_references_to_symbols.values().map(std::collections::HashSet::len).sum::<usize>()
            + self.attribute_references.values().map(std::collections::HashSet::len).sum::<usize>()
            + self.file_references_to_symbols.values().map(std::collections::HashSet::len).sum::<usize>()
    }

    /// Counts the total number of signature references from symbols and files.
    #[inline]
    pub fn count_signature_references(&self) -> usize {
        self.symbol_references_to_symbols_in_signature.values().map(std::collections::HashSet::len).sum::<usize>()
            + self.attribute_references_in_signature.values().map(std::collections::HashSet::len).sum::<usize>()
            + self.file_references_to_symbols_in_signature.values().map(std::collections::HashSet::len).sum::<usize>()
    }

    /// Returns the total number of map entries (keys) across all reference maps.
    /// Useful for memory auditing — this count should remain stable across cycles
    /// in a long-running process.
    #[inline]
    #[must_use]
    pub fn total_map_entries(&self) -> usize {
        self.symbol_references_to_symbols.len()
            + self.symbol_references_to_symbols_in_signature.len()
            + self.attribute_references.len()
            + self.attribute_references_in_signature.len()
            + self.symbol_references_to_overridden_members.len()
            + self.functionlike_references_to_functionlike_returns.len()
            + self.file_references_to_symbols.len()
            + self.file_references_to_symbols_in_signature.len()
            + self.property_write_references.len()
            + self.property_read_references.len()
            + self.file_property_write_references.len()
            + self.file_property_read_references.len()
    }

    /// Counts how many symbols reference the given symbol.
    ///
    /// # Arguments
    /// * `symbol` - The symbol to check references to
    /// * `in_signature` - If true, count signature references; if false, count body references
    ///
    /// # Returns
    /// The number of symbols that reference the given symbol
    #[inline]
    #[must_use]
    pub fn count_referencing_symbols(&self, symbol: &SymbolIdentifier, in_signature: bool) -> usize {
        let (map, attributes) = if in_signature {
            (&self.symbol_references_to_symbols_in_signature, &self.attribute_references_in_signature)
        } else {
            (&self.symbol_references_to_symbols, &self.attribute_references)
        };

        let files =
            if in_signature { &self.file_references_to_symbols_in_signature } else { &self.file_references_to_symbols };

        let referencing: HashSet<&SymbolIdentifier> = map
            .iter()
            .chain(attributes)
            .filter(|(_, referenced_set)| referenced_set.contains(symbol))
            .map(|(referencing, _)| referencing)
            .collect();

        referencing.len() + files.values().filter(|referenced_set| referenced_set.contains(symbol)).count()
    }

    /// Counts how many symbols have a *read* reference to the given property.
    ///
    /// # Arguments
    ///
    /// * `property` - The property symbol identifier `(ClassName, PropertyName)` to check
    ///
    /// # Returns
    ///
    /// The number of symbols that read the given property
    #[inline]
    #[must_use]
    pub fn count_property_reads(&self, property: &SymbolIdentifier) -> usize {
        self.property_read_references.values().filter(|read_set| read_set.contains(property)).count()
            + self.file_property_read_references.values().filter(|read_set| read_set.contains(property)).count()
    }

    /// Counts how many symbols have a *write* reference to the given property.
    ///
    /// # Arguments
    ///
    /// * `property` - The property symbol identifier `(ClassName, PropertyName)` to check
    ///
    /// # Returns
    ///
    /// The number of symbols that write to the given property
    #[inline]
    #[must_use]
    pub fn count_property_writes(&self, property: &SymbolIdentifier) -> usize {
        self.property_write_references.values().filter(|write_set| write_set.contains(property)).count()
            + self.file_property_write_references.values().filter(|write_set| write_set.contains(property)).count()
    }

    /// Records that a top-level symbol (e.g., a function) references a class member.
    ///
    /// Automatically adds a reference from the referencing symbol to the member's class.
    ///
    /// # Arguments
    ///
    /// * `referencing_symbol`: The FQN of the function or global const making the reference.
    /// * `class_member`: A tuple `(ClassName, MemberName)` being referenced.
    /// * `in_signature`: `true` if the reference occurs in a signature context, `false` if in the body.
    #[inline]
    pub fn add_symbol_reference_to_class_member(
        &mut self,
        referencing_symbol: Word,
        class_member: SymbolIdentifier,
        in_signature: bool,
    ) {
        // Reference the class itself implicitly (in body context)
        self.add_symbol_reference_to_symbol(referencing_symbol, class_member.0, false);

        // Use empty member for the referencing symbol key
        let key = (referencing_symbol, empty_word());
        if in_signature {
            self.symbol_references_to_symbols_in_signature.insert(key, class_member);
        } else {
            self.symbol_references_to_symbols.insert(key, class_member);
        }
    }

    /// Records that a top-level symbol references another top-level symbol.
    ///
    /// Skips self-references. Skips body references if already referenced in signature.
    ///
    /// # Arguments
    /// * `referencing_symbol`: The FQN of the symbol making the reference.
    /// * `symbol`: The FQN of the symbol being referenced.
    /// * `in_signature`: `true` if the reference occurs in a signature context, `false` if in the body.
    #[inline]
    pub fn add_symbol_reference_to_symbol(&mut self, referencing_symbol: Word, symbol: Word, in_signature: bool) {
        if referencing_symbol == symbol {
            return;
        }

        // Represent top-level symbols with an empty member identifier
        let referencing_key = (referencing_symbol, empty_word());
        let referenced_key = (symbol, empty_word());

        if in_signature {
            self.symbol_references_to_symbols_in_signature.insert(referencing_key, referenced_key);
        } else {
            // If it's already referenced in the signature, don't add as a body reference
            if let Some(sig_refs) = self.symbol_references_to_symbols_in_signature.get(&referencing_key)
                && sig_refs.contains(&referenced_key)
            {
                return;
            }
            self.symbol_references_to_symbols.insert(referencing_key, referenced_key);
        }
    }

    /// Records a reference between arbitrary top-level symbols or class-like members.
    #[inline]
    pub fn add_symbol_reference(
        &mut self,
        referencing: SymbolIdentifier,
        referenced: SymbolIdentifier,
        in_signature: bool,
    ) {
        match (referencing.1.is_empty(), referenced.1.is_empty()) {
            (true, true) => self.add_symbol_reference_to_symbol(referencing.0, referenced.0, in_signature),
            (true, false) => self.add_symbol_reference_to_class_member(referencing.0, referenced, in_signature),
            (false, true) => self.add_class_member_reference_to_symbol(referencing, referenced.0, in_signature),
            (false, false) => self.add_class_member_reference_to_class_member(referencing, referenced, in_signature),
        }
    }

    /// Records a body or signature reference from a symbol/member or file origin.
    #[inline]
    pub fn add_reference(&mut self, referencing: ReferenceOrigin, referenced: SymbolIdentifier, in_signature: bool) {
        match referencing {
            ReferenceOrigin::Symbol(referencing) => {
                self.add_symbol_reference(referencing, referenced, in_signature);
            }
            ReferenceOrigin::File(file) => {
                self.add_file_reference_to_class_member(file, referenced, in_signature);
            }
        }
    }

    /// Records that an attribute `referencing` carries references `referenced`, its class or its constructor, with the
    /// same implied references [`Self::add_reference`] records, as [`CascadeEdge::Attribute`] edges. An attribute in
    /// top-level code records a file reference, as [`Self::add_reference`] does.
    #[inline]
    pub fn add_attribute_reference(
        &mut self,
        referencing: ReferenceOrigin,
        referenced: SymbolIdentifier,
        in_signature: bool,
    ) {
        if let ReferenceOrigin::File(_) = referencing {
            self.add_reference(referencing, referenced, in_signature);
            return;
        }

        let mut made = SymbolReferences::new();
        made.add_reference(referencing, referenced, in_signature);
        for (from, references) in made.symbol_references_to_symbols_in_signature {
            self.attribute_references_in_signature.extend_from(from, references);
        }
        for (from, references) in made.symbol_references_to_symbols {
            let in_signature = self.attribute_references_in_signature.get(&from);
            for to in references {
                if in_signature.is_none_or(|known| !known.contains(&to)) {
                    self.attribute_references.insert(from, to);
                }
            }
        }
    }

    /// Records a property read from an explicit symbol, member, or file source.
    #[inline]
    pub fn add_property_read_reference(&mut self, referencing: ReferenceOrigin, property: SymbolIdentifier) {
        self.add_reference(referencing, property, false);
        match referencing {
            ReferenceOrigin::Symbol(symbol) => {
                self.property_read_references.entry(symbol).or_default().insert(property);
            }
            ReferenceOrigin::File(file) => {
                self.file_property_read_references.entry(file).or_default().insert(property);
            }
        }
    }

    /// Records a property write from an explicit symbol, member, or file source.
    #[inline]
    pub fn add_property_write_reference(&mut self, referencing: ReferenceOrigin, property: SymbolIdentifier) {
        self.add_reference(referencing, property, false);
        match referencing {
            ReferenceOrigin::Symbol(symbol) => {
                self.property_write_references.entry(symbol).or_default().insert(property);
            }
            ReferenceOrigin::File(file) => {
                self.file_property_write_references.entry(file).or_default().insert(property);
            }
        }
    }

    /// Records an explicit reference to an overridden class-like member.
    #[inline]
    pub fn add_overridden_member_reference(&mut self, referencing: SymbolIdentifier, overridden: SymbolIdentifier) {
        self.symbol_references_to_overridden_members.entry(referencing).or_default().insert(overridden);
    }

    /// Records an explicit use of another function-like's return value.
    #[inline]
    pub fn add_functionlike_return_reference(&mut self, referencing: SymbolIdentifier, referenced: SymbolIdentifier) {
        let referencing = if referencing.1.is_empty() {
            FunctionLikeIdentifier::Function(referencing.0)
        } else {
            FunctionLikeIdentifier::Method(referencing.0, referencing.1)
        };
        let referenced = if referenced.1.is_empty() {
            FunctionLikeIdentifier::Function(referenced.0)
        } else {
            FunctionLikeIdentifier::Method(referenced.0, referenced.1)
        };

        self.add_reference_to_functionlike_return(referencing, referenced);
    }

    /// Records that a class member references another class member.
    ///
    /// Automatically adds references from the referencing member's class to the referenced member's class,
    /// and from the referencing member to the referenced member's class. Skips self-references.
    ///
    /// # Arguments
    /// * `referencing_class_member`: Tuple `(ClassName, MemberName)` making the reference.
    /// * `class_member`: Tuple `(ClassName, MemberName)` being referenced.
    /// * `in_signature`: `true` if the reference occurs in a signature context, `false` if in the body.
    #[inline]
    pub fn add_class_member_reference_to_class_member(
        &mut self,
        referencing_class_member: SymbolIdentifier,
        class_member: SymbolIdentifier,
        in_signature: bool,
    ) {
        if referencing_class_member == class_member {
            return;
        }

        // Add implicit references between the classes/symbols involved
        self.add_symbol_reference_to_symbol(referencing_class_member.0, class_member.0, false);
        self.add_class_member_reference_to_symbol(referencing_class_member, class_member.0, false);

        // Add the direct member-to-member reference
        if in_signature {
            self.symbol_references_to_symbols_in_signature.insert(referencing_class_member, class_member);
        } else {
            // Check signature refs first? (Consistency with add_symbol_reference_to_symbol might be needed)
            // Current logic adds to body refs regardless of signature refs for member->member.
            self.symbol_references_to_symbols.insert(referencing_class_member, class_member);
        }
    }

    /// Records that a class member references a top-level symbol.
    ///
    /// Automatically adds a reference from the referencing member's class to the referenced symbol.
    /// Skips references to the member's own class. Skips body references if already referenced in signature.
    ///
    /// # Arguments
    /// * `referencing_class_member`: Tuple `(ClassName, MemberName)` making the reference.
    /// * `symbol`: The FQN of the symbol being referenced.
    /// * `in_signature`: `true` if the reference occurs in a signature context, `false` if in the body.
    #[inline]
    pub fn add_class_member_reference_to_symbol(
        &mut self,
        referencing_class_member: SymbolIdentifier,
        symbol: Word,
        in_signature: bool,
    ) {
        if referencing_class_member.0 == symbol {
            return;
        }

        // Add implicit reference from the class to the symbol
        self.add_symbol_reference_to_symbol(referencing_class_member.0, symbol, false);

        // Represent the referenced symbol with an empty member identifier
        let referenced_key = (symbol, empty_word());

        if in_signature {
            self.symbol_references_to_symbols_in_signature.insert(referencing_class_member, referenced_key);
        } else {
            // If already referenced in signature, don't add as body reference
            if let Some(sig_refs) = self.symbol_references_to_symbols_in_signature.get(&referencing_class_member)
                && sig_refs.contains(&referenced_key)
            {
                return;
            }
            self.symbol_references_to_symbols.insert(referencing_class_member, referenced_key);
        }
    }

    /// Adds a file-level reference to a symbol or class-like member.
    ///
    /// Member references also imply a reference to their containing class-like symbol.
    #[inline]
    pub fn add_file_reference_to_class_member(
        &mut self,
        file_name: Word,
        class_member: SymbolIdentifier,
        in_signature: bool,
    ) {
        if !class_member.1.is_empty() {
            self.add_file_reference_to_class_member(file_name, (class_member.0, empty_word()), false);
        }

        if in_signature {
            self.file_references_to_symbols_in_signature.insert(file_name, class_member);
        } else {
            // Check if already in signature to avoid duplicate tracking
            if let Some(sig_refs) = self.file_references_to_symbols_in_signature.get(&file_name)
                && sig_refs.contains(&class_member)
            {
                return;
            }
            self.file_references_to_symbols.insert(file_name, class_member);
        }
    }

    /// Convenience method to add a reference *from* the current function context *to* a class member.
    /// Delegates to appropriate `add_*` methods based on the function context.
    #[inline]
    pub fn add_reference_to_class_member(
        &mut self,
        scope: &ScopeContext<'_>,
        class_member: SymbolIdentifier,
        in_signature: bool,
    ) {
        self.add_reference(scope.get_reference_origin(), class_member, in_signature);
    }

    /// Records a call of `method`, by its class's key, spec section 24.
    #[inline]
    pub fn add_reference_for_method_call(
        &mut self,
        codebase: &CodebaseMetadata,
        scope: &ScopeContext<'_>,
        method: &MethodIdentifier,
    ) {
        self.add_reference_to_class_member(
            scope,
            (codebase.class_key(method.get_class_name()), method.get_method_name()),
            false,
        );
    }

    /// Records a read reference to a property (e.g., `$this->prop` used as a value), by its class's key.
    #[inline]
    pub fn add_reference_for_property_read(
        &mut self,
        codebase: &CodebaseMetadata,
        scope: &ScopeContext<'_>,
        class_name: Word,
        property_name: Word,
    ) {
        let class_member = (codebase.class_key(class_name), property_name);

        self.add_property_read_reference(scope.get_reference_origin(), class_member);
    }

    /// Records a write reference to a property (e.g., `$this->prop = value`), by its class's key.
    /// This is tracked separately from read references to enable write-only property detection.
    #[inline]
    pub fn add_reference_for_property_write(
        &mut self,
        codebase: &CodebaseMetadata,
        scope: &ScopeContext<'_>,
        class_name: Word,
        property_name: Word,
    ) {
        let class_member = (codebase.class_key(class_name), property_name);

        self.add_property_write_reference(scope.get_reference_origin(), class_member);
    }

    /// Convenience method to add a reference *from* the current function context *to* an overridden class member (e.g., `parent::foo`).
    /// Delegates based on the function context.
    #[inline]
    pub fn add_reference_to_overridden_class_member(&mut self, scope: &ScopeContext, class_member: SymbolIdentifier) {
        let ReferenceOrigin::Symbol(referencing) = scope.get_reference_origin() else {
            return;
        };

        self.symbol_references_to_overridden_members.entry(referencing).or_default().insert(class_member);
    }

    /// Convenience method to add a reference *from* the current function context *to* a top-level symbol.
    /// Delegates to appropriate `add_*` methods based on the function context.
    #[inline]
    pub fn add_reference_to_symbol(&mut self, scope: &ScopeContext, symbol: Word, in_signature: bool) {
        self.add_reference(scope.get_reference_origin(), (symbol, empty_word()), in_signature);
    }

    /// Records that one function/method references the return value of another. Used for dead code analysis.
    #[inline]
    pub fn add_reference_to_functionlike_return(
        &mut self,
        referencing_functionlike: FunctionLikeIdentifier,
        referenced_functionlike: FunctionLikeIdentifier,
    ) {
        if referencing_functionlike == referenced_functionlike {
            return;
        }

        self.functionlike_references_to_functionlike_returns
            .entry(referencing_functionlike)
            .or_default()
            .insert(referenced_functionlike);
    }

    /// Merges references from another `SymbolReferences` instance into this one.
    /// Existing references are extended, not replaced.
    #[inline]
    pub fn extend(&mut self, other: Self) {
        for (k, v) in other.symbol_references_to_symbols {
            self.symbol_references_to_symbols.extend_from(k, v);
        }
        for (k, v) in other.symbol_references_to_symbols_in_signature {
            self.symbol_references_to_symbols_in_signature.extend_from(k, v);
        }
        for (k, v) in other.attribute_references {
            self.attribute_references.extend_from(k, v);
        }
        for (k, v) in other.attribute_references_in_signature {
            self.attribute_references_in_signature.extend_from(k, v);
        }
        for (k, v) in other.symbol_references_to_overridden_members {
            self.symbol_references_to_overridden_members.entry(k).or_default().extend(v);
        }
        for (k, v) in other.functionlike_references_to_functionlike_returns {
            self.functionlike_references_to_functionlike_returns.entry(k).or_default().extend(v);
        }

        for (k, v) in other.file_references_to_symbols {
            self.file_references_to_symbols.extend_from(k, v);
        }

        for (k, v) in other.file_references_to_symbols_in_signature {
            self.file_references_to_symbols_in_signature.extend_from(k, v);
        }

        for (k, v) in other.property_write_references {
            self.property_write_references.entry(k).or_default().extend(v);
        }

        for (k, v) in other.property_read_references {
            self.property_read_references.entry(k).or_default().extend(v);
        }

        for (k, v) in other.file_property_write_references {
            self.file_property_write_references.entry(k).or_default().extend(v);
        }

        for (k, v) in other.file_property_read_references {
            self.file_property_read_references.entry(k).or_default().extend(v);
        }
    }

    /// Visits every recorded reference without materializing a copy of the graph.
    #[inline]
    pub fn for_each_reference(&self, mut visit: impl FnMut(ReferenceOrigin, SymbolIdentifier, SymbolReferenceKind)) {
        for (source, targets) in self.symbol_references_to_symbols.iter().chain(&self.attribute_references) {
            for target in targets {
                visit(ReferenceOrigin::Symbol(*source), *target, SymbolReferenceKind::Body);
            }
        }
        for (source, targets) in
            self.symbol_references_to_symbols_in_signature.iter().chain(&self.attribute_references_in_signature)
        {
            for target in targets {
                visit(ReferenceOrigin::Symbol(*source), *target, SymbolReferenceKind::Signature);
            }
        }
        for (source, targets) in &self.symbol_references_to_overridden_members {
            for target in targets {
                visit(ReferenceOrigin::Symbol(*source), *target, SymbolReferenceKind::OverriddenMember);
            }
        }
        for (source, targets) in &self.functionlike_references_to_functionlike_returns {
            let Some(source) = function_like_symbol_identifier(source) else {
                continue;
            };
            for target in targets {
                if let Some(target) = function_like_symbol_identifier(target) {
                    visit(ReferenceOrigin::Symbol(source), target, SymbolReferenceKind::FunctionLikeReturn);
                }
            }
        }
        for (source, targets) in &self.file_references_to_symbols {
            for target in targets {
                visit(ReferenceOrigin::File(*source), *target, SymbolReferenceKind::Body);
            }
        }
        for (source, targets) in &self.file_references_to_symbols_in_signature {
            for target in targets {
                visit(ReferenceOrigin::File(*source), *target, SymbolReferenceKind::Signature);
            }
        }
        for (source, targets) in &self.property_read_references {
            for target in targets {
                visit(ReferenceOrigin::Symbol(*source), *target, SymbolReferenceKind::PropertyRead);
            }
        }
        for (source, targets) in &self.property_write_references {
            for target in targets {
                visit(ReferenceOrigin::Symbol(*source), *target, SymbolReferenceKind::PropertyWrite);
            }
        }
        for (source, targets) in &self.file_property_read_references {
            for target in targets {
                visit(ReferenceOrigin::File(*source), *target, SymbolReferenceKind::PropertyRead);
            }
        }
        for (source, targets) in &self.file_property_write_references {
            for target in targets {
                visit(ReferenceOrigin::File(*source), *target, SymbolReferenceKind::PropertyWrite);
            }
        }
    }

    /// Finds all symbols/members that reference a specific target symbol/member.
    /// Checks both body and signature references.
    ///
    /// # Arguments
    ///
    /// * `target_symbol`: The `(SymbolName, MemberName)` tuple being referenced.
    ///
    /// # Returns
    ///
    /// A `HashSet` containing `&(RefSymbol, RefMember)` tuples of all items referencing the target.
    #[inline]
    #[must_use]
    pub fn get_references_to_symbol(&self, target_symbol: SymbolIdentifier) -> HashSet<&SymbolIdentifier> {
        let mut referencing_items = HashSet::default();
        for (referencing_item, referenced_items) in self
            .symbol_references_to_symbols
            .iter()
            .chain(&self.symbol_references_to_symbols_in_signature)
            .chain(&self.attribute_references)
            .chain(&self.attribute_references_in_signature)
        {
            if referenced_items.contains(&target_symbol) {
                referencing_items.insert(referencing_item);
            }
        }
        referencing_items
    }

    /// The symbols and members, and the logical names of the files, whose signatures reference a class-like
    /// `names` holds, by its lowercase name, or a member of one.
    #[must_use]
    pub fn get_signature_references_to_class_likes(&self, names: &WordSet) -> (HashSet<SymbolIdentifier>, WordSet) {
        let reaches_one = |references: &HashSet<SymbolIdentifier>| {
            references.iter().any(|(class_like, _)| names.contains(&ascii_lowercase_word(class_like.as_bytes())))
        };
        let symbols = self
            .symbol_references_to_symbols_in_signature
            .iter()
            .chain(&self.attribute_references_in_signature)
            .filter(|(_, references)| reaches_one(references))
            .map(|(referencing, _)| *referencing)
            .collect();
        let files = self
            .file_references_to_symbols_in_signature
            .iter()
            .filter(|(_, references)| reaches_one(references))
            .map(|(file, _)| *file)
            .collect();

        (symbols, files)
    }

    /// Returns whether a body or signature reference to a symbol originates from top-level file code.
    #[inline]
    #[must_use]
    pub fn has_file_reference_to_symbol(&self, target_symbol: SymbolIdentifier) -> bool {
        self.file_references_to_symbols.values().any(|references| references.contains(&target_symbol))
            || self
                .file_references_to_symbols_in_signature
                .values()
                .any(|references| references.contains(&target_symbol))
    }

    /// Every edge the invalidation cascade follows, from these references and `codebase`'s class hierarchy.
    ///
    /// [`Self::get_invalid_symbols`] walks these edges forward from a change. A walk backward from a file, to the
    /// changes that reach it, reads the same edges, so the two never disagree on what an edge is.
    pub fn cascade_edges<'edges>(
        &'edges self,
        codebase: &'edges CodebaseMetadata,
    ) -> impl Iterator<Item = CascadeEdge> + 'edges {
        let signature = self.symbol_references_to_symbols_in_signature.iter().flat_map(|(from, references)| {
            references.iter().map(move |to| CascadeEdge::Signature { from: *from, to: *to })
        });
        let body = self
            .symbol_references_to_symbols
            .iter()
            .flat_map(|(from, references)| references.iter().map(move |to| CascadeEdge::Body { from: *from, to: *to }));
        let attribute = self
            .attribute_references_in_signature
            .iter()
            .map(|(from, references)| (from, references, true))
            .chain(self.attribute_references.iter().map(|(from, references)| (from, references, false)))
            .flat_map(|(from, references, signature)| {
                references.iter().map(move |to| CascadeEdge::Attribute { from: *from, to: *to, signature })
            });
        let file =
            self.file_references_to_symbols.iter().chain(&self.file_references_to_symbols_in_signature).flat_map(
                |(file, references)| references.iter().map(move |to| CascadeEdge::File { file: *file, to: *to }),
            );
        let inherits = codebase.class_likes.iter().flat_map(|(class, metadata)| {
            metadata
                .direct_parent_class
                .iter()
                .chain(&metadata.direct_parent_interfaces)
                .chain(&metadata.used_traits)
                .map(move |parent| CascadeEdge::Inherits { class: *class, parent: *parent })
        });

        signature.chain(body).chain(attribute).chain(file).chain(inherits)
    }

    /// Calculates sets of invalid symbols and members based on detected code changes (`CodebaseDiff`).
    /// Propagates invalidation back through the [`Self::cascade_edges`] of these references and `codebase`, from each
    /// change, through the edges that reach it alone, each once. A method of `codebase` that takes its return from its
    /// body has a signature its body decides, so its body's references propagate as its signature's do.
    ///
    /// # Arguments
    ///
    /// * `codebase`: The codebase whose class hierarchy the cascade follows.
    /// * `codebase_diff`: Information about added, deleted, or modified symbols/signatures.
    /// * `read`: The symbols the extension callbacks read, which every file counts as its own.
    ///
    /// # Returns
    ///
    /// `(invalid_signatures, partially_invalid, invalid_files, read_invalid)`, where `invalid_signatures` contains all
    /// symbol/member pairs whose signature or body is invalid (including propagated ones), and `partially_invalid`
    /// contains symbols with at least one invalid member, and classes whose direct parent, interface or trait changed.
    /// `invalid_files` contains logical file names whose top-level code references a symbol with an invalid
    /// signature, and `read_invalid` whether `read` names one.
    #[inline]
    #[must_use]
    pub fn get_invalid_symbols(
        &self,
        codebase: &CodebaseMetadata,
        codebase_diff: &CodebaseDiff,
        read: &HashSet<SymbolIdentifier>,
    ) -> (HashSet<SymbolIdentifier>, WordSet, WordSet, bool) {
        let mut invalid_signatures: HashSet<SymbolIdentifier> = HashSet::default();
        let mut partially_invalid_symbols = WordSet::default();
        let invalidate = |item: SymbolIdentifier, invalid: &mut HashSet<SymbolIdentifier>, partially: &mut WordSet| {
            if !item.1.is_empty() {
                partially.insert(item.0);
            }

            invalid.insert(item)
        };

        // A changed class-like invalidates the signature of each of its members whose signature references anything.
        for (class, member) in codebase_diff.get_changed() {
            if !member.is_empty() {
                continue;
            }
            for from in self
                .symbol_references_to_symbols_in_signature
                .sources_of_class(class)
                .chain(self.attribute_references_in_signature.sources_of_class(class))
            {
                invalidate(*from, &mut invalid_signatures, &mut partially_invalid_symbols);
                partially_invalid_symbols.insert(*class);
            }
        }

        // A class whose parent, interface or trait changed is never safe, since its edge to that parent can be lost
        // while the parent is deleted and re-added.
        let changed_parents: WordSet = codebase_diff
            .get_changed()
            .iter()
            .filter(|(class, member)| member.is_empty() && codebase.class_likes.contains_key(class))
            .map(|(class, _)| *class)
            .collect();
        if !changed_parents.is_empty() {
            for (class, metadata) in &codebase.class_likes {
                let mut parents = metadata
                    .direct_parent_class
                    .iter()
                    .chain(&metadata.direct_parent_interfaces)
                    .chain(&metadata.used_traits);
                if parents.any(|parent| changed_parents.contains(parent)) {
                    partially_invalid_symbols.insert(*class);
                }
            }
        }

        // What a signature references invalidates that signature, and so does what the body of a method that takes
        // its return from its body references.
        let mut walk: Vec<SymbolIdentifier> = codebase_diff.get_changed().iter().copied().collect();
        let mut walked: HashSet<SymbolIdentifier> = HashSet::default();
        while let Some(item) = walk.pop() {
            if !walked.insert(item) {
                continue;
            }
            invalidate(item, &mut invalid_signatures, &mut partially_invalid_symbols);
            if !item.1.is_empty() {
                walk.push((item.0, empty_word()));
            }

            let by_signature = self
                .symbol_references_to_symbols_in_signature
                .referrers(&item)
                .chain(self.attribute_references_in_signature.referrers(&item));
            let by_returned_body = self
                .symbol_references_to_symbols
                .referrers(&item)
                .chain(self.attribute_references.referrers(&item))
                .filter(|from| {
                    codebase.function_likes.get(from).is_some_and(|method| method.return_from_body.is_some())
                });
            for from in by_signature.chain(by_returned_body) {
                invalidate(*from, &mut invalid_signatures, &mut partially_invalid_symbols);
                if !walked.contains(from) {
                    walk.push(*from);
                }
            }
        }

        // A body is invalid when it references, in its body or its signature, an item whose signature is invalid, and
        // so is the top-level code of a file.
        let mut invalid_bodies: HashSet<SymbolIdentifier> = HashSet::default();
        let mut invalid_files = WordSet::default();
        for item in &invalid_signatures {
            for from in self
                .symbol_references_to_symbols
                .referrers(item)
                .chain(self.symbol_references_to_symbols_in_signature.referrers(item))
                .chain(self.attribute_references.referrers(item))
                .chain(self.attribute_references_in_signature.referrers(item))
            {
                invalidate(*from, &mut invalid_bodies, &mut partially_invalid_symbols);
            }
            invalid_files.extend(
                self.file_references_to_symbols
                    .referrers(item)
                    .chain(self.file_references_to_symbols_in_signature.referrers(item))
                    .copied(),
            );
        }

        // A read names a class-like as the extension wrote it: exactly, as a PHP# one's key is, or by its key, spec
        // section 24, which a deleted PHP# class-like no longer has.
        let read_invalid = read.iter().any(|&(class, member)| {
            invalid_signatures.contains(&(class, member))
                || invalid_signatures.contains(&(codebase.class_key(class), member))
        });

        let mut all_invalid_symbols = invalid_signatures;
        all_invalid_symbols.extend(invalid_bodies);

        (all_invalid_symbols, partially_invalid_symbols, invalid_files, read_invalid)
    }

    /// Extracts references originating from safe (skipped) symbols and merges them into this instance.
    ///
    /// When incremental analysis runs with `diff = true`, the analyzer skips safe symbols,
    /// which means their body references are not collected. This method copies those missing
    /// references from the previous run's reference graph.
    ///
    /// Only references from symbols that are in `safe_symbols` or `safe_symbol_members`
    /// (and not already present in this instance) are copied.
    ///
    /// # Arguments
    ///
    /// * `previous` - The previous run's complete symbol references
    /// * `safe_symbols` - Set of safe top-level symbol names
    /// * `safe_symbol_members` - Set of safe (symbol, member) pairs
    #[inline]
    pub fn restore_references_for_safe_symbols(
        &mut self,
        previous: &SymbolReferences,
        safe_symbols: &WordSet,
        safe_symbol_members: &HashSet<SymbolIdentifier>,
    ) {
        let is_safe = |key: &SymbolIdentifier| -> bool {
            if key.1.is_empty() { safe_symbols.contains(&key.0) } else { safe_symbol_members.contains(key) }
        };

        // Restore body references for safe symbols
        for (key, refs) in &previous.symbol_references_to_symbols {
            if is_safe(key) && !self.symbol_references_to_symbols.contains_key(key) {
                self.symbol_references_to_symbols.insert_set(*key, refs.clone());
            }
        }

        // Restore the body references of the attributes of safe symbols
        for (key, refs) in &previous.attribute_references {
            if is_safe(key) && !self.attribute_references.contains_key(key) {
                self.attribute_references.insert_set(*key, refs.clone());
            }
        }

        // Restore overridden member references for safe symbols
        for (key, refs) in &previous.symbol_references_to_overridden_members {
            if is_safe(key) && !self.symbol_references_to_overridden_members.contains_key(key) {
                self.symbol_references_to_overridden_members.insert(*key, refs.clone());
            }
        }

        // Restore function-like return references for safe symbols
        for (key, refs) in &previous.functionlike_references_to_functionlike_returns {
            let sym_key = match key {
                FunctionLikeIdentifier::Function(name) => (*name, mago_word::empty_word()),
                FunctionLikeIdentifier::Method(class, method) => (*class, *method),
                _ => continue,
            };

            if is_safe(&sym_key) && !self.functionlike_references_to_functionlike_returns.contains_key(key) {
                self.functionlike_references_to_functionlike_returns.insert(*key, refs.clone());
            }
        }

        // Restore property write references for safe symbols
        for (key, refs) in &previous.property_write_references {
            if is_safe(key) && !self.property_write_references.contains_key(key) {
                self.property_write_references.insert(*key, refs.clone());
            }
        }

        // Restore property read references for safe symbols
        for (key, refs) in &previous.property_read_references {
            if is_safe(key) && !self.property_read_references.contains_key(key) {
                self.property_read_references.insert(*key, refs.clone());
            }
        }
    }

    /// Removes **body** references originating from the given symbols/members.
    ///
    /// Used by the body-only fast path: when only function/method bodies changed (no signature
    /// changes), we remove old body references and let the analyzer rebuild them fresh.
    /// Signature references are kept because signatures didn't change.
    ///
    /// Also removes function-like return references and property read/write references from
    /// the given symbols, as those originate from body code.
    ///
    /// File-level references keyed by the given file names are also removed.
    #[inline]
    pub fn remove_body_references_for_symbols(
        &mut self,
        symbols_and_members: &HashSet<SymbolIdentifier>,
        file_names: &[Word],
    ) {
        // Remove body (not signature) references
        for key in symbols_and_members {
            self.symbol_references_to_symbols.remove(key);
            self.attribute_references.remove(key);
            self.symbol_references_to_overridden_members.remove(key);
            self.property_write_references.remove(key);
            self.property_read_references.remove(key);
        }

        // Remove function-like return references for matching keys
        self.functionlike_references_to_functionlike_returns.retain(|key, _| {
            let sym_key = match key {
                FunctionLikeIdentifier::Function(name) => (*name, mago_word::empty_word()),
                FunctionLikeIdentifier::Method(class, method) => (*class, *method),
                _ => return true,
            };

            !symbols_and_members.contains(&sym_key)
        });

        // Remove file-level body references (signature refs kept)
        for name in file_names {
            self.file_references_to_symbols.remove(name);
            self.file_property_write_references.remove(name);
            self.file_property_read_references.remove(name);
        }
    }

    /// Removes every reference originating from the given files.
    ///
    /// Used when files are fully reanalyzed after a signature change. Both body and signature
    /// references must be rebuilt because either may have changed.
    #[inline]
    pub fn remove_references_from_files(&mut self, file_names: &WordSet) {
        for name in file_names {
            self.file_references_to_symbols.remove(name);
            self.file_references_to_symbols_in_signature.remove(name);
            self.file_property_write_references.remove(name);
            self.file_property_read_references.remove(name);
        }
    }

    /// Removes references whose source files no longer exist in the current codebase.
    #[inline]
    pub fn retain_references_from_files(&mut self, file_names: &WordSet) {
        self.file_references_to_symbols.retain(|name, _| file_names.contains(name));
        self.file_references_to_symbols_in_signature.retain(|name, _| file_names.contains(name));
        self.file_property_write_references.retain(|name, _| file_names.contains(name));
        self.file_property_read_references.retain(|name, _| file_names.contains(name));
    }

    /// Removes all references *originating from* symbols/members that are marked as invalid.
    ///
    /// # Arguments
    ///
    /// * `invalid_symbols_and_members`: A set containing `(SymbolName, MemberName)` tuples for invalid items.
    #[inline]
    pub fn remove_references_from_invalid_symbols(&mut self, invalid_symbols_and_members: &HashSet<SymbolIdentifier>) {
        // Retain only entries where the key (referencing item) is NOT in the invalid set.
        self.symbol_references_to_symbols
            .retain(|referencing_item, _| !invalid_symbols_and_members.contains(referencing_item));
        self.symbol_references_to_symbols_in_signature
            .retain(|referencing_item, _| !invalid_symbols_and_members.contains(referencing_item));
        self.attribute_references.retain(|referencing_item, _| !invalid_symbols_and_members.contains(referencing_item));
        self.attribute_references_in_signature
            .retain(|referencing_item, _| !invalid_symbols_and_members.contains(referencing_item));
        self.symbol_references_to_overridden_members
            .retain(|referencing_item, _| !invalid_symbols_and_members.contains(referencing_item));
        self.property_write_references
            .retain(|referencing_item, _| !invalid_symbols_and_members.contains(referencing_item));
        self.property_read_references
            .retain(|referencing_item, _| !invalid_symbols_and_members.contains(referencing_item));
    }

    /// Retains only references originating from safe (unchanged) symbols, removing all others.
    ///
    /// This is the inverse of [`remove_references_from_invalid_symbols`]: instead of
    /// specifying what to remove, you specify what to keep. References from non-safe symbols
    /// will be rebuilt by `populate_codebase` and the analyzer.
    ///
    /// This method also retains all builtin/prelude references (those where the key symbol
    /// is not user-defined, i.e., is in the base references).
    #[inline]
    pub fn retain_safe_symbol_references(
        &mut self,
        safe_symbols: &WordSet,
        safe_symbol_members: &HashSet<SymbolIdentifier>,
    ) {
        let is_safe = |key: &SymbolIdentifier| -> bool {
            if key.1.is_empty() { safe_symbols.contains(&key.0) } else { safe_symbol_members.contains(key) }
        };

        self.symbol_references_to_symbols.retain(|k, _| is_safe(k));
        self.symbol_references_to_symbols_in_signature.retain(|k, _| is_safe(k));
        self.attribute_references.retain(|k, _| is_safe(k));
        self.attribute_references_in_signature.retain(|k, _| is_safe(k));
        self.symbol_references_to_overridden_members.retain(|k, _| is_safe(k));
        self.property_write_references.retain(|k, _| is_safe(k));
        self.property_read_references.retain(|k, _| is_safe(k));

        self.functionlike_references_to_functionlike_returns.retain(|key, _| {
            let sym_key = match key {
                FunctionLikeIdentifier::Function(name) => (*name, mago_word::empty_word()),
                FunctionLikeIdentifier::Method(class, method) => (*class, *method),
                _ => return true, // Keep closures and other non-symbol function-likes
            };

            is_safe(&sym_key)
        });
    }

    /// Removes references for dirty (non-safe) symbols — O(dirty) instead of O(all).
    ///
    /// This is the inverse of [`retain_safe_symbol_references`]: instead of iterating all
    /// entries and keeping safe ones, it directly removes entries for the given dirty set.
    /// Much faster when the dirty set is small relative to the total number of references.
    pub fn remove_dirty_symbol_references(&mut self, dirty_symbols: &HashSet<SymbolIdentifier>) {
        for key in dirty_symbols {
            self.symbol_references_to_symbols.remove(key);
            self.symbol_references_to_symbols_in_signature.remove(key);
            self.attribute_references.remove(key);
            self.attribute_references_in_signature.remove(key);
            self.symbol_references_to_overridden_members.remove(key);
            self.property_write_references.remove(key);
            self.property_read_references.remove(key);

            let fl_key = if key.1.is_empty() {
                FunctionLikeIdentifier::Function(key.0)
            } else {
                FunctionLikeIdentifier::Method(key.0, key.1)
            };

            self.functionlike_references_to_functionlike_returns.remove(&fl_key);
        }
    }
}

fn function_like_symbol_identifier(identifier: &FunctionLikeIdentifier) -> Option<SymbolIdentifier> {
    match identifier {
        FunctionLikeIdentifier::Function(name) => Some((*name, empty_word())),
        FunctionLikeIdentifier::Method(class, method) => Some((*class, *method)),
        FunctionLikeIdentifier::Closure(_) => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use mago_word::empty_word;
    use mago_word::word;

    fn make_refs_with_body(entries: Vec<(SymbolIdentifier, Vec<SymbolIdentifier>)>) -> SymbolReferences {
        let mut refs = SymbolReferences::new();
        for (key, values) in entries {
            let set: HashSet<SymbolIdentifier> = values.into_iter().collect();
            refs.symbol_references_to_symbols.insert_set(key, set);
        }
        refs
    }

    #[test]
    fn test_for_each_reference_visits_every_reference_kind_and_origin() {
        let function = (word("consumer"), empty_word());
        let signature_function = (word("signature_consumer"), empty_word());
        let class = word("service");
        let method = (class, word("method"));
        let property = (class, word("$property"));
        let file_property = (class, word("$file_property"));
        let override_source = (word("child"), word("method"));
        let return_source = (word("return_consumer"), empty_word());
        let file = word("src/file.php");

        let mut references = SymbolReferences::new();
        assert!(references.is_empty());
        references.add_symbol_reference(function, (class, empty_word()), false);
        references.add_symbol_reference(signature_function, (class, empty_word()), true);
        references.add_property_read_reference(ReferenceOrigin::Symbol(method), property);
        references.add_property_write_reference(ReferenceOrigin::Symbol(function), property);
        references.add_overridden_member_reference(override_source, method);
        references.add_functionlike_return_reference(return_source, method);
        references.add_reference(ReferenceOrigin::File(file), method, false);
        references.add_file_reference_to_class_member(file, property, true);
        references.add_property_read_reference(ReferenceOrigin::File(file), file_property);
        references.add_property_write_reference(ReferenceOrigin::File(file), file_property);
        assert!(!references.is_empty());

        let mut visited = HashSet::default();
        references.for_each_reference(|source, target, kind| {
            visited.insert((source, target, kind));
        });

        assert!(visited.contains(&(
            ReferenceOrigin::Symbol(function),
            (class, empty_word()),
            SymbolReferenceKind::Body
        )));
        assert!(visited.contains(&(
            ReferenceOrigin::Symbol(signature_function),
            (class, empty_word()),
            SymbolReferenceKind::Signature,
        )));
        assert!(visited.contains(&(ReferenceOrigin::Symbol(method), property, SymbolReferenceKind::PropertyRead)));
        assert!(visited.contains(&(ReferenceOrigin::Symbol(function), property, SymbolReferenceKind::PropertyWrite)));
        assert!(visited.contains(&(
            ReferenceOrigin::Symbol(override_source),
            method,
            SymbolReferenceKind::OverriddenMember,
        )));
        assert!(visited.contains(&(
            ReferenceOrigin::Symbol(return_source),
            method,
            SymbolReferenceKind::FunctionLikeReturn,
        )));
        assert!(visited.contains(&(ReferenceOrigin::File(file), method, SymbolReferenceKind::Body)));
        assert!(visited.contains(&(ReferenceOrigin::File(file), property, SymbolReferenceKind::Signature)));
        assert!(visited.contains(&(ReferenceOrigin::File(file), file_property, SymbolReferenceKind::PropertyRead)));
        assert!(visited.contains(&(ReferenceOrigin::File(file), file_property, SymbolReferenceKind::PropertyWrite)));
        assert_eq!(references.count_property_reads(&file_property), 1);
        assert_eq!(references.count_property_writes(&file_property), 1);
    }

    #[test]
    fn test_restore_references_for_safe_symbols_restores_missing_body_refs() {
        let class_a = word("class_a");
        let class_b = word("class_b");
        let method_foo = word("foo");
        let method_bar = word("bar");

        let previous = make_refs_with_body(vec![
            ((class_a, method_foo), vec![(class_b, empty_word())]),
            ((class_b, method_bar), vec![(class_a, empty_word())]),
        ]);

        let mut current = make_refs_with_body(vec![((class_b, method_bar), vec![(class_a, empty_word())])]);

        let safe_symbols = WordSet::default();
        let mut safe_members = HashSet::default();
        safe_members.insert((class_a, method_foo));

        current.restore_references_for_safe_symbols(&previous, &safe_symbols, &safe_members);

        assert!(current.symbol_references_to_symbols.contains_key(&(class_a, method_foo)));
        let restored = &current.symbol_references_to_symbols[&(class_a, method_foo)];
        assert!(restored.contains(&(class_b, empty_word())));

        assert!(current.symbol_references_to_symbols.contains_key(&(class_b, method_bar)));
    }

    #[test]
    fn test_restore_references_does_not_overwrite_existing() {
        let class_a = word("class_a");
        let class_b = word("class_b");
        let class_c = word("class_c");
        let method_foo = word("foo");

        let previous = make_refs_with_body(vec![((class_a, method_foo), vec![(class_b, empty_word())])]);

        let mut current = make_refs_with_body(vec![((class_a, method_foo), vec![(class_c, empty_word())])]);

        let safe_symbols = WordSet::default();
        let mut safe_members = HashSet::default();
        safe_members.insert((class_a, method_foo));

        current.restore_references_for_safe_symbols(&previous, &safe_symbols, &safe_members);

        let refs = &current.symbol_references_to_symbols[&(class_a, method_foo)];
        assert!(refs.contains(&(class_c, empty_word())));
        assert!(!refs.contains(&(class_b, empty_word())));
    }

    #[test]
    fn test_restore_references_for_safe_top_level_symbols() {
        let func_a = word("func_a");
        let class_b = word("class_b");

        let previous = make_refs_with_body(vec![((func_a, empty_word()), vec![(class_b, empty_word())])]);

        let mut current = SymbolReferences::new();

        let mut safe_symbols = WordSet::default();
        safe_symbols.insert(func_a);
        let safe_members = HashSet::default();

        current.restore_references_for_safe_symbols(&previous, &safe_symbols, &safe_members);

        assert!(current.symbol_references_to_symbols.contains_key(&(func_a, empty_word())));
        let restored = &current.symbol_references_to_symbols[&(func_a, empty_word())];
        assert!(restored.contains(&(class_b, empty_word())));
    }

    #[test]
    fn test_restore_skips_non_safe_symbols() {
        let func_a = word("func_a");
        let class_b = word("class_b");
        let previous = make_refs_with_body(vec![((func_a, empty_word()), vec![(class_b, empty_word())])]);

        let mut current = SymbolReferences::new();

        let safe_symbols = WordSet::default();
        let safe_members = HashSet::default();

        current.restore_references_for_safe_symbols(&previous, &safe_symbols, &safe_members);

        assert!(!current.symbol_references_to_symbols.contains_key(&(func_a, empty_word())));
    }

    #[test]
    fn test_get_invalid_symbols_basic_cascade() {
        let class_a = word("class_a");
        let class_b = word("class_b");
        let method_foo = word("foo");

        let mut refs = SymbolReferences::new();
        refs.symbol_references_to_symbols_in_signature.insert_set((class_b, method_foo), {
            let mut set = HashSet::default();
            set.insert((class_a, empty_word()));
            set
        });

        let mut diff = crate::diff::CodebaseDiff::new();
        let mut changed = HashSet::default();
        changed.insert((class_a, empty_word()));
        diff = diff.with_changed(changed);

        let (invalid, partially_invalid, invalid_files, _) =
            refs.get_invalid_symbols(&CodebaseMetadata::default(), &diff, &HashSet::default());

        assert!(invalid.contains(&(class_a, empty_word())));
        assert!(invalid.contains(&(class_b, method_foo)));
        assert!(partially_invalid.contains(&class_b));
        assert!(invalid_files.is_empty());
    }

    #[test]
    fn test_get_invalid_symbols_tracks_file_origins() {
        let changed_class = word("changed_class");
        let file = word("src/bootstrap.php");
        let mut references = SymbolReferences::new();
        references.add_reference(ReferenceOrigin::File(file), (changed_class, empty_word()), false);

        let mut diff = crate::diff::CodebaseDiff::new();
        let mut changed = HashSet::default();
        changed.insert((changed_class, empty_word()));
        diff = diff.with_changed(changed);

        let (_, _, invalid_files, _) =
            references.get_invalid_symbols(&CodebaseMetadata::default(), &diff, &HashSet::default());
        assert_eq!(invalid_files.len(), 1);
        assert!(invalid_files.contains(&file));
    }

    #[test]
    fn test_get_invalid_symbols_tells_a_read_of_a_class_whose_member_changed() {
        let parent = word("parent");
        let child = word("child");
        let mut references = SymbolReferences::new();
        references.add_symbol_reference_to_symbol(child, parent, true);

        let mut diff = crate::diff::CodebaseDiff::new();
        let mut changed = HashSet::default();
        changed.insert((parent, word("run")));
        diff = diff.with_changed(changed);

        let read_invalid = |read: HashSet<SymbolIdentifier>| {
            references.get_invalid_symbols(&CodebaseMetadata::default(), &diff, &read).3
        };
        assert!(read_invalid(HashSet::from_iter([(child, empty_word())])));
        assert!(!read_invalid(HashSet::from_iter([(word("other"), empty_word())])));
    }

    /// An attribute's references are `Attribute` edges, which every other reader takes as the signature and body
    /// references [`SymbolReferences::add_reference`] records for the same calls.
    #[test]
    fn an_attribute_s_references_are_attribute_edges_read_as_the_references_they_stand_for() {
        let class = (word("order"), empty_word());
        let method = (word("order"), word("total"));
        let attribute = (word("tag"), empty_word());
        let constructor = (word("tag"), word("__construct"));
        let record = |add: fn(&mut SymbolReferences, ReferenceOrigin, SymbolIdentifier, bool)| {
            let mut references = SymbolReferences::new();
            add(&mut references, ReferenceOrigin::Symbol(class), attribute, true);
            add(&mut references, ReferenceOrigin::Symbol(class), constructor, false);
            add(&mut references, ReferenceOrigin::Symbol(method), attribute, true);
            add(&mut references, ReferenceOrigin::Symbol(method), constructor, false);
            references
        };
        let attributes = record(SymbolReferences::add_attribute_reference);
        let code = record(SymbolReferences::add_reference);
        let codebase = CodebaseMetadata::default();

        assert!(code.cascade_edges(&codebase).all(|edge| !matches!(edge, CascadeEdge::Attribute { .. })));
        assert!(attributes.cascade_edges(&codebase).all(|edge| matches!(edge, CascadeEdge::Attribute { .. })));
        let visited = |references: &SymbolReferences| {
            let mut visited = HashSet::default();
            references.for_each_reference(|source, target, kind| {
                visited.insert((source, target, kind));
            });
            visited
        };
        assert_eq!(visited(&attributes), visited(&code));
        assert_eq!(attributes.get_references_to_symbol(attribute), code.get_references_to_symbol(attribute));
        for in_signature in [true, false] {
            assert_eq!(
                attributes.count_referencing_symbols(&attribute, in_signature),
                code.count_referencing_symbols(&attribute, in_signature)
            );
        }
        let diff = crate::diff::CodebaseDiff::new().with_changed(HashSet::from_iter([attribute]));
        let invalid =
            |references: &SymbolReferences| references.get_invalid_symbols(&codebase, &diff, &HashSet::default());
        assert_eq!(invalid(&attributes), invalid(&code));
    }
    /// A change whose cascade reaches more than 5,000 symbols invalidates every one of them: a chain of 6,000 classes,
    /// each of whose signatures references the next, is invalid from its end to its start.
    #[test]
    fn a_cascade_through_6000_signatures_invalidates_each_one() {
        let class = |index: usize| word(format!("class_{index}"));
        let mut references = SymbolReferences::new();
        for index in 1..6000 {
            references.add_symbol_reference_to_symbol(class(index), class(index - 1), true);
        }
        let diff = crate::diff::CodebaseDiff::new().with_changed(HashSet::from_iter([(class(0), empty_word())]));

        let (invalid, _, _, _) =
            references.get_invalid_symbols(&CodebaseMetadata::default(), &diff, &HashSet::default());

        assert!((0..6000).all(|index| invalid.contains(&(class(index), empty_word()))));
    }
}
