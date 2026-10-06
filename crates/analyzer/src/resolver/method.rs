use foldhash::HashSet;
use mago_allocator::Arena;

use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::ttype::TypeMetadata;
use mago_codex::misc::GenericParent;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::generic::TGenericParameter;
use mago_codex::ttype::atomic::mixed::TMixed;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::named::TNamedObject;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator::is_contained_by;
use mago_codex::ttype::expander;
use mago_codex::ttype::expander::StaticClassType;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_specialized_template_type;
use mago_codex::ttype::template::GenericTemplate;
use mago_codex::ttype::template::TemplateResult;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::Binding;
use mago_names::binding::php_variable_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::concat_word;
use mago_word::word;

use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::constant_access::field_storage;
use crate::resolver::class_name::report_non_existent_class_like;
use crate::resolver::property::DeclaredPropertyKind;
use crate::resolver::property::localize_property_type;
use crate::resolver::property::resolve_declared_property;
use crate::resolver::selector::resolve_member_selector;
use crate::utils::expression::analyze_member_object;
use crate::utils::expression::get_bare_name_variable_id;
use crate::utils::expression::is_this;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_method_name;
use crate::utils::names::display_sharp_collection;
use crate::visibility::check_method_visibility;
use crate::visibility::is_method_visible;
use crate::visibility::is_visible_from_scope;

#[derive(Debug, Clone)]
pub struct ResolvedMethod {
    /// The name of the class this method is called on, not necessarily the same
    /// as the class of the method itself, especially in cases of inheritance.
    pub classname: Word,
    /// The method identifiers that were successfully resolved.
    pub method_identifier: MethodIdentifier,
    /// The type of `$this` or the static class type if it's a static method.
    pub static_class_type: StaticClassType,
    /// The object the method was found on when it was reached through a `@mixin`.
    /// `Some` marks the method as mixin-resolved, which lets its `static` return
    /// type rebind to the receiver; template inference reads the mixin class's
    /// template arguments from it.
    pub declaring_object: Option<TNamedObject>,
    /// True if this method is static, meaning it can be called without an instance.
    pub is_static: bool,
    /// If Some, this method was found in a mixin but the target class lacks the magic method
    /// needed to forward the call. Contains the mixin class name and whether the target is final.
    pub mixin_without_magic_method: Option<MixinWithoutMagicMethod>,
}

/// An otherwise undocumented method call that can be handled by a magic method.
///
/// Reporting is deferred until return-type providers have had an opportunity to
/// describe the requested method.
#[derive(Debug, Clone)]
pub struct UndocumentedMethod {
    /// The class on which the undocumented method was requested.
    pub classname: Word,
    /// The requested method name, rather than `__call` or `__callStatic`.
    pub method_name: Word,
    /// The span of the receiver or class expression.
    pub target_span: Span,
    /// The span of the requested method selector.
    pub selector_span: Span,
    /// The magic method which handles the call at runtime.
    pub magic_method: ResolvedMethod,
}

/// An otherwise non-existent method whose diagnostic is deferred until an
/// external callable-signature provider has had an opportunity to establish it.
#[derive(Debug, Clone)]
pub struct UnresolvedMethod {
    /// The class on which the method was requested.
    pub classname: Word,
    /// The requested method name.
    pub method_name: Word,
    /// The span of the receiver or class expression.
    pub target_span: Span,
    /// The span of the requested method selector.
    pub selector_span: Span,
    /// The receiver used by external signature and return-type providers.
    pub class_type: StaticClassType,
}

/// A property whose function a PHP# call runs in place of a missing method.
#[derive(Debug, Clone)]
pub struct CalledProperty {
    /// The class that declares the property.
    pub declaring_class: Word,
    /// The property's name, with its `$`.
    pub property_name: Word,
    /// The property's declared type, which the call is checked against.
    pub property_type: TUnion,
}

/// Represents a method found in a mixin where the calling class lacks the required magic method.
#[derive(Debug, Clone)]
pub struct MixinWithoutMagicMethod {
    /// The name of the mixin class where the method was found.
    pub mixin_class_name: Word,
    /// Whether the target class (that has the mixin) is final.
    pub target_is_final: bool,
}

/// A method found on an object, before being turned into a [`ResolvedMethod`].
pub struct MethodCandidate<'ctx> {
    /// Metadata of the class the method was found on.
    pub metadata: &'ctx ClassLikeMetadata,
    /// The identifier of the declaring method.
    pub method_identifier: MethodIdentifier,
    /// The object type the method was found on (the mixin object for
    /// `@mixin`-resolved methods, otherwise the receiver).
    pub object: TObject,
    /// The name of the class the method was resolved against: the receiver's
    /// class, or the mixin / require-extends class the method was found on.
    pub classname: Word,
    /// See [`ResolvedMethod::mixin_without_magic_method`].
    pub mixin_without_magic_method: Option<MixinWithoutMagicMethod>,
    /// The receiver object when the method was reached through `@mixin`, so
    /// `$this`/`static` bind to the calling object instead of the mixin class.
    pub receiver_object: Option<TObject>,
}

/// Holds the results of resolving a method call, including valid targets and summary flags.
#[derive(Default, Debug)]
pub struct MethodResolutionResult {
    /// The template result containing any type variables and bounds.
    pub template_result: TemplateResult,
    /// A list of resolved methods, each with its template result and identifiers.
    pub resolved_methods: Vec<ResolvedMethod>,
    /// Undocumented calls handled by `__call` or `__callStatic` whose diagnostics
    /// must wait until return-type providers have had an opportunity to resolve them.
    pub undocumented_methods: Vec<UndocumentedMethod>,
    /// Missing methods that may be established by an external callable provider.
    pub unresolved_methods: Vec<UnresolvedMethod>,
    /// The properties a PHP# call runs in place of a missing method, see [`resolve_called_property`].
    pub called_properties: Vec<CalledProperty>,
    /// True if any selector was dynamic (e.g., from a generic string), making the method name unknown.
    pub has_dynamic_selector: bool,
    /// True if any resolution path involved an object with an ambiguous type (e.g., `mixed`, generic `object`).
    pub has_ambiguous_target: bool,
    /// True if any resolution path was definitively invalid (e.g., method not found, call on non-object).
    pub has_invalid_target: bool,
    /// True if an access on a `mixed` type was encountered.
    pub encountered_mixed: bool,
    /// True if an access on a `null` type was encountered.
    pub encountered_null: bool,
}

/// Resolves all possible method targets from an object expression and a member selector.
///
/// This utility handles the logic for `$object->selector` by:
///
/// 1. Analyzing the `$object` expression to find its type.
/// 2. Resolving the `selector` to get potential method names.
/// 3. Finding all matching methods on the object's possible types.
/// 4. Reporting any issues found, such as "method not found" or "call on mixed".
pub fn resolve_method_targets<'ctx, 'ast, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    object: &'ast Expression<'arena>,
    selector: &'ast ClassLikeMemberSelector<'arena>,
    is_null_safe: bool,
    access_span: Span,
) -> Result<MethodResolutionResult, AnalysisError>
where
    A: Arena,
{
    let mut result = MethodResolutionResult::default();
    let mut asserted_descendant_method_references = Vec::new();

    analyze_member_object(context, block_context, artifacts, object, is_null_safe)?;

    let resolved_selectors = resolve_member_selector(context, block_context, artifacts, selector, false)?;
    let mut method_names = Vec::new();

    for resolved_selector in resolved_selectors {
        if resolved_selector.is_dynamic() {
            result.has_dynamic_selector = true;
        }

        if let Some(name) = resolved_selector.name() {
            method_names.push(ascii_lowercase_word(name.as_bytes()));
        } else {
            result.has_invalid_target = true;
        }
    }

    if let Some(object_type) = artifacts.get_expression_type(object) {
        let mut object_atomics = object_type.types.iter().collect::<Vec<_>>();

        while let Some(object_atomic) = object_atomics.pop() {
            if let TAtomic::GenericParameter(TGenericParameter { constraint, .. }) = object_atomic {
                object_atomics.extend(constraint.types.iter());
                continue;
            }

            if object_atomic.is_never() {
                continue;
            }

            if object_atomic.is_null() {
                result.encountered_null = true;
                if !object_type.ignore_nullable_issues() && !is_null_safe && !object_type.has_nullsafe_null() {
                    result.has_invalid_target = true;

                    context.collector.report_with_code(
                        if object_type.is_null() {
                            IssueCode::MethodAccessOnNull
                        } else {
                            IssueCode::PossibleMethodAccessOnNull
                        },
                        Issue::error("Attempting to call a method on `null`.")
                            .with_annotation(
                                Annotation::primary(object.span()).with_message("This expression can be `null`"),
                            )
                            .with_help("Use the nullsafe operator (`?->`) if `null` is an expected value."),
                    );
                }

                continue;
            }

            let closure_object;
            let collection_methods;
            let obj_type = match object_atomic {
                TAtomic::Object(obj_type) => obj_type,
                TAtomic::Callable(callable) if callable.is_closure() => {
                    closure_object = TObject::new_named(word("Closure"));
                    &closure_object
                }
                TAtomic::Array(array) if context.dialect.is_sharp() => {
                    let declared = get_declared_collection(context, block_context, artifacts, object);
                    let collection = match &declared {
                        Some(declared) => declared,
                        // An empty literal with no declared type is neither a List nor a Map, and the semantic checks
                        // refuse its declaration.
                        None if array.is_empty() => continue,
                        None => array,
                    };

                    collection_methods = get_collection_methods(collection, context.codebase);
                    &collection_methods
                }
                _ => {
                    if object_atomic.is_mixed() {
                        result.encountered_mixed = true;
                    } else {
                        result.has_invalid_target = true;
                    }

                    report_call_on_non_object(context, object_atomic, object.span(), selector.span());
                    continue;
                }
            };

            let resolved_magic_call_method = resolve_method_from_object(
                context,
                block_context,
                object.span(),
                selector,
                obj_type,
                word(b"__call"),
                access_span,
                true,
                &mut result,
            );

            for &method_name in &method_names {
                let resolved_methods = resolve_method_from_object(
                    context,
                    block_context,
                    object.span(),
                    selector,
                    obj_type,
                    method_name,
                    access_span,
                    !resolved_magic_call_method.is_empty(),
                    &mut result,
                );

                if resolved_methods.is_empty() {
                    collect_asserted_descendant_method_references(
                        context,
                        block_context,
                        obj_type,
                        method_name,
                        &mut asserted_descendant_method_references,
                    );
                }

                if resolved_methods.is_empty() {
                    if let Some(classname) = obj_type.get_name() {
                        let method_name_bytes: &[u8] = method_name.as_ref();
                        let has_method_assertion = type_has_method_assertion(obj_type, method_name_bytes);

                        if !has_method_assertion {
                            let has_incomplete_hierarchy = context
                                .codebase
                                .get_class_like(classname.as_bytes())
                                .is_some_and(ClassLikeMetadata::has_incomplete_hierarchy);

                            if resolved_magic_call_method.is_empty() {
                                let identifier = FunctionLikeIdentifier::Method(classname, method_name);
                                if context.external_analysis_session.is_some()
                                    && context.plugin_registry.may_have_callable_signature_provider(&identifier)
                                {
                                    result.unresolved_methods.push(UnresolvedMethod {
                                        classname,
                                        method_name,
                                        target_span: object.span(),
                                        selector_span: selector.span(),
                                        class_type: StaticClassType::Object(obj_type.clone()),
                                    });
                                    result.encountered_mixed |= has_incomplete_hierarchy;
                                } else if has_incomplete_hierarchy {
                                    result.encountered_mixed = true;
                                } else if let Some(property) =
                                    resolve_called_property(context, block_context, classname, selector)
                                {
                                    result.called_properties.push(property);
                                } else if let Some(collection) = display_sharp_collection(obj_type) {
                                    report_non_existent_collection_method(
                                        context,
                                        object.span(),
                                        selector.span(),
                                        classname,
                                        &collection,
                                        method_name,
                                    );

                                    result.has_invalid_target = true;
                                } else {
                                    report_non_existent_method(
                                        context,
                                        object.span(),
                                        selector.span(),
                                        classname,
                                        method_name,
                                    );

                                    result.has_invalid_target = true;
                                }
                            } else if !resolve_forwarded_call(
                                context,
                                block_context,
                                object.span(),
                                selector,
                                classname,
                                method_name,
                                obj_type,
                                access_span,
                                &mut result,
                            ) {
                                result.undocumented_methods.extend(resolved_magic_call_method.iter().cloned().map(
                                    |magic_method| UndocumentedMethod {
                                        classname,
                                        method_name,
                                        target_span: object.span(),
                                        selector_span: selector.span(),
                                        magic_method,
                                    },
                                ));
                            }
                        }
                    }
                } else {
                    // Check if any resolved method was found in a mixin without magic method support
                    for resolved_method in &resolved_methods {
                        if let Some(mixin_info) = &resolved_method.mixin_without_magic_method
                            && let Some(classname) = obj_type.get_name()
                        {
                            if mixin_info.target_is_final {
                                // Final class - error, method can never work at runtime
                                report_non_existent_mixin_method(
                                    context,
                                    object.span(),
                                    selector.span(),
                                    classname,
                                    method_name,
                                    mixin_info.mixin_class_name,
                                );
                                result.has_invalid_target = true;
                            } else {
                                // Non-final class - warning, subclass might implement __call
                                report_possibly_non_existent_mixin_method(
                                    context,
                                    object.span(),
                                    selector.span(),
                                    classname,
                                    method_name,
                                    mixin_info.mixin_class_name,
                                );
                            }
                        }
                    }
                }

                result.resolved_methods.extend(resolved_methods);
            }
        }
    } else {
        result.has_invalid_target = true;
        result.encountered_mixed = true;
        report_call_on_non_object(context, &TAtomic::Mixed(TMixed::new()), object.span(), selector.span());
    }

    for method_id in asserted_descendant_method_references {
        artifacts.symbol_references.add_reference_for_method_call(&block_context.scope, &method_id);
    }

    Ok(result)
}

/// Spec section 14 calls a property that holds a function as a method: the PHP# call `x.priceOf(line)` of a class
/// with no method `priceOf` runs the function its property `priceOf` holds, as `($x->priceOf)($line)` does, and
/// reads the property where it is visible.
fn resolve_called_property<A>(
    context: &Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    classname: Word,
    selector: &ClassLikeMemberSelector<'_>,
) -> Option<CalledProperty>
where
    A: Arena,
{
    let ClassLikeMemberSelector::Identifier(name) = selector else {
        return None;
    };
    if !context.dialect.is_sharp() {
        return None;
    }

    let property_name = php_variable_name(name.value);
    let scope = block_context.scope.get_class_like_name();
    let class_metadata = context.codebase.get_class_like(classname.as_bytes())?;
    let resolution = resolve_declared_property(context.codebase, class_metadata, property_name, true, scope)?;
    let declaring_class = resolution.declaring_class.name;
    if !matches!(resolution.kind, DeclaredPropertyKind::Real { .. })
        || resolution.property.flags.is_static()
        || !is_visible_from_scope(
            context.codebase,
            resolution.property.read_visibility,
            declaring_class.as_bytes(),
            scope,
        )
    {
        return None;
    }

    Some(CalledProperty { declaring_class, property_name, property_type: resolution.declared_type(context.codebase) })
}

/// Resolves a magic call through the call an external provider says it forwards to.
///
/// Every forwarded method but the last runs with no arguments on the receiver; the last
/// resolves as a mixin candidate on what they answer, so the call's own arguments are
/// checked once and its generics infer. A forwarded call never asks a forwarding provider
/// again: when its last method is itself magic, the call is left to that object's own
/// magic method. Answers false when no provider forwards the call.
pub(crate) fn resolve_forwarded_call<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    target_span: Span,
    selector: &ClassLikeMemberSelector<'arena>,
    classname: Word,
    method_name: Word,
    receiver: &TObject,
    access_span: Span,
    result: &mut MethodResolutionResult,
) -> bool
where
    A: Arena,
{
    let Some(objects) = forwarded_receivers(context, classname, method_name, receiver) else {
        return false;
    };

    let mut resolved = false;
    for object in objects {
        let Some(name) = object.get_name() else {
            continue;
        };

        let magic_call = resolve_method_from_object(
            context,
            block_context,
            target_span,
            selector,
            &object,
            word(b"__call"),
            access_span,
            true,
            result,
        );
        let methods = resolve_method_from_object(
            context,
            block_context,
            target_span,
            selector,
            &object,
            method_name,
            access_span,
            !magic_call.is_empty(),
            result,
        );

        if !methods.is_empty() {
            result.resolved_methods.extend(methods);
            resolved = true;
        } else if !magic_call.is_empty() {
            result.undocumented_methods.extend(magic_call.into_iter().map(|magic_method| UndocumentedMethod {
                classname: name,
                method_name,
                target_span,
                selector_span: selector.span(),
                magic_method,
            }));
            resolved = true;
        }
    }

    resolved
}

/// The objects a forwarded call's last method runs on.
fn forwarded_receivers<A>(
    context: &Context<'_, '_, A>,
    classname: Word,
    method_name: Word,
    receiver: &TObject,
) -> Option<Vec<TObject>>
where
    A: Arena,
{
    let forwarded = context.plugin_registry.get_forwarded_call(
        context.codebase,
        classname.as_bytes(),
        method_name.as_bytes(),
        false,
        &TUnion::from_atomic(TAtomic::Object(receiver.clone())),
        context.external_analysis_session,
    )?;
    let (_, leading) = forwarded.methods.split_last()?;

    run_forwarded_methods(context, forwarded.receiver, leading)?
        .types
        .iter()
        .map(|atomic| match atomic {
            TAtomic::Object(object) => Some(object.clone()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .filter(|objects| !objects.is_empty())
}

/// What the methods answer when each runs with no arguments on what the one before it
/// answered, or `None` when one of them cannot run on an object.
pub(crate) fn run_forwarded_methods<A>(
    context: &Context<'_, '_, A>,
    receiver: TUnion,
    methods: &[Word],
) -> Option<TUnion>
where
    A: Arena,
{
    let mut current = receiver;
    for &method in methods {
        let mut answered = Vec::new();
        for atomic in current.types.iter() {
            let TAtomic::Object(object) = atomic else {
                return None;
            };

            answered.extend(forwarded_return_type(context, object, method)?.types.iter().cloned());
        }

        current = TUnion::from_vec(answered);
    }

    Some(current)
}

/// What a method answers when it runs with no arguments on an object: its declared
/// return, with `static` bound to the object and the object's template arguments applied.
fn forwarded_return_type<A>(context: &Context<'_, '_, A>, object: &TObject, method: Word) -> Option<TUnion>
where
    A: Arena,
{
    let class_metadata = context.codebase.get_class_like(object.get_name()?.as_bytes())?;
    let method_id =
        context.codebase.get_declaring_method_identifier(&MethodIdentifier::new(class_metadata.original_name, method));
    let function_like = context.codebase.get_method_by_id(&method_id)?;
    let declaring_metadata = context.codebase.get_class_like(method_id.get_class_name().as_bytes())?;
    let mut returned = function_like.return_type_metadata.as_ref()?.type_union.clone();

    expander::expand_union(
        context.codebase,
        &mut returned,
        &TypeExpansionOptions {
            self_class: Some(declaring_metadata.name),
            static_class_type: StaticClassType::Object(object.clone()),
            ..Default::default()
        },
    );

    if !declaring_metadata.template_types.is_empty()
        && let TObject::Named(named_object) = object
    {
        returned = localize_property_type(
            context,
            &returned,
            named_object.get_type_parameters().unwrap_or_default(),
            class_metadata,
            declaring_metadata,
        );
    }

    Some(returned)
}

/// Records references to child methods that can satisfy a `method_exists()` assertion.
fn collect_asserted_descendant_method_references<'ctx, A>(
    context: &Context<'ctx, '_, A>,
    block_context: &BlockContext<'ctx>,
    object_type: &TObject,
    method_name: Word,
    references: &mut Vec<MethodIdentifier>,
) where
    A: Arena,
{
    if !type_has_method_assertion(object_type, method_name.as_bytes()) {
        return;
    }

    let Some(class_name) = object_type.get_name() else {
        return;
    };

    for descendant in context.codebase.get_all_descendants(class_name.as_bytes()) {
        if !context.codebase.method_exists(descendant.as_bytes(), method_name.as_bytes())
            || !is_method_visible(context, block_context, descendant.as_bytes(), method_name.as_bytes())
        {
            continue;
        }

        let method_id =
            context.codebase.get_declaring_method_identifier(&MethodIdentifier::new(descendant, method_name));
        if !references.contains(&method_id) {
            references.push(method_id);
        }
    }
}

pub fn resolve_method_from_object<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    object_span: Span,
    selector: &ClassLikeMemberSelector<'arena>,
    object_type: &TObject,
    method_name: Word,
    access_span: Span,
    has_magic_call: bool,
    result: &mut MethodResolutionResult,
) -> Vec<ResolvedMethod>
where
    A: Arena,
{
    let mut resolved_methods = vec![];

    let candidates = get_method_candidates_from_object(
        context,
        block_context,
        object_span,
        selector,
        object_type,
        object_type,
        method_name,
        access_span,
        has_magic_call,
        result,
    );

    for candidate in candidates {
        let MethodCandidate {
            metadata,
            method_identifier: declaring_method_id,
            object,
            classname,
            mixin_without_magic_method,
            receiver_object,
        } = candidate;
        let declaring_class_metadata =
            context.codebase.get_class_like(declaring_method_id.get_class_name().as_bytes()).unwrap_or(metadata);

        // Collect class-template bounds from the object the method was found on:
        // for `@mixin`-resolved methods that is the mixin object, whose parameters
        // instantiate `metadata`'s templates; the receiver's parameters do not.
        let class_template_parameters = super::class_template_type_collector::collect(
            context.codebase,
            metadata,
            declaring_class_metadata,
            Some(&object),
        );

        if let Some(class_template_parameters) = class_template_parameters {
            result.template_result.add_lower_bounds(class_template_parameters);
        }

        for (index, parameter) in object.get_type_parameters().unwrap_or_default().iter().enumerate() {
            let Some(template_name) = metadata.get_template_name_for_index(index) else {
                continue;
            };

            result
                .template_result
                .template_types
                .entry(template_name)
                .or_default()
                .push(GenericTemplate::new(GenericParent::ClassLike(metadata.name), parameter.clone()));
        }

        let (static_class_type, declaring_object) = match receiver_object {
            Some(receiver) => {
                let declaring_object = match object {
                    TObject::Named(named) => Some(named),
                    // A non-named mixin object (an enum) carries no type parameters;
                    // a bare named object keeps template inference from falling back
                    // to the receiver's parameters.
                    other => other.get_name().map(TNamedObject::new),
                };
                (StaticClassType::Object(receiver), declaring_object)
            }
            None => (StaticClassType::Object(object), None),
        };

        resolved_methods.push(ResolvedMethod {
            method_identifier: declaring_method_id,
            static_class_type,
            declaring_object,
            classname,
            is_static: false,
            mixin_without_magic_method,
        });
    }

    resolved_methods
}

pub fn get_method_candidates_from_object<'ctx, 'arena, 'object, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    object_span: Span,
    selector: &ClassLikeMemberSelector<'arena>,
    object_type: &'object TObject,
    outer_object: &'object TObject,
    method_name: Word,
    access_span: Span,
    has_magic_call: bool,
    result: &mut MethodResolutionResult,
) -> Vec<MethodCandidate<'ctx>>
where
    A: Arena,
{
    let mut candidates = vec![];

    let Some(name) = object_type.get_name() else {
        if std::ptr::eq(object_type, outer_object) {
            result.has_ambiguous_target = true;

            if !has_magic_call {
                let method_name_bytes: &[u8] = method_name.as_ref();
                let has_method_assertion = type_has_method_assertion(object_type, method_name_bytes);
                if !has_method_assertion {
                    report_call_on_ambiguous_object(context, object_span, selector.span());
                }
            }
        }

        return candidates;
    };

    let Some(class_metadata) = context.codebase.get_class_like(name.as_bytes()) else {
        result.has_invalid_target = true;
        report_non_existent_class_like(context, object_span, name);
        return candidates;
    };

    let mut method_id = MethodIdentifier::new(class_metadata.original_name, method_name);
    if !context.codebase.method_identifier_exists(&method_id) {
        method_id = context.codebase.get_declaring_method_identifier(&method_id);
    }

    if let Some(function_like_metadata) = context.codebase.get_method_by_id(&method_id) {
        if !check_method_visibility(
            context,
            block_context.scope.get_class_like_name(),
            class_metadata.original_name.as_bytes(),
            method_name.as_bytes(),
            access_span,
            Some(selector.span()),
        ) {
            result.has_invalid_target = true;
        }

        if !check_where_method_constraints(
            context,
            object_type,
            object_span,
            selector,
            class_metadata,
            function_like_metadata,
            class_metadata.original_name,
        ) {
            result.has_invalid_target = true;
        }

        if function_like_metadata.flags.is_magic_method() {
            let lowercase_method = ascii_lowercase_word(method_name.as_bytes());
            let is_pseudo = class_metadata.pseudo_methods.contains(&lowercase_method)
                || class_metadata.all_parent_classes.iter().any(|parent_name| {
                    context
                        .codebase
                        .get_class_like(parent_name.as_bytes())
                        .is_some_and(|parent| parent.pseudo_methods.contains(&lowercase_method))
                });

            let mut is_inherited = false;

            if is_pseudo {
                for parent_class_name in &class_metadata.all_parent_classes {
                    if let Some(parent_metadata) = context.codebase.get_class_like(parent_class_name.as_bytes())
                        && parent_metadata.methods.contains(&lowercase_method)
                        && !parent_metadata.pseudo_methods.contains(&lowercase_method)
                    {
                        is_inherited = true;
                        break;
                    }
                }
            }

            if function_like_metadata.flags.is_static() {
                result.has_invalid_target = true;

                report_dynamic_static_method_call(
                    context,
                    object_span,
                    selector.span(),
                    class_metadata.original_name,
                    method_name,
                    has_magic_call,
                );
            } else if !has_magic_call && !is_inherited && !class_metadata.kind.is_interface() {
                if class_metadata.flags.is_final() && !class_metadata.flags.is_abstract() {
                    report_magic_call_without_call_method(
                        context,
                        object_span,
                        selector.span(),
                        class_metadata.original_name,
                        method_name,
                        false,
                    );
                } else {
                    report_possibly_missing_magic_call(
                        context,
                        object_span,
                        selector.span(),
                        class_metadata.original_name,
                        method_name,
                        false,
                    );
                }
            }
        }

        candidates.push(MethodCandidate {
            metadata: class_metadata,
            method_identifier: method_id,
            object: outer_object.clone(),
            classname: name,
            mixin_without_magic_method: None,
            receiver_object: None,
        });
    } else if !class_metadata.require_extends.is_empty() || !class_metadata.require_implements.is_empty() {
        for required_class in class_metadata.require_extends.iter().chain(class_metadata.require_implements.iter()) {
            let Some(required_metadata) = context.codebase.get_class_like(required_class.as_bytes()) else {
                continue;
            };

            let mut required_method_id = MethodIdentifier::new(required_metadata.original_name, method_name);
            if !context.codebase.method_identifier_exists(&required_method_id) {
                required_method_id = context.codebase.get_declaring_method_identifier(&required_method_id);
            }

            if context.codebase.get_method_by_id(&required_method_id).is_some() {
                candidates.push(MethodCandidate {
                    metadata: required_metadata,
                    method_identifier: required_method_id,
                    object: outer_object.clone(),
                    classname: *required_class,
                    mixin_without_magic_method: None,
                    receiver_object: None,
                });
                break;
            }
        }
    } else if !class_metadata.mixins.is_empty() {
        // Search mixins for the method. If has_magic_call is false, we track that
        // the method was found in a mixin without the required magic method.
        let mixin_types = collect_mixin_types(context.codebase, class_metadata, outer_object, &class_metadata.mixins);
        // A mixin that declares the method answers the call itself, so the call never
        // reaches the mixins it carries.
        let mut answering = HashSet::default();

        for (mixin_class_name, mixin_object, carrier) in mixin_types {
            if carrier.is_some_and(|carrier| answering.contains(&carrier)) {
                answering.insert(mixin_class_name);
                continue;
            }

            let Some(mixin_metadata) = context.codebase.get_class_like(mixin_class_name.as_bytes()) else {
                continue;
            };

            let mut mixin_method_id = MethodIdentifier::new(mixin_metadata.original_name, method_name);
            if !context.codebase.method_identifier_exists(&mixin_method_id) {
                mixin_method_id = context.codebase.get_declaring_method_identifier(&mixin_method_id);
            }

            if let Some(function_like_metadata) = context.codebase.get_method_by_id(&mixin_method_id) {
                answering.insert(mixin_class_name);

                if !check_method_visibility(
                    context,
                    block_context.scope.get_class_like_name(),
                    mixin_metadata.original_name.as_bytes(),
                    method_name.as_bytes(),
                    access_span,
                    Some(selector.span()),
                ) {
                    result.has_invalid_target = true;
                    continue;
                }

                if let Some(method_metadata) = &function_like_metadata.method_metadata
                    && !method_metadata.visibility.is_public()
                {
                    continue;
                }

                // Track if this method was found without magic call support
                let mixin_info = if has_magic_call {
                    None
                } else {
                    Some(MixinWithoutMagicMethod { mixin_class_name, target_is_final: class_metadata.flags.is_final() })
                };

                // Bind `$this`/`static` in the mixin method's return type to the
                // receiver, not the mixin class: `@mixin` methods behave as if
                // declared on the class carrying the tag.
                candidates.push(MethodCandidate {
                    metadata: mixin_metadata,
                    method_identifier: mixin_method_id,
                    object: mixin_object,
                    classname: mixin_class_name,
                    mixin_without_magic_method: mixin_info,
                    receiver_object: Some(outer_object.clone()),
                });
            }
        }
    }

    if let Some(intersection_types) = object_type.get_intersection_types() {
        for intersected_atomic in intersection_types {
            match intersected_atomic {
                TAtomic::Object(intersected_object) => {
                    // Recursively search in the intersection types
                    candidates.extend(get_method_candidates_from_object(
                        context,
                        block_context,
                        object_span,
                        selector,
                        intersected_object,
                        object_type,
                        method_name,
                        access_span,
                        has_magic_call,
                        result,
                    ));
                }
                TAtomic::GenericParameter(generic_parameter) => {
                    // If the intersection type is a generic parameter, we need to check its constraint
                    for constraint_atomic in generic_parameter.constraint.types.as_ref() {
                        if let TAtomic::Object(intersected_object) = constraint_atomic {
                            // Recursively search in the intersection types
                            candidates.extend(get_method_candidates_from_object(
                                context,
                                block_context,
                                object_span,
                                selector,
                                intersected_object,
                                object_type,
                                method_name,
                                access_span,
                                has_magic_call,
                                result,
                            ));
                        }
                    }
                }
                _ => {
                    // For other atomic types, we do not need to do anything special
                }
            }
        }
    }

    let mut seen = HashSet::default();
    candidates.retain(|candidate| seen.insert(candidate.method_identifier));

    candidates
}

fn check_where_method_constraints<A>(
    context: &mut Context<'_, '_, A>,
    object_type: &TObject,
    object_span: Span,
    selector: &ClassLikeMemberSelector,
    class_like_metadata: &ClassLikeMetadata,
    function_like_metadata: &FunctionLikeMetadata,
    defining_class_id: Word,
) -> bool
where
    A: Arena,
{
    let Some(method_metadata) = function_like_metadata.method_metadata.as_ref() else {
        return true;
    };

    if method_metadata.where_constraints.is_empty() {
        return true;
    }

    for (&template_name, constraint) in &method_metadata.where_constraints {
        let actual_template_type = get_specialized_template_type(
            context.codebase,
            template_name,
            defining_class_id,
            class_like_metadata,
            object_type.get_type_parameters(),
        )
        .unwrap_or_else(get_mixed);

        if is_contained_by(
            context.codebase,
            &actual_template_type,
            &constraint.type_union,
            false,
            false,
            false,
            &mut ComparisonResult::default(),
        ) {
            continue;
        }

        let required_constraint_str = constraint.type_union.get_id();
        let actual_template_type_str = actual_template_type.get_id();

        context.collector.report_with_code(
            IssueCode::WhereConstraintViolation,
            Issue::error(format!(
                "Method call violates `@where` constraint for template `{template_name}`.",
            ))
            .with_annotation(
                Annotation::primary(selector.span())
                    .with_message("This method cannot be called here..."),
            )
            .with_annotation(
                Annotation::secondary(object_span)
                    .with_message(format!(
                        "...because this object's template parameter `{template_name}` is type `{actual_template_type_str}`...",
                    )),
            )
            .with_annotation(
                Annotation::secondary(constraint.span)
                    .with_message(format!(
                        "...but this `@where` clause requires it to be `{required_constraint_str}`.",
                    )),
            )
            .with_note(
                "The `@where` tag on a method adds a constraint that must be satisfied by the object's generic types at the time of the call."
            )
            .with_help(
                format!("Ensure the object's template parameter `{template_name}` satisfies the `{required_constraint_str}` constraint before calling this method.")
            ),
        );

        return false;
    }

    true
}

/// The collection type the place a PHP# collection method is called on is declared with: a typed local, a parameter,
/// a property, or `field`, the storage of the property whose accessor is running. The method takes values of that
/// type, as `$list[] = $x` is checked against the declared property, so a value the analyzer saw assigned last, such
/// as `[]` or a list of one implementation, narrows nothing.
fn get_declared_collection<'arena, A>(
    context: &Context<'_, 'arena, A>,
    block_context: &BlockContext<'_>,
    artifacts: &AnalysisArtifacts,
    object: &Expression<'arena>,
) -> Option<TArray>
where
    A: Arena,
{
    let object = match object.unparenthesized() {
        Expression::ConstantAccess(access) if context.resolved_names.binding(&access.name) == Some(Binding::Field) => {
            field_storage(access, context, block_context)?
        }
        object => object,
    };

    let declared = match object {
        Expression::ConstantAccess(access) => {
            let variable_id = get_bare_name_variable_id(&access.name, context.resolved_names)?;

            match block_context.local_types.get(&variable_id) {
                Some((local_type, _)) => local_type.as_ref().clone(),
                None => block_context
                    .scope
                    .get_function_like()?
                    .parameters
                    .iter()
                    .find(|parameter| parameter.get_name().0 == variable_id)?
                    .get_type_metadata()?
                    .type_union
                    .clone(),
            }
        }
        Expression::Access(Access::Property(access)) => {
            let ClassLikeMemberSelector::Identifier(property) = &access.property else {
                return None;
            };
            let property_name = concat_word!(b"$", property.value);
            let property_type =
                |class: Word| context.codebase.get_property_type(class.as_bytes(), property_name.as_bytes());

            // `this` keeps no expression type: its class is the scope's.
            if is_this(access.object, context.resolved_names) {
                property_type(block_context.scope.get_class_like_name()?)?.clone()
            } else {
                artifacts.get_expression_type(access.object)?.types.iter().find_map(|atomic| match atomic {
                    TAtomic::Object(object) => property_type(object.get_name()?).cloned(),
                    _ => None,
                })?
            }
        }
        _ => return None,
    };

    declared.types.iter().find_map(|atomic| match atomic {
        TAtomic::Array(array) => Some(array.clone()),
        _ => None,
    })
}

/// The class whose methods a PHP# collection has, as spec section 12 writes them: a `List<T>` is called as
/// `Sharp\ListMethods<T>` and a `Map<K, V>` as `Sharp\MapMethods<K, V>`, so each method is typed by the elements.
/// A PHP# element type is never narrower than `int` or `string`, so a literal the analyzer knows widens to it.
fn get_collection_methods(array: &TArray, codebase: &CodebaseMetadata) -> TObject {
    let (mut key, mut value) = get_array_parameters(array, codebase);
    key.widen_scalars();
    value.widen_scalars();

    TObject::Named(match array {
        TArray::List(_) => TNamedObject::new_with_type_parameters(word("Sharp\\ListMethods"), Some(vec![value])),
        TArray::Keyed(_) => TNamedObject::new_with_type_parameters(word("Sharp\\MapMethods"), Some(vec![key, value])),
    })
}

fn report_call_on_non_object<A>(
    context: &mut Context<'_, '_, A>,
    atomic_type: &TAtomic,
    obj_span: Span,
    selector_span: Span,
) where
    A: Arena,
{
    let type_str = atomic_type.get_id();

    context.collector.report_with_code(
        if atomic_type.is_mixed() { IssueCode::MixedMethodAccess } else { IssueCode::InvalidMethodAccess },
        Issue::error(format!("Attempting to access a method on a non-object type (`{type_str}`)."))
            .with_annotation(Annotation::primary(selector_span).with_message("Cannot call method here"))
            .with_annotation(
                Annotation::secondary(obj_span).with_message(format!("This expression has type `{type_str}`")),
            ),
    );
}

fn report_call_on_ambiguous_object<A>(context: &mut Context<'_, '_, A>, obj_span: Span, selector_span: Span)
where
    A: Arena,
{
    context.collector.report_with_code(
        IssueCode::AmbiguousObjectMethodAccess,
        Issue::warning("Cannot statically verify method call on a generic `object` type.")
            .with_annotation(Annotation::primary(selector_span).with_message("Cannot verify this method call"))
            .with_annotation(
                Annotation::secondary(obj_span).with_message("This expression has the general type `object`"),
            )
            .with_help("Provide a more specific type hint for the object for robust analysis."),
    );
}

pub(crate) fn report_non_existent_method<A>(
    context: &mut Context<'_, '_, A>,
    obj_span: Span,
    selector_span: Span,
    classname: Word,
    method_name: Word,
) where
    A: Arena,
{
    let classname = display_class_like_name(context, classname);
    let method_name = display_method_name(context, classname, method_name);
    context.collector.report_with_code(
        IssueCode::NonExistentMethod,
        Issue::error(format!("Method `{method_name}` does not exist on type `{classname}`."))
            .with_annotation(Annotation::primary(selector_span).with_message("This method selection is invalid"))
            .with_annotation(
                Annotation::secondary(obj_span).with_message(format!("This expression has type `{classname}`")),
            )
            .with_help(format!("Ensure the `{method_name}` method is defined in the `{classname}` class-like.")),
    );
}

/// Reports a method a PHP# `List` or `Map` does not have, naming the collection type the code wrote and the methods
/// `classname`, its `Sharp\ListMethods` or `Sharp\MapMethods`, gives it.
fn report_non_existent_collection_method<A>(
    context: &mut Context<'_, '_, A>,
    obj_span: Span,
    selector_span: Span,
    classname: Word,
    collection: &str,
    method_name: Word,
) where
    A: Arena,
{
    let mut methods = context
        .codebase
        .get_class_like(classname.as_bytes())
        .map(|metadata| metadata.methods.iter().map(|method| format!("`{method}`")).collect::<Vec<_>>())
        .unwrap_or_default();
    methods.sort();

    context.collector.report_with_code(
        IssueCode::NonExistentMethod,
        Issue::error(format!("Method `{method_name}` does not exist on `{collection}`."))
            .with_annotation(Annotation::primary(selector_span).with_message("This method selection is invalid"))
            .with_annotation(
                Annotation::secondary(obj_span).with_message(format!("This expression has type `{collection}`")),
            )
            .with_help(format!("A `{collection}` has the methods {}.", methods.join(", "))),
    );
}

pub(crate) fn report_non_documented_method<A>(
    context: &mut Context<'_, '_, A>,
    obj_span: Span,
    selector_span: Span,
    classname: Word,
    method_name: Word,
) where
    A: Arena,
{
    let classname = display_class_like_name(context, classname);
    let method_name = display_method_name(context, classname, method_name);
    context.collector.report_with_code(
        IssueCode::NonDocumentedMethod,
        Issue::warning(format!(
            "Ambiguous method call to `{method_name}` on class `{classname}`."
        ))
        .with_annotation(
            Annotation::primary(selector_span).with_message("This method is not explicitly defined"),
        )
        .with_annotation(
            Annotation::secondary(obj_span).with_message(format!("On an object of type `{classname}`")),
        )
        .with_note(
            "While this call might be handled by `__call()` or `__callStatic()`, Mago cannot verify its arguments or return type without a corresponding `@method` docblock tag.",
        )
        .with_help(format!(
            "To enable full analysis, add a `@method` tag to the docblock of the `{classname}` class. For example: `/** @method returnType {method_name}(argType $argName) */`"
        )),
    );
}

/// Reports a warning when a method is found in a mixin but the target class lacks __call.
/// This is a warning because a subclass might implement __call.
fn report_possibly_non_existent_mixin_method<A>(
    context: &mut Context<'_, '_, A>,
    obj_span: Span,
    selector_span: Span,
    classname: Word,
    method_name: Word,
    mixin_classname: Word,
) where
    A: Arena,
{
    let mixin_classname = display_class_like_name(context, mixin_classname);
    let method_name = display_method_name(context, classname, method_name);
    let classname = display_class_like_name(context, classname);
    context.collector.report_with_code(
        IssueCode::PossiblyNonExistentMethod,
        Issue::warning(format!(
            "Method `{method_name}` might not exist on type `{classname}` at runtime."
        ))
        .with_annotation(
            Annotation::primary(selector_span).with_message("Method might not exist"),
        )
        .with_annotation(
            Annotation::secondary(obj_span).with_message(format!("On an instance of `{classname}`")),
        )
        .with_note(format!(
            "The method `{method_name}` is defined in mixin class `{mixin_classname}`, but `{classname}` does not have a `__call` method to forward the call."
        ))
        .with_note(
            "A subclass of this class could implement `__call` to handle this, so the call might succeed at runtime."
        )
        .with_help(format!(
            "Add a `__call` method to `{classname}`, or make `{classname}` final if this should be an error."
        )),
    );
}

/// Reports an error when a method is found in a mixin but the target final class lacks __call.
/// This is an error because no subclass can exist to implement __call.
fn report_non_existent_mixin_method<A>(
    context: &mut Context<'_, '_, A>,
    obj_span: Span,
    selector_span: Span,
    classname: Word,
    method_name: Word,
    mixin_classname: Word,
) where
    A: Arena,
{
    let mixin_classname = display_class_like_name(context, mixin_classname);
    let method_name = display_method_name(context, classname, method_name);
    let classname = display_class_like_name(context, classname);
    context.collector.report_with_code(
        IssueCode::NonExistentMethod,
        Issue::error(format!(
            "Method `{method_name}` does not exist on final type `{classname}`."
        ))
        .with_annotation(
            Annotation::primary(selector_span).with_message("Method does not exist"),
        )
        .with_annotation(
            Annotation::secondary(obj_span).with_message(format!("On an instance of final class `{classname}`")),
        )
        .with_note(format!(
            "The method `{method_name}` is defined in mixin class `{mixin_classname}`, but `{classname}` is final and does not have a `__call` method to forward the call."
        ))
        .with_help(format!(
            "Add a `__call` method to `{classname}` to handle mixin method calls."
        )),
    );
}

pub(super) fn report_possibly_missing_magic_call<A>(
    context: &mut Context<'_, '_, A>,
    obj_span: Span,
    selector_span: Span,
    classname: Word,
    method_name: Word,
    is_static: bool,
) where
    A: Arena,
{
    let magic_method_name = if is_static { "__callStatic" } else { "__call" };
    let classname = display_class_like_name(context, classname);
    let method_name = display_method_name(context, classname, method_name);

    context.collector.report_with_code(
        IssueCode::PossiblyNonExistentMethod,
        Issue::warning(format!(
            "Call to documented magic method `{method_name}()` on a class that may not handle it."
        ))
        .with_annotation(
            Annotation::primary(selector_span).with_message("This magic method is documented but may not be callable"),
        )
        .with_annotation(
            Annotation::secondary(obj_span)
                .with_message(format!("Class `{classname}` is missing the `{magic_method_name}` method")),
        )
        .with_note(format!(
            "The class `{classname}` has a `@method` tag for `{method_name}` but does not have a `{magic_method_name}` method to handle the call. A subclass could provide `{magic_method_name}` at runtime, so this is only a warning; if `{classname}` were final, this would be a hard error."
        ))
        .with_help(format!(
            "Add a `{magic_method_name}` method to `{classname}`, or make `{classname}` final if calls to `{method_name}` should be rejected outright."
        )),
    );
}

pub(super) fn report_magic_call_without_call_method<A>(
    context: &mut Context<'_, '_, A>,
    obj_span: Span,
    selector_span: Span,
    classname: Word,
    method_name: Word,
    is_static: bool,
) where
    A: Arena,
{
    let magic_method_name = if is_static { "__callStatic" } else { "__call" };
    let classname = display_class_like_name(context, classname);
    let method_name = display_method_name(context, classname, method_name);

    context.collector.report_with_code(
        IssueCode::MissingMagicMethod,
        Issue::error(format!(
            "Call to documented magic method `{method_name}()` on a class that cannot handle it."
        ))
        .with_annotation(
            Annotation::primary(selector_span)
                .with_message("This magic method is documented but cannot be called"),
        )
        .with_annotation(
            Annotation::secondary(obj_span).with_message(format!("Class `{classname}` is missing the `{magic_method_name}` method")),
        )
        .with_note(
            format!("The class `{classname}` has a `@method` tag for `{method_name}` but does not have a `{magic_method_name}` method to handle the call. This will cause a fatal `Error` at runtime.")
        )
        .with_help(
            format!("Add a `{magic_method_name}` method to the `{classname}` class to handle calls to magic methods.")
        ),
    );
}

pub(super) fn report_dynamic_static_method_call<A>(
    context: &mut Context<'_, '_, A>,
    obj_span: Span,
    selector_span: Span,
    classname: Word,
    method_name: Word,
    has_magic_call: bool,
) where
    A: Arena,
{
    let classname = display_class_like_name(context, classname);
    let method_name = display_method_name(context, classname, method_name);
    let mut issue =
        Issue::error(format!("Cannot call magic static method `{classname}::{method_name}` on an instance."))
            .with_annotation(
                Annotation::primary(selector_span)
                    .with_message("This magic method is static and must be called statically"),
            )
            .with_annotation(
                Annotation::secondary(obj_span).with_message(format!("Called on an instance of `{classname}`")),
            );

    if has_magic_call {
        issue = issue
            .with_note(format!(
                "The magic method `{method_name}` is documented as `static` and is intended to be handled by `__callStatic()`."
            ))
            .with_note(
                "However, because it's being called on an instance (`->`), the call will be routed to the existing `__call()` method instead."
            )
            .with_note(
                "This is likely not the intended behavior and may lead to unexpected errors."
            );
    } else {
        issue = issue
            .with_note(
                "Magic methods defined with `@method static` are handled by `__callStatic()`."
            )
            .with_note(
                "When called on an instance (`->`), PHP attempts to route the call to a `__call()` method."
            )
            .with_note(format!(
                "Since the class `{classname}` is missing a `__call()` method, this will cause a fatal `Error` at runtime."
            ));
    }

    context.collector.report_with_code(
        IssueCode::DynamicStaticMethodCall,
        issue.with_help(format!("Call this method statically instead: `{classname}::{method_name}`.")),
    );
}

/// Checks if a type has a known method assertion for the given method name.
///
/// This checks for `HasMethod` types or intersection types containing them.
/// Comparison is case-insensitive since PHP method names are case-insensitive.
fn type_has_method_assertion(object_type: &TObject, method_name: &[u8]) -> bool {
    match object_type {
        TObject::HasMethod(has_method) => {
            if has_method.has_method(method_name) {
                return true;
            }

            has_method.intersection_types.as_ref().is_some_and(|types| {
                types.iter().any(|atomic| {
                    if let TAtomic::Object(obj) = atomic { type_has_method_assertion(obj, method_name) } else { false }
                })
            })
        }
        TObject::HasProperty(has_property) => has_property.intersection_types.as_ref().is_some_and(|types| {
            types.iter().any(|atomic| {
                if let TAtomic::Object(obj) = atomic { type_has_method_assertion(obj, method_name) } else { false }
            })
        }),
        TObject::Named(named_object) => named_object.get_intersection_types().is_some_and(|intersection_types| {
            intersection_types.iter().any(|atomic| {
                if let TAtomic::Object(obj) = atomic { type_has_method_assertion(obj, method_name) } else { false }
            })
        }),
        _ => false,
    }
}

fn collect_mixin_types(
    codebase: &CodebaseMetadata,
    class_metadata: &ClassLikeMetadata,
    outer_object: &TObject,
    mixins: &[TypeMetadata],
) -> Vec<(Word, TObject, Option<Word>)> {
    let mut results = Vec::new();
    let mut visited = HashSet::default();
    collect_mixin_types_into(codebase, class_metadata, outer_object, mixins, None, &mut results, &mut visited);

    results
}

/// Collects mixins depth first, each with the mixin that declared it, so a nested
/// mixin always follows its carrier.
fn collect_mixin_types_into(
    codebase: &CodebaseMetadata,
    class_metadata: &ClassLikeMetadata,
    outer_object: &TObject,
    mixins: &[TypeMetadata],
    carrier: Option<Word>,
    results: &mut Vec<(Word, TObject, Option<Word>)>,
    visited: &mut HashSet<Word>,
) {
    let mut direct: Vec<(Word, &TObject)> = Vec::new();

    for mixin_type in mixins {
        for mixin_atomic in mixin_type.type_union.types.as_ref() {
            match mixin_atomic {
                TAtomic::Object(obj @ TObject::Named(named)) => {
                    direct.push((ascii_lowercase_word(named.name.as_bytes()), obj));
                }
                TAtomic::Object(obj @ TObject::Enum(enum_type)) => {
                    direct.push((ascii_lowercase_word(enum_type.name.as_bytes()), obj));
                }
                TAtomic::GenericParameter(TGenericParameter {
                    parameter_name, constraint, defining_entity, ..
                }) => {
                    let mut resolved = false;

                    if let TObject::Named(named_object) = outer_object
                        && let Some(type_params) = named_object.get_type_parameters()
                        && let GenericParent::ClassLike(defining_class) = defining_entity
                        && named_object.name.as_bytes().eq_ignore_ascii_case(defining_class.as_bytes())
                        && let Some(index) = class_metadata.get_template_index_for_name(*parameter_name)
                        && let Some(concrete_type) = type_params.get(index)
                    {
                        for atomic in concrete_type.types.as_ref() {
                            match atomic {
                                TAtomic::Object(obj @ TObject::Named(named)) => {
                                    direct.push((ascii_lowercase_word(named.name.as_bytes()), obj));
                                    resolved = true;
                                }
                                TAtomic::Object(obj @ TObject::Enum(enum_type)) => {
                                    direct.push((ascii_lowercase_word(enum_type.name.as_bytes()), obj));
                                    resolved = true;
                                }
                                _ => {}
                            }
                        }
                    }

                    if !resolved {
                        for constraint_atomic in constraint.types.as_ref() {
                            match constraint_atomic {
                                TAtomic::Object(obj @ TObject::Named(named)) => {
                                    direct.push((ascii_lowercase_word(named.name.as_bytes()), obj));
                                }
                                TAtomic::Object(obj @ TObject::Enum(enum_type)) => {
                                    direct.push((ascii_lowercase_word(enum_type.name.as_bytes()), obj));
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    for (name, obj) in direct {
        if !visited.insert(name) {
            continue;
        }

        let specialized_obj = specialize_mixin_object(codebase, class_metadata, outer_object, obj);
        let obj = specialized_obj.as_ref().unwrap_or(obj);

        results.push((name, obj.clone(), carrier));

        if let Some(mixin_metadata) = codebase.get_class_like(name.as_bytes())
            && !mixin_metadata.mixins.is_empty()
        {
            collect_mixin_types_into(
                codebase,
                mixin_metadata,
                obj,
                &mixin_metadata.mixins,
                Some(name),
                results,
                visited,
            );
        }
    }
}

/// Substitutes the carrier class's template parameters inside a mixin tag's type
/// arguments (`@mixin Builder<TItem>`) with the receiver's actual type arguments.
fn specialize_mixin_object(
    codebase: &CodebaseMetadata,
    carrier_metadata: &ClassLikeMetadata,
    carrier_object: &TObject,
    mixin_object: &TObject,
) -> Option<TObject> {
    let TObject::Named(named) = mixin_object else {
        return None;
    };

    let parameters = named.type_parameters.as_deref()?;

    if !parameters.iter().any(|parameter| parameter.has_template_types()) {
        return None;
    }

    let TObject::Named(carrier) = carrier_object else {
        return None;
    };

    let carrier_parameters = carrier.type_parameters.as_ref()?;
    let mut named = named.clone();
    named.type_parameters = Some(
        parameters
            .iter()
            .map(|parameter| {
                super::class_template_type_collector::resolve_template_parameter(
                    codebase,
                    parameter,
                    carrier_metadata,
                    carrier_parameters,
                )
                .unwrap_or_else(|| parameter.clone())
            })
            .collect(),
    );

    Some(TObject::Named(named))
}
