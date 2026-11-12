//! Every declaration a law reaches, across files.

use foldhash::HashMap;
use foldhash::HashSet;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::reference::CascadeEdge;
use mago_codex::reference::SymbolReferences;
use mago_codex::symbol::SymbolIdentifier;
use mago_database::file::FileId;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::empty_word;
use mago_word::word;

/// What the analysis knows about a class a law reaches, from outside the class's own file.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReachedClass {
    /// The class's fully qualified name as its declaration spells it.
    pub(crate) name: Word,
    pub(crate) file: FileId,
    /// Whether a `.sharp` file declares it, and not plain PHP.
    pub(crate) sharp: bool,
    /// Whether it finds its methods only by their exact names, spec section 24, as a PHP# class and every class below
    /// one do.
    exact_names: bool,
}

/// What the analysis knows about a method a law reaches, from outside its class's file.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReachedMethod {
    /// The method's name as its declaration spells it.
    pub(crate) name: Word,
    /// The arguments a call passes. An extension member's receiver, spec section 26, is written before the dot, as an
    /// object is, and is not one.
    pub(crate) parameters: usize,
    /// Whether its declared return type takes `null`.
    pub(crate) returns_nullable: bool,
}

/// Every declaration each law reaches: the law's body, every method it calls, transitively, and every type those
/// mention, along the codebase's reference edges.
#[derive(Debug, Default)]
pub struct Reach {
    /// Each reached declaration by its class's key and its member as the class keys it, and each reached class with an
    /// empty member.
    declarations: HashSet<SymbolIdentifier>,
    /// Each reached class by its key, as the codebase keys it.
    classes: HashMap<Word, ReachedClass>,
    methods: HashMap<SymbolIdentifier, ReachedMethod>,
    /// Each property that holds a value, by its class's key and its exact name without `$`, as PHP finds a property: it
    /// is not static and has no accessor body.
    stored_fields: HashSet<(Word, Word)>,
    /// Each property whose declared type takes `null`, by its class's key and its exact name without `$`.
    nullable_fields: HashSet<(Word, Word)>,
    /// The files each file's laws reach, its own included.
    files: HashMap<FileId, Vec<FileId>>,
}

impl Reach {
    /// Walks from each law of `codebase` along the signature and body edges of `references`. A law's body records its
    /// references under the law's own symbol, its class and its name as the class keys a method.
    #[must_use]
    pub fn new(codebase: &CodebaseMetadata, references: &SymbolReferences) -> Reach {
        let mut edges: HashMap<SymbolIdentifier, Vec<SymbolIdentifier>> = HashMap::default();
        for edge in references.cascade_edges(codebase) {
            if let CascadeEdge::Signature { from, to } | CascadeEdge::Body { from, to } = edge {
                edges.entry(keyed(codebase, from)).or_default().push(keyed(codebase, to));
            }
        }

        let mut reach = Reach::default();
        for class in codebase.class_likes.values().filter(|class| !class.laws.is_empty()) {
            let law_file = class.span.file_id;
            let mut reached: HashSet<SymbolIdentifier> = HashSet::default();
            let mut pending: Vec<SymbolIdentifier> =
                class.laws.keys().map(|law| keyed(codebase, (class.name, *law))).collect();
            while let Some(next) = pending.pop() {
                if !reached.insert(next) {
                    continue;
                }

                pending.extend(edges.get(&next).into_iter().flatten().copied());
                if !next.1.is_empty() {
                    pending.push((next.0, empty_word()));
                } else if let Some(metadata) = codebase.get_class_like(next.0.as_bytes()) {
                    pending.extend(metadata.properties.keys().map(|property| (next.0, *property)));
                }
            }

            let mut files = vec![law_file];
            for &(class, member) in &reached {
                let Some(metadata) = codebase.get_class_like(class.as_bytes()) else {
                    continue;
                };
                let file = metadata.span.file_id;
                files.push(file);
                reach.classes.entry(class).or_insert(ReachedClass {
                    name: metadata.original_name,
                    file,
                    sharp: metadata.flags.is_sharp(),
                    exact_names: metadata.exact_names,
                });
                if member.is_empty() {
                    reach.record_member(codebase, class, metadata.method_key(b"__construct"));
                } else {
                    reach.record_member(codebase, class, member);
                }
            }

            files.sort_unstable();
            files.dedup();
            let entry = reach.files.entry(law_file).or_default();
            entry.extend(files);
            entry.sort_unstable();
            entry.dedup();
            reach.declarations.extend(reached);
        }

        reach
    }

    /// The files the laws of `file_id` reach, `file_id` included, or none when it states no law.
    #[must_use]
    pub fn files(&self, file_id: FileId) -> &[FileId] {
        self.files.get(&file_id).map_or(&[], Vec::as_slice)
    }

    /// Records what the analysis knows about `class`'s reached member `member`: a method's name, arity and return, or a
    /// property's storage and type.
    fn record_member(&mut self, codebase: &CodebaseMetadata, class: Word, member: Word) {
        if let Some(property) = member.as_bytes().strip_prefix(b"$") {
            let Some(metadata) = codebase.get_declaring_property(class.as_bytes(), member.as_bytes()) else {
                return;
            };
            let field = (class, word(property));
            if !metadata.flags.is_static() && metadata.hooks.values().all(|hook| hook.is_abstract) {
                self.stored_fields.insert(field);
            }
            if metadata.type_metadata.as_ref().is_some_and(|r#type| r#type.type_union.is_nullable()) {
                self.nullable_fields.insert(field);
            }
        } else if !member.is_empty()
            && let Some(metadata) = codebase.get_method(class.as_bytes(), member.as_bytes())
        {
            self.methods.insert(
                (class, member),
                ReachedMethod {
                    name: metadata.original_name,
                    parameters: metadata.parameters.len()
                        - usize::from(metadata.flags.is_extension_method() || metadata.flags.is_extension_property()),
                    returns_nullable: metadata
                        .return_type_declaration_metadata
                        .as_ref()
                        .is_some_and(|r#type| r#type.type_union.is_nullable()),
                },
            );
        }
    }

    /// Whether a law reaches `member` of the fully qualified `class`, or `class` itself when `member` is empty.
    pub(crate) fn reaches(&self, class: &[u8], member: &[u8]) -> bool {
        self.declarations.contains(&self.member_key(class, member))
    }

    /// What the analysis knows about the reached class `class`.
    pub(crate) fn class(&self, class: &[u8]) -> Option<ReachedClass> {
        self.classes.get(&self.class_key(class)).copied()
    }

    /// Whether the names `left` and `right` name one class, by [`Reach::class_key`].
    pub(crate) fn same_class(&self, left: &[u8], right: &[u8]) -> bool {
        self.class_key(left) == self.class_key(right)
    }

    /// The key of the reached class `class` names, as the codebase keys it, spec section 24: a PHP# class's exact name,
    /// so another case names none, and a plain PHP class's lowercase name, as an unreached class's is.
    fn class_key(&self, class: &[u8]) -> Word {
        let exact = word(class);
        if self.classes.contains_key(&exact) {
            return exact;
        }

        let lowercase = ascii_lowercase_word(class);
        if self.classes.get(&lowercase).is_some_and(|reached| reached.sharp) { exact } else { lowercase }
    }

    /// What the analysis knows about the reached method `method` of `class`.
    pub(crate) fn method(&self, class: &[u8], method: &[u8]) -> Option<ReachedMethod> {
        self.methods.get(&self.member_key(class, method)).copied()
    }

    /// `class`'s member `member` as the reach keys it: a method by its exact name on a class that finds its methods so,
    /// and else lowercase, as `keyed` keys it, and a property, written with `$`, by its name.
    fn member_key(&self, class: &[u8], member: &[u8]) -> SymbolIdentifier {
        let class = self.class_key(class);
        let exact = member.starts_with(b"$") || self.classes.get(&class).is_some_and(|reached| reached.exact_names);

        (class, if exact { word(member) } else { ascii_lowercase_word(member) })
    }

    /// Whether `class`'s property `property` holds a value, so its structure has a field for it.
    pub(crate) fn is_stored_field(&self, class: &[u8], property: &[u8]) -> bool {
        self.stored_fields.contains(&(self.class_key(class), word(property)))
    }

    /// Whether the declared type of `class`'s property `property` takes `null`.
    pub(crate) fn is_nullable_field(&self, class: &[u8], property: &[u8]) -> bool {
        self.nullable_fields.contains(&(self.class_key(class), word(property)))
    }
}

/// The symbol with its class and a method as the codebase keys them, spec section 24: by their exact names on a PHP#
/// class, and lowercase on a plain PHP one. A property keeps its name.
fn keyed(codebase: &CodebaseMetadata, (class, member): SymbolIdentifier) -> SymbolIdentifier {
    let class = codebase.class_key(class.as_bytes());
    let member =
        if member.as_bytes().starts_with(b"$") { member } else { codebase.method_key(class, member.as_bytes()) };

    (class, member)
}
