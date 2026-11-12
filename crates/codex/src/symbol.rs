use mago_word::WordSet;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use mago_word::Word;
use mago_word::WordMap;

/// A pair of `Word`s representing a symbol and its member.
///
/// This is used to uniquely identify a symbol and its member within the codebase,
/// where the first `Word` is the symbol's fully qualified class name (FQCN)
/// and the second `Word` is the member's name (e.g., method, property, constant),
/// or an empty string if the symbol itself is being referenced (e.g., a class or function
/// without a specific member).
pub type SymbolIdentifier = (Word, Word);

/// Represents the different kinds of top-level class-like structures in PHP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SymbolKind {
    Class,
    Enum,
    Trait,
    Interface,
}

impl SymbolKind {
    /// Checks if this symbol kind is `Class`.
    #[inline]
    #[must_use]
    pub const fn is_class(&self) -> bool {
        matches!(self, SymbolKind::Class)
    }

    /// Checks if this symbol kind is `Enum`.
    #[inline]
    #[must_use]
    pub const fn is_enum(&self) -> bool {
        matches!(self, SymbolKind::Enum)
    }

    /// Checks if this symbol kind is `Trait`.
    #[inline]
    #[must_use]
    pub const fn is_trait(&self) -> bool {
        matches!(self, SymbolKind::Trait)
    }

    /// Checks if this symbol kind is `Interface`.
    #[inline]
    #[must_use]
    pub const fn is_interface(&self) -> bool {
        matches!(self, SymbolKind::Interface)
    }

    /// Returns the string representation of the symbol kind.
    #[inline]
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            SymbolKind::Class => "class",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Interface => "interface",
        }
    }
}

/// Stores a map of all known class-like symbol names (FQCNs) to their corresponding `SymbolKind`.
/// Provides basic methods for adding symbols and querying.
///
/// Each symbol is held under its key, spec section 24: a PHP# class-like's exact name, and a plain PHP one's lowercase
/// name, as [`Symbols::class_key`] finds them.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Symbols {
    all: WordMap<SymbolKind>,
    namespaces: WordSet,
    /// The key of each PHP# class-like, its exact name.
    #[cfg_attr(feature = "serde", serde(default))]
    sharp: WordSet,
}

impl Symbols {
    /// Creates a new, empty `Symbols` map.
    #[inline]
    #[must_use]
    pub fn new() -> Symbols {
        Symbols::default()
    }

    /// Adds or updates a symbol name identified as the given kind. Its namespaces are kept lowercase, since a namespace
    /// is found in any case, whatever case the key of a class-like in it keeps.
    #[inline]
    pub fn add_symbol_name(&mut self, name: Word, kind: SymbolKind) {
        self.namespaces.extend(get_symbol_namespaces(ascii_lowercase_word(name.as_bytes())));
        self.all.insert(name, kind);
    }

    /// Adds or updates the class-like a file declares, by its key `name`, identified as the given kind: a PHP# one's
    /// when `sharp`, whose key is its exact name.
    #[inline]
    pub fn add_class_like(&mut self, name: Word, kind: SymbolKind, sharp: bool) {
        self.add_symbol_name(name, kind);
        if sharp {
            self.sharp.insert(name);
        }
    }

    /// The key of the class-like `name` names, spec section 24: a PHP# class-like's exact name, so a name in another
    /// case names no PHP# class-like, and a plain PHP one's lowercase name, so it is found in any case, as PHP finds
    /// it. A name of no known symbol keys as a plain PHP one would, and a key is its own key.
    #[must_use]
    pub fn class_key(&self, name: &[u8]) -> Word {
        let exact = word(name);
        if self.all.contains_key(&exact) {
            return exact;
        }

        let lowercase = ascii_lowercase_word(name);
        // A PHP# class-like whose whole name is lowercase, as `order`, is no plain one that `ORDER` finds.
        if self.sharp.contains(&lowercase) { exact } else { lowercase }
    }

    /// The key of the PHP# class-like `name` names in another case, as `Demo\ORDER` names `Demo\Order`, when `name`
    /// names no class-like itself, spec section 24: the first by its bytes when several PHP# class-likes differ from
    /// `name` only in case. It is for the error that names the class-like the code meant.
    #[must_use]
    pub fn sharp_class_like_in_another_case(&self, name: &[u8]) -> Option<Word> {
        if self.contains(self.class_key(name)) {
            return None;
        }

        self.sharp
            .iter()
            .filter(|key| key.as_bytes().eq_ignore_ascii_case(name))
            .min_by_key(|key| key.as_bytes())
            .copied()
    }

    /// Retrieves the `SymbolKind` for a given symbol name, if known.
    ///
    /// # Arguments
    ///
    /// * `name`: The `Word` (likely FQCN) of the symbol to look up.
    ///
    /// # Returns
    ///
    /// `Some(SymbolKind)` if the symbol exists in the map, `None` otherwise.
    #[inline]
    #[must_use]
    pub fn get_kind(&self, name: Word) -> Option<SymbolKind> {
        self.all.get(&name).copied() // Use copied() since SymbolKind is Copy
    }

    /// Checks if a symbol with the given name is known.
    ///
    /// # Arguments
    ///
    /// * `name`: The `Word` (likely FQCN) of the symbol to check.
    ///
    /// # Returns
    ///
    /// `true` if the symbol exists in the map, `false` otherwise.
    #[inline]
    #[must_use]
    pub fn contains(&self, name: Word) -> bool {
        self.all.contains_key(&name)
    }

    /// Check if any symbol within the table is part of the given namespace.
    ///
    /// # Arguments
    ///
    /// * `namespace`: The `Word` of the namespace to check for.
    ///
    /// # Returns
    ///
    /// `true` if the namespace is present, `false` otherwise.
    #[must_use]
    pub fn contains_namespace(&self, namespace: Word) -> bool {
        self.namespaces.contains(&namespace)
    }

    /// Checks if a symbol with the given name is an `Enum`.
    ///
    /// # Arguments
    ///
    /// * `name`: The `Word` (likely FQCN) of the symbol to check.
    ///
    /// # Returns
    ///
    /// `true` if the symbol is an `Enum`, `false` otherwise.
    #[inline]
    #[must_use]
    pub fn contains_enum(&self, name: Word) -> bool {
        matches!(self.get_kind(name), Some(SymbolKind::Enum))
    }

    /// Extends the current `Symbols` map with another one.
    #[inline]
    pub fn extend(&mut self, other: Symbols) {
        self.namespaces.extend(other.namespaces);
        self.sharp.extend(other.sharp);
        for (entry, kind) in other.all {
            self.all.entry(entry).or_insert(kind);
        }
    }

    /// Extends the current `Symbols` map from a reference without consuming the source.
    #[inline]
    pub fn extend_ref(&mut self, other: &Symbols) {
        self.namespaces.extend(other.namespaces.iter().copied());
        self.sharp.extend(other.sharp.iter().copied());

        for (entry, kind) in &other.all {
            self.all.entry(*entry).or_insert(*kind);
        }
    }

    /// Removes a symbol by its key.
    ///
    /// Note: does not remove namespaces (they may be shared by other symbols).
    #[inline]
    pub fn remove(&mut self, name: Word) {
        self.all.remove(&name);
        self.sharp.remove(&name);
    }
}

/// Returns an iterator that yields all parent namespaces of a given symbol.
///
/// For example, if the symbol is `Foo\Bar\Baz\Qux`, the iterator yields:
/// 1. `Foo`
/// 2. `Foo\Bar`
/// 3. `Foo\Bar\Baz`
pub(super) fn get_symbol_namespaces(symbol_name: Word) -> impl Iterator<Item = Word> {
    let bytes: Vec<u8> = symbol_name.as_bytes().to_vec();

    (0..bytes.len()).filter_map(move |i| if bytes[i] == b'\\' { Some(word(&bytes[..i])) } else { None })
}
