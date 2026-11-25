use std::borrow::Cow;
use std::collections::VecDeque;
use std::collections::hash_map::Entry;

use foldhash::HashMap;
use foldhash::HashSet;

use mago_database::file::DevPackage;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_database::file::FolderNamespace;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_span::Span;
use mago_word::Word;
use mago_word::WordMap;
use mago_word::WordSet;
use mago_word::ascii_lowercase_constant_name_word;
use mago_word::ascii_lowercase_word;
use mago_word::concat_word;
use mago_word::empty_word;
use mago_word::word;

use crate::DEFAULTS_TRAIT_SUFFIX;
use crate::assertion::Assertion;
use crate::defaults_trait_name;
use crate::diff::CodebaseDiff;
use crate::graph::strongly_connected_parts;
use crate::identifier::method::MethodIdentifier;
use crate::issue::ScanningIssueKind;
use crate::metadata::attribute::AttributeMetadata;
use crate::metadata::class_like::ClassLikeMetadata;
use crate::metadata::class_like::ParsedField;

use crate::metadata::class_like_constant::ClassLikeConstantMetadata;
use crate::metadata::constant::ConstantMetadata;
use crate::metadata::enum_case::EnumCaseMetadata;
use crate::metadata::r#extern::ExternMetadata;
use crate::metadata::r#extern::ExternNamespaceMetadata;
use crate::metadata::flags::MetadataFlags;
use crate::metadata::function_like::FunctionLikeMetadata;
use crate::metadata::module::ModuleMetadata;
use crate::metadata::module::NamespaceImportMetadata;
use crate::metadata::parameter::FunctionLikeParameterMetadata;
use crate::metadata::property::PropertyMetadata;
use crate::metadata::shared::Shared;
use crate::metadata::ttype::TypeMetadata;
use crate::reference::SymbolReferences;
use crate::signature::FileSignature;
use crate::symbol::SymbolIdentifier;
use crate::symbol::SymbolKind;
use crate::symbol::Symbols;
use crate::ttype::TType;
use crate::ttype::TypeRef;
use crate::ttype::atomic::TAtomic;
use crate::ttype::atomic::object::TObject;
use crate::ttype::template::GenericTemplate;
use crate::ttype::union::TUnion;
use crate::visibility::Visibility;

pub mod attribute;
pub mod class_like;
pub mod class_like_constant;
pub mod constant;
pub mod enum_case;
pub mod r#extern;
pub mod flags;
pub mod function_like;
pub mod machine;
pub mod module;
pub mod parameter;
pub mod property;
pub mod property_hook;
pub mod shared;
pub mod ttype;
pub mod version_constraint;

/// Lightweight set of keys extracted from a per-file [`CodebaseMetadata`].
///
/// Used by the incremental engine to efficiently remove a file's contributions from the
/// merged codebase without keeping a full `CodebaseMetadata` clone per file.
/// Created via [`CodebaseMetadata::extract_keys()`].
#[derive(Debug, Clone)]
pub struct CodebaseEntryKeys {
    /// Class-like FQCN atoms (also used for symbol removal).
    pub class_like_names: Vec<Word>,
    pub class_like_aliases: Vec<(Word, Word, Span)>,
    /// Function-like `(scope, name)` tuples.
    pub function_like_keys: Vec<(Word, Word)>,
    /// Constant FQN atoms.
    pub constant_names: Vec<Word>,
    /// File IDs that had signatures in this metadata.
    pub file_ids: Vec<FileId>,
}

/// A member a class-like declares, by the name the codex keys it by: a property's with its `$`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Member {
    Method(Word),
    Property(Word),
    Constant(Word),
}

/// Holds all analyzed information about the symbols, structures, and relationships within a codebase.
///
/// This acts as the central repository for metadata gathered during static analysis,
/// including details about classes, interfaces, traits, enums, functions, constants,
/// their members, inheritance, dependencies, and associated types.
#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
#[allow(clippy::unsafe_derive_deserialize)]
pub struct CodebaseMetadata {
    /// Configuration flag: Should types be inferred based on usage patterns?
    pub infer_types_from_usage: bool,
    /// Map from class-like FQCN (`Word`) to its detailed metadata (`ClassLikeMetadata`), which each copy of the
    /// codebase shares until it changes the entry.
    pub class_likes: WordMap<Shared<ClassLikeMetadata>>,
    /// How many class-likes of the project, its vendors and the standard library have each short name, by the short
    /// name in lowercase. Made again on each population.
    #[cfg_attr(feature = "serde", serde(skip))]
    short_names: WordMap<u32>,
    #[cfg_attr(feature = "serde", serde(default))]
    pub class_like_aliases: WordMap<Word>,
    #[cfg_attr(feature = "serde", serde(default))]
    class_like_alias_declarations: WordMap<(Word, Span, MetadataFlags)>,
    #[cfg_attr(feature = "serde", serde(skip, default))]
    class_like_aliases_dirty: bool,
    /// Map from a function/method identifier tuple `(scope_id, function_id)` to its metadata (`FunctionLikeMetadata`).
    /// `scope_id` is the FQCN for methods or often `Word::empty()` for global functions. Each copy of the codebase
    /// shares an entry until it changes it.
    pub function_likes: HashMap<(Word, Word), Shared<FunctionLikeMetadata>>,
    /// Stores the kind (Class, Interface, etc.) for every known symbol FQCN.
    pub symbols: Symbols,
    /// Map from global constant FQN (`Word`) to its metadata (`ConstantMetadata`).
    pub constants: WordMap<ConstantMetadata>,
    /// Map from the target of a PHP# `extern` declaration, keyed as [`ExternMetadata::target`], to every declaration
    /// of it in the order [`ExternMetadata`] sorts them. The first one declares the target, and each later one is a
    /// duplicate.
    #[cfg_attr(feature = "serde", serde(default))]
    pub externs: HashMap<(Word, Word), Vec<ExternMetadata>>,
    /// The method or function a PHP# caller sees for each plain PHP method and function whose `extern` line states its
    /// signature, spec section 23: its own, with the line's parameters and return type. Keyed as `function_likes` keys
    /// it, and made again on each population.
    #[cfg_attr(feature = "serde", serde(default))]
    pub extern_signatures: HashMap<(Word, Word), Shared<FunctionLikeMetadata>>,
    /// Every PHP# module, spec section 32, in the order [`ModuleMetadata`] sorts them.
    #[cfg_attr(feature = "serde", serde(default))]
    pub modules: Vec<ModuleMetadata>,
    /// Every PHP# `extern namespace`, spec section 23, sorted.
    #[cfg_attr(feature = "serde", serde(default))]
    pub extern_namespaces: Vec<ExternNamespaceMetadata>,
    /// Every name a PHP# file of the project imports from another namespace, spec section 23, sorted.
    #[cfg_attr(feature = "serde", serde(default))]
    pub namespace_imports: Vec<NamespaceImportMetadata>,
    /// Each import of `namespace_imports` that closes a cycle, by its span: the import, then the shortest chain of
    /// imports that leads from the namespace it imports back to its file's. Made again on each population.
    #[cfg_attr(feature = "serde", serde(skip))]
    import_cycles: HashMap<Span, Vec<NamespaceImportMetadata>>,
    /// Map from the lowercase name of a PHP# extension member, spec section 26, to every extension member of that name:
    /// its span and the static method it runs as, in span order.
    #[cfg_attr(feature = "serde", serde(default))]
    pub extensions: WordMap<Vec<(Span, MethodIdentifier)>>,
    /// Map from class/interface FQCN to the set of all its descendants (recursive).
    pub all_class_like_descendants: WordMap<WordSet>,
    /// Map from class/interface FQCN to the set of its direct descendants (children).
    pub direct_classlike_descendants: WordMap<WordSet>,
    /// Set of symbols (FQCNs) that are considered safe/validated.
    pub safe_symbols: WordSet,
    /// Set of specific members `(SymbolFQCN, MemberName)` that are considered safe/validated.
    pub safe_symbol_members: HashSet<(Word, Word)>,
    /// Each `FileSignature` contains a hierarchical tree of `DefSignatureNode` representing
    /// top-level symbols (classes, functions, constants) and their nested members (methods, properties).
    pub file_signatures: HashMap<FileId, FileSignature>,
    /// Per-patch class-like metadata, keyed by FQCN.
    ///
    /// Vendor and patch files declare symbols under the same FQCN, so patches cannot share
    /// the `class_likes` map. At most one patch may target a given symbol; a second patch for
    /// the same FQCN is diagnosed as a [`PatchDuplicateTarget`](ScanningIssueKind::PatchDuplicateTarget)
    /// rather than silently overwriting the first. Entries here are folded into `class_likes`
    /// by [`apply_patches_pass`](Self::apply_patches_pass).
    pub patch_class_likes: WordMap<ClassLikeMetadata>,
    /// Per-patch function-like metadata, keyed by `(scope, name)`.
    ///
    /// The key matches the existing `function_likes` key shape: the FQCN for methods,
    /// `empty_word()` for free functions.
    pub patch_function_likes: HashMap<(Word, Word), FunctionLikeMetadata>,
    /// Per-patch constant metadata, keyed by FQN.
    pub patch_constants: WordMap<ConstantMetadata>,
    /// The name of each host or vendored file the scan read, its path from the workspace, or its absolute path for a
    /// file outside the workspace, keyed by the file. Of two declarations of one name, the one whose file's name sorts
    /// first keeps it, and the duplicate's error names that file.
    #[cfg_attr(feature = "serde", serde(default))]
    file_names: HashMap<FileId, Word>,
    /// The files of the standard library, the PHP# Composer package, the only code that reaches PHP's root namespace
    /// through `global`, spec section 23.
    #[cfg_attr(feature = "serde", serde(default))]
    standard_library_files: HashSet<FileId>,
    /// The test files, those of a folder `composer.json` lists under `autoload-dev`, spec section 5: their imports are
    /// no edges of the namespace graph, a module's `imports` line doesn't limit them, and production code can't use a
    /// class one declares, spec sections 23 and 32.
    #[cfg_attr(feature = "serde", serde(default))]
    test_files: HashSet<FileId>,
    /// The dev-only packages each production PHP# file can't use, since a production install leaves them out, by the
    /// file, spec section 5.
    #[cfg_attr(feature = "serde", serde(default))]
    dev_packages: HashMap<FileId, Vec<DevPackage>>,
    /// Each user-defined class-like population filled, as its scan gave it, by its key. Population fills such a
    /// class-like again from it, as a full analysis does, so nothing an earlier fill derived stays.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(crate) scanned_class_likes: WordMap<Shared<ClassLikeMetadata>>,
}

impl CodebaseMetadata {
    /// Creates a new, empty `CodebaseMetadata` with default values.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn add_class_like_alias(&mut self, alias: Word, target: Word, span: Span, flags: MetadataFlags) {
        let replace =
            self.class_like_alias_declarations.get(&alias).is_none_or(|(_, existing_span, existing_flags)| {
                should_replace_metadata(
                    &self.file_names,
                    &self.standard_library_files,
                    (*existing_flags, *existing_span),
                    (flags, span),
                )
            });

        if replace {
            self.class_like_alias_declarations.insert(alias, (target, span, flags));
            self.class_like_aliases_dirty = true;
        }
    }

    #[inline]
    pub fn class_like_alias_declarations(&self) -> impl Iterator<Item = (Word, Word, Span)> + '_ {
        self.class_like_alias_declarations.iter().map(|(alias, (target, span, _))| (*alias, *target, *span))
    }

    /// Every class-like a scan of this codebase's files names, each as written: the parents, interfaces, traits,
    /// requirements, inheritors, attributes and alias targets of its class-likes, and each class-like a type its
    /// declarations, docblocks, constants and enum cases give names, which the population resolves.
    #[must_use]
    pub fn named_class_likes(&self) -> WordSet {
        let mut names = WordSet::default();
        let mut unions: Vec<&TUnion> = Vec::new();
        let mut atomics: Vec<&TAtomic> = Vec::new();
        let mut attributes: Vec<&AttributeMetadata> = Vec::new();

        for class_like in self.class_likes.values() {
            names.extend(class_like.direct_parent_class);
            names.extend(&class_like.direct_parent_interfaces);
            names.extend(&class_like.used_traits);
            names.extend(&class_like.require_extends);
            names.extend(&class_like.require_implements);
            names.extend(class_like.permitted_inheritors.iter().flatten());
            names.extend(class_like.imported_type_aliases.values().map(|(class, _, _)| *class));
            attributes.extend(&class_like.attributes);

            for property in class_like.properties.values().chain(class_like.magic_properties.values()) {
                attributes.extend(&property.attributes);
                unions.extend(
                    [
                        &property.type_declaration_metadata,
                        &property.type_metadata,
                        &property.write_type_metadata,
                        &property.default_type_metadata,
                    ]
                    .into_iter()
                    .flatten()
                    .map(|metadata| &metadata.type_union),
                );
                for hook in property.hooks.values() {
                    attributes.extend(&hook.attributes);
                    unions.extend(hook.return_type_metadata.iter().map(|metadata| &metadata.type_union));
                    if let Some(parameter) = &hook.parameter {
                        parameter_types(parameter, &mut unions, &mut attributes);
                    }
                }
            }

            for template in class_like.template_types.values() {
                template_types(template, &mut unions);
            }
            unions.extend(class_like.template_extended_offsets.values().flatten());
            unions.extend(class_like.template_extended_parameters.values().flat_map(|parameters| parameters.values()));
            unions.extend(
                class_like
                    .template_extended_parameter_paths
                    .values()
                    .flatten()
                    .flat_map(|parameters| parameters.values()),
            );
            unions.extend(class_like.type_aliases.values().map(|alias| &alias.type_union));
            unions.extend(class_like.mixins.iter().map(|mixin| &mixin.type_union));

            for constant in class_like.constants.values() {
                attributes.extend(&constant.attributes);
                unions.extend(
                    [&constant.type_declaration, &constant.type_metadata]
                        .into_iter()
                        .flatten()
                        .map(|metadata| &metadata.type_union),
                );
                atomics.extend(&constant.inferred_type);
            }
            for case in class_like.enum_cases.values() {
                attributes.extend(&case.attributes);
                atomics.extend(&case.value_type);
            }
            atomics.extend(&class_like.enum_type);

            for law in class_like.laws.values() {
                function_like_types(law, &mut names, &mut unions, &mut atomics, &mut attributes);
            }
        }

        for function_like in self.function_likes.values() {
            function_like_types(function_like, &mut names, &mut unions, &mut atomics, &mut attributes);
        }

        for constant in self.constants.values() {
            attributes.extend(&constant.attributes);
            unions.extend(constant.type_metadata.iter().map(|metadata| &metadata.type_union));
            unions.extend(&constant.inferred_type);
        }

        names.extend(self.class_like_alias_declarations().map(|(_, target, _)| target));
        names.extend(attributes.into_iter().map(|attribute| attribute.name));
        names.extend(
            unions
                .into_iter()
                .flat_map(TType::get_all_child_nodes)
                .chain(atomics.into_iter().flat_map(|atomic| {
                    let mut nodes = atomic.get_all_child_nodes();
                    nodes.push(TypeRef::Atomic(atomic));
                    nodes
                }))
                .filter_map(TypeRef::named_class_like),
        );

        names
    }

    pub(crate) fn populate_class_like_aliases(&mut self) -> bool {
        if !self.class_like_aliases_dirty {
            return false;
        }

        let previous_aliases = std::mem::take(&mut self.class_like_aliases);
        for alias in previous_aliases.keys().copied() {
            if let Some(metadata) = self.class_likes.get(&alias) {
                self.symbols.add_symbol_name(alias, metadata.kind);
            } else {
                self.symbols.remove(alias);
            }
        }

        for metadata in self.class_likes.values_mut() {
            if !metadata.aliases.is_empty() {
                metadata.aliases.clear();
            }
        }

        let mut aliases = WordMap::default();
        for alias in self.class_like_alias_declarations.keys().copied() {
            if self.class_likes.contains_key(&alias) {
                continue;
            }

            if let Some(actual) = self.resolve_class_like_alias_declaration(alias) {
                aliases.insert(alias, actual);
            }
        }

        let aliases_changed = aliases != previous_aliases;
        self.class_like_aliases = aliases;
        let mut aliased_classes = WordSet::default();
        for (alias, actual) in &self.class_like_aliases {
            let Some(metadata) = self.class_likes.get_mut(actual) else {
                continue;
            };

            metadata.aliases.push(*alias);
            aliased_classes.insert(*actual);
            self.symbols.add_symbol_name(*alias, metadata.kind);
        }

        for actual in aliased_classes {
            if let Some(metadata) = self.class_likes.get_mut(&actual) {
                metadata.aliases.sort_unstable();
            }
        }

        self.class_like_aliases_dirty = false;
        aliases_changed
    }

    /// Counts the class-likes of each short name that [`Self::shares_short_name`] reads.
    pub(crate) fn populate_short_names(&mut self) {
        let mut short_names = WordMap::default();
        for (name, metadata) in &self.class_likes {
            let name = name.as_bytes();
            if !metadata.flags.is_built_in() || (name.len() > 6 && name[..6].eq_ignore_ascii_case(b"Sharp\\")) {
                *short_names.entry(short_name_key(name)).or_insert(0) += 1;
            }
        }

        self.short_names = short_names;
    }

    /// Finds each import that closes a cycle of imports between namespaces, spec section 23. The namespaces of one
    /// strongly connected part of the import graph lead to each other, so an import closes a cycle when the namespace
    /// it imports is another one of its own namespace's part, and one walk breadth first from that namespace, over the
    /// imports in their order, finds the shortest chain back to every namespace of the part.
    pub(crate) fn populate_import_cycles(&mut self) {
        let mut nodes = WordMap::default();
        let mut node = |namespace: &[u8]| {
            let next = nodes.len();
            *nodes.entry(ascii_lowercase_word(namespace)).or_insert(next)
        };
        let edges: Vec<(usize, usize)> = self
            .namespace_imports
            .iter()
            .map(|import| (node(import.namespace.as_bytes()), node(self.imported_namespace(import.name.as_bytes()))))
            .collect();
        let mut outgoing = vec![Vec::new(); nodes.len()];
        for (import, &(from, _)) in edges.iter().enumerate() {
            outgoing[from].push(import);
        }
        let successors: Vec<Vec<usize>> =
            outgoing.iter().map(|imports| imports.iter().map(|&import| edges[import].1).collect()).collect();
        let mut parts = vec![0; nodes.len()];
        for (part, members) in strongly_connected_parts(&successors).into_iter().enumerate() {
            for member in members {
                parts[member] = part;
            }
        }

        let mut walks: HashMap<usize, Vec<Option<usize>>> = HashMap::default();
        let mut cycles = HashMap::default();
        for (own, &(from, start)) in edges.iter().enumerate() {
            if from == start || parts[from] != parts[start] {
                continue;
            }

            let reached_by = walks.entry(start).or_insert_with(|| {
                let mut reached_by = vec![None; outgoing.len()];
                let mut pending = VecDeque::from([start]);
                while let Some(namespace) = pending.pop_front() {
                    for &import in &outgoing[namespace] {
                        let next = edges[import].1;
                        if next != start && reached_by[next].is_none() {
                            reached_by[next] = Some(import);
                            pending.push_back(next);
                        }
                    }
                }

                reached_by
            });
            let mut chain = Vec::new();
            let mut at = from;
            while at != start
                && let Some(import) = reached_by[at]
            {
                chain.push(self.namespace_imports[import]);
                at = edges[import].0;
            }
            chain.push(self.namespace_imports[own]);
            chain.reverse();
            cycles.insert(self.namespace_imports[own].span, chain);
        }

        self.import_cycles = cycles;
    }

    /// The namespace an import of `name` reaches: the namespace of the class-like, function or constant it names, or
    /// the namespace it names itself.
    fn imported_namespace<'name>(&self, name: &'name [u8]) -> &'name [u8] {
        let parent = name.iter().rposition(|byte| *byte == b'\\').map_or(&name[..0], |separator| &name[..separator]);
        let named = word(name);
        if !self.class_like_exists(named) && self.namespace_exists(named) { name } else { parent }
    }

    /// The class-like `alias` names in the end, by its key: a `class_alias()` declaration names its target as written,
    /// which [`Self::class_key`] keys, exact for a PHP# class-like, spec section 24, and lowercase for an alias.
    fn resolve_class_like_alias_declaration(&self, alias: Word) -> Option<Word> {
        let mut current = self.class_key(self.class_like_alias_declarations.get(&alias)?.0);

        for _ in 0..self.class_like_alias_declarations.len() {
            if self.class_likes.contains_key(&current) {
                return Some(current);
            }

            current = self.class_key(self.class_like_alias_declarations.get(&current)?.0);
        }

        None
    }

    #[inline]
    pub(crate) fn resolve_class_like_word(&self, name: Word) -> Option<Word> {
        if self.class_likes.contains_key(&name) {
            return Some(name);
        }

        if self.class_like_aliases.is_empty() {
            return None;
        }

        self.class_like_aliases.get(&name).copied()
    }

    #[inline]
    pub(crate) fn get_class_like_by_word(&self, name: Word) -> Option<&ClassLikeMetadata> {
        if let Some(metadata) = self.class_likes.get(&name) {
            return Some(metadata);
        }

        if self.class_like_aliases.is_empty() {
            return None;
        }

        self.class_like_aliases.get(&name).and_then(|actual| self.class_likes.get(actual)).map(|metadata| &**metadata)
    }

    #[inline]
    fn get_method_by_words(&self, class: Word, method: Word) -> Option<&FunctionLikeMetadata> {
        if let Some(metadata) = self.function_likes.get(&(class, method)) {
            return Some(metadata);
        }

        if self.class_like_aliases.is_empty() {
            return None;
        }

        let actual = self.class_like_aliases.get(&class)?;
        self.function_likes.get(&(*actual, method)).map(|metadata| &**metadata)
    }

    /// The key of the class-like `name` names in `class_likes` and every map of class-likes, as [`Symbols::class_key`]
    /// gives it, spec section 24: a PHP# class-like's exact name, as [`class_like::class_like_key`] gives it, and a
    /// plain PHP one's lowercase name.
    #[inline]
    #[must_use]
    pub fn class_key(&self, name: Word) -> Word {
        self.symbols.class_key(name)
    }

    /// The key the class-like `class`, by any name [`Self::class_key`] keys, holds the method `method` under, as
    /// [`ClassLikeMetadata::method_key`] says: the exact name for one with `exact_names`, and the lowercase name
    /// otherwise, an unknown class-like's included.
    #[inline]
    #[must_use]
    pub fn method_key(&self, class: Word, method: Word) -> Word {
        self.get_class_like(class).map_or_else(|| method.to_ascii_lowercase(), |meta| meta.method_key(method))
    }

    /// Checks if a class exists in the codebase, by [`Self::class_key`].
    ///
    /// # Examples
    /// ```ignore
    /// if codebase.class_exists("MyClass") {
    ///     // MyClass is a class
    /// }
    /// ```
    #[inline]
    #[must_use]
    pub fn class_exists(&self, name: Word) -> bool {
        matches!(self.symbols.get_kind(self.class_key(name)), Some(SymbolKind::Class))
    }

    /// Checks if an interface exists in the codebase, by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn interface_exists(&self, name: Word) -> bool {
        matches!(self.symbols.get_kind(self.class_key(name)), Some(SymbolKind::Interface))
    }

    /// Checks if a trait exists in the codebase, by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn trait_exists(&self, name: Word) -> bool {
        matches!(self.symbols.get_kind(self.class_key(name)), Some(SymbolKind::Trait))
    }

    /// Checks if an enum exists in the codebase, by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn enum_exists(&self, name: Word) -> bool {
        matches!(self.symbols.get_kind(self.class_key(name)), Some(SymbolKind::Enum))
    }

    /// Checks if a class-like (class, interface, trait, or enum) exists, by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn class_like_exists(&self, name: Word) -> bool {
        self.symbols.contains(self.class_key(name))
    }

    /// Checks if a namespace exists, in any case, as PHP finds a namespace.
    #[inline]
    #[must_use]
    pub fn namespace_exists(&self, name: Word) -> bool {
        self.symbols.contains_namespace(name.to_ascii_lowercase())
    }

    /// The import at `span`, then the shortest chain of imports that leads from the namespace it imports back to its
    /// file's namespace, spec section 23, or `None` when no chain leads back.
    #[must_use]
    pub fn import_cycle(&self, span: Span) -> Option<&[NamespaceImportMetadata]> {
        self.import_cycles.get(&span).map(Vec::as_slice)
    }

    /// Whether another class-like of the project or its vendors has the short name of the class-like `name`, compared
    /// without case as PHP compares class names. PHP's built-in class-likes don't count: a `.sharp` file reaches one,
    /// like `Dom\Text`, only through an import, and one file can't import two classes of one short name without an
    /// alias. The prelude's `Sharp\` class-likes do count, as a `.sharp` file reaches them with no import.
    #[must_use]
    pub fn shares_short_name(&self, name: Word) -> bool {
        self.short_names.get(&short_name_key(name.as_bytes())).is_some_and(|count| *count > 1)
    }

    /// Checks if a class or trait exists in the codebase, by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn class_or_trait_exists(&self, name: Word) -> bool {
        matches!(self.symbols.get_kind(self.class_key(name)), Some(SymbolKind::Class | SymbolKind::Trait))
    }

    /// Checks if a class or interface exists in the codebase, by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn class_or_interface_exists(&self, name: Word) -> bool {
        matches!(self.symbols.get_kind(self.class_key(name)), Some(SymbolKind::Class | SymbolKind::Interface))
    }

    /// Checks if a method identifier exists in the codebase.
    #[inline]
    #[must_use]
    pub fn method_identifier_exists(&self, method_id: &MethodIdentifier) -> bool {
        let class = self.class_key(method_id.get_class_name());
        let method_key = self.method_key(class, method_id.get_method_name());
        self.get_method_by_words(class, method_key).is_some()
    }

    /// Checks if a global function exists in the codebase (case-insensitive).
    #[inline]
    #[must_use]
    pub fn function_exists(&self, name: Word) -> bool {
        self.function_likes.contains_key(&(empty_word(), name.to_ascii_lowercase()))
    }

    /// Checks if a global constant exists in the codebase.
    /// The namespace part is case-insensitive, but the constant name is case-sensitive.
    #[inline]
    #[must_use]
    pub fn constant_exists(&self, name: Word) -> bool {
        self.constants.contains_key(&constant_key(name))
    }

    /// Checks if a method exists on a class-like, including inherited methods (case-insensitive, and exact for a
    /// class-like with `exact_names`).
    #[inline]
    #[must_use]
    pub fn method_exists(&self, class: Word, method: Word) -> bool {
        self.get_class_like(class).is_some_and(|meta| meta.appearing_method_ids.contains_key(&meta.method_key(method)))
    }

    /// Checks if a property exists on a class-like, including inherited properties.
    /// The class is found by [`Self::class_key`], and the property name is case-sensitive.
    /// Sees real declarations only; magic `@property*` tags are reachable through
    /// `ClassLikeMetadata::magic_property_ids`.
    #[inline]
    #[must_use]
    pub fn property_exists(&self, class: Word, property: Word) -> bool {
        self.get_class_like(class).is_some_and(|meta| meta.appearing_property_ids.contains_key(&property))
    }

    /// Checks if a magic `@property*` exists on a class-like, including inherited tags.
    /// The class is found by [`Self::class_key`], and the property name is case-sensitive.
    #[inline]
    #[must_use]
    pub fn magic_property_exists(&self, class: Word, property: Word) -> bool {
        self.get_class_like(class).is_some_and(|meta| meta.magic_property_ids.contains_key(&property))
    }

    /// Checks if a class constant or enum case exists on a class-like.
    /// The class is found by [`Self::class_key`], and the constant or case name is case-sensitive.
    #[inline]
    #[must_use]
    pub fn class_constant_exists(&self, class: Word, constant: Word) -> bool {
        self.get_class_like(class)
            .is_some_and(|meta| meta.constants.contains_key(&constant) || meta.enum_cases.contains_key(&constant))
    }

    /// Retrieves metadata for a class, by [`Self::class_key`].
    /// Returns `None` if the name doesn't correspond to a class.
    #[inline]
    #[must_use]
    pub fn get_class(&self, name: Word) -> Option<&ClassLikeMetadata> {
        self.get_class_like(name).filter(|metadata| metadata.kind.is_class())
    }

    /// Retrieves metadata for an interface, by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn get_interface(&self, name: Word) -> Option<&ClassLikeMetadata> {
        self.get_class_like(name).filter(|metadata| metadata.kind.is_interface())
    }

    /// Retrieves metadata for a trait, by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn get_trait(&self, name: Word) -> Option<&ClassLikeMetadata> {
        self.get_class_like(name).filter(|metadata| metadata.kind.is_trait())
    }

    /// Retrieves metadata for an enum, by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn get_enum(&self, name: Word) -> Option<&ClassLikeMetadata> {
        self.get_class_like(name).filter(|metadata| metadata.kind.is_enum())
    }

    /// Whether a value of `class` never changes, spec section 13: an enum, an enum whose cases carry data, a case of
    /// one, or a struct. Such a value is readonly wherever `readonly` is asked for, so `readonly` before it changes
    /// nothing.
    #[must_use]
    pub fn never_changes(&self, class: &ClassLikeMetadata) -> bool {
        class.kind.is_enum()
            || class.flags.is_struct()
            || class.flags.is_enum_case_class()
            || class.permitted_inheritors.iter().flatten().any(|case| {
                self.get_class_like(*case)
                    .is_some_and(|case| case.flags.is_enum_case_class() && case.direct_parent_class == Some(class.name))
            })
    }

    /// Retrieves metadata for any class-like structure, by [`Self::class_key`]: a PHP# one only by its exact name, and
    /// a plain PHP one in any case.
    #[inline]
    #[must_use]
    pub fn get_class_like(&self, name: Word) -> Option<&ClassLikeMetadata> {
        self.get_class_like_by_word(self.class_key(name))
    }

    /// Retrieves metadata for a global function (case-insensitive).
    #[inline]
    #[must_use]
    pub fn get_function(&self, name: Word) -> Option<&FunctionLikeMetadata> {
        self.function_likes.get(&(empty_word(), name.to_ascii_lowercase())).map(|metadata| &**metadata)
    }

    /// Retrieves metadata for a method: the class by [`Self::class_key`], and the method by [`Self::method_key`].
    #[inline]
    #[must_use]
    pub fn get_method(&self, class: Word, method: Word) -> Option<&FunctionLikeMetadata> {
        let class = self.class_key(class);
        self.get_method_by_words(class, self.method_key(class, method))
    }

    /// Retrieves metadata for a closure or arrow function by its synthetic
    /// name (e.g. `{closure:src/foo.php:12:5}`).
    #[inline]
    #[must_use]
    pub fn get_closure(&self, synthetic_name: &Word) -> Option<&FunctionLikeMetadata> {
        self.function_likes.get(&(empty_word(), *synthetic_name)).map(|metadata| &**metadata)
    }

    /// Retrieves metadata for a closure declared at the given file and span.
    /// Convenience wrapper that rebuilds the synthetic name and delegates to
    /// [`Self::get_closure`].
    #[inline]
    #[must_use]
    pub fn get_closure_at(&self, file: &File, span: Span) -> Option<&FunctionLikeMetadata> {
        let name = crate::build_synthetic_name("closure", file, span);
        self.get_closure(&name)
    }

    /// The name of the method `class` has whose name differs from `method` only in case, for the error a call of
    /// `method` gets on a class-like with `exact_names`, which finds a method only as declared, spec section 24. It is
    /// the name the class answers to, so a trait's method under an alias is the alias. Of twins, as `total` and
    /// `Total`, the one that sorts first.
    #[must_use]
    pub fn get_method_name_in_another_case(&self, class: Word, method: Word) -> Option<Word> {
        let class_meta = self.get_class_like(class).filter(|class_meta| class_meta.exact_names)?;

        class_meta
            .appearing_method_ids
            .keys()
            .filter(|key| **key != method && key.as_bytes().eq_ignore_ascii_case(method.as_bytes()))
            .min_by_key(|key| key.as_bytes())
            .copied()
    }

    /// Retrieves method metadata by `MethodIdentifier`.
    #[inline]
    #[must_use]
    pub fn get_method_by_id(&self, method_id: &MethodIdentifier) -> Option<&FunctionLikeMetadata> {
        self.get_method(method_id.get_class_name(), method_id.get_method_name())
    }

    /// Retrieves the declaring method metadata, following the inheritance chain.
    /// This finds where the method is actually implemented.
    #[inline]
    #[must_use]
    pub fn get_declaring_method(&self, class: Word, method: Word) -> Option<&FunctionLikeMetadata> {
        let declaring_method_id = self.get_declaring_method_identifier(&MethodIdentifier::new(class, method));
        self.get_method(declaring_method_id.get_class_name(), declaring_method_id.get_method_name())
    }

    /// Retrieves the constructor `parent::__construct` reaches from `class`: the one its direct parent class declares
    /// or inherits.
    #[inline]
    #[must_use]
    pub fn get_parent_constructor(&self, class: Word) -> Option<&FunctionLikeMetadata> {
        let parent = self.get_class_like(class)?.direct_parent_class?;
        let constructor = self.get_class_like(parent)?.declaring_method_ids.get(&word("__construct"))?;

        self.get_method_by_id(constructor)
    }

    /// The return type a plain PHP method that the method `method` of `class_like` overrides or implements declares,
    /// such as `JsonSerializable::jsonSerialize(): mixed`, which PHP requires its override to declare too. None when no
    /// such method declares one. A trait's method counts only when it is abstract, since PHP checks a class's method
    /// against no other method of a trait it uses.
    #[must_use]
    pub fn get_overridden_php_result(&self, class_like: &ClassLikeMetadata, method: Word) -> Option<&TypeMetadata> {
        class_like.overridden_method_ids.get(&class_like.method_key(method)).and_then(|overridden| {
            overridden
                .values()
                .filter_map(|parent| {
                    Some((self.get_class_like_by_word(parent.get_class_name())?, self.get_method_by_id(parent)?))
                })
                .filter(|(owner, parent)| {
                    !parent.flags.is_sharp()
                        && (!owner.kind.is_trait()
                            || parent.method_metadata.as_ref().is_some_and(|method| method.is_abstract))
                })
                .find_map(|(_, parent)| parent.return_type_declaration_metadata.as_ref())
        })
    }

    /// Retrieves the traits of default bodies a PHP# class-like uses itself, spec section 22: the trait of each
    /// interface its header names, or one of those extends, that has default bodies and that its parent class does not
    /// already implement, in name order. The populator merges them, and the bridge writes them in the class's `use`.
    #[must_use]
    pub fn get_defaults_traits(&self, class_like: &ClassLikeMetadata) -> Vec<&ClassLikeMetadata> {
        let parent = class_like.direct_parent_class.and_then(|parent| self.get_class_like_by_word(parent));
        let mut interfaces: Vec<Word> = Vec::new();
        for interface in &class_like.direct_parent_interfaces {
            interfaces.push(*interface);
            if let Some(metadata) = self.get_class_like_by_word(*interface) {
                interfaces.extend(metadata.all_parent_interfaces.iter().copied());
            }
        }
        interfaces.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        interfaces.dedup();

        interfaces
            .into_iter()
            .filter(|interface| !parent.is_some_and(|parent| parent.all_parent_interfaces.contains(interface)))
            .filter_map(|interface| self.get_interface(interface))
            .filter_map(|interface| self.get_trait(defaults_trait_name(interface.original_name.as_bytes())))
            .collect()
    }

    /// Retrieves the interface whose default bodies `class_like` holds, when it is such a trait, spec section 22. A
    /// message names the interface, which PHP# code writes, for the trait, which it never names.
    #[must_use]
    pub fn get_defaults_interface(&self, class_like: &ClassLikeMetadata) -> Option<&ClassLikeMetadata> {
        if !class_like.kind.is_trait() || !class_like.flags.is_sharp() {
            return None;
        }

        let interface = class_like.original_name.as_bytes().strip_suffix(DEFAULTS_TRAIT_SUFFIX)?;

        self.get_interface(word(interface))
    }

    /// Retrieves metadata for any function-like construct (function, method, or closure).
    /// This is a convenience method that delegates to the appropriate getter based on the identifier type.
    #[inline]
    #[must_use]
    pub fn get_function_like(
        &self,
        identifier: &crate::identifier::function_like::FunctionLikeIdentifier,
    ) -> Option<&FunctionLikeMetadata> {
        use crate::identifier::function_like::FunctionLikeIdentifier;
        match identifier {
            FunctionLikeIdentifier::Function(name) => self.get_function(*name),
            FunctionLikeIdentifier::Method(class, method) => self.get_method(*class, *method),
            FunctionLikeIdentifier::Closure(name) => self.get_closure(name),
        }
    }

    /// The method or function `identifier` as a PHP# caller sees it when its `extern` line states its signature, spec
    /// section 23: the line's parameters and return type, which the call is checked against.
    #[must_use]
    pub fn get_extern_signature(
        &self,
        identifier: &crate::identifier::function_like::FunctionLikeIdentifier,
    ) -> Option<&FunctionLikeMetadata> {
        use crate::identifier::function_like::FunctionLikeIdentifier;
        let key = match identifier {
            FunctionLikeIdentifier::Function(name) => (empty_word(), name.to_ascii_lowercase()),
            FunctionLikeIdentifier::Method(class, method) => {
                let class = self.class_key(*class);

                (class, self.method_key(class, *method))
            }
            FunctionLikeIdentifier::Closure(_) => return None,
        };

        self.extern_signatures.get(&key).map(|metadata| &**metadata)
    }

    /// Retrieves metadata for a global constant.
    /// Namespace lookup is case-insensitive, constant name is case-sensitive.
    #[inline]
    #[must_use]
    pub fn get_constant(&self, name: Word) -> Option<&ConstantMetadata> {
        self.constants.get(&constant_key(name))
    }

    /// Retrieves the constant an unqualified read reaches, as PHP finds it: `resolved`, the name in the reading file's
    /// namespace, or else the global constant named `written`, as the code wrote it.
    #[inline]
    #[must_use]
    pub fn get_constant_or_global(&self, resolved: Word, written: Word) -> Option<&ConstantMetadata> {
        self.get_constant(resolved).or_else(|| self.get_constant(written))
    }

    /// The declaration name span of a top-level symbol named `name`: a
    /// class-like (class/interface/trait/enum), a function, or a constant.
    ///
    /// Prefers the symbol's name span over its full declaration span, so callers
    /// land on the identifier rather than the whole declaration body. The lookup
    /// is case-insensitive, like every symbol lookup here.
    #[inline]
    #[must_use]
    pub fn span_of(&self, name: Word) -> Option<Span> {
        if let Some(meta) = self.get_class_like(name) {
            return Some(meta.name_span.unwrap_or(meta.span));
        }

        if let Some(meta) = self.get_function(name) {
            return Some(meta.name_span.unwrap_or(meta.span));
        }

        self.get_constant(name).map(|meta| meta.span)
    }

    /// Records the name of the scanned file `file`, when it is a host or a vendored file, whether the standard library
    /// holds it, whether it is a test file, and the dev-only packages it can't use.
    pub(crate) fn add_file(&mut self, file: &File) {
        if !file.dev_packages.is_empty() {
            self.dev_packages.insert(file.id, file.dev_packages.to_vec());
        }
        if matches!(file.file_type, FileType::Host | FileType::Vendored) {
            self.file_names.insert(file.id, word(&file.name));
        }
        if file.is_standard_library {
            self.standard_library_files.insert(file.id);
        }
        if file.folder_namespace == Some(FolderNamespace::Test) {
            self.test_files.insert(file.id);
        }
    }

    /// Names the file `file_id` `name`, its path from the workspace, as a codebase another workspace scanned names it by
    /// its path there: the standard library's codex names each library file by its path in the library's package.
    pub fn name_file(&mut self, file_id: FileId, name: &[u8]) {
        self.file_names.insert(file_id, word(name));
    }

    /// The name of the host or vendored file `file_id`, its path from the workspace, as `app/Shop/Cart.sharp`.
    #[inline]
    #[must_use]
    pub fn file_name(&self, file_id: FileId) -> Option<Word> {
        self.file_names.get(&file_id).copied()
    }

    /// The folder of the host or vendored file `file_id`, as `app/Shop` for `app/Shop/Cart.sharp`.
    #[inline]
    #[must_use]
    pub fn folder(&self, file_id: FileId) -> Option<Word> {
        let name = self.file_name(file_id)?;
        let name = name.as_bytes();

        Some(word(name.iter().rposition(|byte| *byte == b'/').map_or(&name[..0], |separator| &name[..separator])))
    }

    /// Whether the standard library, the PHP# Composer package, holds the file `file_id`.
    #[inline]
    #[must_use]
    pub fn is_standard_library_file(&self, file_id: FileId) -> bool {
        self.standard_library_files.contains(&file_id)
    }

    /// Whether the file `file_id` is a test file, one of a folder `composer.json` lists under `autoload-dev`.
    #[inline]
    #[must_use]
    pub fn is_test_file(&self, file_id: FileId) -> bool {
        self.test_files.contains(&file_id)
    }

    /// The name of the dev-only package that installs the file `declared_in`, when the production PHP# file `user`
    /// can't use what it declares, since a production install leaves the package out, spec section 5.
    #[must_use]
    pub fn dev_package(&self, user: FileId, declared_in: FileId) -> Option<&str> {
        let packages = self.dev_packages.get(&user)?;
        let name = self.file_name(declared_in)?;

        packages.iter().find(|package| name.as_bytes().starts_with(&package.folder)).map(|package| &*package.name)
    }

    /// The folder whose classes, functions and constants of PHP's root namespace `declaration` gives its namespace, spec
    /// section 23: the folder of the plain PHP library's class, function or constant it names, as Laravel's
    /// `Illuminate/Foundation` for `abort`. None when it names none, as for a class PHP# declares, a name in a
    /// namespace, one of PHP's built-ins, which has no folder, and a name nothing has.
    #[must_use]
    pub fn extern_namespace_folder(&self, declaration: &ExternNamespaceMetadata) -> Option<Word> {
        let name = declaration.name;
        if name.as_bytes().contains(&b'\\') {
            return None;
        }
        let class = self
            .get_class_like(name)
            .filter(|class_like| !class_like.flags.is_sharp() && !class_like.flags.is_module())
            .map(|class_like| class_like.span);
        let span = class
            .or_else(|| self.get_function(name).map(|function| function.span))
            .or_else(|| self.get_constant(name).map(|constant| constant.span))?;

        self.folder(span.file_id)
    }

    /// The namespace an `extern namespace` line gives the library's function, constant or class `name` of PHP's root
    /// namespace, spec section 23: the line's that names a class, function or constant of the same folder, as
    /// `Illuminate\Foundation` for `abort` after `extern namespace Illuminate.Foundation for abort;`, and for `now`
    /// beside it. A function's name reads first, then a constant's, then a class's. None for a name a line gives no
    /// namespace, and for one of PHP's built-ins, which has no folder.
    #[must_use]
    pub fn extern_namespace_of(&self, name: Word) -> Option<Word> {
        let span = self
            .get_function(name)
            .map(|function| function.span)
            .or_else(|| self.get_constant(name).map(|constant| constant.span))
            .or_else(|| {
                self.get_class_like(name)
                    .filter(|class_like| !class_like.flags.is_sharp() && !class_like.flags.is_module())
                    .map(|class_like| class_like.span)
            })
            .filter(|_| !name.as_bytes().contains(&b'\\'))?;
        let folder = self.folder(span.file_id)?;

        self.extern_namespaces
            .iter()
            .find(|declaration| self.extern_namespace_folder(declaration) == Some(folder))
            .map(|declaration| declaration.namespace)
    }

    /// The `extern` declaration a use of `member` (empty for a function's own name) that `class` (empty for a function)
    /// declares falls under, spec section 29: the member's line, which overrides the class's, then the class's. Nothing
    /// passes from another class, so a member a class inherits takes the line of the class that declares it, as
    /// [`Self::extern_of_method`] finds it. A function in a namespace is declared as `extern Pdf.render`, which the
    /// binder reads as a class and its member, spec section 23, and so is one a library declares in PHP's root
    /// namespace, through the namespace an `extern namespace` line gives it, as `extern Foundation.now` declares
    /// Laravel's `now()` after `extern namespace Foundation for now;`.
    #[must_use]
    pub fn extern_of(&self, class: Word, member: Word) -> Option<&ExternMetadata> {
        let declared = |key: (Word, Word)| self.externs.get(&key).and_then(|declarations| declarations.first());
        if class.is_empty() {
            return declared((empty_word(), member)).or_else(|| {
                let name = member.as_bytes();
                match name.iter().rposition(|byte| *byte == b'\\') {
                    Some(separator) => declared((word(&name[..separator]), word(&name[separator + 1..]))),
                    None => declared((self.extern_namespace_of(member)?.to_ascii_lowercase(), member)),
                }
            });
        }

        // A class is found by its key, spec section 24, and an `extern` names a plain PHP class in lowercase.
        let named = self.class_key(class).to_ascii_lowercase();

        declared((named, member)).or_else(|| declared((named, empty_word())))
    }

    /// The `extern` declaration a call of `method` falls under, spec section 29: a call runs the body of
    /// `declaring_class`, the class, trait or interface that declares the method, as a facade's docblock `@method`
    /// declares one on the facade, so that class's line for the method, or else its line for the whole class, declares
    /// the call's effects, as [`Self::extern_of`] finds them.
    #[must_use]
    pub fn extern_of_method(&self, declaring_class: Word, method: Word) -> Option<&ExternMetadata> {
        self.extern_of(declaring_class, ascii_lowercase_word(method.as_bytes()))
    }

    /// What the `extern` line `declaration` declares, keyed as `class_likes` keys a class and `function_likes` a method
    /// or a function, spec section 29: `(class, "")`, `(class, method)` for a method the class itself declares, or
    /// `("", function)`. `extern Pdf.render` after `import Vendor.Pdf;` declares the function `Vendor\Pdf\render`, and
    /// `extern Foundation.now` the `now` of PHP's root namespace that an `extern namespace` line gives
    /// `Illuminate\Foundation`, spec section 23. None when it declares nothing that exists.
    #[must_use]
    pub fn extern_declaration(&self, declaration: &ExternMetadata) -> Option<(Word, Word)> {
        let (class, member) = declaration.target;
        let declared = |key: (Word, Word)| self.function_likes.contains_key(&key).then_some(key);
        if class.is_empty() {
            return declared(declaration.target);
        }
        if self.class_likes.contains_key(&class) {
            return if member.is_empty() { Some(declaration.target) } else { declared(declaration.target) };
        }

        declared((empty_word(), ascii_lowercase_word(&[class.as_bytes(), b"\\", member.as_bytes()].concat()))).or_else(
            || {
                let namespace = self.extern_namespace_of(member)?;

                namespace.as_bytes().eq_ignore_ascii_case(class.as_bytes()).then_some(())?;
                declared((empty_word(), member))
            },
        )
    }

    /// Retrieves metadata for a class constant.
    /// The class is found by [`Self::class_key`], and the constant name is case-sensitive.
    #[inline]
    #[must_use]
    pub fn get_class_constant(&self, class: Word, constant: Word) -> Option<&ClassLikeConstantMetadata> {
        self.get_class_like(class).and_then(|meta| meta.constants.get(&constant))
    }

    /// Retrieves metadata for an enum case.
    #[inline]
    #[must_use]
    pub fn get_enum_case(&self, class: Word, case: Word) -> Option<&EnumCaseMetadata> {
        self.get_class_like(class).and_then(|meta| meta.enum_cases.get(&case))
    }

    /// Retrieves metadata for a property directly from the class where it's declared.
    /// The class is found by [`Self::class_key`], and the property name is case-sensitive.
    /// Sees real declarations only; magic `@property*` tags are reachable through
    /// `ClassLikeMetadata::magic_property_ids`.
    #[inline]
    #[must_use]
    pub fn get_property(&self, class: Word, property: Word) -> Option<&PropertyMetadata> {
        self.get_class_like(class)?.properties.get(&property)
    }

    /// Retrieves magic `@property*` metadata declared directly on a class-like.
    /// The class is found by [`Self::class_key`], and the property name is case-sensitive.
    #[inline]
    #[must_use]
    pub fn get_magic_property(&self, class: Word, property: Word) -> Option<&PropertyMetadata> {
        self.get_class_like(class)?.magic_properties.get(&property)
    }

    /// Retrieves the property metadata, potentially from a parent class if inherited.
    #[inline]
    #[must_use]
    pub fn get_declaring_property(&self, class: Word, property: Word) -> Option<&PropertyMetadata> {
        let declaring_class = self.get_class_like(class)?.declaring_property_ids.get(&property)?;
        self.class_likes.get(declaring_class)?.properties.get(&property)
    }

    /// Whether the engine holds `member`, which `class` declares, to the namespace of `class`, spec section 5: a
    /// member written `internal`, or a `public` one of an `internal` class, unless it replaces a member of a parent
    /// class or an interface that reaches further, through which code reaches it. The lowering sets the engine's flag
    /// from it, and the analyzer refuses the reach the engine refuses.
    #[must_use]
    pub fn is_held_to_namespace(&self, class: Word, member: Member) -> bool {
        let Some(metadata) = self.get_class_like(class) else {
            return false;
        };
        let Some((flags, public)) = self.own_member(class, member) else {
            return false;
        };

        flags.is_internal_reach()
            || (public
                && metadata.flags.is_internal_reach()
                && !metadata.all_parent_classes.iter().chain(&metadata.all_parent_interfaces).any(|ancestor| {
                    self.own_member(*ancestor, member).is_some() && !self.is_held_to_namespace(*ancestor, member)
                }))
    }

    /// The flags of `member` as `class` itself declares it, and whether it is `public`, or none when `class` doesn't. The
    /// codex keeps an `extern` property as a magic property, spec section 14.3.
    fn own_member(&self, class: Word, member: Member) -> Option<(MetadataFlags, bool)> {
        match member {
            Member::Method(name) => self.get_method(class, name).map(|method| {
                (method.flags, method.method_metadata.as_ref().is_some_and(|metadata| metadata.visibility.is_public()))
            }),
            Member::Property(name) => self
                .get_property(class, name)
                .or_else(|| self.get_magic_property(class, name))
                .map(|property| (property.flags, property.read_visibility.is_public())),
            Member::Constant(name) => {
                self.get_class_constant(class, name).map(|constant| (constant.flags, constant.visibility.is_public()))
            }
        }
    }

    /// Whether `class` has the `parse` spec section 10 gives a struct or a PHP# class, which reads a `Map<string, Any?>`
    /// or an object, rather than one it declares, inherits from a parent that declares one, or none. The codex's spans
    /// the whole type, which no declared member does. A backed struct's reads its value instead.
    #[must_use]
    pub fn parses(&self, class: Word) -> bool {
        let Some(metadata) = self.get_class_like(class) else {
            return false;
        };

        metadata.struct_backing_type.is_none()
            && self.get_declaring_method_class(class, word("parse")) == Some(metadata.name)
            && self.get_method(class, word("parse")).is_some_and(|parse| parse.span == metadata.span)
    }

    /// The family above `class`, spec section 10: the parent class, interface or enum marked `[Tag]` whose `parse`
    /// builds `class` by its `[Key]`. None for a class-like with none above it, a family itself included.
    #[must_use]
    pub fn family_of(&self, class: &ClassLikeMetadata) -> Option<&ClassLikeMetadata> {
        class
            .all_parent_classes
            .iter()
            .chain(&class.all_parent_interfaces)
            .filter_map(|parent| self.get_class_like(*parent))
            .find(|parent| parent.tag_key().is_some())
    }

    /// The name the variant `variant` of a family goes by, spec section 10, by which its family's `parse` and `withTag`
    /// find it and which its encoding writes: a backed machine's case's value, spec section 20.2, and the name
    /// `[Key("…")]` gives any other variant. None for a variant with neither, which the analyzer refuses.
    #[must_use]
    pub fn variant_name<'codebase>(&'codebase self, variant: &'codebase ClassLikeMetadata) -> Option<&'codebase [u8]> {
        self.machine_value(variant).or_else(|| variant.attribute_text(b"Sharp\\Key", b"name"))
    }

    /// Whether the machine `class` stores as its case's value, as a backed enum does, spec section 20.2: it is backed,
    /// not marked `[Tag]`, and none of its cases holds data. A machine with a case that holds data stores as a tagged
    /// family.
    #[must_use]
    pub fn stores_as_value(&self, class: Word) -> bool {
        let Some(metadata) = self.get_class_like(class) else {
            return false;
        };
        let Some(machine) = &metadata.machine else {
            return false;
        };

        metadata.tag_key().is_none()
            && machine.cases.iter().all(|case| {
                case.value.is_some()
                    && self
                        .get_class_like(word([metadata.original_name.as_bytes(), b"\\", case.name.as_bytes()].concat()))
                        .is_some_and(|case| case.struct_properties().is_empty())
            })
    }

    /// The value the case `case` of a backed machine is backed by, spec section 20.2. None for any other class-like.
    #[must_use]
    pub fn machine_value(&self, case: &ClassLikeMetadata) -> Option<&[u8]> {
        let machine = self.get_class_like(case.direct_parent_class?)?.machine.as_ref()?;
        let name = case.original_name.as_bytes().rsplit(|byte| *byte == b'\\').next()?;

        machine
            .cases
            .iter()
            .find(|machine_case| machine_case.name.as_bytes() == name)?
            .value
            .as_ref()
            .map(Word::as_bytes)
    }

    /// The variants of the family `family`, spec section 10, in the order of their names: each PHP# class, struct and
    /// enum case below it that is not abstract, with the name [`CodebaseMetadata::variant_name`] gives, or None.
    #[must_use]
    pub fn variants(&self, family: &ClassLikeMetadata) -> Vec<(Option<&[u8]>, &ClassLikeMetadata)> {
        let mut variants: Vec<(Option<&[u8]>, &ClassLikeMetadata)> = self
            .all_class_like_descendants
            .get(&family.name)
            .into_iter()
            .flatten()
            .filter_map(|descendant| self.get_class_like(*descendant))
            .filter(|descendant| descendant.flags.is_sharp() && descendant.is_variant_kind())
            .map(|variant| (self.variant_name(variant), variant))
            .collect();
        variants
            .sort_by(|(a, a_class), (b, b_class)| a.cmp(b).then(a_class.name.as_bytes().cmp(b_class.name.as_bytes())));

        variants
    }

    /// Whether the class `class` is a struct stored as the built-in value its header names, spec section 10, as
    /// `struct Email : string`: `Json.encode` writes its `value`, and a field `parse` reads gives that value to its
    /// `parse`.
    #[must_use]
    pub fn is_backed_struct(&self, class: Word) -> bool {
        self.get_class_like(class).is_some_and(|metadata| metadata.struct_backing_type.is_some())
    }

    /// Whether the class `class` declares a static `parse` of one `string` and a `toString()` that takes no argument
    /// without being a backed struct, spec section 10. That pair stores nothing: `toString()` is display only, so the
    /// checker refuses a stored field of it and names `: string`.
    #[must_use]
    pub fn declares_text_pair(&self, class: Word) -> bool {
        let Some(parse) = self.get_method(class, word("parse")) else {
            return false;
        };
        let Some(to_string) = self.get_method(class, word("toString")) else {
            return false;
        };

        !self.is_backed_struct(class)
            && is_static_method(parse)
            && matches!(parse.parameters.as_slice(), [text] if text.type_metadata.as_ref().is_some_and(|written| written.type_union.is_string()))
            && !is_static_method(to_string)
            && to_string.parameters.iter().all(|parameter| parameter.flags.has_default())
    }

    /// Whether the class `class` declares a static `parse` of one `Any?`, which reads any value, as `JsonValue` does,
    /// spec section 24, so a field `parse` reads gives it the value as it is.
    #[must_use]
    pub fn parses_any_value(&self, class: Word) -> bool {
        self.get_method(class, word("parse")).is_some_and(|parse| {
            is_static_method(parse)
                && matches!(parse.parameters.as_slice(), [value] if value.type_metadata.as_ref().is_some_and(|written| {
                    written.type_union.types.iter().any(|atomic| matches!(atomic, TAtomic::Mixed(mixed) if !mixed.is_non_null()))
                }))
        })
    }

    /// The fields of a PHP# struct's or class's stored form, which its `parse` reads and `Json.encode` writes, spec
    /// section 10: its main constructor's parameters in order, then each other stored property, whatever its write
    /// visibility, so a `private set` one and a machine's are kept. A computed property stores nothing. A parameter
    /// that declares its property is that property's one field. A property the main constructor sets on every path may
    /// be left out, and is read once, by the parameter, when a parameter reads its key. Any other property a
    /// parameter's key names is a second field of that key, which the struct check refuses.
    #[must_use]
    pub fn struct_parse_fields(&self, class: Word) -> Vec<ParsedField<'_>> {
        let constructor = self.get_declaring_method(class, word("__construct"));
        let parameters: Vec<ParsedField<'_>> = constructor
            .into_iter()
            .flat_map(|constructor| {
                constructor.parameters.iter().filter_map(move |parameter| {
                    let r#type = parameter.type_metadata.as_ref().or(parameter.type_declaration_metadata.as_ref())?;
                    let name = parameter.name.0.as_bytes();
                    let name = name.strip_prefix(b"$").unwrap_or(name);
                    let (key, omits_null) = field_key(&parameter.attributes, name);

                    Some(ParsedField {
                        name,
                        key,
                        omits_null,
                        r#type: &r#type.type_union,
                        has_default: parameter.flags.has_default(),
                        is_parameter: true,
                        span: Some(parameter.name_span),
                    })
                })
            })
            .collect();
        let set_by_constructor = constructor
            .and_then(|constructor| constructor.method_metadata.as_ref())
            .map(|constructor| &constructor.assigned_properties);
        let properties = self.get_class_like(class).into_iter().flat_map(|metadata| {
            metadata
                .struct_properties()
                .into_iter()
                .filter(|property| !property.flags.is_promoted_property())
                .filter_map(|property| {
                    let r#type = property.type_metadata.as_ref().or(property.type_declaration_metadata.as_ref())?;
                    let name = property.name.0.as_bytes();
                    let name = name.strip_prefix(b"$").unwrap_or(name);
                    let set_by_constructor =
                        set_by_constructor.is_some_and(|assigned| assigned.contains(&property.name.0));
                    if set_by_constructor && parameters.iter().any(|parameter| parameter.key == name) {
                        return None;
                    }
                    let (key, omits_null) = field_key(&property.attributes, name);

                    Some(ParsedField {
                        name,
                        key,
                        omits_null,
                        r#type: &r#type.type_union,
                        has_default: property.flags.has_default() || set_by_constructor,
                        is_parameter: false,
                        span: property.name_span,
                    })
                })
                .collect::<Vec<_>>()
        });

        parameters.iter().copied().chain(properties).collect()
    }

    /// Retrieves magic `@property*` metadata, potentially from an inherited tag.
    /// The class is found by [`Self::class_key`], and the property name is case-sensitive.
    #[inline]
    #[must_use]
    pub fn get_declaring_magic_property(&self, class: Word, property: Word) -> Option<&PropertyMetadata> {
        let declaring_class = self.get_class_like(class)?.magic_property_ids.get(&property)?;
        self.class_likes.get(declaring_class)?.magic_properties.get(&property)
    }
    // Type Resolution

    /// Gets the type of a property, resolving it from the declaring class if needed.
    #[inline]
    #[must_use]
    pub fn get_property_type(&self, class: Word, property: Word) -> Option<&TUnion> {
        let declaring_class = self.get_class_like(class)?.declaring_property_ids.get(&property)?;
        let property_meta = self.class_likes.get(declaring_class)?.properties.get(&property)?;
        property_meta.type_metadata.as_ref().map(|tm| &tm.type_union)
    }

    /// Gets the type of a class constant, considering both type hints and inferred types.
    #[must_use]
    pub fn get_class_constant_type(&self, class: Word, constant_name: Word) -> Option<Cow<'_, TUnion>> {
        let class_meta = self.get_class_like(class)?;

        // Check if it's an enum case
        if class_meta.kind.is_enum() && class_meta.enum_cases.contains_key(&constant_name) {
            let atomic = TAtomic::Object(TObject::new_enum_case(class_meta.original_name, constant_name));
            return Some(Cow::Owned(TUnion::from_atomic(atomic)));
        }

        // It's a regular class constant
        let constant_meta = class_meta.constants.get(&constant_name)?;

        // Prefer the type signature if available
        if let Some(type_meta) = constant_meta.type_metadata.as_ref() {
            return Some(Cow::Borrowed(&type_meta.type_union));
        }

        // Fall back to inferred type
        constant_meta.inferred_type.as_ref().map(|atomic| Cow::Owned(TUnion::from_atomic(atomic.clone())))
    }
    // Inheritance Queries

    /// Checks if a child class extends a parent class, each found by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn class_extends(&self, child: Word, parent: Word) -> bool {
        let parent = self.class_key(parent);
        let Some(metadata) = self.get_class_like(child) else {
            return false;
        };

        if metadata.all_parent_classes.contains(&parent) {
            return true;
        }

        if self.class_like_aliases.is_empty() || self.class_likes.contains_key(&parent) {
            return false;
        }

        self.class_like_aliases.get(&parent).is_some_and(|actual| metadata.all_parent_classes.contains(actual))
    }

    /// Checks if a class implements an interface, each found by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn class_implements(&self, class: Word, interface: Word) -> bool {
        let interface = self.class_key(interface);
        let Some(metadata) = self.get_class_like(class) else {
            return false;
        };

        if metadata.all_parent_interfaces.contains(&interface) {
            return true;
        }

        if self.class_like_aliases.is_empty() || self.class_likes.contains_key(&interface) {
            return false;
        }

        self.class_like_aliases.get(&interface).is_some_and(|actual| metadata.all_parent_interfaces.contains(actual))
    }

    /// Checks if a class uses a trait, each found by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn class_uses_trait(&self, class: Word, trait_name: Word) -> bool {
        let trait_key = self.class_key(trait_name);
        let Some(metadata) = self.get_class_like(class) else {
            return false;
        };

        if metadata.used_traits.contains(&trait_key) {
            return true;
        }

        if self.class_like_aliases.is_empty() || self.class_likes.contains_key(&trait_key) {
            return false;
        }

        self.class_like_aliases.get(&trait_key).is_some_and(|actual| metadata.used_traits.contains(actual))
    }

    /// Checks if child is an instance of parent (via extends or implements), each found by [`Self::class_key`].
    #[inline]
    #[must_use]
    pub fn is_instance_of(&self, child: Word, parent: Word) -> bool {
        if child == parent {
            return true;
        }

        let child_key = self.class_key(child);
        let parent_key = self.class_key(parent);

        if child_key == parent_key {
            return true;
        }

        let Some(metadata) = self.get_class_like_by_word(child_key) else {
            return false;
        };

        let matches =
            |parent| metadata.name == parent || metadata.instance_of().iter().any(|classes| classes.contains(&parent));

        if matches(parent_key) {
            return true;
        }

        if self.class_like_aliases.is_empty() || self.class_likes.contains_key(&parent_key) {
            return false;
        }

        self.class_like_aliases.get(&parent_key).is_some_and(|actual| matches(*actual))
    }

    /// Checks if the given name is an enum or final class.
    #[inline]
    #[must_use]
    pub fn is_enum_or_final_class(&self, name: Word) -> bool {
        self.get_class_like(name).is_some_and(|meta| meta.kind.is_enum() || meta.flags.is_final())
    }

    /// Checks if a class-like can be part of an intersection.
    /// Generally, only final classes and enums cannot be intersected.
    #[inline]
    #[must_use]
    pub fn is_inheritable(&self, name: Word) -> bool {
        let key = self.class_key(name);
        match self.symbols.get_kind(key) {
            Some(SymbolKind::Class) => self.get_class_like_by_word(key).is_some_and(|meta| !meta.flags.is_final()),
            Some(SymbolKind::Enum) => false,
            Some(SymbolKind::Interface | SymbolKind::Trait) | None => true,
        }
    }

    /// Gets all descendants of a class (recursive).
    #[inline]
    #[must_use]
    pub fn get_class_descendants(&self, class: Word) -> WordSet {
        let class_key = self.class_key(class);
        let class_key = self.resolve_class_like_word(class_key).unwrap_or(class_key);
        let mut all_descendants = WordSet::default();
        let mut queue = vec![&class_key];
        let mut visited = WordSet::default();
        visited.insert(class_key);

        while let Some(current_name) = queue.pop() {
            if let Some(direct_descendants) = self.direct_classlike_descendants.get(current_name) {
                for descendant in direct_descendants {
                    if visited.insert(*descendant) {
                        all_descendants.insert(*descendant);
                        queue.push(descendant);
                    }
                }
            }
        }

        all_descendants
    }

    /// Whether `class` is a PHP# class or struct that is an `Iterable<T>` of its `elements()`, spec section 12: its
    /// header, a PHP# base class's or a PHP# interface's it implements names `Iterable<T>`, which the scanner links as
    /// `IteratorAggregate<int, T>`. A PHP# class whose plain PHP base implements PHP's `IteratorAggregate` is a
    /// `Traversable` of that base's `getIterator()`. An interface asks its classes for `elements()`, so it is none.
    #[must_use]
    pub fn is_sharp_iterable(&self, class: Word) -> bool {
        let iterated = word("iteratoraggregate");
        let names_iterable = |metadata: &ClassLikeMetadata| {
            metadata.flags.is_sharp() && metadata.direct_parent_interfaces.contains(&iterated)
        };
        self.get_class_like(class).is_some_and(|metadata| {
            metadata.flags.is_sharp()
                && metadata.kind.is_class()
                && (names_iterable(metadata)
                    || metadata
                        .all_parent_classes
                        .iter()
                        .chain(&metadata.all_parent_interfaces)
                        .filter_map(|parent| self.get_class_like(*parent))
                        .any(names_iterable))
        })
    }

    /// The classes that declare a real member a PHP# extension member called `name` would hide on a value of type
    /// `receiver`, spec section 26, sorted: the member of that name that a class the value can be declares or
    /// inherits, a method or property, or one a docblock declares with `@method` or `@property`. `__call` and `__get`
    /// declare none. A `List`, `Map`, `Set` or `Iterable` has only the standard library's extension members, spec
    /// section 12. A type parameter
    /// shows its bound, so it has the members its bound's classes have. A class below the type competes with no
    /// extension, which is chosen from the type the code shows.
    #[must_use]
    pub fn get_classes_hidden_by_extension(&self, receiver: &TUnion, name: Word) -> Vec<Word> {
        /// The types a value of type `atomic` shows: a type parameter's bound's, at any depth, or `atomic` itself.
        fn shown(atomic: &TAtomic) -> Vec<&TAtomic> {
            match atomic {
                TAtomic::GenericParameter(parameter) => parameter.constraint.types.iter().flat_map(shown).collect(),
                _ => vec![atomic],
            }
        }

        let property_name = concat_word!(b"$", name);

        let mut classes: Vec<Word> = receiver
            .types
            .iter()
            .flat_map(shown)
            .filter_map(|atomic| match atomic {
                TAtomic::Object(object) => object.get_name(),
                _ => None,
            })
            .filter_map(|name| self.get_class_like(name))
            .filter_map(|class| {
                let method_name = class.method_key(name);
                class
                    .declaring_method_ids
                    .get(&method_name)
                    .map(MethodIdentifier::get_class_name)
                    .or_else(|| {
                        (class.pseudo_methods.contains(&method_name)
                            || class.static_pseudo_methods.contains(&method_name))
                        .then_some(class.name)
                    })
                    .or_else(|| class.declaring_property_ids.get(&property_name).copied())
                    .or_else(|| class.magic_property_ids.get(&property_name).copied())
            })
            .collect();
        classes.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        classes.dedup();

        classes
    }

    /// Gets all ancestors of a class (parents + interfaces).
    #[inline]
    #[must_use]
    pub fn get_class_ancestors(&self, class: Word) -> WordSet {
        let mut ancestors = WordSet::default();
        if let Some(meta) = self.get_class_like(class) {
            ancestors.extend(meta.all_parent_classes.iter().copied());
            ancestors.extend(meta.all_parent_interfaces.iter().copied());
        }
        ancestors
    }

    /// Gets the class where a method is declared (following inheritance).
    #[inline]
    #[must_use]
    pub fn get_declaring_method_class(&self, class: Word, method: Word) -> Option<Word> {
        let class_meta = self.get_class_like(class)?;

        class_meta.declaring_method_ids.get(&class_meta.method_key(method)).map(|method_id| method_id.get_class_name())
    }

    /// Gets the declaring method identifier for a method.
    #[must_use]
    pub fn get_declaring_method_identifier(&self, method_id: &MethodIdentifier) -> MethodIdentifier {
        let Some(class_meta) = self.get_class_like(method_id.get_class_name()) else {
            return *method_id;
        };
        let lowercase_method = class_meta.method_key(method_id.get_method_name());

        if let Some(declaring_method_id) = class_meta.declaring_method_ids.get(&lowercase_method) {
            return *declaring_method_id;
        }

        if class_meta.flags.is_abstract()
            && let Some(overridden_map) = class_meta.overridden_method_ids.get(&lowercase_method)
            && let Some((_, first_method_id)) = overridden_map.first()
        {
            return *first_method_id;
        }

        *method_id
    }

    /// Gets the identifier of the function-like `identifier` names where it is declared: the declaring method's for a
    /// method a class may inherit, and `identifier` itself for a function or a closure.
    #[must_use]
    pub fn get_declaring_function_like_identifier(
        &self,
        identifier: &crate::identifier::function_like::FunctionLikeIdentifier,
    ) -> crate::identifier::function_like::FunctionLikeIdentifier {
        use crate::identifier::function_like::FunctionLikeIdentifier;
        match identifier {
            FunctionLikeIdentifier::Method(class, method) => {
                self.get_declaring_method_identifier(&MethodIdentifier::new(*class, *method)).into()
            }
            _ => *identifier,
        }
    }

    /// Checks if a method is overriding a parent method.
    #[inline]
    #[must_use]
    pub fn method_is_overriding(&self, class: Word, method: Word) -> bool {
        self.get_class_like(class).is_some_and(|meta| meta.overridden_method_ids.contains_key(&meta.method_key(method)))
    }

    /// Checks if a method is abstract.
    #[inline]
    #[must_use]
    pub fn method_is_abstract(&self, class: Word, method: Word) -> bool {
        self.get_method(class, method)
            .and_then(|meta| meta.method_metadata.as_ref())
            .is_some_and(|method_meta| method_meta.is_abstract)
    }

    /// Checks if a method is final.
    #[inline]
    #[must_use]
    pub fn method_is_final(&self, class: Word, method: Word) -> bool {
        self.get_method(class, method)
            .and_then(|meta| meta.method_metadata.as_ref())
            .is_some_and(|method_meta| method_meta.is_final)
    }

    /// Gets the effective visibility of a method, taking into account trait alias visibility overrides.
    ///
    /// When a trait method is aliased with a visibility modifier (e.g., `use Trait { method as public aliasedMethod; }`),
    /// the visibility is stored in the class's `trait_visibility_map`. This method checks that map first,
    /// then falls back to the method's declared visibility.
    #[inline]
    #[must_use]
    pub fn get_method_visibility(&self, class: Word, method: Word) -> Option<Visibility> {
        let class_meta = self.get_class_like(class)?;
        let method_key = class_meta.method_key(method);
        let declaring = class_meta.declaring_method_ids.get(&method_key)?;

        // First check if there's a trait visibility override for the trait method the class takes
        if let Some(overridden_visibility) = class_meta.trait_visibility(self, method, declaring.get_class_name()) {
            return Some(overridden_visibility);
        }

        // Fall back to the method's declared visibility, by the key its declaring class gives it
        self.function_likes
            .get(&declaring.get_key())
            .and_then(|meta| meta.method_metadata.as_ref())
            .map(|method_meta| method_meta.visibility)
    }

    /// Gets thrown types for a function-like, including inherited throws.
    #[must_use]
    pub fn get_function_like_thrown_types<'meta>(
        &'meta self,
        class_like: Option<&'meta ClassLikeMetadata>,
        function_like: &'meta FunctionLikeMetadata,
    ) -> &'meta [TypeMetadata] {
        if !function_like.thrown_types.is_empty() {
            return function_like.thrown_types.as_slice();
        }

        if !function_like.kind.is_method() || function_like.flags.is_sharp() {
            return &[];
        }

        let Some(class_like) = class_like else {
            return &[];
        };

        let method_name = &function_like.name;

        if let Some(overridden_map) = class_like.overridden_method_ids.get(method_name) {
            for (parent_class_name, parent_method_id) in overridden_map {
                if class_like.name == *parent_class_name {
                    continue; // Skip self-recursion if the method overrides itself
                }

                let Some(parent_class) = self.class_likes.get(parent_class_name) else {
                    continue;
                };

                let parent_method_key = (parent_method_id.get_class_name(), parent_method_id.get_method_name());
                if let Some(parent_method) = self.function_likes.get(&parent_method_key) {
                    let thrown = self.get_function_like_thrown_types(Some(parent_class), parent_method);
                    if !thrown.is_empty() {
                        return thrown;
                    }
                }
            }
        }

        &[]
    }

    /// Gets the class where a property is declared.
    /// Sees real declarations only; magic `@property*` tags are reachable through
    /// `ClassLikeMetadata::magic_property_ids`.
    #[inline]
    #[must_use]
    pub fn get_declaring_property_class(&self, class: Word, property: Word) -> Option<Word> {
        let property_name = word(property);
        self.get_class_like(class)?.declaring_property_ids.get(&property_name).copied()
    }

    /// Gets all descendants of a class (recursive).
    #[must_use]
    pub fn get_all_descendants(&self, class: Word) -> WordSet {
        let class_key = self.class_key(class);
        let class_key = self.resolve_class_like_word(class_key).unwrap_or(class_key);
        let mut all_descendants = WordSet::default();
        let mut queue = vec![&class_key];
        let mut visited = WordSet::default();
        visited.insert(class_key);

        while let Some(current_name) = queue.pop() {
            if let Some(direct_descendants) = self.direct_classlike_descendants.get(current_name) {
                for descendant in direct_descendants {
                    if visited.insert(*descendant) {
                        all_descendants.insert(*descendant);
                        queue.push(descendant);
                    }
                }
            }
        }

        all_descendants
    }

    /// Generates the synthetic display name for an anonymous class based on
    /// its declaring file and span. Delegates to [`crate::get_anonymous_class_name`].
    #[must_use]
    pub fn get_anonymous_class_name(file: &File, span: Span) -> Word {
        crate::get_anonymous_class_name(file, span)
    }

    /// Retrieves the metadata for an anonymous class based on its declaring
    /// file and span.
    #[must_use]
    pub fn get_anonymous_class(&self, file: &File, span: Span) -> Option<&ClassLikeMetadata> {
        let name = Self::get_anonymous_class_name(file, span);
        self.get_class_like(name)
    }

    /// Gets the file signature for a given file ID.
    ///
    /// # Arguments
    ///
    /// * `file_id` - The file identifier
    ///
    /// # Returns
    ///
    /// A reference to the `FileSignature` if it exists, or `None` if the file has no signature.
    #[inline]
    #[must_use]
    pub fn get_file_signature(&self, file_id: &FileId) -> Option<&FileSignature> {
        self.file_signatures.get(file_id)
    }

    /// Adds or updates a file signature for a given file ID.
    ///
    /// # Arguments
    ///
    /// * `file_id` - The file identifier
    /// * `signature` - The file signature
    ///
    /// # Returns
    ///
    /// The previous `FileSignature` if it existed.
    #[inline]
    pub fn set_file_signature(&mut self, file_id: FileId, signature: FileSignature) -> Option<FileSignature> {
        self.file_signatures.insert(file_id, signature)
    }

    /// Marks safe symbols based on diff and invalidation cascade.
    ///
    /// After this function runs, `self.safe_symbols` and `self.safe_symbol_members`
    /// will contain all symbols that can be safely skipped during analysis.
    ///
    /// # Arguments
    ///
    /// * `diff` - The computed diff between old and new code
    /// * `references` - Symbol reference graph from previous run
    /// * `read` - The symbols the extension callbacks read, which every file counts as its own
    ///
    /// # Returns
    /// Returns the logical names of files whose top-level code references an invalidated symbol,
    /// and whether `read` names one. Returns `None` if the cascade was too large to compute.
    pub fn mark_safe_symbols(
        &mut self,
        diff: &CodebaseDiff,
        references: &SymbolReferences,
        read: &HashSet<SymbolIdentifier>,
    ) -> Option<(WordSet, bool)> {
        let (invalid_symbols, partially_invalid, invalid_files, read_invalid) =
            references.get_invalid_symbols(self, diff, read)?;

        // Mark all symbols in 'keep' set as safe (unless invalidated by cascade)
        for keep_symbol in diff.get_keep() {
            if !invalid_symbols.contains(keep_symbol) {
                if keep_symbol.1.is_empty() {
                    // Top-level symbol (class, function, constant)
                    if !partially_invalid.contains(&keep_symbol.0) {
                        self.safe_symbols.insert(keep_symbol.0);
                    }
                } else {
                    // Member (method, property, class constant)
                    self.safe_symbol_members.insert(*keep_symbol);
                }
            }
        }

        Some((invalid_files, read_invalid))
    }

    /// Drops each PHP# module whose name a plain PHP class holds in any case, with its members, as [`Self::extend`]
    /// drops a module that shares its key with a plain PHP class, spec section 32: the module keys its exact name and
    /// the class its lowercase name, spec section 24, and plain PHP finds the class in any case. The module's file then
    /// reports it as a duplicate.
    pub(crate) fn drop_modules_named_like_plain_classes(&mut self) {
        let modules: Vec<Word> = self
            .class_likes
            .iter()
            .filter(|(key, metadata)| {
                let lowercase = ascii_lowercase_word(key.as_bytes());
                metadata.flags.is_module()
                    && lowercase != **key
                    && self.class_likes.get(&lowercase).is_some_and(|plain| !plain.flags.is_sharp())
            })
            .map(|(key, _)| *key)
            .collect();

        for module in modules {
            self.class_likes.remove(&module);
            self.symbols.remove(module);
            self.function_likes.retain(|(class, _), _| *class != module);
        }
    }

    /// Merges information from another `CodebaseMetadata` into this one.
    ///
    /// When both metadata have the same priority, the one whose file's name sorts first is kept, as
    /// [`should_replace_metadata`] decides, for the same result whatever order the files are scanned in.
    pub fn extend(&mut self, other: CodebaseMetadata) {
        self.file_names.extend(other.file_names);
        self.standard_library_files.extend(other.standard_library_files);
        let class_likes_changed = !other.class_likes.is_empty();
        for (k, mut v) in other.class_likes {
            match self.class_likes.entry(k) {
                Entry::Occupied(mut entry) => {
                    if should_replace_metadata(
                        &self.file_names,
                        &self.standard_library_files,
                        (entry.get().flags, entry.get().span),
                        (v.flags, v.span),
                    ) {
                        v.version_constraint.merge(entry.get().version_constraint.clone());
                        entry.insert(v);
                    } else {
                        entry.get_mut().version_constraint.merge(v.version_constraint.clone());
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(v);
                }
            }
        }

        if class_likes_changed && !self.class_like_alias_declarations.is_empty() {
            self.class_like_aliases_dirty = true;
        }

        self.merge_class_like_alias_declarations(other.class_like_alias_declarations);

        for (k, mut v) in other.function_likes {
            match self.function_likes.entry(k) {
                Entry::Occupied(mut entry) => {
                    if should_replace_metadata(
                        &self.file_names,
                        &self.standard_library_files,
                        (entry.get().flags, entry.get().span),
                        (v.flags, v.span),
                    ) {
                        v.version_constraint.merge(entry.get().version_constraint.clone());
                        entry.insert(v);
                    } else {
                        entry.get_mut().version_constraint.merge(v.version_constraint.clone());
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(v);
                }
            }
        }

        for (k, mut v) in other.constants {
            match self.constants.entry(k) {
                Entry::Occupied(mut entry) => {
                    if should_replace_metadata(
                        &self.file_names,
                        &self.standard_library_files,
                        (entry.get().flags, entry.get().span),
                        (v.flags, v.span),
                    ) {
                        v.version_constraint.merge(entry.get().version_constraint.clone());
                        entry.insert(v);
                    } else {
                        entry.get_mut().version_constraint.merge(v.version_constraint);
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(v);
                }
            }
        }

        self.symbols.extend(other.symbols);
        self.merge_externs(other.externs);
        self.merge_modules(other.modules);
        self.merge_extern_namespaces(other.extern_namespaces);
        self.merge_namespace_imports(other.namespace_imports);
        self.merge_extensions(other.extensions);

        for (k, v) in other.all_class_like_descendants {
            self.all_class_like_descendants.entry(k).or_default().extend(v);
        }

        for (k, v) in other.direct_classlike_descendants {
            self.direct_classlike_descendants.entry(k).or_default().extend(v);
        }

        self.file_signatures.extend(other.file_signatures);
        self.test_files.extend(other.test_files);
        self.dev_packages.extend(other.dev_packages);
        self.safe_symbols.extend(other.safe_symbols);
        self.safe_symbol_members.extend(other.safe_symbol_members);
        self.infer_types_from_usage |= other.infer_types_from_usage;
        self.merge_patch_class_likes(other.patch_class_likes);
        self.merge_patch_function_likes(other.patch_function_likes);
        self.merge_patch_constants(other.patch_constants);
    }

    /// Extends this codebase with another by reference, cloning only individual entries.
    ///
    /// This is more efficient than `extend(other.clone())` because it avoids allocating
    /// a full clone of the source metadata's outer HashMap/WordMap structures. Only
    /// individual entries that need insertion are cloned.
    pub fn extend_ref(&mut self, other: &CodebaseMetadata) {
        self.file_names.extend(other.file_names.iter().map(|(file, name)| (*file, *name)));
        let class_likes_changed = !other.class_likes.is_empty();
        for (k, v) in &other.class_likes {
            match self.class_likes.entry(*k) {
                Entry::Occupied(mut entry) => {
                    if should_replace_metadata(
                        &self.file_names,
                        &self.standard_library_files,
                        (entry.get().flags, entry.get().span),
                        (v.flags, v.span),
                    ) {
                        let mut new = v.clone();
                        new.version_constraint.merge(entry.get().version_constraint.clone());
                        entry.insert(new);
                    } else {
                        entry.get_mut().version_constraint.merge(v.version_constraint.clone());
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(v.clone());
                }
            }
        }

        if class_likes_changed && !self.class_like_alias_declarations.is_empty() {
            self.class_like_aliases_dirty = true;
        }

        self.merge_class_like_alias_declarations(
            other.class_like_alias_declarations.iter().map(|(alias, declaration)| (*alias, *declaration)),
        );

        for (k, v) in &other.function_likes {
            match self.function_likes.entry(*k) {
                Entry::Occupied(mut entry) => {
                    if should_replace_metadata(
                        &self.file_names,
                        &self.standard_library_files,
                        (entry.get().flags, entry.get().span),
                        (v.flags, v.span),
                    ) {
                        let mut new = v.clone();
                        new.version_constraint.merge(entry.get().version_constraint.clone());
                        entry.insert(new);
                    } else {
                        entry.get_mut().version_constraint.merge(v.version_constraint.clone());
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(v.clone());
                }
            }
        }

        for (k, v) in &other.constants {
            match self.constants.entry(*k) {
                Entry::Occupied(mut entry) => {
                    if should_replace_metadata(
                        &self.file_names,
                        &self.standard_library_files,
                        (entry.get().flags, entry.get().span),
                        (v.flags, v.span),
                    ) {
                        let mut new = v.clone();
                        new.version_constraint.merge(entry.get().version_constraint.clone());
                        entry.insert(new);
                    } else {
                        entry.get_mut().version_constraint.merge(v.version_constraint.clone());
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(v.clone());
                }
            }
        }

        self.symbols.extend_ref(&other.symbols);
        self.merge_externs(other.externs.iter().map(|(target, declarations)| (*target, declarations.clone())));
        self.merge_modules(other.modules.iter().cloned());
        self.merge_extern_namespaces(other.extern_namespaces.iter().copied());
        self.merge_namespace_imports(other.namespace_imports.iter().copied());
        self.merge_extensions(other.extensions.iter().map(|(name, members)| (*name, members.clone())));

        for (k, v) in &other.all_class_like_descendants {
            self.all_class_like_descendants.entry(*k).or_default().extend(v.iter().copied());
        }

        for (k, v) in &other.direct_classlike_descendants {
            self.direct_classlike_descendants.entry(*k).or_default().extend(v.iter().copied());
        }

        for (k, v) in &other.file_signatures {
            self.file_signatures.insert(*k, v.clone());
        }
        self.standard_library_files.extend(other.standard_library_files.iter().copied());
        self.test_files.extend(other.test_files.iter().copied());
        self.dev_packages.extend(other.dev_packages.iter().map(|(file, packages)| (*file, packages.clone())));
        self.safe_symbols.extend(other.safe_symbols.iter().copied());
        self.safe_symbol_members.extend(other.safe_symbol_members.iter().copied());
        self.infer_types_from_usage |= other.infer_types_from_usage;
        self.merge_patch_class_likes(other.patch_class_likes.iter().map(|(k, v)| (*k, v.clone())));
        self.merge_patch_function_likes(other.patch_function_likes.iter().map(|(k, v)| (*k, v.clone())));
        self.merge_patch_constants(other.patch_constants.iter().map(|(k, v)| (*k, v.clone())));
    }

    fn merge_class_like_alias_declarations(
        &mut self,
        declarations: impl IntoIterator<Item = (Word, (Word, Span, MetadataFlags))>,
    ) {
        for (alias, (target, span, flags)) in declarations {
            self.add_class_like_alias(alias, target, span, flags);
        }
    }

    /// Adds another codebase's `extern` declarations, keeping each target's declarations in the order
    /// [`ExternMetadata`] sorts them, whatever order the files merge in.
    fn merge_externs(&mut self, incoming: impl IntoIterator<Item = ((Word, Word), Vec<ExternMetadata>)>) {
        for (target, declarations) in incoming {
            let merged = self.externs.entry(target).or_default();
            merged.extend(declarations);
            merged.sort_unstable();
            merged.dedup();
        }
    }

    /// Gives each class, method and function an `extern` line declares the attributes written before the line, as its
    /// own, spec section 29. A declaration keeps one it holds already, and [`Self::remove_entries_by_keys`] takes away
    /// those of a file that goes.
    pub(crate) fn apply_extern_attributes(&mut self) {
        let lines: Vec<((Word, Word), Vec<AttributeMetadata>)> = self
            .externs
            .values()
            .flatten()
            .filter(|declaration| !declaration.attributes.is_empty())
            .filter_map(|declaration| Some((self.extern_declaration(declaration)?, declaration.attributes.clone())))
            .collect();
        for ((class, member), added) in lines {
            let attributes = if member.is_empty() {
                self.class_likes.get_mut(&class).map(|class_like| &mut class_like.attributes)
            } else {
                self.function_likes.get_mut(&(class, member)).map(|function_like| &mut function_like.attributes)
            };
            let Some(attributes) = attributes else {
                continue;
            };
            for attribute in &added {
                if !attributes.contains(attribute) {
                    attributes.push(attribute.clone());
                }
            }
        }
    }

    /// Adds another codebase's modules, keeping them in the order [`ModuleMetadata`] sorts them, whatever order the
    /// files merge in.
    fn merge_modules(&mut self, incoming: impl IntoIterator<Item = ModuleMetadata>) {
        self.modules.extend(incoming);
        self.modules.sort_by_key(|module| (module.file, module.span.start.offset));
        self.modules.dedup();
    }

    /// Adds another codebase's `extern namespace` declarations, keeping them sorted, whatever order the files merge in.
    fn merge_namespace_imports(&mut self, incoming: impl IntoIterator<Item = NamespaceImportMetadata>) {
        merge_sorted(&mut self.namespace_imports, incoming);
    }

    fn merge_extern_namespaces(&mut self, incoming: impl IntoIterator<Item = ExternNamespaceMetadata>) {
        merge_sorted(&mut self.extern_namespaces, incoming);
    }

    /// Adds another codebase's extension members, keeping each name's members in span order, whatever order the files
    /// merge in.
    fn merge_extensions(&mut self, incoming: impl IntoIterator<Item = (Word, Vec<(Span, MethodIdentifier)>)>) {
        for (name, members) in incoming {
            let merged = self.extensions.entry(name).or_default();
            merged.extend(members);
            merged.sort_unstable();
            merged.dedup();
        }
    }

    /// Merges patch class-likes from another codebase, diagnosing collisions.
    ///
    /// At most one patch may target a given symbol. When two patches collide, the first-merged
    /// entry is kept and a [`PatchDuplicateTarget`](ScanningIssueKind::PatchDuplicateTarget)
    /// diagnostic referencing both sites is attached to it, rather than letting one silently
    /// overwrite the other in hash-order.
    fn merge_patch_class_likes(&mut self, incoming: impl IntoIterator<Item = (Word, ClassLikeMetadata)>) {
        for (k, v) in incoming {
            match self.patch_class_likes.entry(k) {
                Entry::Occupied(mut entry) => {
                    let diagnostic = duplicate_patch_class_diagnostic(entry.get(), &v);
                    entry.get_mut().issues.push(diagnostic);
                }
                Entry::Vacant(entry) => {
                    entry.insert(v);
                }
            }
        }
    }

    /// Merges patch function-likes from another codebase, diagnosing collisions on free
    /// functions. Method collisions are subsumed by the enclosing class's duplicate
    /// diagnostic, so only keys with an empty class component are reported here.
    fn merge_patch_function_likes(&mut self, incoming: impl IntoIterator<Item = ((Word, Word), FunctionLikeMetadata)>) {
        for (k, v) in incoming {
            match self.patch_function_likes.entry(k) {
                Entry::Occupied(mut entry) => {
                    if k.0.is_empty() {
                        let diagnostic = duplicate_patch_function_diagnostic(entry.get(), &v);
                        entry.get_mut().issues.push(diagnostic);
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(v);
                }
            }
        }
    }

    /// Merges patch constants from another codebase, diagnosing collisions.
    fn merge_patch_constants(&mut self, incoming: impl IntoIterator<Item = (Word, ConstantMetadata)>) {
        for (k, v) in incoming {
            match self.patch_constants.entry(k) {
                Entry::Occupied(mut entry) => {
                    let diagnostic = duplicate_patch_constant_diagnostic(entry.get(), &v);
                    entry.get_mut().issues.push(diagnostic);
                }
                Entry::Vacant(entry) => {
                    entry.insert(v);
                }
            }
        }
    }

    /// Moves every scanned entry of this per-file partial into the patch maps.
    ///
    /// Called on a per-file partial right after `scan_program` when the file is a
    /// [`FileType::Patch`]. Symbols and descendants from a patch partial are dropped — the
    /// FQCN belongs to whichever non-patch source originally declared it (or it's an orphan
    /// which `apply_patches_pass` will diagnose later).
    pub fn convert_partial_to_patch(&mut self) {
        for (k, v) in std::mem::take(&mut self.class_likes) {
            self.patch_class_likes.insert(k, v.into_inner());
        }

        for (k, v) in std::mem::take(&mut self.function_likes) {
            self.patch_function_likes.insert(k, v.into_inner());
        }

        for (k, v) in std::mem::take(&mut self.constants) {
            self.patch_constants.insert(k, v);
        }

        self.symbols = Symbols::new();
        self.class_like_aliases.clear();
        self.class_like_alias_declarations.clear();
        self.class_like_aliases_dirty = false;
        self.all_class_like_descendants.clear();
        self.direct_classlike_descendants.clear();
    }

    /// Folds every entry in the `patch_*` maps into the matching vendor / built-in entry,
    /// attaching validation diagnostics to the patch entry's `issues` list.
    ///
    /// At most one patch may target a given symbol, so each entry is applied directly to its
    /// target. A patch whose target is user-defined is inert (user definitions win); a patch
    /// with no matching target is diagnosed as an orphan.
    ///
    /// Must be called after all partials have been merged so the slots patches target are
    /// present.
    pub fn apply_patches_pass(&mut self) {
        // `(class, method)` slots where a patch overrides a method inherited from an ancestor.
        // No function-like exists at these keys yet, so the function loop below materializes
        // them from the patch's own scanned declaration rather than treating them as orphans.
        let mut inherited_overrides: HashSet<(Word, Word)> = HashSet::default();

        let class_keys: Vec<Word> = self.patch_class_likes.keys().copied().collect();
        for fqcn in class_keys {
            let Some(target) = self.class_likes.get(&fqcn) else {
                if let Some(p) = self.patch_class_likes.get_mut(&fqcn) {
                    let diag = orphan_patch_class_diagnostic(p);
                    p.issues.push(diag);
                }
                continue;
            };
            // User-defined targets win; the patch entry is inert. Leave any scan-time issues
            // on it intact — they still belong to the patch source.
            if target.flags.is_user_defined() {
                continue;
            }

            let mut working = target.clone();
            let inherited = collect_inherited_patch_methods(
                &working,
                &self.patch_class_likes[&fqcn],
                &self.class_likes,
                &self.symbols,
            );
            inherited_overrides.extend(inherited.iter().map(|method| (fqcn, *method)));
            if let Some(patch_entry) = self.patch_class_likes.get_mut(&fqcn) {
                working.apply_patch(patch_entry, &inherited);
            }
            self.class_likes.insert(fqcn, working);
        }

        let func_keys: Vec<(Word, Word)> = self.patch_function_likes.keys().copied().collect();
        for key in func_keys {
            let Some(target) = self.function_likes.get(&key) else {
                if inherited_overrides.contains(&key) {
                    // The patch overrides a method inherited from an ancestor, so no slot exists
                    // at `(class, method)` yet. The patch file declares the method in full, so
                    // promote its scanned function-like as this class's own declaration; the
                    // class loop has already pointed the declaring/appearing ids at this slot.
                    if let Some(p) = self.patch_function_likes.get(&key) {
                        let materialized = p.clone();
                        self.function_likes.insert(key, materialized.into());
                    }
                    continue;
                }
                // Methods of an orphan patch class are covered by the class-level diagnostic;
                // only free functions need their own orphan diagnostic.
                if key.0.is_empty()
                    && let Some(p) = self.patch_function_likes.get_mut(&key)
                {
                    let diag = orphan_patch_function_diagnostic(p);
                    p.issues.push(diag);
                }
                continue;
            };
            if target.flags.is_user_defined() {
                continue;
            }

            let mut working = target.clone();
            if let Some(patch_entry) = self.patch_function_likes.get_mut(&key) {
                working.apply_patch(patch_entry);
            }
            self.function_likes.insert(key, working);
        }

        let const_keys: Vec<Word> = self.patch_constants.keys().copied().collect();
        for fqcn in const_keys {
            let Some(target) = self.constants.get(&fqcn) else {
                if let Some(p) = self.patch_constants.get_mut(&fqcn) {
                    let diag = orphan_patch_constant_diagnostic(p);
                    p.issues.push(diag);
                }
                continue;
            };
            if target.flags.is_user_defined() {
                continue;
            }

            let mut working = target.clone();
            if let Some(patch_entry) = self.patch_constants.get(&fqcn) {
                working.apply_patch(patch_entry);
            }
            self.constants.insert(fqcn, working);
        }
    }

    /// Extracts only the keys that this per-file metadata currently "owns" in the given
    /// merged codebase; i.e. keys whose span in `merged` matches this metadata's span.
    ///
    /// This is what you want for incremental fingerprints. [`extract_keys`](Self::extract_keys)
    /// captures *every* key the scan produced, including ones that lost the tiebreak in
    /// [`extend`](Self::extend) / [`extend_ref`](Self::extend_ref) when another file defined
    /// the same FQN. Using `extract_keys` as a removal fingerprint then causes a nasty
    /// cross-file bug: touching file *B* can remove an entry that file *A* actually owns,
    /// because [`remove_entries_by_keys`](Self::remove_entries_by_keys) deletes by FQN
    /// without checking who the current owner is. The analyzer then reports a spurious
    /// "duplicate definition" when it walks *A* and finds *B*'s span in the codebase.
    ///
    /// By only recording the keys whose spans still match *this* metadata, removing the
    /// fingerprint later becomes a safe no-op when another file won the merge. The
    /// removal only drops the entries this file genuinely put into the merged codebase.
    #[must_use]
    pub fn extract_owned_keys(&self, merged: &CodebaseMetadata) -> CodebaseEntryKeys {
        let class_like_names = self
            .class_likes
            .iter()
            .filter(|(name, meta)| merged.class_likes.get(*name).is_some_and(|m| m.span == meta.span))
            .map(|(name, _)| *name)
            .collect();

        let class_like_aliases = self
            .class_like_alias_declarations
            .iter()
            .filter(|(name, (_, span, _))| {
                merged.class_like_alias_declarations.get(*name).is_some_and(|(_, merged_span, _)| merged_span == span)
            })
            .map(|(alias, (target, span, _))| (*alias, *target, *span))
            .collect();

        let function_like_keys = self
            .function_likes
            .iter()
            .filter(|(key, meta)| merged.function_likes.get(*key).is_some_and(|m| m.span == meta.span))
            .map(|(key, _)| *key)
            .collect();

        let constant_names = self
            .constants
            .iter()
            .filter(|(name, meta)| merged.constants.get(*name).is_some_and(|m| m.span == meta.span))
            .map(|(name, _)| *name)
            .collect();

        // A file signature and a folder are always owned by their file (there is at most one of each per file).
        let mut file_ids: Vec<FileId> = self.file_signatures.keys().chain(self.file_names.keys()).copied().collect();
        file_ids.sort_unstable();
        file_ids.dedup();

        CodebaseEntryKeys { class_like_names, class_like_aliases, function_like_keys, constant_names, file_ids }
    }

    /// Removes entries whose keys match the given [`CodebaseEntryKeys`].
    ///
    /// This is the lightweight equivalent of [`remove_entries_of()`] — it performs the
    /// same removals but from a compact key set instead of a full `CodebaseMetadata` reference.
    pub fn remove_entries_by_keys(&mut self, keys: &CodebaseEntryKeys) {
        if !keys.class_like_names.is_empty() && !self.class_like_alias_declarations.is_empty() {
            self.class_like_aliases_dirty = true;
        }

        for k in &keys.class_like_names {
            self.class_likes.remove(k);
            self.scanned_class_likes.remove(k);
            self.symbols.remove(*k);
        }

        for (alias, _, _) in &keys.class_like_aliases {
            self.class_like_alias_declarations.remove(alias);
            self.class_like_aliases_dirty = true;
        }

        for k in &keys.function_like_keys {
            self.function_likes.remove(k);
        }

        for k in &keys.constant_names {
            self.constants.remove(k);
        }

        for k in &keys.file_ids {
            self.file_signatures.remove(k);
            self.file_names.remove(k);
            self.standard_library_files.remove(k);
            self.test_files.remove(k);
            self.dev_packages.remove(k);
        }

        // Drop any patch entry that originated from a file signature we just removed; a patch
        // entry's originating file is recorded on its span.
        let removed_files: HashSet<FileId> = keys.file_ids.iter().copied().collect();
        self.patch_class_likes.retain(|_, m| !removed_files.contains(&m.span.file_id));
        self.patch_function_likes.retain(|_, m| !removed_files.contains(&m.span.file_id));
        self.patch_constants.retain(|_, m| !removed_files.contains(&m.span.file_id));
        for declarations in self.externs.values_mut() {
            declarations.retain(|declaration| !removed_files.contains(&declaration.span.file_id));
        }
        self.externs.retain(|_, declarations| !declarations.is_empty());
        // An `extern` line of a file that goes takes its attributes away from what it declares.
        for class_like in self.class_likes.values_mut() {
            class_like.attributes.retain(|attribute| !removed_files.contains(&attribute.span.file_id));
        }
        for function_like in self.function_likes.values_mut() {
            function_like.attributes.retain(|attribute| !removed_files.contains(&attribute.span.file_id));
        }
        self.modules.retain(|module| !removed_files.contains(&module.span.file_id));
        self.extern_namespaces.retain(|declaration| !removed_files.contains(&declaration.span.file_id));
        self.namespace_imports.retain(|import| !removed_files.contains(&import.span.file_id));
        for members in self.extensions.values_mut() {
            members.retain(|(span, _)| !removed_files.contains(&span.file_id));
        }
        self.extensions.retain(|_, members| !members.is_empty());
    }

    /// Takes all issues from the codebase metadata.
    pub fn take_issues(&mut self, user_defined: bool) -> IssueCollection {
        let mut issues = IssueCollection::new();

        // An entry without issues is left as it is, so no copy of the codebase copies an entry it shares to take none.
        for meta in self.class_likes.values_mut() {
            if (user_defined && !meta.flags.is_user_defined()) || meta.issues.is_empty() {
                continue;
            }
            issues.extend(meta.take_issues());
        }

        for meta in self.function_likes.values_mut() {
            if (user_defined && !meta.flags.is_user_defined()) || meta.issues.is_empty() {
                continue;
            }
            issues.extend(meta.take_issues());
        }

        for meta in self.constants.values_mut() {
            if user_defined && !meta.flags.is_user_defined() {
                continue;
            }
            issues.extend(meta.take_issues());
        }

        // Patches are user-authored, so their issues are always reported regardless of the
        // `user_defined` filter. They live in their own maps and never appear in the regular
        // class_likes/function_likes/constants iteration above.
        for meta in self.patch_class_likes.values_mut() {
            issues.extend(meta.take_issues());
        }

        for meta in self.patch_function_likes.values_mut() {
            issues.extend(meta.take_issues());
        }

        for meta in self.patch_constants.values_mut() {
            issues.extend(meta.take_issues());
        }

        issues
    }

    /// Gets all file IDs that have signatures in this metadata.
    ///
    /// This is a helper method for incremental analysis to iterate over all files.
    #[must_use]
    pub fn get_all_file_ids(&self) -> Vec<FileId> {
        self.file_signatures.keys().copied().collect()
    }
}

/// The key of the field named `name` whose declaration carries `attributes`, spec section 10, and whether the encoding
/// leaves it out when null: the name `[Key("…")]` gives, or the field's own, and `[Key(omitNull: true)]`'s flag.
/// `Sharp\Key` is a PHP# class-like, keyed by its exact name, spec section 24.
fn field_key<'codebase>(attributes: &'codebase [AttributeMetadata], name: &'codebase [u8]) -> (&'codebase [u8], bool) {
    let Some(key) = attributes.iter().find(|attribute| attribute.name.as_bytes() == b"Sharp\\Key") else {
        return (name, false);
    };

    (key.text_argument(0, b"name").unwrap_or(name), key.flag_argument(1, b"omitNull"))
}

/// Whether `method` is static, which its method metadata holds, not its flags.
fn is_static_method(method: &FunctionLikeMetadata) -> bool {
    method.method_metadata.as_ref().is_some_and(|method| method.is_static)
}

/// Returns the subset of methods declared by `patch` that are inherited by `target` from
/// an ancestor but not declared on `target` itself. Used by `apply_patch` on class-like
/// metadata to distinguish patch-declared overrides of inherited methods (allowed) from
/// patch-introduced new methods (disallowed).
fn collect_inherited_patch_methods(
    target: &ClassLikeMetadata,
    patch: &ClassLikeMetadata,
    class_likes: &WordMap<Shared<ClassLikeMetadata>>,
    symbols: &Symbols,
) -> WordSet {
    if patch.methods.is_empty() {
        return WordSet::default();
    }
    let ancestor_methods = class_like::collect_ancestor_methods(target, class_likes, symbols);
    patch.methods.iter().filter(|m| ancestor_methods.contains(*m)).copied().collect()
}

fn duplicate_patch_class_diagnostic(kept: &ClassLikeMetadata, dropped: &ClassLikeMetadata) -> Issue {
    Issue::error(format!(
        "Multiple patches target `{}`; at most one patch may target a given symbol.",
        kept.original_name
    ))
    .with_code(ScanningIssueKind::PatchDuplicateTarget)
    .with_annotation(Annotation::primary(dropped.span).with_message("Duplicate patch for this symbol."))
    .with_annotation(Annotation::secondary(kept.span).with_message("Already patched here."))
    .with_help("Merge the conflicting declarations into a single patch, or remove all but one.")
}

fn duplicate_patch_function_diagnostic(kept: &FunctionLikeMetadata, dropped: &FunctionLikeMetadata) -> Issue {
    Issue::error(format!(
        "Multiple patches target function `{}`; at most one patch may target a given symbol.",
        kept.name
    ))
    .with_code(ScanningIssueKind::PatchDuplicateTarget)
    .with_annotation(Annotation::primary(dropped.span).with_message("Duplicate patch for this function."))
    .with_annotation(Annotation::secondary(kept.span).with_message("Already patched here."))
    .with_help("Merge the conflicting declarations into a single patch, or remove all but one.")
}

fn duplicate_patch_constant_diagnostic(kept: &ConstantMetadata, dropped: &ConstantMetadata) -> Issue {
    Issue::error(format!(
        "Multiple patches target constant `{}`; at most one patch may target a given symbol.",
        kept.name
    ))
    .with_code(ScanningIssueKind::PatchDuplicateTarget)
    .with_annotation(Annotation::primary(dropped.span).with_message("Duplicate patch for this constant."))
    .with_annotation(Annotation::secondary(kept.span).with_message("Already patched here."))
    .with_help("Merge the conflicting declarations into a single patch, or remove all but one.")
}

fn orphan_patch_class_diagnostic(meta: &ClassLikeMetadata) -> Issue {
    Issue::error(format!(
        "Patch declares `{}` but no vendored or built-in definition exists to patch.",
        meta.original_name,
    ))
    .with_code(ScanningIssueKind::PatchIntroducesNewSymbol)
    .with_annotation(Annotation::primary(meta.span))
    .with_help(
        "The patch may be misnamed or out-of-date relative to the vendored or built-in definition; \
         check the symbol name and verify the patch still matches the upstream source.",
    )
}

fn orphan_patch_function_diagnostic(meta: &FunctionLikeMetadata) -> Issue {
    Issue::error(format!(
        "Patch declares function `{}` but no vendored or built-in definition exists to patch.",
        meta.name,
    ))
    .with_code(ScanningIssueKind::PatchIntroducesNewSymbol)
    .with_annotation(Annotation::primary(meta.span))
    .with_help(
        "The patch may be misnamed or out-of-date relative to the vendored or built-in definition; \
         check the function name and verify the patch still matches the upstream source.",
    )
}

fn orphan_patch_constant_diagnostic(meta: &ConstantMetadata) -> Issue {
    Issue::error(format!(
        "Patch declares constant `{}` but no vendored or built-in definition exists to patch.",
        meta.name,
    ))
    .with_code(ScanningIssueKind::PatchIntroducesNewSymbol)
    .with_annotation(Annotation::primary(meta.span))
    .with_help(
        "The patch may be misnamed or out-of-date relative to the vendored or built-in definition; \
         check the constant name and verify the patch still matches the upstream source.",
    )
}

/// The types and attributes of `function_like`'s signature, docblock and assertions, and the effects it `uses`, for
/// [`CodebaseMetadata::named_class_likes`].
fn function_like_types<'metadata>(
    function_like: &'metadata FunctionLikeMetadata,
    names: &mut WordSet,
    unions: &mut Vec<&'metadata TUnion>,
    atomics: &mut Vec<&'metadata TAtomic>,
    attributes: &mut Vec<&'metadata AttributeMetadata>,
) {
    names.extend(&function_like.uses);
    attributes.extend(&function_like.attributes);
    unions.extend(&function_like.listens);
    unions.extend(
        [&function_like.return_type_declaration_metadata, &function_like.return_type_metadata]
            .into_iter()
            .flatten()
            .chain(&function_like.thrown_types)
            .chain(function_like.method_metadata.iter().flat_map(|method| method.where_constraints.values()))
            .map(|metadata| &metadata.type_union),
    );
    for parameter in &function_like.parameters {
        parameter_types(parameter, unions, attributes);
    }
    for template in function_like.template_types.values() {
        template_types(template, unions);
    }
    atomics.extend(
        function_like
            .assertions
            .values()
            .chain(function_like.if_true_assertions.values())
            .chain(function_like.if_false_assertions.values())
            .flatten()
            .filter_map(Assertion::get_type),
    );
}

fn parameter_types<'metadata>(
    parameter: &'metadata FunctionLikeParameterMetadata,
    unions: &mut Vec<&'metadata TUnion>,
    attributes: &mut Vec<&'metadata AttributeMetadata>,
) {
    attributes.extend(&parameter.attributes);
    unions.extend(
        [
            &parameter.type_declaration_metadata,
            &parameter.type_metadata,
            &parameter.out_type,
            &parameter.closure_this_type,
            &parameter.default_type,
        ]
        .into_iter()
        .flatten()
        .map(|metadata| &metadata.type_union),
    );
}

fn template_types<'metadata>(template: &'metadata GenericTemplate, unions: &mut Vec<&'metadata TUnion>) {
    unions.push(&template.constraint);
    unions.extend(&template.default);
    unions.extend(template.written_bound.iter().map(|bound| &bound.type_union));
}

/// Adds `incoming` to `sorted`, which stays sorted with no duplicate. One file's entries sort together, so a file's
/// entries land in one gap of the others and move only the entries after them, where sorting the whole on each of the
/// project's files made merging them quadratic.
fn merge_sorted<T>(sorted: &mut Vec<T>, incoming: impl IntoIterator<Item = T>)
where
    T: Ord + Copy,
{
    let mut incoming: Vec<T> = incoming.into_iter().collect();
    let (Some(&first), Some(&last)) = (incoming.iter().min(), incoming.iter().max()) else {
        return;
    };
    let start = sorted.partition_point(|entry| *entry < first);
    if sorted.partition_point(|entry| *entry <= last) == start {
        incoming.sort_unstable();
        incoming.dedup();
        sorted.splice(start..start, incoming);
    } else {
        sorted.extend(incoming);
        sorted.sort_unstable();
        sorted.dedup();
    }
}

/// Determines which metadata value to keep when merging duplicates.
///
/// Priority:
///   1. plain PHP > PHP# module — a PHP# module never changes what plain PHP
///      code means, so a plain PHP class-like of its name keeps the name.
///   2. user-defined > patch > external > built-in > other.
///   3. non-polyfill > polyfill — tools like rector/phpstan/psalm ship
///      skeleton stubs gated by `if (!class_exists('X'))` that should never
///      shadow a concrete definition.
///   4. the declaration whose file's name in `file_names` sorts first, as
///      `app/Shop/Cart.sharp` before `src/Shop/Cart.sharp`, and in one file
///      the earlier one, so the user can tell which file keeps the name. A
///      file `file_names` doesn't name, a built-in's, sorts first, and two of
///      them by their spans.
///
/// Returns `true` if the new value should replace the existing one.
fn should_replace_metadata(
    file_names: &HashMap<FileId, Word>,
    standard_library_files: &HashSet<FileId>,
    (existing_flags, existing_span): (MetadataFlags, Span),
    (new_flags, new_span): (MetadataFlags, Span),
) -> bool {
    // Spec section 23 reserves the standard library's names: a declaration of the library keeps its name over any other
    // file's, which the analysis refuses, so every file resolves the name to the library's code.
    let new_is_library = standard_library_files.contains(&new_span.file_id);
    if new_is_library != standard_library_files.contains(&existing_span.file_id) {
        return new_is_library;
    }

    if new_flags.is_module() && !existing_flags.is_sharp() {
        return false;
    }

    if existing_flags.is_module() && !new_flags.is_sharp() {
        return true;
    }

    // A library's polyfill declares a name only when the engine lacks it, as `if (!function_exists('str_starts_with'))`
    // does, so it never takes the name of one the engine builds in, for its signature, its effects or its file.
    if existing_flags.is_built_in() && new_flags.is_polyfill() {
        return false;
    }
    if new_flags.is_built_in() && existing_flags.is_polyfill() {
        return true;
    }

    let new_is_user_defined = new_flags.is_user_defined();
    let existing_is_user_defined = existing_flags.is_user_defined();

    if new_is_user_defined != existing_is_user_defined {
        return new_is_user_defined;
    }

    let new_is_patch = new_flags.is_patch();
    let existing_is_patch = existing_flags.is_patch();

    if new_is_patch != existing_is_patch {
        return new_is_patch;
    }

    let new_is_external = new_flags.is_external();
    let existing_is_external = existing_flags.is_external();

    if new_is_external != existing_is_external {
        return new_is_external;
    }

    let new_is_built_in = new_flags.is_built_in();
    let existing_is_built_in = existing_flags.is_built_in();

    if new_is_built_in != existing_is_built_in {
        return new_is_built_in;
    }

    let new_is_polyfill = new_flags.is_polyfill();
    let existing_is_polyfill = existing_flags.is_polyfill();

    if new_is_polyfill != existing_is_polyfill {
        return !new_is_polyfill;
    }

    let declared_at = |span: Span| (file_names.get(&span.file_id), span);

    declared_at(new_span) < declared_at(existing_span)
}

/// The lowercase short name of the class-like `name`, the part after its last `\`.
/// The key of the global constant `name` in `constants`: its namespace in lowercase, as PHP finds a namespace in any
/// case, and its own name exact. A name whose namespace is lowercase is its own key, so only another one interns.
fn constant_key(name: Word) -> Word {
    let bytes = name.as_bytes();
    match bytes.iter().rposition(|byte| *byte == b'\\') {
        Some(separator) if bytes[..separator].iter().any(u8::is_ascii_uppercase) => {
            ascii_lowercase_constant_name_word(bytes)
        }
        _ => name,
    }
}

fn short_name_key(name: &[u8]) -> Word {
    ascii_lowercase_word(name.rsplit(|byte| *byte == b'\\').next().unwrap_or(name))
}

#[cfg(test)]
mod should_replace_metadata_tests {
    use super::*;

    /// [`should_replace_metadata`] on two declarations of files that no name is known for.
    fn replaces(existing_flags: MetadataFlags, existing_span: Span, new_flags: MetadataFlags, new_span: Span) -> bool {
        should_replace_metadata(
            &HashMap::default(),
            &HashSet::default(),
            (existing_flags, existing_span),
            (new_flags, new_span),
        )
    }

    /// Of two declarations with the same priority, the one whose file's name sorts first is kept, whichever file id
    /// sorts first and whichever is merged first, and in one file the earlier one.
    #[test]
    fn the_file_whose_name_sorts_first_keeps_the_name() {
        let flags = MetadataFlags::USER_DEFINED;
        let (app, src) = (FileId::new(b"app/Shop/Cart.sharp"), FileId::new(b"src/Shop/Cart.sharp"));
        let file_names: HashMap<FileId, Word> =
            [(app, word("app/Shop/Cart.sharp")), (src, word("src/Shop/Cart.sharp"))].into_iter().collect();
        let (in_app, in_src) = (Span::new(app, 40.into(), 60.into()), Span::new(src, 0.into(), 20.into()));

        assert!(src < app, "the file ids sort the other way from the names");
        assert!(should_replace_metadata(&file_names, &HashSet::default(), (flags, in_src), (flags, in_app)));
        assert!(!should_replace_metadata(&file_names, &HashSet::default(), (flags, in_app), (flags, in_src)));

        let earlier = Span::new(app, 0.into(), 20.into());
        assert!(should_replace_metadata(&file_names, &HashSet::default(), (flags, in_app), (flags, earlier)));
    }

    /// A declaration of the standard library keeps its name over a project's, whichever is merged first.
    #[test]
    fn the_standard_library_keeps_its_name_over_a_project_file() {
        let flags = MetadataFlags::USER_DEFINED;
        let (library, project) = (FileId::new(b"library/Sharp/Bool.sharp"), FileId::new(b"src/App/Bool.sharp"));
        let library_files: HashSet<FileId> = std::iter::once(library).collect();
        let (in_library, in_project) =
            (Span::new(library, 0.into(), 20.into()), Span::new(project, 0.into(), 20.into()));

        assert!(!should_replace_metadata(
            &HashMap::default(),
            &library_files,
            (flags, in_library),
            (flags, in_project)
        ));
        assert!(should_replace_metadata(&HashMap::default(), &library_files, (flags, in_project), (flags, in_library)));
    }

    #[test]
    fn non_polyfill_replaces_polyfill() {
        let polyfill = MetadataFlags::POLYFILL;
        let real = MetadataFlags::empty();
        assert!(replaces(polyfill, Span::dummy(0, 100), real, Span::dummy(0, 100)));
        assert!(!replaces(real, Span::dummy(0, 100), polyfill, Span::dummy(0, 100)));
    }

    #[test]
    fn polyfill_does_not_replace_non_polyfill_even_with_smaller_span() {
        let real = MetadataFlags::empty();
        let polyfill = MetadataFlags::POLYFILL;
        assert!(!replaces(real, Span::dummy(500, 600), polyfill, Span::dummy(0, 10)));
    }

    #[test]
    fn user_defined_beats_polyfill_flag() {
        let polyfill_user = MetadataFlags::POLYFILL | MetadataFlags::USER_DEFINED;
        let plain = MetadataFlags::empty();
        assert!(!replaces(polyfill_user, Span::dummy(0, 10), plain, Span::dummy(0, 10)));
        assert!(replaces(plain, Span::dummy(0, 10), polyfill_user, Span::dummy(0, 10)));
    }

    #[test]
    fn two_user_defined_fall_through_to_polyfill_check() {
        let a = MetadataFlags::POLYFILL | MetadataFlags::USER_DEFINED;
        let b = MetadataFlags::USER_DEFINED;
        assert!(replaces(a, Span::dummy(0, 10), b, Span::dummy(0, 10)));
        assert!(!replaces(b, Span::dummy(0, 10), a, Span::dummy(0, 10)));
    }

    /// A library's polyfill never replaces what the engine builds in, though it is user-defined, and the built-in
    /// replaces the polyfill whichever the scan met first.
    #[test]
    fn a_built_in_keeps_its_name_from_a_user_defined_polyfill() {
        let builtin = MetadataFlags::BUILTIN;
        let polyfill = MetadataFlags::POLYFILL | MetadataFlags::USER_DEFINED;
        assert!(!replaces(builtin, Span::dummy(0, 10), polyfill, Span::dummy(0, 10)));
        assert!(replaces(polyfill, Span::dummy(0, 10), builtin, Span::dummy(0, 10)));
    }

    #[test]
    fn two_non_polyfills_fall_through_to_priority_rules() {
        let user = MetadataFlags::USER_DEFINED;
        let builtin = MetadataFlags::BUILTIN;
        assert!(!replaces(user, Span::dummy(0, 10), builtin, Span::dummy(0, 10)));
        assert!(replaces(builtin, Span::dummy(0, 10), user, Span::dummy(0, 10)));
    }

    #[test]
    fn patch_beats_vendored() {
        let vendored = MetadataFlags::empty();
        let patch = MetadataFlags::PATCH;
        assert!(replaces(vendored, Span::dummy(0, 100), patch, Span::dummy(0, 100)));
        assert!(!replaces(patch, Span::dummy(0, 100), vendored, Span::dummy(0, 100)));
    }

    #[test]
    fn patch_beats_builtin() {
        let builtin = MetadataFlags::BUILTIN;
        let patch = MetadataFlags::PATCH;
        assert!(replaces(builtin, Span::dummy(0, 100), patch, Span::dummy(0, 100)));
        assert!(!replaces(patch, Span::dummy(0, 100), builtin, Span::dummy(0, 100)));
    }

    #[test]
    fn external_beats_builtin_and_vendored() {
        let external = MetadataFlags::EXTERNAL;
        let builtin = MetadataFlags::BUILTIN;
        let vendored = MetadataFlags::empty();

        assert!(replaces(builtin, Span::dummy(0, 100), external, Span::dummy(500, 600)));
        assert!(!replaces(external, Span::dummy(500, 600), builtin, Span::dummy(0, 100)));
        assert!(replaces(vendored, Span::dummy(0, 100), external, Span::dummy(500, 600)));
        assert!(!replaces(external, Span::dummy(500, 600), vendored, Span::dummy(0, 100)));
    }

    #[test]
    fn patch_and_user_defined_beat_external() {
        let external = MetadataFlags::EXTERNAL;
        let patch = MetadataFlags::PATCH;
        let user = MetadataFlags::USER_DEFINED;

        assert!(replaces(external, Span::dummy(0, 100), patch, Span::dummy(500, 600)));
        assert!(!replaces(patch, Span::dummy(500, 600), external, Span::dummy(0, 100)));
        assert!(replaces(external, Span::dummy(0, 100), user, Span::dummy(500, 600)));
        assert!(!replaces(user, Span::dummy(500, 600), external, Span::dummy(0, 100)));
    }

    #[test]
    fn user_defined_beats_patch() {
        let user = MetadataFlags::USER_DEFINED;
        let patch = MetadataFlags::PATCH;
        assert!(!replaces(user, Span::dummy(0, 100), patch, Span::dummy(0, 100)));
        assert!(replaces(patch, Span::dummy(0, 100), user, Span::dummy(0, 100)));
    }

    #[test]
    fn patch_does_not_beat_user_defined_even_with_smaller_span() {
        let user = MetadataFlags::USER_DEFINED;
        let patch = MetadataFlags::PATCH;
        assert!(!replaces(user, Span::dummy(500, 600), patch, Span::dummy(0, 10)));
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn patch_function_like_leaves_vendor_owning_slot() {
        // Patches may only refine type information on an existing function-like; they must
        // never become the slot owner. The non-patch source's span/file id stay put so that
        // `extract_owned_keys` records vendor-as-owner — otherwise the patch's entry would
        // outlive a vendor deletion in incremental mode (orphan function-like bug).
        use crate::metadata::function_like::FunctionLikeKind;

        let name = word("foo");
        let key = (empty_word(), name);
        let vendor_span = Span::dummy(0, 100);
        let patch_span = Span::dummy(500, 600);

        let vendor =
            FunctionLikeMetadata::new(FunctionLikeKind::Function, name, name, vendor_span, MetadataFlags::empty());
        let mut codebase = CodebaseMetadata::new();
        codebase.function_likes.insert(key, vendor.into());

        let patch = FunctionLikeMetadata::new(FunctionLikeKind::Function, name, name, patch_span, MetadataFlags::PATCH);
        codebase.patch_function_likes.insert(key, patch);

        codebase.apply_patches_pass();

        let merged = codebase.function_likes.get(&key).expect("function-like must remain after patch");
        assert_eq!(merged.span, vendor_span, "patch must not move the slot's span");
        assert!(!merged.flags.is_patch(), "patch must not flip the slot's flags");
    }

    #[test]
    fn patch_does_not_apply_to_user_defined_class() {
        let class_name = word("MyClass");
        let method_existing = word("doIt");

        let mut user_class =
            ClassLikeMetadata::new(class_name, class_name, Span::dummy(0, 100), None, MetadataFlags::USER_DEFINED);
        user_class.methods.insert(method_existing);

        let mut codebase = CodebaseMetadata::new();
        codebase.class_likes.insert(class_name, user_class.into());

        let mut patch_class =
            ClassLikeMetadata::new(class_name, class_name, Span::dummy(0, 50), None, MetadataFlags::PATCH);
        let method_new = word("patchedMethod");
        patch_class.methods.insert(method_new);

        codebase.patch_class_likes.insert(class_name, patch_class);

        codebase.apply_patches_pass();

        let class = &codebase.class_likes[&class_name];
        // Patch must not apply to a user-defined class.
        assert!(!class.methods.contains(&method_new));
        // User-defined class must be preserved intact.
        assert!(class.methods.contains(&method_existing));
        assert!(class.flags.is_user_defined());
        // No issues should be emitted.
        assert!(class.issues.is_empty(), "{:?}", class.issues);
    }
}

#[cfg(test)]
mod get_constant_or_global_tests {
    use super::*;

    fn codebase_with(names: &[&str]) -> CodebaseMetadata {
        let mut codebase = CodebaseMetadata::new();
        for name in names {
            codebase.constants.insert(
                ascii_lowercase_constant_name_word(name.as_bytes()),
                ConstantMetadata::new(word(name), Span::dummy(0, 1), MetadataFlags::empty()),
            );
        }

        codebase
    }

    fn found(codebase: &CodebaseMetadata, resolved: &str, written: &str) -> Option<String> {
        codebase.get_constant_or_global(word(resolved), word(written)).map(|constant| constant.name.to_string())
    }

    #[test]
    fn a_namespaced_constant_wins_over_the_global_one_of_its_name() {
        let codebase = codebase_with(&["Lib\\LIMIT", "LIMIT"]);

        assert_eq!(found(&codebase, "Lib\\LIMIT", "LIMIT").as_deref(), Some("Lib\\LIMIT"));
    }

    #[test]
    fn the_global_constant_is_the_fallback_when_the_namespace_declares_none() {
        let codebase = codebase_with(&["LIMIT"]);

        assert_eq!(found(&codebase, "Lib\\LIMIT", "LIMIT").as_deref(), Some("LIMIT"));
        assert_eq!(found(&codebase, "Lib\\OTHER", "OTHER"), None);
    }
}

#[cfg(test)]
mod import_cycle_tests {
    use super::*;

    /// A codebase whose file `{namespace}\File{index}.sharp` imports each of `imports`, a class of another namespace.
    fn importing(files: &[(&str, &[&str])]) -> CodebaseMetadata {
        let mut codebase = CodebaseMetadata::new();
        for (index, (namespace, imports)) in files.iter().enumerate() {
            let file = word(format!("{namespace}\\File{index}.sharp").replace('\\', "/"));
            for (offset, import) in (0u32..).zip(imports.iter()) {
                let span = Span::new(FileId::new(file.as_bytes()), (offset * 10).into(), (offset * 10 + 5).into());
                codebase.namespace_imports.push(NamespaceImportMetadata::new(
                    file,
                    span,
                    word(namespace),
                    word(import),
                ));
            }
        }
        codebase.namespace_imports.sort_unstable();
        codebase.populate_import_cycles();

        codebase
    }

    fn cycle(codebase: &CodebaseMetadata, import: &str) -> Option<Vec<String>> {
        let own = codebase.namespace_imports.iter().find(|own| own.name.as_bytes() == import.as_bytes())?;

        Some(codebase.import_cycle(own.span)?.iter().map(|import| import.name.to_string()).collect())
    }

    /// Each import of a cycle names the shortest chain back, and an import into the cycle from outside it, or out of
    /// it, closes none.
    #[test]
    fn each_import_of_a_cycle_names_the_shortest_chain_back_and_no_other_import_names_one() {
        let codebase = importing(&[
            ("App\\Shop", &["App\\Billing\\Ledger", "App\\Stock\\Shelf"]),
            ("App\\Billing", &["App\\Shop\\Cart", "App\\Stock\\Count"]),
            ("App\\Stock", &["App\\Shop\\Order"]),
            ("App\\Web", &["App\\Shop\\Basket"]),
            ("App\\Shop", &["Lib\\Clock"]),
        ]);

        assert_eq!(
            cycle(&codebase, "App\\Billing\\Ledger"),
            Some(vec!["App\\Billing\\Ledger".to_string(), "App\\Shop\\Cart".to_string()])
        );
        assert_eq!(
            cycle(&codebase, "App\\Stock\\Count"),
            Some(vec![
                "App\\Stock\\Count".to_string(),
                "App\\Shop\\Order".to_string(),
                "App\\Billing\\Ledger".to_string()
            ])
        );
        assert_eq!(cycle(&codebase, "App\\Shop\\Basket"), None);
        assert_eq!(cycle(&codebase, "Lib\\Clock"), None);
    }
}
