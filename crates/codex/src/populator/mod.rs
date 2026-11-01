use std::sync::Arc;

use foldhash::HashSet;
use indexmap::IndexMap;

use mago_word::Word;
use mago_word::WordMap;
use mago_word::WordSet;
use mago_word::ascii_lowercase_word;

use crate::metadata::CodebaseMetadata;
use crate::metadata::class_like::ClassLikeMetadata;
use crate::metadata::constant::ConstantMetadata;
use crate::metadata::flags::MetadataFlags;
use crate::misc::GenericParent;
use crate::reference::ReferenceSource;
use crate::reference::SymbolReferences;
use crate::symbol::SymbolIdentifier;
use crate::symbol::Symbols;
use crate::ttype::TType;
use crate::ttype::atomic::TAtomic;
use crate::ttype::atomic::array::TArray;
use crate::ttype::atomic::callable::TCallable;
use crate::ttype::atomic::object::TObject;
use crate::ttype::atomic::reference::TReference;
use crate::ttype::atomic::scalar::TScalar;
use crate::ttype::atomic::scalar::class_like_string::TClassLikeString;
use crate::ttype::template::GenericTemplate;
use crate::ttype::template::refuse_bounds;
use crate::ttype::union::TUnion;
use crate::ttype::union::populate_union_type;

mod docblock;
mod hierarchy;
mod merge;
mod methods;
mod properties;
mod signatures;
mod sorter;
mod templates;

/// Populates the codebase metadata, resolving types and inheritance.
///
/// This function processes class-likes, function-likes, and constants to:
///
/// - Resolve type signatures (populating `TUnion` and `TAtomic` types).
/// - Calculate inheritance hierarchies (parent classes, interfaces, traits).
/// - Determine method and property origins (declaring vs. appearing).
/// - Build descendant maps for efficient lookup.
#[allow(clippy::implicit_hasher)]
pub fn populate_codebase(
    codebase: &mut CodebaseMetadata,
    symbol_references: &mut SymbolReferences,
    safe_symbols: WordSet,
    safe_symbol_members: HashSet<SymbolIdentifier>,
) {
    populate_codebase_inner(codebase, symbol_references, safe_symbols, safe_symbol_members, None)
}

/// Populates the codebase with an optional set of dirty (invalidated) symbols for targeted iteration.
///
/// When `dirty_symbols` is provided, function-like, class-type, and constant repopulation
/// uses targeted `get_mut()` lookups instead of scanning the entire HashMap — O(dirty) instead of O(all).
/// This is critical for incremental mode where only a few symbols change per cycle.
#[allow(clippy::implicit_hasher)]
pub fn populate_codebase_targeted(
    codebase: &mut CodebaseMetadata,
    symbol_references: &mut SymbolReferences,
    safe_symbols: WordSet,
    safe_symbol_members: HashSet<SymbolIdentifier>,
    dirty_symbols: HashSet<SymbolIdentifier>,
) {
    populate_codebase_inner(codebase, symbol_references, safe_symbols, safe_symbol_members, Some(dirty_symbols))
}

fn populate_codebase_inner(
    codebase: &mut CodebaseMetadata,
    symbol_references: &mut SymbolReferences,
    mut safe_symbols: WordSet,
    mut safe_symbol_members: HashSet<SymbolIdentifier>,
    mut dirty_symbols: Option<HashSet<SymbolIdentifier>>,
) {
    if codebase.populate_class_like_aliases() {
        safe_symbols.clear();
        safe_symbol_members.clear();
        dirty_symbols = None;
    }

    let mut class_likes_to_repopulate = WordSet::default();
    if let Some(dirty) = &dirty_symbols {
        let mut dirty_class_names = WordSet::default();
        for (name, _) in dirty {
            dirty_class_names.insert(*name);
        }

        for class_name in &dirty_class_names {
            if let Some(metadata) = codebase.class_likes.get(class_name)
                && (!metadata.flags.is_populated()
                    || (metadata.flags.is_user_defined() && !safe_symbols.contains(class_name)))
            {
                class_likes_to_repopulate.insert(*class_name);
            }
        }

        // Also repopulate user-defined classes that were invalidated by the
        // cascade (not in safe_symbols) but are not directly in dirty_symbols.
        // This handles e.g. a child class whose parent's method was removed:
        // the child is invalidated (not safe) but its file didn't change,
        // so it's not in dirty_symbols.
        for (class_name, metadata) in &codebase.class_likes {
            if metadata.flags.is_user_defined()
                && !safe_symbols.contains(class_name)
                && !class_likes_to_repopulate.contains(class_name)
            {
                class_likes_to_repopulate.insert(*class_name);
            }
        }
    } else {
        for (name, metadata) in &codebase.class_likes {
            if !metadata.flags.is_populated() || (metadata.flags.is_user_defined() && !safe_symbols.contains(name)) {
                class_likes_to_repopulate.insert(*name);
            }
        }
    }

    for class_like_name in &class_likes_to_repopulate {
        if let Some(classlike_info) = codebase.class_likes.get_mut(class_like_name) {
            classlike_info.flags &= !MetadataFlags::POPULATED;
            classlike_info.declaring_property_ids.clear();
            classlike_info.appearing_property_ids.clear();
            classlike_info.declaring_method_ids.clear();
            classlike_info.appearing_method_ids.clear();
            classlike_info.overridden_method_ids.clear();
            classlike_info.overridden_property_ids.clear();
            classlike_info.invalid_dependencies.clear();
        }
    }

    populate_sealed_interfaces(codebase);
    codebase.populate_folder_namespaces();

    let sorted_classes = sorter::sort_class_likes(codebase, &class_likes_to_repopulate);
    for class_name in sorted_classes {
        hierarchy::populate_class_like_metadata_iterative(class_name, codebase, symbol_references);
    }

    let incremental = !safe_symbols.is_empty() || !safe_symbol_members.is_empty();

    if let Some(dirty) = &dirty_symbols {
        for dirty_key in dirty {
            if let Some(function_like_metadata) = codebase.function_likes.get_mut(dirty_key) {
                let force_repopulation = function_like_metadata.flags.is_user_defined();
                if function_like_metadata.flags.is_populated() && !force_repopulation {
                    continue;
                }

                let reference_source = if dirty_key.1.is_empty() || function_like_metadata.get_kind().is_closure() {
                    ReferenceSource::Symbol(true, dirty_key.0)
                } else {
                    ReferenceSource::ClassLikeMember(true, dirty_key.0, dirty_key.1)
                };

                signatures::populate_function_like_metadata(
                    function_like_metadata,
                    &codebase.symbols,
                    &reference_source,
                    symbol_references,
                    force_repopulation,
                );
            }
        }

        // Also repopulate non-dirty function_likes that are not safe but need repopulation.
        // This handles e.g. a child method when the parent class was re-added:
        // the child method isn't dirty (file didn't change) but it's not safe
        // (parent class changed), so it needs type signature repopulation.
        for (name, function_like_metadata) in &mut codebase.function_likes {
            if dirty.contains(name) {
                continue;
            }

            let is_closure_or_arrow =
                function_like_metadata.get_kind().is_closure() || function_like_metadata.get_kind().is_arrow_function();

            let is_safe = if is_closure_or_arrow {
                true
            } else if name.1.is_empty() {
                safe_symbols.contains(&name.0)
            } else {
                safe_symbol_members.contains(name) || safe_symbols.contains(&name.0)
            };

            let force_repopulation = function_like_metadata.flags.is_user_defined() && !is_safe;
            if function_like_metadata.flags.is_populated() && !force_repopulation {
                continue;
            }

            let reference_source = if name.1.is_empty() || function_like_metadata.get_kind().is_closure() {
                ReferenceSource::Symbol(true, name.0)
            } else {
                ReferenceSource::ClassLikeMember(true, name.0, name.1)
            };

            signatures::populate_function_like_metadata(
                function_like_metadata,
                &codebase.symbols,
                &reference_source,
                symbol_references,
                force_repopulation,
            );
        }
    } else {
        for (name, function_like_metadata) in &mut codebase.function_likes {
            let is_closure_or_arrow =
                function_like_metadata.get_kind().is_closure() || function_like_metadata.get_kind().is_arrow_function();

            let is_safe = if is_closure_or_arrow {
                true
            } else if name.1.is_empty() {
                safe_symbols.contains(&name.0)
            } else {
                safe_symbol_members.contains(name) || safe_symbols.contains(&name.0)
            };

            let force_repopulation = function_like_metadata.flags.is_user_defined() && !is_safe;
            if incremental && function_like_metadata.flags.is_populated() && !force_repopulation {
                continue;
            }

            let reference_source = if name.1.is_empty() || function_like_metadata.get_kind().is_closure() {
                ReferenceSource::Symbol(true, name.0)
            } else {
                ReferenceSource::ClassLikeMember(true, name.0, name.1)
            };

            signatures::populate_function_like_metadata(
                function_like_metadata,
                &codebase.symbols,
                &reference_source,
                symbol_references,
                force_repopulation,
            );
        }
    }

    if let Some(_dirty) = &dirty_symbols {
        for class_name in &class_likes_to_repopulate {
            if let Some(metadata) = codebase.class_likes.get_mut(class_name) {
                hierarchy::populate_class_like_types(
                    *class_name,
                    metadata,
                    &codebase.symbols,
                    symbol_references,
                    true, // force: these are in the repopulate set
                );
            }
        }
    } else {
        for (name, metadata) in &mut codebase.class_likes {
            let force_repopulation =
                if incremental { !safe_symbols.contains(name) } else { metadata.flags.is_user_defined() };

            if incremental && metadata.flags.is_populated() && !force_repopulation {
                continue;
            }

            hierarchy::populate_class_like_types(
                *name,
                metadata,
                &codebase.symbols,
                symbol_references,
                force_repopulation,
            );
        }
    }

    if let Some(dirty) = &dirty_symbols {
        let mut dirty_const_names: WordSet = WordSet::default();
        for (name, member) in dirty {
            if member.is_empty() {
                dirty_const_names.insert(*name);
            }
        }

        for const_name in dirty_const_names {
            if let Some(constant) = codebase.constants.get_mut(&const_name) {
                let force_repopulation = constant.flags.is_user_defined();
                if constant.flags.is_populated() && !force_repopulation {
                    continue;
                }

                populate_constant(const_name, constant, &codebase.symbols, symbol_references, force_repopulation);
            }
        }
    } else {
        for (name, constant) in &mut codebase.constants {
            let force_repopulation = constant.flags.is_user_defined() && !safe_symbols.contains(name);
            if incremental && constant.flags.is_populated() && !force_repopulation {
                continue;
            }

            populate_constant(*name, constant, &codebase.symbols, symbol_references, force_repopulation);
        }
    }

    refuse_missing_bounds(codebase);

    // A module's services are no symbol of their own, and a change to any module re-analyzes the whole project, so
    // every binding's service is populated each time.
    for module in &mut codebase.modules {
        for binding in &mut module.bindings {
            populate_union_type(&mut binding.service.type_union, &codebase.symbols, None, symbol_references, true);
        }
    }

    if !incremental || !class_likes_to_repopulate.is_empty() {
        let mut direct_classlike_descendants: WordMap<WordSet> = WordMap::default();
        let mut all_classlike_descendants: WordMap<WordSet> = WordMap::default();

        for (class_like_name, class_like_metadata) in &codebase.class_likes {
            for parent_interface in &class_like_metadata.all_parent_interfaces {
                all_classlike_descendants.entry(*parent_interface).or_default().insert(*class_like_name);
            }

            for parent_interface in &class_like_metadata.direct_parent_interfaces {
                direct_classlike_descendants.entry(*parent_interface).or_default().insert(*class_like_name);
            }

            for parent_class in &class_like_metadata.all_parent_classes {
                all_classlike_descendants.entry(*parent_class).or_default().insert(*class_like_name);
            }

            for used_trait in &class_like_metadata.used_traits {
                all_classlike_descendants.entry(*used_trait).or_default().insert(*class_like_name);
            }

            if let Some(parent_class) = &class_like_metadata.direct_parent_class {
                direct_classlike_descendants.entry(*parent_class).or_default().insert(*class_like_name);
            }
        }

        for (parent_name, children) in &direct_classlike_descendants {
            if let Some(parent_metadata) = codebase.class_likes.get_mut(parent_name) {
                parent_metadata.child_class_likes = Some(children.clone());
            }
        }

        codebase.all_class_like_descendants = all_classlike_descendants;
        codebase.direct_classlike_descendants = direct_classlike_descendants;
    }

    if !incremental || !class_likes_to_repopulate.is_empty() {
        let dirty_classes = if dirty_symbols.is_some() { Some(&class_likes_to_repopulate) } else { None };

        docblock::inherit_method_docblocks(codebase, &safe_symbols, dirty_classes);
        docblock::inherit_property_docblocks(codebase, &safe_symbols, dirty_classes);
    }

    codebase.safe_symbols = safe_symbols;
    codebase.safe_symbol_members = safe_symbol_members;
}

/// Sets the permitted inheritors of each PHP# sealed interface, spec section 20.1: the classes and enums of its
/// namespace that implement it, directly or through an interface that extends it. A class elsewhere is then not
/// permitted to implement it, and a `match` over it covers the set. A new implementer changes the set without changing
/// the interface, so this runs on every population, before the hierarchy gives each class its interfaces.
fn populate_sealed_interfaces(codebase: &mut CodebaseMetadata) {
    fn namespace(name: &Word) -> &[u8] {
        let name = name.as_bytes();

        &name[..name.iter().rposition(|&byte| byte == b'\\').unwrap_or(0)]
    }

    /// Whether `class_like` declares `interface` among its interfaces, or one of them extends it.
    fn implements(codebase: &CodebaseMetadata, class_like: &ClassLikeMetadata, interface: Word) -> bool {
        let mut pending: Vec<Word> = class_like.direct_parent_interfaces.iter().copied().collect();
        let mut seen = WordSet::default();
        while let Some(parent) = pending.pop() {
            if parent == interface {
                return true;
            }
            if seen.insert(parent)
                && let Some(parent) = codebase.class_likes.get(&parent)
            {
                pending.extend(parent.direct_parent_interfaces.iter().copied());
            }
        }

        false
    }

    let sealed: Vec<Word> =
        codebase.class_likes.iter().filter(|(_, metadata)| metadata.flags.is_sealed()).map(|(name, _)| *name).collect();

    for interface in sealed {
        let implementers: WordSet = codebase
            .class_likes
            .iter()
            .filter(|(name, metadata)| {
                (metadata.kind.is_class() || metadata.kind.is_enum())
                    && namespace(name) == namespace(&interface)
                    && implements(codebase, metadata, interface)
            })
            .map(|(name, _)| *name)
            .collect();

        // Each member holds the set too, as a member of an enum's does, so a `match` knows it for one of the set. An
        // interface that extends the sealed one holds none: its own implementers are only a part of the set.
        for implementer in &implementers {
            if let Some(metadata) = codebase.class_likes.get_mut(implementer) {
                metadata.permitted_inheritors.get_or_insert_default().extend(implementers.iter().copied());
            }
        }
        if let Some(metadata) = codebase.class_likes.get_mut(&interface) {
            metadata.permitted_inheritors = Some(implementers);
        }
    }
}

/// Spec section 11: a PHP# bound that names a type no file declares, as `T : Missing`, bounds nothing, as a bound the
/// checker refuses does, so its one error is the missing type's. The step refuses each such template's bound, and the
/// bound of each type parameter a PHP# type names whose template it refused. Declaring a type a refused bound names
/// runs a full analysis, which scans every bound again.
fn refuse_missing_bounds(codebase: &mut CodebaseMetadata) {
    let CodebaseMetadata { symbols, class_likes, function_likes, .. } = codebase;
    let declares = |name: Word| symbols.get_kind(ascii_lowercase_word(name.as_bytes())).is_some();

    let sharp_classes = class_likes.values_mut().filter(|class| class.flags.is_sharp());
    let sharp_functions = function_likes.values_mut().filter(|function| function.flags.is_sharp());
    // The type parameters whose bound names a missing type: the checker's refusals are refused where they are scanned.
    let mut refused: HashSet<(GenericParent, Word)> = HashSet::default();
    for template_types in sharp_classes
        .map(|class| &mut class.template_types)
        .chain(sharp_functions.map(|function| &mut function.template_types))
    {
        refuse_bounds(template_types.iter_mut(), declares);
        refused.extend(
            template_types
                .iter()
                .filter(|(_, template)| {
                    template.written_bound.is_some() && !template.bound_refused && is_refused(&template.constraint)
                })
                .map(|(name, template)| (template.defining_entity, *name)),
        );
    }
    if refused.is_empty() {
        return;
    }

    let read = |union: &mut TUnion| refuse_type_parameters(union, &refused);
    for class in class_likes.values_mut().filter(|class| class.flags.is_sharp()) {
        for template in class.template_types.values_mut() {
            read_template(template, &read);
        }
        for property in class.properties.values_mut() {
            for type_metadata in property.type_declaration_metadata.iter_mut().chain(property.type_metadata.iter_mut())
            {
                read(&mut type_metadata.type_union);
            }
        }
        // The type arguments the header writes, as `Shelved<TItem>`, name the class's type parameters too.
        let extended_parameters = class.template_extended_parameters.values_mut().flat_map(IndexMap::values_mut);
        let extended_paths =
            class.template_extended_parameter_paths.values_mut().flatten().flat_map(IndexMap::values_mut);
        let extended_offsets = class.template_extended_offsets.values_mut().flatten();
        for parameter in extended_parameters.chain(extended_paths).chain(extended_offsets) {
            read(parameter);
        }
    }
    for function in function_likes.values_mut().filter(|function| function.flags.is_sharp()) {
        for template in function.template_types.values_mut() {
            read_template(template, &read);
        }
        let context_templates = function
            .type_resolution_context
            .iter_mut()
            .flat_map(|context| context.get_template_definitions_mut().iter_mut())
            .flat_map(|(name, templates)| templates.iter_mut().map(move |template| (*name, template)));
        for (name, template) in context_templates {
            if refused.contains(&(template.defining_entity, name)) {
                template.constraint = TUnion::from_atomic(TAtomic::Error);
            }
            read_template(template, &read);
        }
        let return_types =
            function.return_type_declaration_metadata.iter_mut().chain(function.return_type_metadata.iter_mut());
        let parameter_types = function.parameters.iter_mut().flat_map(|parameter| {
            parameter
                .type_declaration_metadata
                .iter_mut()
                .chain(parameter.type_metadata.iter_mut())
                .chain(parameter.out_type.iter_mut())
                .chain(parameter.default_type.iter_mut())
        });
        for type_metadata in return_types.chain(parameter_types).chain(function.thrown_types.iter_mut()) {
            read(&mut type_metadata.type_union);
        }
    }
}

/// Refuses the bound of each refused type parameter `template`'s bound and written bound name.
fn read_template(template: &mut GenericTemplate, read: &impl Fn(&mut TUnion)) {
    read(&mut template.constraint);
    if let Some(bound) = template.written_bound.as_mut() {
        read(&mut bound.type_union);
    }
}

/// Refuses the bound of each type parameter `union` names, at any depth, that `refused` holds, keyed by the type
/// parameter's declaration and name.
fn refuse_type_parameters(union: &mut TUnion, refused: &HashSet<(GenericParent, Word)>) {
    if !union.has_template_types() {
        return;
    }

    let read = |union: &mut TUnion| refuse_type_parameters(union, refused);
    for atomic in union.types.to_mut() {
        refuse_atomic_type_parameters(atomic, &read, refused);
    }
}

/// Whether `bound` is the refused type, which a refused bound is.
fn is_refused(bound: &TUnion) -> bool {
    bound.types.iter().all(|atomic| matches!(atomic, TAtomic::Error))
}

/// [`refuse_type_parameters`] for one atomic type.
fn refuse_atomic_type_parameters(
    atomic: &mut TAtomic,
    read: &impl Fn(&mut TUnion),
    refused: &HashSet<(GenericParent, Word)>,
) {
    let read_atomic = |atomic: &mut TAtomic| refuse_atomic_type_parameters(atomic, read, refused);
    match atomic {
        TAtomic::GenericParameter(parameter) => {
            if refused.contains(&(parameter.defining_entity, parameter.parameter_name)) {
                if !is_refused(&parameter.constraint) {
                    parameter.constraint = Arc::new(TUnion::from_atomic(TAtomic::Error));
                }
            } else {
                read(Arc::make_mut(&mut parameter.constraint));
            }
            for intersection_type in parameter.intersection_types.iter_mut().flatten() {
                read_atomic(intersection_type);
            }
        }
        TAtomic::Array(TArray::List(list)) => {
            read(Arc::make_mut(&mut list.element_type));
            for (_, element) in list.known_elements.iter_mut().flat_map(|elements| elements.values_mut()) {
                read(element);
            }
        }
        TAtomic::Array(TArray::Keyed(keyed)) => {
            for (_, item) in keyed.known_items.iter_mut().flat_map(|items| items.values_mut()) {
                read(item);
            }
            if let Some((key, value)) = keyed.parameters.as_mut() {
                read(Arc::make_mut(key));
                read(Arc::make_mut(value));
            }
        }
        TAtomic::Array(TArray::Set(element)) => read(Arc::make_mut(element)),
        TAtomic::Callable(TCallable::Signature(signature)) => {
            if let Some(return_type) = signature.get_return_type_mut() {
                read(return_type);
            }
            for parameter in signature.get_parameters_mut() {
                if let Some(parameter_type) = parameter.get_type_signature_mut() {
                    read(parameter_type);
                }
            }
        }
        TAtomic::Object(TObject::Named(named)) => {
            for parameter in named.get_type_parameters_mut().into_iter().flatten() {
                read(parameter);
            }
            for intersection_type in named.get_intersection_types_mut().into_iter().flatten() {
                read_atomic(intersection_type);
            }
        }
        TAtomic::Iterable(iterable) => {
            read(iterable.get_key_type_mut());
            read(iterable.get_value_type_mut());
        }
        TAtomic::Reference(TReference::Symbol { parameters, intersection_types, .. }) => {
            for parameter in parameters.iter_mut().flatten() {
                read(parameter);
            }
            for intersection_type in intersection_types.iter_mut().flatten() {
                read_atomic(intersection_type);
            }
        }
        TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::OfType { constraint, .. })) => {
            read(Arc::make_mut(constraint));
        }
        TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::Generic { constraint, .. })) => {
            read_atomic(Arc::make_mut(constraint));
        }
        _ => {}
    }
}

/// Populates a single constant's type metadata.
fn populate_constant(
    name: Word,
    constant: &mut ConstantMetadata,
    symbols: &Symbols,
    symbol_references: &mut SymbolReferences,
    force_repopulation: bool,
) {
    for attribute_metadata in &constant.attributes {
        symbol_references.add_symbol_reference_to_symbol(
            name,
            ascii_lowercase_word(attribute_metadata.name.as_bytes()),
            true,
        );
    }

    if let Some(type_metadata) = &mut constant.type_metadata {
        populate_union_type(
            &mut type_metadata.type_union,
            symbols,
            Some(&ReferenceSource::Symbol(true, name)),
            symbol_references,
            force_repopulation,
        );
    }

    if let Some(inferred_type) = &mut constant.inferred_type {
        populate_union_type(
            inferred_type,
            symbols,
            Some(&ReferenceSource::Symbol(true, name)),
            symbol_references,
            force_repopulation,
        );
    }

    constant.flags |= MetadataFlags::POPULATED;
}
