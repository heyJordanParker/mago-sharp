use std::sync::Arc;

use mago_allocator::Arena;
use mago_names::ResolvedNames;
use mago_names::scope::NamespaceScope;
use mago_phpdoc_syntax::cst::r#type::Type;
use mago_span::HasSpan;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::UnionHint;
use mago_syntax::dialect::Dialect;
use mago_word::Word;
use mago_word::word;

use crate::metadata::ttype::TypeMetadata;
use crate::scanner::Context;
use crate::ttype::TType;
use crate::ttype::atomic::TAtomic;
use crate::ttype::atomic::array::TArray;
use crate::ttype::atomic::callable::TCallable;
use crate::ttype::atomic::callable::TCallableSignature;
use crate::ttype::atomic::callable::parameter::TCallableParameter;
use crate::ttype::atomic::mixed::TMixed;
use crate::ttype::atomic::object::TObject;
use crate::ttype::atomic::object::named::TNamedObject;
use crate::ttype::atomic::reference::TReference;
use crate::ttype::builder;
use crate::ttype::error::TypeError;
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
use crate::ttype::union::TUnion;
use crate::ttype::wrap_atomic;

#[inline]
pub fn get_type_metadata_from_hint<'arena, A>(
    hint: &'arena Hint<'arena>,
    classname: Option<Word>,
    context: &Context<'_, 'arena, A>,
) -> TypeMetadata
where
    A: Arena,
{
    let type_union = union_from_hint(hint, classname, context.resolved_names, context.program.dialect);

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

/// Converts a type written in code into its `TUnion`, resolving class names through `resolved_names`. `classname`
/// is the class that `self` and `static` name, if any.
#[inline]
#[must_use]
pub fn get_union_from_hint(hint: &Hint<'_>, classname: Option<Word>, resolved_names: &ResolvedNames<'_>) -> TUnion {
    union_from_hint(hint, classname, resolved_names, Dialect::Php)
}

/// `get_union_from_hint` for a type written in `dialect`. PHP#'s `Self` is PHP's `static`, spec section 25, and the
/// checker refuses PHP's `self`, which the lexer reads as the same keyword.
fn union_from_hint(
    hint: &Hint<'_>,
    classname: Option<Word>,
    resolved_names: &ResolvedNames<'_>,
    dialect: Dialect,
) -> TUnion {
    match hint {
        Hint::Parenthesized(parenthesized_hint) => {
            union_from_hint(parenthesized_hint.hint, classname, resolved_names, dialect)
        }
        Hint::Identifier(identifier) => get_union_from_identifier_hint(identifier, resolved_names),
        Hint::Nullable(nullable_hint) => match nullable_hint.hint {
            Hint::Null(_) => get_null(),
            Hint::String(_) => get_nullable_string(),
            Hint::Integer(_) => get_nullable_int(),
            Hint::Float(_) => get_nullable_float(),
            Hint::Object(_) => get_nullable_object(),
            _ => union_from_hint(nullable_hint.hint, classname, resolved_names, dialect).as_nullable(),
        },
        Hint::Union(UnionHint { left: Hint::Null(_), right, .. }) => match right {
            Hint::Null(_) => get_null(),
            Hint::String(_) => get_nullable_string(),
            Hint::Integer(_) => get_nullable_int(),
            Hint::Float(_) => get_nullable_float(),
            Hint::Object(_) => get_nullable_object(),
            _ => union_from_hint(right, classname, resolved_names, dialect).as_nullable(),
        },
        Hint::Union(UnionHint { left, right: Hint::Null(_), .. }) => match left {
            Hint::Null(_) => get_null(),
            Hint::String(_) => get_nullable_string(),
            Hint::Integer(_) => get_nullable_int(),
            Hint::Float(_) => get_nullable_float(),
            Hint::Object(_) => get_nullable_object(),
            _ => union_from_hint(left, classname, resolved_names, dialect).as_nullable(),
        },
        Hint::Union(union_hint) => {
            let left = union_from_hint(union_hint.left, classname, resolved_names, dialect);
            let right = union_from_hint(union_hint.right, classname, resolved_names, dialect);

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
            let left = union_from_hint(intersection.left, classname, resolved_names, dialect);
            let right = union_from_hint(intersection.right, classname, resolved_names, dialect);

            let left_types = left.types;
            let right_types = right.types;
            let mut intersection_types = vec![];
            for left_type in left_types.into_owned() {
                if !left_type.can_be_intersected() {
                    // should be an error.
                    continue;
                }

                for right_type in right_types.as_ref() {
                    if !right_type.can_be_intersected() {
                        // should be an error.
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

            TUnion::from_vec(intersection_types)
        }
        Hint::Iterable(_) => get_mixed_iterable(),
        Hint::Generic(generic) => {
            let mut arguments =
                generic.arguments.iter().map(|argument| get_union_from_hint(argument, classname, resolved_names));

            match (generic.name.value, arguments.next(), arguments.next(), arguments.next()) {
                (b"List", Some(element), None, None) => get_list(element),
                (b"Map", Some(key), Some(value), None) => get_keyed_array(key, value),
                (b"Set", Some(element), None, None) => wrap_atomic(TAtomic::Array(TArray::Set(Arc::new(element)))),
                _ => get_mixed_keyed_array(),
            }
        }
        // Spec section 14.1: `Function<R(P)>` is PHP's `Closure(P): R`, the only function value PHP# makes, and spec
        // section 29 gives it the effects its `uses` lists.
        Hint::Function(function) => {
            let parameters = function
                .parameters
                .iter()
                .map(|parameter| {
                    TCallableParameter::new(
                        Some(Arc::new(get_union_from_hint(parameter, classname, resolved_names))),
                        false,
                        false,
                        false,
                    )
                })
                .collect();
            let return_type = get_union_from_hint(function.return_type, classname, resolved_names);
            let effects = function
                .uses
                .iter()
                .flat_map(|uses| uses.names.iter())
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

#[inline]
fn get_union_from_identifier_hint(identifier: &Identifier<'_>, resolved_names: &ResolvedNames<'_>) -> TUnion {
    let name = resolved_names.get(identifier);

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

    if name.eq_ignore_ascii_case(b"Closure") {
        return wrap_atomic(TAtomic::Callable(TCallable::Signature(TCallableSignature::mixed(true))));
    }

    wrap_atomic(TAtomic::Reference(TReference::Symbol {
        name: word(name),
        parameters: None,
        variances: None,
        intersection_types: None,
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
