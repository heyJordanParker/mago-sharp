use std::borrow::Cow;
use std::sync::Arc;

use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::parameter::FunctionLikeParameterMetadata;
use mago_codex::metadata::ttype::TypeMetadata;
use mago_codex::misc::GenericParent;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::generic::TGenericParameter;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::combiner;
use mago_codex::ttype::combiner::CombinerOptions;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::expander::StaticClassType;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::expander::expand_union;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_mixed_callable;
use mago_codex::ttype::get_mixed_closure;
use mago_codex::ttype::get_mixed_iterable;
use mago_codex::ttype::get_mixed_keyed_array;
use mago_codex::ttype::get_string;
use mago_codex::ttype::template::TemplateResult;
use mago_codex::ttype::template::inferred_type_replacer;
use mago_codex::ttype::template::variance::Variance;
use mago_codex::ttype::union::TUnion;
use mago_codex::ttype::wrap_atomic;
use mago_codex::visibility::Visibility;
use mago_syntax::dialect::Dialect;
use mago_word::Word;
use mago_word::word;

use crate::context::Context;
use crate::utils::names::display_sharp_type;
use mago_allocator::Arena;
use mago_names::short_name;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureCompatibilityIssue {
    FinalMethodOverride,
    StaticModifierMismatch { child_is_static: bool, parent_is_static: bool },
    VisibilityNarrowed { child_visibility: Visibility, parent_visibility: Visibility },
    ParameterCountMismatch { child_required_count: usize, parent_required_count: usize },
    MissingVariadicParameter { parameter_index: usize },
    IncompatibleParameterType { parameter_index: usize, child_type: TUnion, parent_type: TUnion },
    IncompatibleReturnType { child_type: TUnion, parent_type: TUnion },
    MissingReturnTypeDeclaration { parent_type: TUnion },
    ParameterNameMismatch { parameter_index: usize, child_name: Word, parent_name: Word },
    ErasedParameterNarrowed { parameter_index: usize, child_type: TUnion, parent_type: TUnion, bound: Option<Word> },
    ChangedTemplateBound { template: Word, child_method: String, parent_method: String, bound: Option<TUnion> },
    ChangedTemplateCount { child_method: String, parent_method: String, parent_count: usize },
}

/// Validates that a child method signature is compatible with a parent method signature.
///
/// This function checks the Liskov Substitution Principle (LSP) rules:
/// - Static modifier must match exactly (invariant)
/// - Visibility can only widen (public >= protected >= private)
/// - Parameters are contravariant (child must accept >= parent accepts)
/// - Return type is covariant (child must return <= parent returns)
/// - Return type declaration must be present when a non-builtin parent declares one
///   (builtin declarations may be tentative, where omission only deprecates)
/// - Parameter count (child must accept at least parent's required parameters)
/// - In a PHP# file, a variadic parent parameter stays variadic, as the engine requires when it links the class
/// - Parameter names should match (warning only - breaks named arguments)
///
/// # Arguments
///
/// * `codebase` - The codebase metadata for type lookups
/// * `child_class_name` - The fully qualified name of the class containing the overriding method
/// * `child_method` - The overriding method
/// * `parent_method` - The parent/interface method being overridden/implemented
/// * `dialect` - The dialect of the file that declares the class
///
/// # Returns
///
/// A vector of issues found. Empty vector if signatures are fully compatible.
/// Errors are returned first, then warnings.
pub fn validate_method_signature_compatibility(
    codebase: &CodebaseMetadata,
    child_class_name: Word,
    child_method: &FunctionLikeMetadata,
    parent_method: &FunctionLikeMetadata,
    dialect: Dialect,
) -> Vec<SignatureCompatibilityIssue> {
    if !child_method.flags.is_user_defined() {
        // The child method is not user-defined; skip validation.
        return Vec::new();
    }

    let (child_method, changed_bound) = if dialect.is_sharp() {
        match with_parent_type_parameters(codebase, child_method, parent_method) {
            Ok(child_method) => (child_method, None),
            Err(changed_bound) => (Cow::Borrowed(child_method), Some(changed_bound)),
        }
    } else {
        (Cow::Borrowed(child_method), None)
    };
    let child_method = &child_method;

    let mut issues = Vec::new();

    let Some(child_method_meta) = child_method.method_metadata.as_ref() else {
        return issues;
    };

    let Some(parent_method_meta) = parent_method.method_metadata.as_ref() else {
        return issues;
    };

    if parent_method_meta.is_final {
        issues.push(SignatureCompatibilityIssue::FinalMethodOverride);

        return issues;
    }

    if child_method_meta.is_static != parent_method_meta.is_static {
        issues.push(SignatureCompatibilityIssue::StaticModifierMismatch {
            child_is_static: child_method_meta.is_static,
            parent_is_static: parent_method_meta.is_static,
        });

        return issues;
    }

    if is_visibility_narrowed(child_method_meta.visibility, parent_method_meta.visibility) {
        issues.push(SignatureCompatibilityIssue::VisibilityNarrowed {
            child_visibility: child_method_meta.visibility,
            parent_visibility: parent_method_meta.visibility,
        });

        return issues;
    }

    let parent_param_count = parent_method.parameters.len();
    let child_param_count = child_method.parameters.len();

    if child_param_count < parent_param_count {
        let child_required_count = child_method.parameters.iter().filter(|p| !p.flags.has_default()).count();
        let parent_required_count = parent_method.parameters.iter().filter(|p| !p.flags.has_default()).count();

        issues
            .push(SignatureCompatibilityIssue::ParameterCountMismatch { child_required_count, parent_required_count });

        return issues;
    }

    for (index, parent_param) in parent_method.parameters.iter().enumerate() {
        let child_param = &child_method.parameters[index];

        let parent_is_optional = parent_param.flags.has_default();
        let child_is_optional = child_param.flags.has_default();

        if parent_is_optional && !child_is_optional {
            let child_required_count = child_method.parameters.iter().filter(|p| !p.flags.has_default()).count();
            let parent_required_count = parent_method.parameters.iter().filter(|p| !p.flags.has_default()).count();

            issues.push(SignatureCompatibilityIssue::ParameterCountMismatch {
                child_required_count,
                parent_required_count,
            });

            return issues;
        }
    }

    for index in parent_param_count..child_param_count {
        let child_param = &child_method.parameters[index];
        if !child_param.flags.has_default() {
            let child_required_count = child_method.parameters.iter().filter(|p| !p.flags.has_default()).count();
            let parent_required_count = parent_method.parameters.iter().filter(|p| !p.flags.has_default()).count();

            issues.push(SignatureCompatibilityIssue::ParameterCountMismatch {
                child_required_count,
                parent_required_count,
            });

            return issues;
        }
    }

    // Spec section 7 keeps an overridden variadic parameter variadic. Upstream Mago leaves it to the engine in PHP.
    if dialect.is_sharp()
        && let Some(parameter_index) = parent_method.parameters.iter().position(|p| p.flags.is_variadic())
        && !child_method.parameters.iter().any(|p| p.flags.is_variadic())
    {
        issues.push(SignatureCompatibilityIssue::MissingVariadicParameter { parameter_index });

        return issues;
    }

    if let Some(changed_bound) = changed_bound {
        issues.push(changed_bound);

        return issues;
    }

    for (index, parent_param) in parent_method.parameters.iter().enumerate() {
        let Some(child_param) = child_method.parameters.get(index) else {
            continue;
        };

        let parent_param_type = match &parent_param.type_metadata {
            Some(t) => &t.type_union,
            None => continue,
        };

        let child_param_type = match &child_param.type_metadata {
            Some(t) => &t.type_union,
            None => continue,
        };

        let mut expanded_parent_param_type = parent_param_type.clone();
        let mut expanded_child_param_type = child_param_type.clone();

        let expansion_options = TypeExpansionOptions {
            self_class: Some(child_class_name),
            static_class_type: StaticClassType::Name(child_class_name),
            function_is_final: child_method_meta.is_final,
            ..Default::default()
        };

        if expanded_parent_param_type.is_expandable() {
            expand_union(codebase, &mut expanded_parent_param_type, &expansion_options);
        }
        if expanded_child_param_type.is_expandable() {
            expand_union(codebase, &mut expanded_child_param_type, &expansion_options);
        }

        let is_compatible = union_comparator::is_contained_by(
            codebase,
            &expanded_parent_param_type,
            &expanded_child_param_type,
            false,
            false,
            false,
            &mut ComparisonResult::for_dialect(dialect),
        );

        if !is_compatible {
            issues.push(SignatureCompatibilityIssue::IncompatibleParameterType {
                parameter_index: index,
                child_type: expanded_child_param_type,
                parent_type: expanded_parent_param_type,
            });

            return issues;
        }
    }

    for (index, parent_param) in parent_method.parameters.iter().enumerate() {
        let Some(child_param) = child_method.parameters.get(index) else {
            continue;
        };

        if parent_param.name != child_param.name {
            issues.push(SignatureCompatibilityIssue::ParameterNameMismatch {
                parameter_index: index,
                child_name: child_param.name.0,
                parent_name: parent_param.name.0,
            });
        }
    }

    if let Some(parent_return) = &parent_method.return_type_declaration_metadata
        && child_method.return_type_declaration_metadata.is_none()
        && !parent_method.flags.is_built_in()
    {
        let mut expanded_parent_return_type = parent_return.type_union.clone();

        let expansion_options = TypeExpansionOptions {
            self_class: Some(child_class_name),
            static_class_type: StaticClassType::Name(child_class_name),
            function_is_final: child_method_meta.is_final,
            ..Default::default()
        };

        if expanded_parent_return_type.is_expandable() {
            expand_union(codebase, &mut expanded_parent_return_type, &expansion_options);
        }

        issues.push(SignatureCompatibilityIssue::MissingReturnTypeDeclaration {
            parent_type: expanded_parent_return_type,
        });
        return issues;
    }

    if let (Some(parent_return), Some(child_return)) =
        (&parent_method.return_type_declaration_metadata, &child_method.return_type_declaration_metadata)
    {
        let mut expanded_parent_return_type = parent_return.type_union.clone();
        let mut expanded_child_return_type = child_return.type_union.clone();

        let expansion_options = TypeExpansionOptions {
            self_class: Some(child_class_name),
            static_class_type: StaticClassType::Name(child_class_name),
            function_is_final: child_method_meta.is_final,
            ..Default::default()
        };

        if expanded_parent_return_type.is_expandable() {
            expand_union(codebase, &mut expanded_parent_return_type, &expansion_options);
        }
        if expanded_child_return_type.is_expandable() {
            expand_union(codebase, &mut expanded_child_return_type, &expansion_options);
        }

        let mut comparison_result = ComparisonResult::for_dialect(dialect);
        let is_compatible = union_comparator::is_contained_by(
            codebase,
            &expanded_child_return_type,
            &expanded_parent_return_type,
            false,
            false,
            false,
            &mut comparison_result,
        );

        if !is_compatible {
            issues.push(SignatureCompatibilityIssue::IncompatibleReturnType {
                child_type: expanded_child_return_type,
                parent_type: expanded_parent_return_type,
            });
            return issues;
        }
    }

    if let (Some(parent_return), Some(child_return)) =
        (&parent_method.return_type_metadata, &child_method.return_type_metadata)
        && (child_return.from_docblock || child_method.flags.is_sharp())
    {
        let mut expanded_parent_return_type = parent_return.type_union.clone();
        let mut expanded_child_return_type = child_return.type_union.clone();

        let expansion_options = TypeExpansionOptions {
            self_class: Some(child_class_name),
            static_class_type: StaticClassType::Name(child_class_name),
            function_is_final: child_method_meta.is_final,
            ..Default::default()
        };

        if expanded_parent_return_type.is_expandable() {
            expand_union(codebase, &mut expanded_parent_return_type, &expansion_options);
        }
        if expanded_child_return_type.is_expandable() {
            expand_union(codebase, &mut expanded_child_return_type, &expansion_options);
        }

        if !expanded_parent_return_type.has_template_types() && !expanded_child_return_type.has_template_types() {
            let mut comparison_result = ComparisonResult::for_dialect(dialect);
            let is_compatible = union_comparator::is_return_type_contained_by(
                codebase,
                &expanded_child_return_type,
                &expanded_parent_return_type,
                false,
                &mut comparison_result,
            );

            if !is_compatible {
                issues.push(SignatureCompatibilityIssue::IncompatibleReturnType {
                    child_type: expanded_child_return_type,
                    parent_type: expanded_parent_return_type,
                });
                return issues;
            }
        }
    }

    issues
}

/// `child_method` with each of its own type parameters replaced by the type parameter `parent_method` declares at the
/// same position, as C# matches a generic method's type parameters, so `T pick<T>(T item)` overriding
/// `T pick<T>(T item)` returns the parent's `T`. C# lets no override write its own bound, so the type parameters
/// match only when each pair has the same bound, and only when the override declares as many as the method it
/// overrides, as C# requires. Otherwise returns the issue of a count that differs, or of the first type parameter whose
/// bound differs. An override that declares none keeps its own types.
fn with_parent_type_parameters<'method>(
    codebase: &CodebaseMetadata,
    child_method: &'method FunctionLikeMetadata,
    parent_method: &FunctionLikeMetadata,
) -> Result<Cow<'method, FunctionLikeMetadata>, SignatureCompatibilityIssue> {
    if child_method.template_types.is_empty() {
        return Ok(Cow::Borrowed(child_method));
    }
    if child_method.template_types.len() != parent_method.template_types.len() {
        return Err(SignatureCompatibilityIssue::ChangedTemplateCount {
            child_method: generic_method_name(child_method),
            parent_method: generic_method_name(parent_method),
            parent_count: parent_method.template_types.len(),
        });
    }

    let pairs = || child_method.template_types.iter().zip(&parent_method.template_types);
    let mut template_result = TemplateResult::default();
    for ((child_name, child_template), (parent_name, parent_template)) in pairs() {
        template_result.add_lower_bound(
            *child_name,
            child_template.defining_entity,
            wrap_atomic(TAtomic::GenericParameter(TGenericParameter::new(
                *parent_name,
                Arc::new(parent_template.constraint.clone()),
                parent_template.defining_entity,
            ))),
        );
    }

    let is_same_bound = |child_bound: &TUnion, parent_bound: &TUnion| {
        let child_bound = inferred_type_replacer::replace(child_bound, &template_result, codebase);
        let contains = |input: &TUnion, container: &TUnion| {
            union_comparator::is_contained_by(
                codebase,
                input,
                container,
                false,
                false,
                false,
                &mut ComparisonResult::for_dialect(Dialect::Sharp),
            )
        };

        contains(&child_bound, parent_bound) && contains(parent_bound, &child_bound)
    };
    if let Some(((template, _), (_, parent_template))) = pairs().find(|((_, child_template), (_, parent_template))| {
        !is_same_bound(&child_template.constraint, &parent_template.constraint)
    }) {
        return Err(SignatureCompatibilityIssue::ChangedTemplateBound {
            template: *template,
            child_method: generic_method_name(child_method),
            parent_method: generic_method_name(parent_method),
            bound: (!parent_template.constraint.is_mixed()).then(|| parent_template.constraint.clone()),
        });
    }

    Ok(Cow::Owned(super::apply_template_substitution_to_method(child_method, &template_result, codebase)))
}

/// `method` as a PHP# message names it with its type parameters, as in `pick<T>`, or as `pick` without any.
fn generic_method_name(method: &FunctionLikeMetadata) -> String {
    if method.template_types.is_empty() {
        return method.original_name.to_string();
    }

    let templates: Vec<String> = method.template_types.keys().map(ToString::to_string).collect();

    format!("{}<{}>", method.original_name, templates.join(", "))
}

/// Adds to `issues` the parameter of a PHP# method that no longer links against the method it overrides or implements
/// once generics are erased, as the engine links the class PHP# compiles to: each parameter takes at least the type the
/// parent's parameter erases to. The engine links the erased declarations whenever `child_class` or `parent_class` is
/// PHP#, and it checks them only when `issues`, found on the substituted signatures, hold at most renamed parameters,
/// which hide no erased type. `child_method` and `parent_method` are the declarations as written, before the type
/// arguments of the class's header replace the parent's type parameters. PHP links a constructor against its parent's
/// only when the parent's is abstract.
///
/// A return type needs no check here: the substituted return type is contained by the parent's, and a type argument
/// is contained by its bound, so the erased return type is contained by the parent's erased one.
pub fn validate_erased_signature_compatibility(
    codebase: &CodebaseMetadata,
    child_class: &ClassLikeMetadata,
    parent_class: &ClassLikeMetadata,
    child_method: &FunctionLikeMetadata,
    parent_method: &FunctionLikeMetadata,
    issues: &mut Vec<SignatureCompatibilityIssue>,
) {
    if !(child_class.flags.is_sharp() || parent_class.flags.is_sharp())
        || !issues.iter().all(|issue| matches!(issue, SignatureCompatibilityIssue::ParameterNameMismatch { .. }))
    {
        return;
    }
    if child_method.name.as_bytes().eq_ignore_ascii_case(b"__construct")
        && !parent_method.method_metadata.as_ref().is_some_and(|parent| parent.is_abstract)
    {
        return;
    }

    // A plain PHP parameter's `parent` is the parent of the class that declares it. No parameter is `static`, since
    // PHP refuses one and the checker refuses a PHP# `Self` parameter.
    let erased = |parameter: &FunctionLikeParameterMetadata, class: &ClassLikeMetadata| {
        let mut erased = parameter
            .type_declaration_metadata
            .as_ref()
            .map_or_else(get_mixed, |declared| erase(&declared.type_union, codebase));
        if erased.is_expandable() {
            expand_union(
                codebase,
                &mut erased,
                &TypeExpansionOptions { self_class: Some(class.name), ..Default::default() },
            );
        }

        erased
    };

    issues.extend(parent_method.parameters.iter().zip(&child_method.parameters).enumerate().find_map(
        |(parameter_index, (parent_parameter, child_parameter))| {
            let parent_type = erased(parent_parameter, parent_class);
            let child_type = erased(child_parameter, child_class);
            if union_comparator::is_contained_by(
                codebase,
                &parent_type,
                &child_type,
                false,
                false,
                false,
                &mut ComparisonResult::new(),
            ) {
                return None;
            }

            Some(SignatureCompatibilityIssue::ErasedParameterNarrowed {
                parameter_index,
                bound: bound_example(codebase, parent_parameter.type_declaration_metadata.as_ref(), &child_type),
                child_type,
                parent_type,
            })
        },
    ));
}

/// How a bound reads that makes the class's type parameter `declared`, the parent's written type, erase to `erased`,
/// the child's erased type, such as `Validator<in TItem : Order>`. None when `declared` is no type parameter of a
/// class, or `erased` is not one class.
pub(super) fn bound_example(
    codebase: &CodebaseMetadata,
    declared: Option<&TypeMetadata>,
    erased: &TUnion,
) -> Option<Word> {
    let (parameter, class) = declared?.type_union.types.iter().find_map(|atomic| match atomic {
        TAtomic::GenericParameter(
            parameter @ TGenericParameter { defining_entity: GenericParent::ClassLike(class), .. },
        ) => Some((parameter, class)),
        _ => None,
    })?;
    let [bound] = erased.types.iter().filter(|atomic| !atomic.is_null()).collect::<Vec<_>>()[..] else {
        return None;
    };
    let bound = match bound {
        TAtomic::Object(TObject::Named(object)) => object.name,
        TAtomic::Object(TObject::Enum(object)) => object.name,
        _ => return None,
    };
    let bound = short_name(codebase.get_class_like(bound.as_bytes()).map_or(bound, |class| class.original_name));
    let class = codebase.get_class_like(class.as_bytes())?;
    let name = parameter.parameter_name;
    let marker = match class.get_template_index_for_name(name).and_then(|index| class.template_variance.get(index)) {
        Some(Variance::Covariant) => "out ",
        Some(Variance::Contravariant) => "in ",
        _ => "",
    };

    Some(word(format!("{}<{marker}{name} : {bound}>", short_name(class.original_name))))
}

/// The help of an issue whose `name` erases to another type than `erased_parent_type`, the type its parent's
/// declaration erases to as `display_erased` writes it, naming the `bound` that `bound_example` finds when there is one.
pub(super) fn erased_type_help(name: impl std::fmt::Display, erased_parent_type: &str, bound: Option<Word>) -> String {
    match bound {
        Some(bound) => format!(
            "Write `{name}` with a type that erases to {erased_parent_type}, or bound the type parameter, as in `{bound}`, so both sides erase to the bound."
        ),
        None => format!("Write `{name}` with a type that erases to {erased_parent_type}."),
    }
}

/// The type PHP sees for `r#type`, a PHP# type, once generics are erased, as the bridge's `erase` writes it: a type
/// parameter is its bound, or `mixed` without one, and its bound's classes joined with `&` one intersection; a generic
/// class is its class; `Class<T>` is `string`; a `List` or a `Map` is `array`; a function type, which is a closure
/// signature, is `Closure`, and PHP's `callable` stays `callable`; and `Any` is `mixed`. Members that erase to one type
/// are one type.
pub(super) fn erase(r#type: &TUnion, codebase: &CodebaseMetadata) -> TUnion {
    let erased = r#type
        .types
        .iter()
        .flat_map(|atomic| match atomic {
            TAtomic::GenericParameter(parameter) => erase(&parameter.constraint, codebase).types.into_owned(),
            TAtomic::Mixed(_) => get_mixed().types.into_owned(),
            TAtomic::Array(_) => get_mixed_keyed_array().types.into_owned(),
            TAtomic::Iterable(_) => get_mixed_iterable().types.into_owned(),
            TAtomic::Callable(callable) if callable.is_closure() => get_mixed_closure().types.into_owned(),
            TAtomic::Callable(_) => get_mixed_callable().types.into_owned(),
            TAtomic::Scalar(TScalar::ClassLikeString(_)) => get_string().types.into_owned(),
            TAtomic::Object(TObject::Named(object)) => {
                let mut object = object.clone();
                object.type_parameters = None;

                vec![TAtomic::Object(TObject::Named(object))]
            }
            atomic => vec![atomic.clone()],
        })
        .collect();

    TUnion::from_vec(combiner::combine(erased, codebase, CombinerOptions::default()))
}

/// The type `erased`, which `erase` returns, in backticks, as a `dialect` file writes it. PHP writes it as in a
/// declaration, `array`, `Closure`, `callable`, `mixed`, or a class by its full name. PHP# writes it as
/// `display_sharp_type` does, `Order`, `Any?` or `Iterable<Any?>`, but has no `array`, `Closure` or `callable`, so a
/// PHP# file writes a type with any of them in it as PHP's, as in PHP's `Closure` or PHP's `array|null`. No PHP#
/// declaration erases to an `iterable`, and an `array` beside one joins it, so PHP's form never holds one.
pub(super) fn display_erased<A>(erased: &TUnion, context: &Context<'_, '_, A>) -> String
where
    A: Arena,
{
    let is_php_only = erased.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(_) | TAtomic::Callable(_)));
    if context.dialect.is_sharp() && !is_php_only {
        return format!("`{}`", display_sharp_type(context, erased));
    }

    let members: Vec<String> = erased
        .types
        .iter()
        .map(|atomic| match atomic {
            TAtomic::Array(_) => "array".to_owned(),
            TAtomic::Callable(callable) if callable.is_closure() => "Closure".to_owned(),
            TAtomic::Callable(_) => "callable".to_owned(),
            atomic => atomic.get_id().to_string(),
        })
        .collect();
    let erased = members.join("|");

    if context.dialect.is_sharp() { format!("PHP's `{erased}`") } else { format!("`{erased}`") }
}

const fn is_visibility_narrowed(child_visibility: Visibility, parent_visibility: Visibility) -> bool {
    matches!(
        (parent_visibility, child_visibility),
        (Visibility::Public, Visibility::Protected | Visibility::Private)
            | (Visibility::Protected, Visibility::Private)
    )
}
