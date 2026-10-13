use foldhash::HashMap;
use foldhash::fast::RandomState;
use indexmap::IndexMap;
use mago_allocator::Arena;

use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::class_like::TemplateTypes;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::property::PropertyMetadata;
use mago_codex::misc::GenericParent;
use mago_codex::ttype::TType;
use mago_codex::ttype::TypeRef;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::generic::TGenericParameter;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::named::TNamedObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::expander;
use mago_codex::ttype::expander::StaticClassType;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::template::GenericTemplate;
use mago_codex::ttype::template::variance::Variance;
use mago_codex::ttype::union::TUnion;
use mago_codex::ttype::wrap_atomic;
use mago_codex::visibility::Visibility;
use mago_names::short_name;
use mago_reporting::Annotation;
use mago_reporting::AnnotationKind;
use mago_reporting::Issue;
use mago_span::Span;
use mago_syntax::cst::Expression;
use mago_word::Word;
use mago_word::WordMap;
use mago_word::word;

use crate::code::IssueCode;
use crate::context::Context;
use crate::utils::expression::is_this;
use crate::utils::names::display_sharp_type;

/// Type alias for template lower bounds - maps parameter names to their bounds per defining entity.
pub type TemplateLowerBounds = HashMap<Word, HashMap<GenericParent, TUnion>>;

/// Resolves and expands template types applicable to a class member (method or property)
/// within a specific call context.
///
/// This function determines the concrete types for template parameters defined either
/// directly on the member (`existing_template_types`) or on its declaring class.
/// It considers the context of the call (`calling_class_meta`) and how the calling
/// class might extend the declaring class (`template_extended_parameters`), merging these
/// with any template arguments already resolved at the class level (`class_template_parameters`).
///
/// It handles template resolution through inheritance chains using `get_generic_parameter_for_offset`.
/// Finally, it expands the resolved types using `expander::expand_union` to resolve
/// types like `self`, `static`, etc., within the final template type definitions.
///
/// # Arguments
/// * `context` - The analysis context, providing codebase metadata.
/// * `declaring_class_meta` - Metadata of the class where the member is originally declared.
/// * `appearing_class_name` - The name of the class through which the member is being accessed (might differ from declaring class due to inheritance). Used for `self::` resolution during final expansion.
/// * `calling_class_meta` - Metadata of the class context from which the call originates (`$this` or `static::class`). Used for `static::` resolution.
/// * `existing_template_types` - Template types defined directly on the function/method itself (e.g., `@template TMethod`). These take precedence.
/// * `class_template_parameters` - Concrete types already resolved for the *class's* template parameters in the current context (e.g., if analyzing `$obj` of type `Vec<int>`, this map would contain `TValue => int`).
///
/// # Returns
///
/// An `IndexMap` where keys are template parameter names (`str`) and values are
/// `HashMap`s mapping the defining entity (`GenericParent` - class or function) to the
/// fully resolved and expanded `TUnion` for that template parameter in this specific context.
pub fn get_template_types_for_class_member<A>(
    context: &Context<'_, '_, A>,
    declaring_class_meta: Option<&ClassLikeMetadata>,
    appearing_class_name: Option<Word>,
    calling_class_meta: Option<&ClassLikeMetadata>,
    existing_template_types: &TemplateTypes,
    class_template_parameters: &IndexMap<Word, Vec<GenericTemplate>, RandomState>,
) -> TemplateLowerBounds
where
    A: Arena,
{
    let codebase = context.codebase;

    // Convert existing_template_types to internal Vec-based format for accumulation
    let mut template_types: IndexMap<Word, Vec<GenericTemplate>, RandomState> =
        existing_template_types.iter().map(|(name, template)| (*name, vec![template.clone()])).collect();

    if let Some(declaring_class_meta) = declaring_class_meta {
        let declaring_class_name = declaring_class_meta.name;

        if let Some(calling_meta) = calling_class_meta
            && calling_meta.name != declaring_class_name
            && !calling_meta.template_extended_parameters.is_empty()
        {
            let calling_template_extended = &calling_meta.template_extended_parameters;

            for (extended_class_name, type_map) in calling_template_extended {
                if extended_class_name == &declaring_class_name {
                    for (template_name, provided_type_arc) in type_map {
                        let resolved_type = if provided_type_arc.has_template_types() {
                            let mut resolved_union = None;
                            for atomic_type in provided_type_arc.types.as_ref() {
                                let resolved_atomic_type_union = if let TAtomic::GenericParameter(TGenericParameter {
                                    defining_entity: GenericParent::ClassLike(defining_entity),
                                    parameter_name,
                                    ..
                                }) = atomic_type
                                {
                                    let mut combined_parameters = class_template_parameters.clone();
                                    combined_parameters.extend(template_types.clone());

                                    get_generic_parameter_for_offset(
                                        *defining_entity,
                                        *parameter_name,
                                        calling_template_extended,
                                        &combined_parameters.into_iter().collect::<WordMap<_>>(),
                                    )
                                } else {
                                    wrap_atomic(atomic_type.clone())
                                };

                                resolved_union = Some(add_optional_union_type(
                                    resolved_atomic_type_union,
                                    resolved_union.as_ref(),
                                    codebase,
                                ));
                            }

                            resolved_union.unwrap_or_else(get_mixed)
                        } else {
                            provided_type_arc.clone()
                        };

                        template_types
                            .entry(*template_name)
                            .or_default()
                            .push(GenericTemplate::new(GenericParent::ClassLike(declaring_class_name), resolved_type));
                    }
                }
            }
        } else if !declaring_class_meta.template_types.is_empty() {
            for (template_name, template) in &declaring_class_meta.template_types {
                let concrete_type = class_template_parameters.get(template_name).and_then(|parameters| {
                    parameters
                        .iter()
                        .find(|t| t.defining_entity == template.defining_entity)
                        .map(|t| t.constraint.clone())
                });

                let resolved_type = concrete_type.unwrap_or_else(|| template.constraint.clone());

                template_types
                    .entry(*template_name)
                    .or_default()
                    .push(GenericTemplate::new(template.defining_entity, resolved_type));
            }
        }
    }

    let mut expanded_template_types: TemplateLowerBounds = HashMap::default();
    for (template_name, type_map_vec) in template_types {
        let final_map_entry: &mut HashMap<GenericParent, TUnion> =
            expanded_template_types.entry(template_name).or_default();

        for GenericTemplate { defining_entity: template_source, constraint: mut template_type, .. } in type_map_vec {
            expander::expand_union(
                codebase,
                &mut template_type,
                &TypeExpansionOptions {
                    self_class: appearing_class_name,
                    static_class_type: if let Some(calling_meta) = calling_class_meta {
                        StaticClassType::Name(calling_meta.name)
                    } else {
                        StaticClassType::None
                    },
                    function_is_final: calling_class_meta.is_some_and(|m| m.flags.is_final()),
                    ..Default::default()
                },
            );

            final_map_entry.insert(template_source, template_type);
        }
    }

    expanded_template_types
}

/// Recursively resolves the concrete type for a specific template parameter within a class hierarchy.
///
/// This function traces template parameter substitutions through class extensions. For example,
/// if `ClassC`<U> extends `ClassB`<U>, and `ClassB`<T> extends `ClassA`<T>, calling this function
/// to find the type for `T` in the context of `ClassC<int>` would first look up `T` in `ClassB`'s
/// context (finding `U`), and then recursively look up `U` in `ClassC`'s context, ultimately
/// resolving to `int`.
///
/// # Returns
///
/// An `TUnion` representing the resolved concrete type for the template parameter,
/// or `any` if it cannot be resolved.
pub fn get_generic_parameter_for_offset(
    class_like_name: Word,
    template_name: Word,
    template_extended_parameters: &WordMap<IndexMap<Word, TUnion, RandomState>>,
    found_generic_parameters: &WordMap<Vec<GenericTemplate>>,
) -> TUnion {
    if let Some(result_map) = found_generic_parameters.get(&template_name)
        && let Some(found_parameter_type) = result_map
            .iter()
            .find(|t| t.defining_entity == GenericParent::ClassLike(class_like_name))
            .map(|t| &t.constraint)
    {
        return found_parameter_type.clone();
    }

    for (extending_class_name, type_map) in template_extended_parameters {
        for (extended_template_name, extended_type_union) in type_map {
            for extended_atomic_type in extended_type_union.types.as_ref() {
                if let TAtomic::GenericParameter(TGenericParameter {
                    parameter_name: current_parameter_name,
                    defining_entity: GenericParent::ClassLike(current_defining_class),
                    ..
                }) = extended_atomic_type
                    && *current_parameter_name == template_name
                    && *current_defining_class == class_like_name
                {
                    return get_generic_parameter_for_offset(
                        *extending_class_name,
                        *extended_template_name,
                        template_extended_parameters,
                        found_generic_parameters,
                    );
                }
            }
        }
    }

    get_mixed()
}

/// A member of a class that another class reaches, and the position it uses one of the class's own type parameters
/// in, spec section 11.1.
pub(crate) struct TemplateUse {
    pub template: Word,
    /// `Covariant` where the member hands the type out, `Contravariant` where it takes it in, and `Invariant` where it
    /// does both.
    pub position: Variance,
    /// The member as a message names it: `` `add` `` or ``the property `current` ``.
    pub member: String,
    pub span: Span,
}

/// Every use of a type parameter of `class` by a member it declares or by its header, one per member and type
/// parameter. A method's parameters take their types in and its return type hands its type out, a property hands its
/// type out and takes it in when another class can write it, and the header hands its type arguments out, as C# and
/// Kotlin check a base type's. Private members and the constructor are exempt, as in Kotlin, and a private member that
/// breaks a marker is reachable only through `this`, which `check_private_method_reach` and
/// `check_private_property_reach` check where it is reached. A header entry stands at its span in `header_spans`, by
/// its parent's lowercase name, or at the class's name without one.
pub(crate) fn find_template_uses<A>(
    context: &Context<'_, '_, A>,
    class: &ClassLikeMetadata,
    header_spans: &WordMap<Span>,
) -> Vec<TemplateUse>
where
    A: Arena,
{
    let codebase = context.codebase;
    let owner = GenericParent::ClassLike(class.name);
    let mut template_uses = Vec::new();

    for (parent_name, arguments) in &class.template_extended_offsets {
        let Some(parent) = codebase.get_class_like(parent_name.as_bytes()) else {
            continue;
        };

        let header = TAtomic::Object(TObject::Named(
            TNamedObject::new(parent.name).with_type_parameters(Some(arguments.clone())),
        ));
        let mut positions = Vec::new();
        find_atomic_template_positions(codebase, &header, &owner, Variance::Covariant, &mut positions);

        let arguments: Vec<String> = arguments
            .iter()
            .map(|argument| match argument.types.as_ref() {
                [TAtomic::GenericParameter(parameter)] => parameter.parameter_name.to_string(),
                _ => display_sharp_type(context, argument),
            })
            .collect();
        let member = format!("the header `{}<{}>`", short_name(parent.original_name), arguments.join(", "));
        let span = header_spans.get(parent_name).copied().unwrap_or(class.name_span.unwrap_or(class.span));
        add_template_uses(&mut template_uses, positions, &member, span);
    }

    for method_name in &class.methods {
        let Some(method) = codebase.get_method(class.name.as_bytes(), method_name.as_bytes()) else {
            continue;
        };
        if method.method_metadata.as_ref().is_some_and(|metadata| metadata.is_constructor) || is_private_method(method)
        {
            continue;
        }

        let positions = find_method_positions(codebase, method, &owner);
        let member = format!("`{}`", method.original_name);
        add_template_uses(&mut template_uses, positions, &member, method.name_span.unwrap_or(method.span));
    }

    for property in class.properties.values() {
        let (Some(property_type), Some(span)) = (&property.type_metadata, property.name_span.or(property.span)) else {
            continue;
        };
        if matches!(property.read_visibility, Visibility::Private) {
            continue;
        }

        let takes_in = is_writable(property) && !matches!(property.write_visibility, Visibility::Private);
        let position = if takes_in { Variance::Invariant } else { Variance::Covariant };

        let mut positions = Vec::new();
        find_template_positions(codebase, &property_type.type_union, &owner, position, &mut positions);

        let member = format!("the property `{}`", property_name(property));
        add_template_uses(&mut template_uses, positions, &member, span);
    }

    template_uses
}

/// The marker, `out` or `in`, of the type parameter `template` of `class` that a use at `position` breaks, spec
/// section 11.1: an `out` type parameter is only handed out, and an `in` type parameter is only taken in.
pub(crate) fn find_broken_marker(
    class: &ClassLikeMetadata,
    template: Word,
    position: Variance,
) -> Option<&'static str> {
    let index = class.template_types.get_index_of(&template)?;

    match (class.template_variance.get(index)?, position) {
        (Variance::Covariant, Variance::Contravariant | Variance::Invariant) => Some("out"),
        (Variance::Contravariant, Variance::Covariant | Variance::Invariant) => Some("in"),
        _ => None,
    }
}

/// Reports a call or read of the method `method` through `object` when the method is private, `object` is not `this`,
/// and the method's use of a type parameter of its PHP# class breaks the parameter's marker, spec section 11.1. Such a
/// method is reachable only through `this`, as Scala's `private[this]`, so no instance with other type arguments
/// passes it a value.
pub(crate) fn check_private_method_reach<A>(
    context: &mut Context<'_, '_, A>,
    object: &Expression<'_>,
    method: &MethodIdentifier,
    span: Span,
) where
    A: Arena,
{
    let codebase = context.codebase;
    let method = codebase.get_declaring_method_identifier(method);
    let (Some(class), Some(metadata)) =
        (codebase.get_class_like(method.get_class_name().as_bytes()), codebase.get_method_by_id(&method))
    else {
        return;
    };
    if !is_private_method(metadata) {
        return;
    }

    let positions = find_method_positions(codebase, metadata, &GenericParent::ClassLike(class.name));
    report_private_reach(context, object, class, &metadata.original_name.to_string(), positions, "reachable", span);
}

/// Reports a read of `property` through `object` when its read is private, or a write when its `set` is private, when
/// `object` is not `this`, and the property's use of a type parameter of `class`, its PHP# class, breaks the
/// parameter's marker, spec section 11.1. A property another instance writes takes its type in, and one it reads hands
/// its type out. A property whose `get` is public is written only through `this`.
pub(crate) fn check_private_property_reach<A>(
    context: &mut Context<'_, '_, A>,
    object: &Expression<'_>,
    class: &ClassLikeMetadata,
    property: &PropertyMetadata,
    is_write: bool,
    span: Span,
) where
    A: Arena,
{
    let visibility = if is_write { property.write_visibility } else { property.read_visibility };
    if !matches!(visibility, Visibility::Private) {
        return;
    }
    let Some(property_type) = &property.type_metadata else {
        return;
    };

    let position = if is_writable(property) { Variance::Invariant } else { Variance::Covariant };
    let mut positions = Vec::new();
    find_template_positions(
        context.codebase,
        &property_type.type_union,
        &GenericParent::ClassLike(class.name),
        position,
        &mut positions,
    );
    let reach = if matches!(property.read_visibility, Visibility::Private) { "reachable" } else { "written" };
    report_private_reach(context, object, class, &property_name(property), positions, reach, span);
}

fn report_private_reach<A>(
    context: &mut Context<'_, '_, A>,
    object: &Expression<'_>,
    class: &ClassLikeMetadata,
    member: &str,
    positions: Vec<(Word, Variance)>,
    reach: &str,
    span: Span,
) where
    A: Arena,
{
    if !class.flags.is_sharp() || is_this(object, context.resolved_names) {
        return;
    }
    let Some((template, marker)) = positions
        .into_iter()
        .find_map(|(template, position)| Some((template, find_broken_marker(class, template, position)?)))
    else {
        return;
    };

    context.collector.report_with_code(
        IssueCode::InvalidTemplateParameter,
        Issue::error(format!("`{member}` breaks `{marker} {template}`, so it is {reach} only through `this`."))
            .with_annotation(
                Annotation::primary(span).with_message(format!("`{member}` is reached here through another value")),
            )
            .with_note("A private member may break the marker, and is then reachable only through `this`."),
    );
}

/// The positions `method` uses a type parameter of `owner` at: each parameter takes its type in, and the return type
/// hands its type out.
fn find_method_positions(
    codebase: &CodebaseMetadata,
    method: &FunctionLikeMetadata,
    owner: &GenericParent,
) -> Vec<(Word, Variance)> {
    let mut positions = Vec::new();
    for parameter_type in method.parameters.iter().filter_map(|parameter| parameter.type_metadata.as_ref()) {
        find_template_positions(codebase, &parameter_type.type_union, owner, Variance::Contravariant, &mut positions);
    }
    if let Some(return_type) = &method.return_type_metadata {
        find_template_positions(codebase, &return_type.type_union, owner, Variance::Covariant, &mut positions);
    }

    positions
}

fn is_private_method(method: &FunctionLikeMetadata) -> bool {
    method.method_metadata.as_ref().is_some_and(|metadata| matches!(metadata.visibility, Visibility::Private))
}

/// Whether a write changes `property` after it is built: it is not `readonly`, and it keeps a value or has a `set`.
fn is_writable(property: &PropertyMetadata) -> bool {
    !property.flags.is_readonly()
        && (!property.flags.is_virtual_property() || property.hooks.contains_key(&word("set")))
}

fn property_name(property: &PropertyMetadata) -> String {
    property.name.0.as_str_lossy().trim_start_matches('$').to_owned()
}

/// Adds one use per type parameter in `positions`, which a member uses at each of them.
fn add_template_uses(template_uses: &mut Vec<TemplateUse>, positions: Vec<(Word, Variance)>, member: &str, span: Span) {
    for (template, position) in positions {
        match template_uses
            .iter_mut()
            .find(|template_use| template_use.span == span && template_use.template == template)
        {
            Some(template_use) => template_use.position = join_positions(template_use.position, position),
            None => template_uses.push(TemplateUse { template, position, member: member.to_string(), span }),
        }
    }
}

/// The position of a type used at both `position` and `other`.
fn join_positions(position: Variance, other: Variance) -> Variance {
    if position == other { position } else { Variance::Invariant }
}

/// Adds the position of each use of a type parameter of `owner` in `type_union`, which sits at `position`. Inside
/// `C<…>` a type argument keeps the position under an `out` type parameter of `C`, flips it under an `in` one and takes
/// both under an invariant one. A `List` or `Map` keeps it, and a `Function` keeps it for its return and flips it for
/// each parameter.
fn find_template_positions(
    codebase: &CodebaseMetadata,
    type_union: &TUnion,
    owner: &GenericParent,
    position: Variance,
    positions: &mut Vec<(Word, Variance)>,
) {
    for atomic in type_union.types.as_ref() {
        find_atomic_template_positions(codebase, atomic, owner, position, positions);
    }
}

fn find_atomic_template_positions(
    codebase: &CodebaseMetadata,
    atomic: &TAtomic,
    owner: &GenericParent,
    position: Variance,
    positions: &mut Vec<(Word, Variance)>,
) {
    match atomic {
        TAtomic::GenericParameter(parameter) if parameter.defining_entity == *owner => {
            positions.push((parameter.parameter_name, position));
        }
        // `Class<T>` holds `T` or a subclass, so it hands `T` out.
        TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::Generic {
            parameter_name,
            defining_entity,
            ..
        })) if defining_entity == owner => {
            positions.push((*parameter_name, position));
        }
        TAtomic::Object(TObject::Named(named)) => {
            let variances = codebase.get_class_like(named.name.as_bytes()).map(|class| &class.template_variance);
            for (index, argument) in named.get_type_parameters().unwrap_or_default().iter().enumerate() {
                let argument_position = match variances.and_then(|variances| variances.get(index)) {
                    Some(Variance::Covariant) => position,
                    Some(Variance::Contravariant) => position.flip(),
                    _ => Variance::Invariant,
                };

                find_template_positions(codebase, argument, owner, argument_position, positions);
            }
        }
        TAtomic::Callable(TCallable::Signature(signature)) => {
            for parameter in signature.get_parameters() {
                if let Some(parameter_type) = parameter.get_type_signature() {
                    find_template_positions(codebase, parameter_type, owner, position.flip(), positions);
                }
            }
            if let Some(return_type) = signature.get_return_type() {
                find_template_positions(codebase, return_type, owner, position, positions);
            }
        }
        _ => {
            for child in atomic.get_child_nodes() {
                match child {
                    TypeRef::Union(child) => find_template_positions(codebase, child, owner, position, positions),
                    TypeRef::Atomic(child) => {
                        find_atomic_template_positions(codebase, child, owner, position, positions)
                    }
                }
            }
        }
    }
}

/// Rewrites `issue`, reported with `code` for `input` where `expected` is required, when only a missing `out` or `in`
/// blocks the substitution in a PHP# file: `C<A>` given where `C<B>` is expected, `C` a PHP# class, and the one type
/// argument that differs belongs to an invariant type parameter whose `out` (`A` within `B`) or `in` (`B` within `A`)
/// would accept it. The value is then of another type, not a wider one, so the issue becomes `invalid`, with spec
/// section 11.1's message and note. The help names the marker by where `C` uses the type parameter, unless that marker
/// allows only the opposite substitution, as `out` lets `C<B>` take a `C<A>` but never the other way.
pub(crate) fn explain_blocked_substitution<A>(
    context: &Context<'_, '_, A>,
    input: &TUnion,
    expected: &TUnion,
    invalid: IssueCode,
    (code, mut issue): (IssueCode, Issue),
) -> (IssueCode, Issue)
where
    A: Arena,
{
    let unchanged = |issue| (code, issue);
    if !context.dialect.is_sharp() {
        return unchanged(issue);
    }

    let codebase = context.codebase;
    let [TAtomic::Object(TObject::Named(given_object))] = input.types.as_ref() else {
        return unchanged(issue);
    };
    let Some((wanted, wanted_object)) = expected.types.iter().find_map(|atomic| match atomic {
        TAtomic::Object(TObject::Named(object))
            if object.name.as_bytes().eq_ignore_ascii_case(given_object.name.as_bytes()) =>
        {
            Some((atomic, object))
        }
        _ => None,
    }) else {
        return unchanged(issue);
    };
    let Some(class) = codebase.get_class_like(given_object.name.as_bytes()).filter(|class| class.flags.is_sharp())
    else {
        return unchanged(issue);
    };
    let given_arguments = given_object.get_type_parameters().unwrap_or_default();
    let wanted_arguments = wanted_object.get_type_parameters().unwrap_or_default();
    if given_arguments.len() != wanted_arguments.len() {
        return unchanged(issue);
    }

    let contains = |input: &TUnion, container: &TUnion| {
        union_comparator::is_contained_by(
            codebase,
            input,
            container,
            false,
            false,
            false,
            &mut ComparisonResult::for_dialect(context.dialect),
        )
    };

    let mut blocked = None;
    for (index, (given_argument, wanted_argument)) in given_arguments.iter().zip(wanted_arguments).enumerate() {
        let widens = contains(given_argument, wanted_argument);
        let narrows = contains(wanted_argument, given_argument);
        if widens && narrows {
            continue;
        }

        let is_invariant = class.template_variance.get(index).is_none_or(Variance::is_invariant);
        if blocked.is_some() || !is_invariant || !(widens || narrows) {
            return unchanged(issue);
        }

        blocked = Some((index, widens));
    }

    let Some((template, widens)) =
        blocked.and_then(|(index, widens)| Some((*class.template_types.get_index(index)?.0, widens)))
    else {
        return unchanged(issue);
    };

    let class_name = short_name(class.original_name);
    let position = find_template_uses(context, class, &WordMap::default())
        .into_iter()
        .filter(|template_use| template_use.template == template)
        .map(|template_use| template_use.position)
        .reduce(join_positions);

    let given = display_sharp_type(context, input);
    issue.message = format!("{given} cannot be used as {}.", display_sharp_type(context, &wrap_atomic(wanted.clone())));
    for annotation in &mut issue.annotations {
        if matches!(annotation.kind, AnnotationKind::Primary) {
            annotation.message = Some(format!("This has type `{given}`."));
        }
    }
    issue.notes =
        vec!["A type parameter without `out` or `in` is invariant, so neither substitution is allowed.".to_owned()];
    issue.help = match position {
        Some(Variance::Covariant) if widens => {
            Some(format!("{template} is only returned by {class_name}, so declare it `out {template}`."))
        }
        Some(Variance::Contravariant) if !widens => {
            Some(format!("{template} is only taken in by {class_name}, so declare it `in {template}`."))
        }
        Some(Variance::Covariant | Variance::Contravariant) => None,
        Some(_) => Some(format!("{template} is both taken in and returned by {class_name}, so neither marker fits.")),
        None => Some(format!(
            "{template} is neither taken in nor returned by {class_name}, so declare it `{} {template}`.",
            if widens { "out" } else { "in" }
        )),
    };

    (invalid, issue)
}
