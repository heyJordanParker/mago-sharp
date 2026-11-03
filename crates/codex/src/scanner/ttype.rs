use std::sync::Arc;

use mago_allocator::Arena;
use mago_names::EQUATABLE;
use mago_names::ResolvedNames;
use mago_names::TypeParametersNamed;
use mago_names::kind::NameKind;
use mago_names::scope::NamespaceScope;
use mago_phpdoc_syntax::cst::r#type::Type;
use mago_span::HasSpan;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::TypeParameterList;
use mago_syntax::cst::UnionHint;
use mago_syntax::cst::built_in_generic_arity;
use mago_syntax::dialect::Dialect;
use mago_syntax::walker::Walker;
use mago_word::Word;
use mago_word::word;

use crate::metadata::ttype::TypeMetadata;
use crate::misc::GenericParent;
use crate::scanner::Context;
use crate::ttype::TType;
use crate::ttype::atomic::TAtomic;
use crate::ttype::atomic::array::TArray;
use crate::ttype::atomic::array::list::TList;
use crate::ttype::atomic::callable::TCallable;
use crate::ttype::atomic::callable::TCallableSignature;
use crate::ttype::atomic::callable::parameter::TCallableParameter;
use crate::ttype::atomic::iterable::TIterable;
use crate::ttype::atomic::mixed::TMixed;
use crate::ttype::atomic::object::TObject;
use crate::ttype::atomic::object::named::TNamedObject;
use crate::ttype::atomic::reference::TReference;
use crate::ttype::atomic::scalar::TScalar;
use crate::ttype::atomic::scalar::class_like_string::TClassLikeString;
use crate::ttype::atomic::scalar::class_like_string::TClassLikeStringKind;
use crate::ttype::builder;
use crate::ttype::error::TypeError;
use crate::ttype::flags::UnionFlags;
use crate::ttype::get_bool;
use crate::ttype::get_false;
use crate::ttype::get_float;
use crate::ttype::get_int;
use crate::ttype::get_keyed_array;
use crate::ttype::get_list;
use crate::ttype::get_mixed;
use crate::ttype::get_mixed_callable;
use crate::ttype::get_mixed_iterable;
use crate::ttype::get_mixed_keyed_array;
use crate::ttype::get_never;
use crate::ttype::get_null;
use crate::ttype::get_nullable_float;
use crate::ttype::get_nullable_int;
use crate::ttype::get_nullable_object;
use crate::ttype::get_nullable_string;
use crate::ttype::get_object;
use crate::ttype::get_string;
use crate::ttype::get_true;
use crate::ttype::get_void;
use crate::ttype::resolution::TypeResolutionContext;
use crate::ttype::template::GenericTemplate;
use crate::ttype::template::refuse_bounds;
use crate::ttype::union::TUnion;
use crate::ttype::wrap_atomic;

#[inline]
pub fn get_type_metadata_from_hint<'arena, A>(
    hint: &'arena Hint<'arena>,
    classname: Option<Word>,
    type_context: &TypeResolutionContext,
    context: &Context<'_, 'arena, A>,
) -> TypeMetadata
where
    A: Arena,
{
    let type_union = union_from_hint(hint, classname, context.resolved_names, type_context, context.program.dialect);

    let mut type_metadata = TypeMetadata::new(type_union, hint.span());
    type_metadata.from_docblock = false;
    type_metadata
}

#[inline]
pub fn get_type_metadata_from_type(
    ttype: &Type<'_>,
    classname: Option<Word>,
    type_context: &TypeResolutionContext,
    scope: &NamespaceScope,
) -> Result<TypeMetadata, TypeError> {
    builder::get_union_from_type(ttype, scope, type_context, classname).map(|type_union| {
        let mut type_metadata = TypeMetadata::new(type_union, ttype.span());
        type_metadata.from_docblock = true;
        type_metadata
    })
}

/// The templates a PHP# type parameter list declares, in order, spec section 11.
///
/// Each is the template a `@template` tag declares for `defining_entity`, its bound the tag's `of`, or `mixed` without
/// one. [`refuse_bounds`] refuses a bound from its [written bound](GenericTemplate::written_bound), now and on each
/// population.
///
/// Every name of the list is a template while the bounds are read, so a bound names a type parameter of its own list,
/// its own or a later one, as in `<TItem : Comparable<TItem>>`. The bounds are read once per type parameter, each time
/// with the templates of the read before, so a bound sees the bound of every type parameter it names, directly or
/// through another bound, as `<TKey : DatabaseEntity, TItem : Holder<TKey>>` sees `TKey`'s. `type_context` holds the
/// templates once the list is read.
pub fn scan_type_parameters<'arena, A>(
    type_parameters: &'arena TypeParameterList<'arena>,
    defining_entity: GenericParent,
    classname: Word,
    type_context: &mut TypeResolutionContext,
    context: &Context<'_, 'arena, A>,
    scope: &mut NamespaceScope,
) -> Vec<(Word, GenericTemplate)>
where
    A: Arena,
{
    for parameter in &type_parameters.parameters {
        scope.add(NameKind::Default, parameter.name.value, &(None as Option<&str>));
        type_context
            .get_template_definitions_mut()
            .insert(word(parameter.name.value), vec![GenericTemplate::new(defining_entity, get_mixed())]);
    }

    // Whether the checker refuses each bound, and the other type parameters the binder finds in a refused one, which its
    // refusal marks: a refused bound reads as no type PHP# has, as `Query<TOther> & int` reads as nothing.
    let refusals: Vec<(bool, Vec<&[u8]>)> = type_parameters
        .parameters
        .iter()
        .map(|parameter| {
            let refused =
                context.resolved_names.is_refused_bound(type_parameters, parameter, context.file.is_standard_library);
            let mut named = Vec::new();
            if let Some(bound) = &parameter.bound
                && refused
            {
                TypeParametersNamed(context.resolved_names).walk_type_parameter_bound(bound, &mut named);
                named.retain(|name| *name != parameter.name.value);
            }

            (refused, named)
        })
        .collect();

    let mut templates: Vec<(Word, GenericTemplate)> = Vec::new();
    for _ in &type_parameters.parameters {
        templates = type_parameters
            .parameters
            .iter()
            .zip(&refusals)
            .map(|(parameter, (refused, named))| {
                let mut template = GenericTemplate::new(defining_entity, get_mixed());
                if let Some(bound) = &parameter.bound {
                    let mut written =
                        get_type_metadata_from_hint(&bound.hint, Some(classname), &*type_context, context);
                    template.constraint = written.type_union.clone();
                    template.bound_refused = *refused;
                    let named: Vec<TAtomic> = named
                        .iter()
                        .filter_map(|name| {
                            let definitions = type_context.get_template_definition(word(name))?;

                            Some(builder::get_template_atomic(definitions, word(name)))
                        })
                        .collect();
                    if !named.is_empty() {
                        written.type_union = TUnion::from_vec(named);
                    }
                    template.written_bound = Some(written);
                }

                (word(parameter.name.value), template)
            })
            .collect();
        // Population decides which types exist, so the scan reads each bound as the checker decides it.
        refuse_bounds(templates.iter_mut().map(|(name, template)| (&*name, template)), |_| true);

        for (name, definition) in &templates {
            type_context.get_template_definitions_mut().insert(*name, vec![definition.clone()]);
        }
    }

    templates
}

/// Converts a type written in code into its `TUnion`, resolving class names through `resolved_names`.
///
/// A PHP# type parameter is the template of its name in `type_context`. `classname` is the class that `self` and
/// `static` name, if any.
#[inline]
#[must_use]
pub fn get_union_from_hint(
    hint: &Hint<'_>,
    classname: Option<Word>,
    resolved_names: &ResolvedNames<'_>,
    type_context: &TypeResolutionContext,
    dialect: Dialect,
) -> TUnion {
    union_from_hint(hint, classname, resolved_names, type_context, dialect)
}

/// `get_union_from_hint` for a type written in `dialect`. PHP#'s `Self` is PHP's `static`, spec section 25, and the
/// checker refuses PHP's `self`, which the lexer reads as the same keyword.
fn union_from_hint(
    hint: &Hint<'_>,
    classname: Option<Word>,
    resolved_names: &ResolvedNames<'_>,
    type_context: &TypeResolutionContext,
    dialect: Dialect,
) -> TUnion {
    let convert = |hint: &Hint<'_>| union_from_hint(hint, classname, resolved_names, type_context, dialect);

    match hint {
        Hint::Parenthesized(parenthesized_hint) => convert(parenthesized_hint.hint),
        Hint::Identifier(identifier) => {
            get_union_from_identifier_hint(identifier, resolved_names, type_context, dialect)
        }
        // Spec section 13: `readonly` before a type makes the value readonly, and everything read through it.
        Hint::Readonly(readonly) => convert(readonly.hint).to_readonly(),
        Hint::Nullable(nullable_hint) => match nullable_hint.hint {
            Hint::Null(_) => get_null(),
            Hint::String(_) => get_nullable_string(),
            Hint::Integer(_) => get_nullable_int(),
            Hint::Float(_) => get_nullable_float(),
            Hint::Object(_) => get_nullable_object(),
            _ => convert(nullable_hint.hint).as_nullable(),
        },
        Hint::Union(UnionHint { left: Hint::Null(_), right, .. }) => match right {
            Hint::Null(_) => get_null(),
            Hint::String(_) => get_nullable_string(),
            Hint::Integer(_) => get_nullable_int(),
            Hint::Float(_) => get_nullable_float(),
            Hint::Object(_) => get_nullable_object(),
            _ => convert(right).as_nullable(),
        },
        Hint::Union(UnionHint { left, right: Hint::Null(_), .. }) => match left {
            Hint::Null(_) => get_null(),
            Hint::String(_) => get_nullable_string(),
            Hint::Integer(_) => get_nullable_int(),
            Hint::Float(_) => get_nullable_float(),
            Hint::Object(_) => get_nullable_object(),
            _ => convert(left).as_nullable(),
        },
        Hint::Union(union_hint) => {
            let left = convert(union_hint.left);
            let right = convert(union_hint.right);

            let combined_types: Vec<TAtomic> = left.types.iter().chain(right.types.iter()).cloned().collect();

            TUnion::from_vec(combined_types)
        }
        Hint::Null(_) => get_null(),
        Hint::True(_) => get_true(),
        Hint::False(_) => get_false(),
        Hint::Array(_) => get_mixed_keyed_array(),
        Hint::Callable(_) => get_mixed_callable(),
        Hint::Static(_) | Hint::Self_(_) => {
            let classname = classname.unwrap_or_else(|| word("static"));
            let is_static = matches!(hint, Hint::Static(_)) || dialect.is_sharp();

            wrap_atomic(TAtomic::Object(TObject::Named(TNamedObject::new(classname).with_is_static(is_static))))
        }
        Hint::Void(_) => get_void(),
        Hint::Never(_) => get_never(),
        Hint::Float(_) => get_float(),
        Hint::Bool(_) => get_bool(),
        Hint::Integer(_) => get_int(),
        Hint::String(_) => get_string(),
        Hint::Object(_) => get_object(),
        // PHP# writes `mixed` as `Any?`, so its `Any` never holds null.
        Hint::Mixed(any) if any.value == b"Any" => wrap_atomic(TAtomic::Mixed(TMixed::new().with_is_non_null(true))),
        Hint::Mixed(_) => get_mixed(),
        Hint::Parent(_) => wrap_atomic(TAtomic::Object(TObject::Named(TNamedObject::new(word("parent"))))),
        Hint::Intersection(intersection) => {
            let left = convert(intersection.left);
            let right = convert(intersection.right);
            // Spec section 11: `Equatable` joins a bound as the `==` its values have, and adds no type of its own.
            let is_equatable = |side: &TUnion| side.flags.contains(UnionFlags::EQUATABLE);
            if is_equatable(&left) || is_equatable(&right) {
                let mut joined = match (left.is_mixed(), right.is_mixed()) {
                    (true, _) => right,
                    (_, true) => left,
                    _ => union_from_intersection(left, right),
                };
                joined.flags.insert(UnionFlags::EQUATABLE);

                return joined;
            }

            union_from_intersection(left, right)
        }
        Hint::Iterable(_) => get_mixed_iterable(),
        // A generic class type is the docblock's `C<A, B>`, and `Class<T>` is its `class-string<T>`, spec section 25.
        Hint::Generic(generic) => {
            let mut arguments: Vec<TUnion> = generic.type_arguments.arguments.iter().map(convert).collect();

            if let Some(name) = resolved_names.resolve(&generic.name) {
                wrap_atomic(TAtomic::Reference(TReference::Symbol {
                    name: word(name),
                    parameters: Some(arguments),
                    variances: None,
                    intersection_types: None,
                    is_readonly: false,
                }))
            } else {
                match generic.name.value {
                    name if built_in_generic_arity(name) != Some(arguments.len()) => get_mixed_keyed_array(),
                    b"List" => get_list(arguments.swap_remove(0)),
                    b"Map" => {
                        let value = arguments.swap_remove(1);

                        get_keyed_array(arguments.swap_remove(0), value)
                    }
                    b"Set" => wrap_atomic(TAtomic::Array(TArray::Set(Arc::new(arguments.swap_remove(0))))),
                    // Spec section 12: anything a loop can read, PHP's `iterable` of its elements.
                    b"Iterable" => {
                        wrap_atomic(TAtomic::Iterable(TIterable::sharp_of(Arc::new(arguments.swap_remove(0)))))
                    }
                    // A class value of one class, or of a type parameter, names it. Of any other type it holds that
                    // whole type, as `Class<int|string>` holds `int|string`, spec section 25.
                    b"Class" => {
                        let held = arguments.swap_remove(0);
                        let class = if held.is_single() {
                            builder::get_class_strings_of(TClassLikeStringKind::Class, held.clone(), generic.span())
                                .ok()
                        } else {
                            None
                        };

                        class.unwrap_or_else(|| {
                            wrap_atomic(TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::OfType {
                                kind: TClassLikeStringKind::Class,
                                constraint: Arc::new(held),
                            })))
                        })
                    }
                    _ => get_mixed_keyed_array(),
                }
            }
        }
        // Spec section 30: a tuple is the sealed list of its element types, which a PHP list holds while the code runs.
        Hint::Tuple(tuple) => wrap_atomic(TAtomic::Array(TArray::List(TList::tuple(
            tuple
                .elements
                .iter()
                .map(|element| (convert(&element.hint), element.name.map(|name| word(name.value))))
                .collect(),
        )))),
        // Spec section 14.1: `Function<R(P)>` is PHP's `Closure(P): R`, the only function value PHP# makes, and spec
        // section 29 gives it the effects its `uses` lists, which `uses none` writes empty.
        Hint::Function(function) => {
            let parameters = function
                .parameters
                .iter()
                .map(|parameter| TCallableParameter::new(Some(Arc::new(convert(parameter))), false, false, false))
                .collect();
            let return_type = convert(function.return_type);
            let effects = function
                .uses
                .iter()
                .flat_map(|uses| uses.names.iter())
                .filter(|effect| effect.value != b"none")
                .map(|effect| word(resolved_names.get(effect)))
                .collect();

            wrap_atomic(TAtomic::Callable(TCallable::Signature(TCallableSignature {
                effects: Some(effects),
                ..TCallableSignature::new(false, true)
                    .with_parameters(parameters)
                    .with_return_type(Some(Arc::new(return_type)))
            })))
        }
    }
}

/// The type `left & right` writes: each type of `left` joined with each type of `right`.
fn union_from_intersection(left: TUnion, right: TUnion) -> TUnion {
    let right_types = right.types;
    let mut intersection_types = vec![];
    // A type that can't be intersected, as `int` in `int & string`, is the semantics checker's error in PHP#.
    for left_type in left.types.into_owned() {
        if !left_type.can_be_intersected() {
            continue;
        }

        for right_type in right_types.as_ref() {
            if !right_type.can_be_intersected() {
                continue;
            }

            let mut intersection = left_type.clone();
            if let Some(nested_intersections) = right_type.get_intersection_types() {
                let mut right_base = right_type.clone();
                if let Some(intersections) = right_base.get_intersection_types_mut() {
                    intersections.clear();
                }

                intersection.add_intersection_type(right_base);
                for nested in nested_intersections {
                    intersection.add_intersection_type(nested.clone());
                }
            } else {
                intersection.add_intersection_type(right_type.clone());
            }

            intersection_types.push(intersection);
        }
    }

    // With nothing left to join, the type is refused, as a union bound is, so it adds no issue of its own.
    if intersection_types.is_empty() { wrap_atomic(TAtomic::Error) } else { TUnion::from_vec(intersection_types) }
}

/// The type a name in a hint writes. A PHP# type parameter is the template of the same name in `type_context`, as a
/// docblock's `@template` name is. PHP#'s `Equatable` is `Any?` marked [`UnionFlags::EQUATABLE`], the bound alone, and
/// the checker refuses it anywhere else. PHP's `Closure` is its function type, and a `dialect` PHP# file, whose function
/// types all go by one name, spec section 14.1, reads it as the class it names, which is no PHP# type.
#[inline]
fn get_union_from_identifier_hint(
    identifier: &Identifier<'_>,
    resolved_names: &ResolvedNames<'_>,
    type_context: &TypeResolutionContext,
    dialect: Dialect,
) -> TUnion {
    let name = resolved_names.get(identifier);

    if resolved_names.is_type_parameter(identifier)
        && let Some(definitions) = type_context.get_template_definition(word(name))
    {
        return wrap_atomic(builder::get_template_atomic(definitions, word(name)));
    }

    // Spec section 11: `Equatable` bounds a type parameter by the `==` its values have, and adds no type of its own.
    if dialect.is_sharp() && matches!(identifier, Identifier::Local(local) if local.value == EQUATABLE.as_bytes()) {
        let mut equatable = get_mixed();
        equatable.flags.insert(UnionFlags::EQUATABLE);

        return equatable;
    }

    if name.eq_ignore_ascii_case(b"Generator") {
        let mixed_default = || {
            let mut union = get_mixed();
            union.set_from_template_default(true);
            union
        };

        return wrap_atomic(TAtomic::Object(TObject::Named(
            TNamedObject::new(word(name)).with_type_parameters(Some(vec![
                mixed_default(),
                mixed_default(),
                mixed_default(),
                mixed_default(),
            ])),
        )));
    }

    if !dialect.is_sharp() && name.eq_ignore_ascii_case(b"Closure") {
        return wrap_atomic(TAtomic::Callable(TCallable::Signature(TCallableSignature::mixed(true))));
    }

    wrap_atomic(TAtomic::Reference(TReference::Symbol {
        name: word(name),
        parameters: None,
        variances: None,
        intersection_types: None,
        is_readonly: false,
    }))
}

/// Merges a docblock type with a real type, preserving nullability from the real type.
///
/// If the real type is nullable but the docblock type is not, this function makes
/// the docblock type nullable. This ensures that the actual signature's nullability
/// is respected even when a more specific type is provided in the docblock.
///
/// # Examples
///
/// - Real: `?string`, Docblock: `non-empty-string` → Result: `?non-empty-string`
/// - Real: `null|int`, Docblock: `int` → Result: `null|int`
/// - Real: `string`, Docblock: `non-empty-string` → Result: `non-empty-string`
///
/// # Arguments
///
/// * `docblock_type` - The type from the @param, @var, or @return annotation
/// * `real_type` - The actual type from the code signature (if any)
///
/// # Returns
///
/// The docblock type, potentially modified to be nullable if the real type was nullable
#[inline]
pub fn merge_type_preserving_nullability(
    docblock_type: TypeMetadata,
    real_type: Option<&TypeMetadata>,
) -> TypeMetadata {
    if docblock_type.type_union.types.iter().any(|t| t.is_conditional()) {
        return docblock_type;
    }

    if real_type.is_some_and(|tm| tm.type_union.is_nullable()) && !docblock_type.type_union.accepts_null() {
        docblock_type.map_type_union(super::super::ttype::union::TUnion::as_nullable)
    } else {
        docblock_type
    }
}
