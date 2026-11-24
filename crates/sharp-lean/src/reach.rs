//! Every declaration a law reaches, across files.

use foldhash::HashMap;
use foldhash::HashSet;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::flags::MetadataFlags;
use mago_codex::metadata::machine::MachineMetadata;
use mago_codex::reference::CascadeEdge;
use mago_codex::reference::SymbolReferences;
use mago_codex::symbol::SymbolIdentifier;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::union::TUnion;
use mago_database::file::FileId;
use mago_span::Span;
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
    /// Where its declaration names it.
    pub(crate) span: Span,
    /// Whether a `.sharp` file declares it, and not plain PHP.
    pub(crate) sharp: bool,
    /// Whether it finds its methods only by their exact names, spec section 24, as a PHP# class and every class below
    /// one do.
    exact_names: bool,
    /// The full name of the class it extends.
    pub(crate) parent: Option<Word>,
    /// The full name of the first plain PHP class above it, when that class declares or inherits a constructor, which
    /// its `new` runs.
    pub(crate) plain_constructor: Option<Word>,
    /// Whether `new` builds its values: a class that is not abstract, and no interface, trait or enum.
    pub(crate) instantiable: bool,
}

/// What the analysis knows about a method a law reaches, or a computed property, from outside its class's file.
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
    /// Each reached method, and each computed property, spec section 6, which runs its `get` body as a method of no
    /// arguments, by its class's key and its name as the class keys a method.
    methods: HashMap<SymbolIdentifier, ReachedMethod>,
    /// Each property that holds a value, by its class's key and its exact name without `$`, as PHP finds a property: it
    /// is not static, has no accessor body, and has storage of its own.
    stored_fields: HashSet<(Word, Word)>,
    /// Each property with no storage of its own whose value each implementation gives, by its class's key and its exact
    /// name without `$`: one with no accessor body, as an interface's and an abstract one are, and a computed one that a
    /// subclass or an implementer can override.
    abstract_properties: HashSet<(Word, Word)>,
    /// Each property whose declared type takes `null`, by its class's key and its exact name without `$`.
    nullable_fields: HashSet<(Word, Word)>,
    /// The files each file's laws reach, its own included.
    files: HashMap<FileId, Vec<FileId>>,
    /// The implementers of each reached sealed interface, by the interface's key, each by its full name, in name order.
    implementers: HashMap<Word, Vec<Word>>,
    /// The cases of each reached enum, spec section 20, by the enum's key, each by its name, in the order the enum
    /// declares them.
    cases: HashMap<Word, Vec<Word>>,
    /// The interfaces each reached class names in its own header, by the class's key, each by its full name, in name
    /// order.
    interfaces: HashMap<Word, Vec<Word>>,
    /// Each reached class-like whose every value keeps a precondition, by its key: a class whose constructor checks
    /// its arguments, spec section 28, and a class-like that holds a value of one in a field, as [`valid_classes`]
    /// finds them.
    valid: HashSet<Word>,
    /// The PHP# classes that prove the laws of each interface that states laws, spec sections 22 and 28, by the
    /// interface's key: each one that implements it and is not abstract, by its full name, in name order.
    law_implementers: HashMap<Word, Vec<Word>>,
    /// The laws of each interface that states laws, by the interface's key: each law's name as written and the number
    /// of its parameters, in the order the interface states them.
    interface_laws: HashMap<Word, Vec<(Word, usize)>>,
    /// The invariants of each case of a machine, spec section 20.2, by the key of the case's class: each law of the
    /// machine whose one parameter is the case, by its name as written, in the order the machine states them.
    invariants: HashMap<Word, Vec<Word>>,
}

impl Reach {
    /// Walks from each law of `codebase` along the signature and body edges of `references`. A law's body records its
    /// references under the law's own symbol, its class and its name as the class keys a method. A law reaches only
    /// what the members it reaches run, so the walk skips each [`CascadeEdge::Attribute`]: an attribute's constructor
    /// never runs as part of them. A sealed interface reaches each of its implementers, the closed set a law tells its
    /// values apart by, spec section 20.1. A struct's generated `==` reaches the `==` of each class its fields hold. A
    /// class reaches the PHP# class it extends, whose fields it holds, and a member a class inherits reaches the
    /// member of the PHP# class that declares it. The laws of a machine reach each of its entry points and moves.
    #[must_use]
    pub fn new(codebase: &CodebaseMetadata, references: &SymbolReferences) -> Reach {
        let mut edges: HashMap<SymbolIdentifier, Vec<SymbolIdentifier>> = HashMap::default();
        for edge in references.cascade_edges(codebase) {
            if let CascadeEdge::Signature { from, to } | CascadeEdge::Body { from, to } = edge {
                edges.entry(keyed(codebase, from)).or_default().push(keyed(codebase, to));
            }
        }

        let mut reach = Reach::default();
        for interface in
            codebase.class_likes.values().filter(|class| class.kind.is_interface() && !class.laws.is_empty())
        {
            let mut laws: Vec<(u32, Word, usize)> = interface
                .laws
                .values()
                .map(|law| (law.span.start.offset, law.original_name, law.parameters.len()))
                .collect();
            laws.sort_unstable_by_key(|(offset, ..)| *offset);
            reach.interface_laws.insert(
                codebase.class_key(interface.name),
                laws.into_iter().map(|(_, name, parameters)| (name, parameters)).collect(),
            );
        }
        for class in codebase
            .class_likes
            .values()
            .filter(|class| class.kind.is_class() && class.flags.is_sharp() && !class.flags.is_abstract())
        {
            for interface in &class.all_parent_interfaces {
                let key = codebase.class_key(*interface);
                if reach.interface_laws.contains_key(&key) {
                    reach.law_implementers.entry(key).or_default().push(class.original_name);
                }
            }
        }
        for implementers in reach.law_implementers.values_mut() {
            implementers.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        }
        for machine in codebase.class_likes.values().filter(|class| class.machine.is_some()) {
            let mut invariants: Vec<(u32, Word, Word)> = machine
                .laws
                .values()
                .filter_map(|law| {
                    let [parameter] = law.parameters.as_slice() else {
                        return None;
                    };
                    let case = parameter.type_metadata.as_ref()?.type_union.get_single_named_object()?;
                    let class = codebase.get_class_like(case.name)?;
                    let machine_key = codebase.class_key(machine.name);
                    (class.flags.is_enum_case_class() && class.direct_parent_class == Some(machine_key))
                        .then(|| (law.span.start.offset, codebase.class_key(case.name), law.original_name))
                })
                .collect();
            invariants.sort_unstable_by_key(|(offset, ..)| *offset);
            for (_, case, law) in invariants {
                reach.invariants.entry(case).or_default().push(law);
            }
        }

        for class in codebase.class_likes.values().filter(|class| !class.laws.is_empty()) {
            let law_file = class.span.file_id;
            let mut reached: HashSet<SymbolIdentifier> = HashSet::default();
            let mut pending: Vec<SymbolIdentifier> =
                class.laws.keys().map(|law| keyed(codebase, (class.name, *law))).collect();
            pending.extend(class.machine.iter().flat_map(|machine| machine_steps(codebase, class, machine)));
            // Each PHP# implementer of an interface proves the interface's laws, so they reach what it gives for each
            // member of the interface they reach.
            let implementers = reach.law_implementers.get(&codebase.class_key(class.name)).cloned().unwrap_or_default();
            while let Some(next) = pending.pop() {
                if !reached.insert(next) {
                    continue;
                }

                pending.extend(edges.get(&next).into_iter().flatten().copied());
                if !next.1.is_empty() {
                    pending.push((next.0, empty_word()));
                    pending.extend(field_equalities(codebase, next));
                    pending.extend(sharp_declaration(codebase, next));
                    if next.0 == codebase.class_key(class.name) {
                        pending.extend(implementers.iter().map(|implementer| keyed(codebase, (*implementer, next.1))));
                    }
                } else if let Some(metadata) = codebase.get_class_like(next.0) {
                    // A law over an interface's values takes the interface's laws as facts, spec section 28.
                    if metadata.kind.is_interface() {
                        pending.extend(metadata.laws.keys().map(|law| keyed(codebase, (next.0, *law))));
                    }
                    pending.extend(metadata.properties.keys().map(|property| (next.0, *property)));
                    pending.extend(
                        metadata
                            .direct_parent_class
                            .filter(|parent| {
                                codebase.get_class_like(*parent).is_some_and(|parent| parent.flags.is_sharp())
                            })
                            .map(|parent| (parent, empty_word())),
                    );
                    if metadata.flags.is_sealed()
                        && let Some(inheritors) = &metadata.permitted_inheritors
                    {
                        pending
                            .extend(inheritors.iter().map(|inheritor| (codebase.class_key(*inheritor), empty_word())));
                        let mut implementers: Vec<Word> = inheritors
                            .iter()
                            .filter_map(|inheritor| codebase.get_class_like(*inheritor))
                            .map(|implementer| implementer.original_name)
                            .collect();
                        implementers.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
                        reach.implementers.insert(next.0, implementers);
                    }
                    // An enum whose cases carry no data lists them itself, and the cases of one whose cases carry
                    // data are the classes it permits, spec section 20.
                    let mut cases: Vec<(u32, Word)> =
                        metadata.enum_cases.values().map(|case| (case.span.start.offset, case.name)).collect();
                    cases.extend(
                        metadata
                            .permitted_inheritors
                            .iter()
                            .flatten()
                            .filter_map(|inheritor| codebase.get_class_like(*inheritor))
                            .filter(|inheritor| inheritor.flags.is_enum_case_class())
                            .filter_map(|case| {
                                let name = case.original_name.as_bytes().rsplit(|&byte| byte == b'\\').next()?;
                                Some((case.span.start.offset, word(name)))
                            }),
                    );
                    if !cases.is_empty() {
                        cases.sort_unstable_by_key(|(offset, _)| *offset);
                        reach.cases.insert(next.0, cases.into_iter().map(|(_, name)| name).collect());
                    }
                }
            }

            let mut files = vec![law_file];
            for &(class, member) in &reached {
                let Some(metadata) = codebase.get_class_like(class) else {
                    continue;
                };
                let file = metadata.span.file_id;
                files.push(file);
                reach.classes.entry(class).or_insert(ReachedClass {
                    name: metadata.original_name,
                    file,
                    span: metadata.name_span.unwrap_or(metadata.span),
                    sharp: metadata.flags.is_sharp(),
                    exact_names: metadata.exact_names,
                    parent: metadata
                        .direct_parent_class
                        .and_then(|parent| codebase.get_class_like(parent))
                        .map(|parent| parent.original_name),
                    plain_constructor: plain_constructor(codebase, metadata),
                    instantiable: !metadata.kind.is_interface()
                        && !metadata.kind.is_trait()
                        && !metadata.kind.is_enum()
                        && !metadata.flags.is_abstract(),
                });
                reach.interfaces.entry(class).or_insert_with(|| {
                    let mut interfaces: Vec<Word> = metadata
                        .direct_parent_interfaces
                        .iter()
                        .filter_map(|interface| codebase.get_class_like(*interface))
                        .map(|interface| interface.original_name)
                        .collect();
                    interfaces.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
                    interfaces
                });
                if member.is_empty() {
                    reach.record_member(codebase, class, metadata.method_key(word("__construct")));
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
        reach.valid = valid_classes(codebase, &reach);

        reach
    }

    /// The files the laws of `file_id` reach, `file_id` included, or none when it states no law.
    #[must_use]
    pub fn files(&self, file_id: FileId) -> &[FileId] {
        self.files.get(&file_id).map_or(&[], Vec::as_slice)
    }

    /// Each file that states a law.
    pub fn law_files(&self) -> impl Iterator<Item = FileId> + '_ {
        self.files.keys().copied()
    }

    /// Records what the analysis knows about `class`'s reached member `member`: a method's name, arity and return, or a
    /// property's storage and type. A computed property, with a `get` body and no `set` body, is a method of no
    /// arguments when no subclass or implementer can override it, and else a property each implementation gives.
    fn record_member(&mut self, codebase: &CodebaseMetadata, class: Word, member: Word) {
        if let Some(property) = member.as_bytes().strip_prefix(b"$") {
            let Some(metadata) = codebase.get_declaring_property(class, member) else {
                return;
            };
            let field = (class, word(property));
            let virtual_property = metadata.flags.is_virtual_property();
            let has_body = |hook: &str| metadata.hooks.get(&word(hook)).is_some_and(|hook| !hook.is_abstract);
            let nullable = metadata.type_metadata.as_ref().is_some_and(|r#type| r#type.type_union.is_nullable());
            if !metadata.flags.is_static() {
                if metadata.hooks.values().all(|hook| hook.is_abstract) {
                    if virtual_property {
                        self.abstract_properties.insert(field);
                    } else {
                        self.stored_fields.insert(field);
                    }
                } else if virtual_property && has_body("get") && !has_body("set") {
                    if metadata.flags.is_final() {
                        self.methods.insert(
                            (class, codebase.method_key(class, word(property))),
                            ReachedMethod { name: word(property), parameters: 0, returns_nullable: nullable },
                        );
                    } else {
                        self.abstract_properties.insert(field);
                    }
                }
            }
            if nullable {
                self.nullable_fields.insert(field);
            }
        } else if !member.is_empty()
            && let Some(metadata) = codebase.get_method(class, member)
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

    /// The implementers of the reached sealed interface `interface`, each by its full name, in name order. None for an
    /// interface that is not sealed.
    pub(crate) fn implementers(&self, interface: &[u8]) -> Option<&[Word]> {
        self.implementers.get(&self.class_key(interface)).map(Vec::as_slice)
    }

    /// The PHP# classes that prove the laws of `interface`, spec sections 22 and 28, each by its full name, in name
    /// order: none for an interface that states no law.
    pub(crate) fn law_implementers(&self, interface: &[u8]) -> &[Word] {
        self.law_implementers.get(&self.class_key(interface)).map_or(&[], Vec::as_slice)
    }

    /// The laws `interface` states, each by its name as written and the number of its parameters, in the order it
    /// states them: none for any other class-like.
    pub(crate) fn interface_laws(&self, interface: &[u8]) -> &[(Word, usize)] {
        self.interface_laws.get(&self.class_key(interface)).map_or(&[], Vec::as_slice)
    }

    /// The invariants of the case of a machine whose class is `case`, spec section 20.2, each by its name as written, in
    /// the order the machine states them: none for any other class-like.
    pub(crate) fn invariants(&self, case: &[u8]) -> &[Word] {
        self.invariants.get(&self.class_key(case)).map_or(&[], Vec::as_slice)
    }

    /// The full name of each interface whose laws `class` proves, in name order.
    pub(crate) fn law_interfaces(&self, class: &[u8]) -> Vec<Word> {
        let mut interfaces: Vec<Word> = self
            .law_implementers
            .iter()
            .filter(|(_, implementers)| {
                implementers.iter().any(|implementer| self.same_class(implementer.as_bytes(), class))
            })
            .filter_map(|(interface, _)| self.classes.get(interface).map(|reached| reached.name))
            .collect();
        interfaces.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));

        interfaces
    }

    /// The full name of each reached sealed interface, spec section 20.1.
    pub(crate) fn sealed(&self) -> impl Iterator<Item = Word> + '_ {
        self.implementers.keys().filter_map(|interface| self.classes.get(interface).map(|reached| reached.name))
    }

    /// The cases of the reached enum `r#enum`, each by its name, in the order it declares them. None for any other
    /// class-like.
    pub(crate) fn cases(&self, r#enum: &[u8]) -> &[Word] {
        self.cases.get(&self.class_key(r#enum)).map_or(&[], Vec::as_slice)
    }

    /// The full names of the reached sealed interfaces `class` implements, spec section 20.1, in name order.
    pub(crate) fn sealed_interfaces(&self, class: &[u8]) -> Vec<Word> {
        let mut interfaces: Vec<Word> = self
            .implementers
            .iter()
            .filter(|(_, implementers)| {
                implementers.iter().any(|implementer| self.same_class(implementer.as_bytes(), class))
            })
            .filter_map(|(interface, _)| self.classes.get(interface).map(|reached| reached.name))
            .collect();
        interfaces.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));

        interfaces
    }

    /// The full names of the interfaces the reached class `class` names in its own header, in name order.
    pub(crate) fn interfaces(&self, class: &[u8]) -> &[Word] {
        self.interfaces.get(&self.class_key(class)).map_or(&[], Vec::as_slice)
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

    /// Whether `class`'s property `property` has no storage of its own and each implementation gives its value, as one
    /// with no accessor body and an overridable computed one do, so a law reads it through `class`'s reader.
    pub(crate) fn is_abstract_property(&self, class: &[u8], property: &[u8]) -> bool {
        self.abstract_properties.contains(&(self.class_key(class), word(property)))
    }

    /// Whether every value of the reached class-like `class` keeps a precondition, which `«$valid»` states.
    pub(crate) fn is_valid(&self, class: &[u8]) -> bool {
        self.valid.contains(&self.class_key(class))
    }

    /// Whether the declared type of `class`'s property `property` takes `null`.
    pub(crate) fn is_nullable_field(&self, class: &[u8], property: &[u8]) -> bool {
        self.nullable_fields.contains(&(self.class_key(class), word(property)))
    }
}

/// The `==` of each class a field of a struct holds, or holds the elements of in a collection at any depth, when
/// `equality` is the `==` the struct gets for declaring none, spec section 19, which compares each field with that
/// field's own `==`, and two collections element by element. It has no source, so no reference records the `==` it
/// runs.
fn field_equalities(codebase: &CodebaseMetadata, equality: SymbolIdentifier) -> Vec<SymbolIdentifier> {
    let generated = codebase
        .get_method(equality.0, equality.1)
        .is_some_and(|method| method.flags.is_operator() && method.flags.contains(MetadataFlags::GENERATED));
    let Some(r#struct) = codebase.get_class_like(equality.0).filter(|class| generated && class.flags.is_struct())
    else {
        return Vec::new();
    };

    r#struct
        .properties
        .keys()
        .filter_map(|property| codebase.get_declaring_property(equality.0, *property)?.type_metadata.as_ref())
        .flat_map(|r#type| compared_classes(codebase, &r#type.type_union))
        .map(|class| keyed(codebase, (class, word("op_Equality"))))
        .filter(|(class, method)| codebase.get_method(*class, *method).is_some())
        .collect()
}

/// Each class-like of `reach` whose every value keeps a precondition a law reads, spec section 28, by its key: each one
/// whose constructor checks its arguments, as [`checks_arguments`] finds it, and each one that holds a value of such a
/// class-like in a field. A class-like that holds a value of itself keeps none, since a law reasons about no such type.
fn valid_classes(codebase: &CodebaseMetadata, reach: &Reach) -> HashSet<Word> {
    let held: HashMap<Word, Vec<Word>> =
        reach.classes.keys().map(|class| (*class, held_classes(codebase, *class))).collect();
    let candidates: Vec<Word> = held.keys().copied().filter(|class| !holds_itself(&held, *class)).collect();
    let mut valid: HashSet<Word> =
        candidates.iter().copied().filter(|class| checks_arguments(codebase, reach, *class)).collect();
    loop {
        let added: Vec<Word> = candidates
            .iter()
            .copied()
            .filter(|class| !valid.contains(class) && held[class].iter().any(|value| valid.contains(value)))
            .collect();
        if added.is_empty() {
            return valid;
        }
        valid.extend(added);
    }
}

/// Whether every value of `class` passed its constructor's checks: a PHP# class, with no parent and no sealed
/// interface, whose constructor throws, takes each argument as a promoted property, and leaves each stored property
/// get-only, so no value of the class is built or changed any other way.
fn checks_arguments(codebase: &CodebaseMetadata, reach: &Reach, class: Word) -> bool {
    let Some(metadata) = codebase.get_class_like(class) else {
        return false;
    };
    let Some(constructor) = codebase.get_method(class, metadata.method_key(word("__construct"))) else {
        return false;
    };
    let get_only = metadata.properties.keys().all(|property| {
        codebase.get_declaring_property(class, *property).is_none_or(|property| {
            let stored = !property.flags.is_static()
                && !property.flags.is_virtual_property()
                && property.hooks.values().all(|hook| hook.is_abstract);
            !stored || property.flags.is_readonly()
        })
    });

    metadata.flags.is_sharp()
        && !metadata.kind.is_interface()
        && !metadata.kind.is_trait()
        && !metadata.kind.is_enum()
        && !metadata.flags.is_abstract()
        && metadata.direct_parent_class.is_none()
        && reach.sealed_interfaces(metadata.original_name.as_bytes()).is_empty()
        && constructor.flags.has_throw()
        && constructor.parameters.iter().all(|parameter| parameter.flags.is_promoted_property())
        && get_only
}

/// The key of each class-like a field of `class`, or of a case of `class`, an enum whose cases run as classes, holds a
/// value of, alone, in a collection or where it takes `null`.
fn held_classes(codebase: &CodebaseMetadata, class: Word) -> Vec<Word> {
    let Some(metadata) = codebase.get_class_like(class) else {
        return Vec::new();
    };
    let cases = metadata
        .permitted_inheritors
        .iter()
        .flatten()
        .filter_map(|inheritor| codebase.get_class_like(*inheritor))
        .filter(|inheritor| inheritor.flags.is_enum_case_class());

    std::iter::once(metadata)
        .chain(cases)
        .flat_map(|holder| {
            holder
                .properties
                .keys()
                .filter_map(|property| codebase.get_declaring_property(holder.name, *property)?.type_metadata.as_ref())
        })
        .flat_map(|r#type| compared_classes(codebase, &r#type.type_union))
        .map(|value| codebase.class_key(value))
        .collect()
}

/// Whether a value of `class` holds a value of `class` itself, at any depth, through the class-likes `held` gives the
/// fields of each one hold.
fn holds_itself(held: &HashMap<Word, Vec<Word>>, class: Word) -> bool {
    let mut seen: HashSet<Word> = HashSet::default();
    let mut pending: Vec<Word> = held.get(&class).cloned().unwrap_or_default();
    while let Some(value) = pending.pop() {
        if value == class {
            return true;
        }
        if seen.insert(value) {
            pending.extend(held.get(&value).into_iter().flatten().copied());
        }
    }

    false
}

/// Each class whose `==` compares two values of `union`: a class it names, and a class the elements of a collection it
/// holds name, at any depth, spec section 19.
fn compared_classes(codebase: &CodebaseMetadata, union: &TUnion) -> Vec<Word> {
    union
        .types
        .iter()
        .flat_map(|atomic| match atomic {
            TAtomic::Object(TObject::Named(named)) => vec![named.name],
            TAtomic::Array(array) => compared_classes(codebase, &get_array_parameters(array, codebase).1),
            _ => Vec::new(),
        })
        .collect()
}

/// Each entry point and each move of the machine `machine`, which `class` is, spec section 20.2: the laws of a machine
/// reach them, since its invariants are proved after each one. A move a case's block holds is a method of the case's
/// class, and one of the machine's body a method of the machine.
fn machine_steps(
    codebase: &CodebaseMetadata,
    class: &ClassLikeMetadata,
    machine: &MachineMetadata,
) -> Vec<SymbolIdentifier> {
    let mut steps: Vec<SymbolIdentifier> =
        machine.entry_points.iter().map(|entry| keyed(codebase, (class.name, entry.name))).collect();
    for r#move in &machine.moves {
        for from in &r#move.from {
            let case = word([class.original_name.as_bytes(), b"\\", from.as_bytes()].concat());
            let declaring = if codebase.get_method(case, r#move.name).is_some() { case } else { class.name };
            steps.push(keyed(codebase, (declaring, r#move.name)));
        }
    }

    steps
}

/// `class`'s member `member`, keyed under the PHP# class that declares it, when a parent of `class` declares it.
fn sharp_declaration(codebase: &CodebaseMetadata, (class, member): SymbolIdentifier) -> Option<SymbolIdentifier> {
    let declaring = if member.as_bytes().starts_with(b"$") {
        *codebase.get_class_like(class)?.declaring_property_ids.get(&member)?
    } else {
        codebase.get_declaring_method_identifier(&MethodIdentifier::new(class, member)).get_class_name()
    };
    let sharp = codebase.get_class_like(declaring).is_some_and(|declaring| declaring.flags.is_sharp());

    (sharp && codebase.class_key(declaring) != class).then(|| keyed(codebase, (declaring, member)))
}

/// The full name of the first plain PHP class above `class` when it declares or inherits a constructor, which `new`
/// of `class` runs.
fn plain_constructor(codebase: &CodebaseMetadata, class: &ClassLikeMetadata) -> Option<Word> {
    let mut parent = codebase.get_class_like(class.direct_parent_class?)?;
    while parent.flags.is_sharp() {
        parent = codebase.get_class_like(parent.direct_parent_class?)?;
    }

    codebase.get_declaring_method(parent.name, word("__construct")).map(|_| parent.original_name)
}

/// The symbol with its class and a method as the codebase keys them, spec section 24: by their exact names on a PHP#
/// class, and lowercase on a plain PHP one. A property keeps its name.
fn keyed(codebase: &CodebaseMetadata, (class, member): SymbolIdentifier) -> SymbolIdentifier {
    let class = codebase.class_key(class);
    let member = if member.as_bytes().starts_with(b"$") { member } else { codebase.method_key(class, member) };

    (class, member)
}
