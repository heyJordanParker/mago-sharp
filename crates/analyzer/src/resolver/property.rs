use std::sync::Arc;

use foldhash::HashMap;
use indexmap::IndexMap;
use mago_allocator::Arena;
use mago_bytes::trim_start_byte;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::flags::MetadataFlags;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::property::PropertyMetadata;
use mago_codex::metadata::ttype::TypeMetadata;
use mago_codex::misc::GenericParent;
use mago_codex::ttype::TType;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::generic::TGenericParameter;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::r#enum::TEnum;
use mago_codex::ttype::atomic::object::named::TNamedObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::expander;
use mago_codex::ttype::expander::StaticClassType;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::expander::get_signature_of_function_like_metadata;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_codex::ttype::get_php_passed_type;
use mago_codex::ttype::get_php_received_type;
use mago_codex::ttype::get_sharp_received_type;
use mago_codex::ttype::template::TemplateBound;
use mago_codex::ttype::template::TemplateResult;
use mago_codex::ttype::template::bounds::get_most_specific_type_from_bounds;
use mago_codex::ttype::template::inferred_type_replacer;
use mago_codex::ttype::union::TUnion;
use mago_codex::ttype::wrap_atomic;
use mago_names::ResolvedNames;
use mago_names::display_sharp_member;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Variable;
use mago_text_edit::TextEdit;
use mago_word::Word;
use mago_word::WordSet;
use mago_word::ascii_lowercase_word;
use mago_word::concat_word;
use mago_word::word;

use crate::artifacts::AnalysisArtifacts;
use crate::artifacts::CallTarget;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::is_refused_type;
use crate::expression::with::CREATED_OBJECT_PREFIX;
use crate::expression::with::is_set_by_with;
use crate::external::PropertyAccessKind;
use crate::invocation::infer_parameter_templates_from_argument;
use crate::invocation::infer_unfixed_templates;
use crate::invocation::rename_trait_objects;
use crate::resolver::class_name::class_value_classes;
use crate::resolver::class_name::holds_no_class;
use crate::resolver::class_name::report_non_existent_class_like;
use crate::resolver::extension::Extension;
use crate::resolver::extension::resolve_extension;
use crate::resolver::method::check_call_through_readonly;
use crate::resolver::method::get_receiver_collections;
use crate::resolver::method::object_member_issue;
use crate::resolver::method::run_forwarded_methods;
use crate::resolver::selector::resolve_member_selector;
use crate::statement::function_like::closure_parameter_types;
use crate::statement::function_like::get_this_type;
use crate::statement::function_like::known_type_or_error;
use crate::utils::expression::analyze_member_object;
use crate::utils::expression::expression_is_nullsafe;
use crate::utils::expression::is_this;
use crate::utils::names::display_atomic;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_enum_case;
use crate::utils::names::display_member;
use crate::utils::names::display_property_name;
use crate::utils::names::display_type;
use crate::utils::names::readonly_writer;
use crate::utils::quoted_source;
use crate::utils::template::check_private_method_reach;
use crate::utils::template::check_private_property_reach;
use crate::utils::template::class_type_arguments;
use crate::utils::template::get_template_types_for_class_member;
use crate::visibility::check_method_visibility;
use crate::visibility::check_resolved_property_read_visibility;
use crate::visibility::check_resolved_property_write_visibility;
use crate::visibility::effective_write_visibility;
use crate::visibility::is_visible_from_scope;

/// Represents a successfully resolved instance property.
#[derive(Debug)]
pub struct ResolvedProperty {
    pub property_name: Word,
    pub declaring_class_id: Option<Word>,
    pub property_span: Option<Span>,
    pub property_type: TUnion,
    pub is_magic: bool,
    /// When resolved for assignment and writes diverge from reads, the type a read produces
    /// regardless of the written value: the `@property-read` type of a magic property (reads go
    /// through `__get`) or the declared type of a hook property with a `set` parameter (reads go
    /// through `get`). `property_type` here is the (possibly wider) write type — the `@property-write`
    /// type or the `set` parameter type — so a written value that is not assignable to `read_type`
    /// does not survive a read; memoization clamps to `read_type` in that case. `None` for reads
    /// and for real/dynamic properties, whose writes round-trip (`property_type` is the read type).
    pub read_type: Option<TUnion>,
}

/// Holds the results of a property resolution attempt.
#[derive(Debug, Default)]
pub struct PropertyResolutionResult {
    pub properties: Vec<ResolvedProperty>,
    pub has_ambiguous_path: bool,
    pub has_error_path: bool,
    pub has_invalid_path: bool,
    pub encountered_null: bool,
    pub encountered_mixed: bool,
    pub has_possibly_defined_property: bool,
    /// True when a later read of a written property sees nothing the write gave it: an extension property that cannot
    /// be read, or, in PHP#, a property `__get` serves, spec section 14.3.
    pub has_unreadable_property: bool,
    /// True if all resolved properties are non-nullable.
    /// When combined with `encountered_null` and nullsafe access, indicates
    /// the null in the result type came ONLY from nullsafe short-circuit.
    pub all_properties_non_nullable: bool,
    /// The static method of the extension's computed property a PHP# read runs, spec section 26, which leaves the
    /// other properties empty.
    pub extension: Option<MethodIdentifier>,
    /// True when the object is a PHP# class value and the member is no member of `Class`, so the access reads or
    /// writes a static member of the classes the value holds, spec section 25, which leaves the properties empty.
    pub class_value: bool,
}

/// Resolves all possible instance properties from an object expression and a member selector.
pub fn resolve_instance_properties<'ctx, 'ast, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    object_expression: &'ast Expression<'arena>,
    property_selector: &'ast ClassLikeMemberSelector<'arena>,
    operator_span: Span,
    is_null_safe: bool,
    for_assignment: bool,
) -> Result<PropertyResolutionResult, AnalysisError>
where
    A: Arena,
{
    let mut result = PropertyResolutionResult::default();

    analyze_member_object(context, block_context, artifacts, object_expression, is_null_safe)?;

    let selectors = resolve_member_selector(context, block_context, artifacts, property_selector, true)?;

    let Some(object_type) = artifacts.get_rc_expression_type(object_expression).cloned() else {
        return Ok(result);
    };

    let is_all_null = object_type.is_null() || object_type.is_void();

    if is_null_safe && !for_assignment {
        check_redundant_nullsafe(context, operator_span, object_expression, &object_type, false);
    }

    if context.dialect.is_sharp()
        && let ClassLikeMemberSelector::Identifier(name) = property_selector
        && let Some(extension) = resolve_extension(context, &object_type, name, MetadataFlags::EXTENSION_PROPERTY)
    {
        match extension {
            Extension::Member(_) if for_assignment => {
                result.has_error_path = true;
                context.collector.report_with_code(
                    IssueCode::InvalidPropertyWrite,
                    Issue::error(format!(
                        "`{}` is a computed property an extension adds: it is read, never written.",
                        String::from_utf8_lossy(name.value)
                    ))
                    .with_annotation(Annotation::primary(name.span()).with_message("Written here.")),
                );
            }
            Extension::Member(method) => result.extension = Some(method),
            Extension::Value(_) if for_assignment => {
                result.has_error_path = true;
                context.collector.report_with_code(
                    IssueCode::InvalidPropertyWrite,
                    Issue::error(format!(
                        "`{}` is a method an extension adds: it is called or read as a function value, never written.",
                        String::from_utf8_lossy(name.value)
                    ))
                    .with_annotation(Annotation::primary(name.span()).with_message("Written here.")),
                );
            }
            Extension::Value(method) => {
                let read = object_expression.span().join(name.span());
                let property_type =
                    resolve_extension_value(context, artifacts, method, &object_type.to_non_nullable(), read);
                artifacts.record_call_target(
                    read,
                    CallTarget::FunctionLike {
                        callee: FunctionLikeIdentifier::Method(method.get_class_name(), method.get_method_name()),
                        class: None,
                    },
                );
                result.encountered_null = is_null_safe && object_type.is_nullable();
                result.properties.push(ResolvedProperty {
                    property_span: None,
                    property_name: concat_word!("$", name.value),
                    declaring_class_id: None,
                    property_type,
                    is_magic: false,
                    read_type: None,
                });
            }
            Extension::Refused => result.has_error_path = true,
        }
        if object_type.is_nullable() && !is_null_safe && !object_type.has_nullsafe_null() {
            report_access_on_null(context, block_context, object_expression.span(), operator_span, false, false);
        }

        return Ok(result);
    }

    if context.dialect.is_sharp()
        && !for_assignment
        && let ClassLikeMemberSelector::Identifier(name) = property_selector
        && let Some(collections) = get_receiver_collections(context, block_context, artifacts, object_expression)
        && let Some(property_type) = resolve_collection_method_value(
            context,
            block_context,
            artifacts,
            &collections,
            object_expression,
            name,
            operator_span,
        )
    {
        result.encountered_null = object_type.is_nullable();
        if object_type.is_nullable() && !is_null_safe && !object_type.has_nullsafe_null() {
            report_access_on_null(context, block_context, object_expression.span(), operator_span, false, false);
        }
        result.properties.push(ResolvedProperty {
            property_span: None,
            property_name: concat_word!("$", name.value),
            declaring_class_id: None,
            property_type,
            is_magic: false,
            read_type: None,
        });

        return Ok(result);
    }

    if context.dialect.is_sharp()
        && let ClassLikeMemberSelector::Identifier(_) = property_selector
        && (class_value_classes(&object_type).is_some() || holds_no_class(&object_type))
    {
        result.class_value = true;

        return Ok(result);
    }

    let mut property_names = Vec::new();
    for selector in selectors {
        if selector.is_dynamic() {
            result.has_ambiguous_path = true;
        }

        if let Some(name) = selector.name() {
            property_names.push(concat_word!("$", &name));
        } else {
            result.has_invalid_path = true;
        }
    }

    let mut object_atomics = object_type.types.iter().map(|atomic| (atomic, atomic)).collect::<Vec<_>>();
    while let Some((object_atomic, written_atomic)) = object_atomics.pop() {
        if let TAtomic::GenericParameter(TGenericParameter { constraint, .. }) = object_atomic {
            object_atomics.extend(constraint.types.iter().map(|bound| (bound, written_atomic)));

            continue;
        }

        if matches!(object_atomic, TAtomic::Error) {
            continue;
        }

        if object_atomic.is_null() || object_atomic.is_void() {
            result.encountered_null = true;

            if !is_null_safe && !object_type.has_nullsafe_null() {
                report_access_on_null(
                    context,
                    block_context,
                    object_expression.span(),
                    operator_span,
                    is_all_null,
                    object_atomic.is_void(),
                );
            }

            continue;
        }

        let closure_object;
        let object = match object_atomic {
            TAtomic::Object(object) => object,
            TAtomic::Callable(callable) if callable.is_closure() => {
                closure_object = TObject::new_named(word("Closure"));
                &closure_object
            }
            TAtomic::Array(TArray::List(list))
                if let Some(elements) = list.tuple_elements()
                    && let ClassLikeMemberSelector::Identifier(name) = property_selector =>
            {
                // A refused read gives no type, so its later uses report nothing again.
                let element = resolve_tuple_element(context, object_atomic, &elements, name, for_assignment);
                result.properties.push(ResolvedProperty {
                    property_span: None,
                    property_name: concat_word!("$", name.value),
                    declaring_class_id: None,
                    property_type: element.cloned().unwrap_or_else(|| wrap_atomic(TAtomic::Error)),
                    is_magic: false,
                    read_type: None,
                });

                continue;
            }
            _ => {
                result.has_invalid_path = true;
                if object_type.is_mixed() {
                    result.encountered_mixed = true;
                }

                if !block_context.flags.inside_isset() || !object_atomic.is_mixed() {
                    report_access_on_non_object(
                        context,
                        object_atomic,
                        written_atomic,
                        property_selector,
                        object_expression.span(),
                    );
                }

                continue;
            }
        };

        let classname = match object {
            TObject::Any => {
                result.has_ambiguous_path = true;

                // A PHP# `Object` has no members until `is` narrows it, as an `Any` has none, spec section 24.
                if context.dialect.is_sharp() {
                    context.collector.report_with_code(
                        IssueCode::AmbiguousObjectPropertyAccess,
                        object_member_issue(object_expression.span(), property_selector.span()),
                    );
                } else if !block_context.flags.inside_isset() {
                    let object_type = display_atomic(context, object_atomic);
                    report_ambiguous_access(context, property_selector, object_expression.span(), &object_type);
                }

                continue;
            }
            TObject::HasMethod(has_method) => {
                let all_properties_match = property_names.iter().all(|prop_name| {
                    let prop_name_without_dollar = word(trim_start_byte(prop_name.as_bytes(), b'$'));
                    type_has_property_assertion(has_method.intersection_types.as_deref(), prop_name_without_dollar)
                });

                if all_properties_match {
                    for prop_name in &property_names {
                        result.properties.push(ResolvedProperty {
                            property_span: None,
                            property_name: *prop_name,
                            declaring_class_id: None,
                            property_type: get_mixed(),
                            is_magic: false,
                            read_type: None,
                        });
                    }
                } else {
                    result.has_ambiguous_path = true;
                    if !block_context.flags.inside_isset() {
                        let object_type = display_type(context, &object_type);
                        report_ambiguous_access(context, property_selector, object_expression.span(), &object_type);
                    }
                }

                continue;
            }
            TObject::HasProperty(has_property) => {
                let all_properties_match = property_names.iter().all(|prop_name| {
                    let prop_name_without_dollar = word(trim_start_byte(prop_name.as_bytes(), b'$'));
                    has_property.has_property(prop_name_without_dollar)
                        || type_has_property_assertion(
                            has_property.intersection_types.as_deref(),
                            prop_name_without_dollar,
                        )
                });

                if all_properties_match {
                    for prop_name in &property_names {
                        result.properties.push(ResolvedProperty {
                            property_span: None,
                            property_name: *prop_name,
                            declaring_class_id: None,
                            property_type: get_mixed(),
                            is_magic: false,
                            read_type: None,
                        });
                    }
                } else {
                    result.has_ambiguous_path = true;
                    if !block_context.flags.inside_isset() {
                        let object_type = display_type(context, &object_type);
                        report_ambiguous_access(context, property_selector, object_expression.span(), &object_type);
                    }
                }

                continue;
            }
            TObject::WithProperties(object) => {
                for prop_name in &property_names {
                    let key = word(trim_start_byte(prop_name.as_bytes(), b'$'));
                    let Some((_, value)) = object.known_properties.get_key_value(&key) else {
                        if object.sealed {
                            result.has_invalid_path = true;

                            report_non_existent_property(
                                context,
                                object_type.get_id(),
                                *prop_name,
                                property_selector.span(),
                                object_expression.span(),
                                true,
                            );

                            continue;
                        }

                        result.has_ambiguous_path = true;

                        if !block_context.flags.inside_isset() {
                            let object_type = display_type(context, &object_type);
                            report_ambiguous_access(context, property_selector, object_expression.span(), &object_type);
                        }

                        continue;
                    };

                    let is_optional = value.0;
                    let mut property_type = value.1.clone();

                    if is_optional {
                        if !block_context.flags.inside_isset() {
                            report_possibly_non_existent_property(
                                context,
                                &object_type,
                                *prop_name,
                                property_selector.span(),
                                object_expression.span(),
                            );
                        }

                        property_type = property_type.as_nullable();
                    }

                    let resolved_property = ResolvedProperty {
                        property_span: None,
                        property_name: *prop_name,
                        declaring_class_id: None,
                        property_type,
                        is_magic: false,
                        read_type: None,
                    };

                    result.properties.push(resolved_property);
                }

                continue;
            }
            TObject::Named(named_object) => named_object.get_name(),
            TObject::Enum(r#enum) => r#enum.get_name(),
        };

        let magic_method_name = if for_assignment { "__set" } else { "__get" };
        let mut magic_method_identifier = MethodIdentifier::new(classname, word(magic_method_name));
        magic_method_identifier = context.codebase.get_declaring_method_identifier(&magic_method_identifier);
        let magic_method = context.codebase.get_method_by_id(&magic_method_identifier);

        for prop_name in &property_names {
            if let TObject::Enum(enum_type) = object
                && let Some(resolved) = resolve_enum_builtin_property(context, enum_type, *prop_name)
            {
                result.properties.push(resolved);
                continue;
            }

            let resolved_property = match resolve_external_property(
                context,
                classname,
                *prop_name,
                property_selector.span(),
                object,
                for_assignment,
                &mut result,
            )? {
                Some(ExternalPropertyResolution::Resolved(property)) => Some(property),
                Some(ExternalPropertyResolution::Invalid) => None,
                None => find_property_in_class(
                    context,
                    block_context,
                    artifacts,
                    classname,
                    *prop_name,
                    property_selector,
                    object_expression,
                    object,
                    written_atomic,
                    operator_span,
                    for_assignment,
                    &mut result,
                    magic_method.is_some(),
                ),
            };

            let Some(mut resolved_property) = resolved_property else {
                result.has_invalid_path = true;

                continue;
            };

            // Spec section 13: everything read through a readonly value is readonly, and nothing is written through it.
            if context.dialect.is_sharp()
                && let TObject::Named(named_object) = object
                && named_object.is_readonly
            {
                if for_assignment {
                    report_readonly_write(context, block_context, object_expression, property_selector);
                    result.has_error_path = true;

                    continue;
                }

                resolved_property.property_type = resolved_property.property_type.to_readonly();
            }

            if resolved_property.is_magic {
                if magic_method.is_none() {
                    report_magic_property_without_get_set_method(
                        context,
                        object_expression.span(),
                        property_selector.span(),
                        classname,
                        *prop_name,
                        for_assignment,
                    );
                }

                artifacts
                    .symbol_references
                    .add_reference_for_method_call(&block_context.scope, &magic_method_identifier);
            }

            if let Some(declaring_class_id) = resolved_property.declaring_class_id {
                if for_assignment {
                    artifacts.symbol_references.add_reference_for_property_write(
                        &block_context.scope,
                        declaring_class_id,
                        resolved_property.property_name,
                    );
                } else {
                    artifacts.symbol_references.add_reference_for_property_read(
                        &block_context.scope,
                        declaring_class_id,
                        resolved_property.property_name,
                    );
                }
            }

            result.properties.push(resolved_property);
        }
    }

    result.all_properties_non_nullable =
        !result.properties.is_empty() && result.properties.iter().all(|p| !p.property_type.is_nullable());

    Ok(result)
}

/// Reports a write of `object.property` where `object` is readonly, spec section 13, once for the write.
fn report_readonly_write<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    object: &Expression<'_>,
    property: &ClassLikeMemberSelector<'_>,
) where
    A: Arena,
{
    let receiver = quoted_source(context, object.span());
    let written = quoted_source(context, object.span().join(property.span()));
    let writer = readonly_writer(block_context.scope.get_function_like());
    context.collector.report_with_code(
        IssueCode::InvalidPropertyWrite,
        Issue::error(format!("{writer} cannot set {written}: {receiver} is readonly."))
            .with_annotation(Annotation::primary(property.span()).with_message("Written here."))
            .with_note("Through a `readonly` value, only `get` accessors and methods marked `readonly` run."),
    );
}

enum ExternalPropertyResolution {
    Resolved(ResolvedProperty),
    Invalid,
}

fn resolve_external_property<A>(
    context: &mut Context<'_, '_, A>,
    class: Word,
    property: Word,
    span: Span,
    object: &TObject,
    for_assignment: bool,
    result: &mut PropertyResolutionResult,
) -> Result<Option<ExternalPropertyResolution>, AnalysisError>
where
    A: Arena,
{
    if context.external_analysis_session.is_none() || !context.plugin_registry.may_have_property_type_provider() {
        return Ok(None);
    }

    let property_without_dollar = trim_start_byte(property.as_bytes(), b'$');
    let receiver_type = TUnion::from_atomic(TAtomic::Object(object.clone()));
    let Some(effective) = context.plugin_registry.get_property_type(
        context.codebase,
        class.as_bytes(),
        property_without_dollar,
        if for_assignment { PropertyAccessKind::Write } else { PropertyAccessKind::Read },
        &receiver_type,
        span,
        context.external_analysis_session,
    ) else {
        return Ok(None);
    };

    if for_assignment {
        let Some(write_type) = effective.write_type else {
            report_external_invalid_property_access(context, class, property, span, true);
            result.has_error_path = true;
            return Ok(Some(ExternalPropertyResolution::Invalid));
        };

        result.has_unreadable_property |= effective.read_type.is_none();
        return Ok(Some(ExternalPropertyResolution::Resolved(ResolvedProperty {
            property_name: property,
            declaring_class_id: None,
            property_span: None,
            property_type: write_type,
            is_magic: true,
            read_type: effective.read_type,
        })));
    }

    let Some(read_type) = effective.read_type else {
        report_external_invalid_property_access(context, class, property, span, false);
        result.has_error_path = true;
        return Ok(Some(ExternalPropertyResolution::Invalid));
    };

    Ok(Some(ExternalPropertyResolution::Resolved(ResolvedProperty {
        property_name: property,
        declaring_class_id: None,
        property_span: None,
        property_type: read_type,
        is_magic: true,
        read_type: None,
    })))
}

fn report_external_invalid_property_access<A>(
    context: &mut Context<'_, '_, A>,
    class: Word,
    property: Word,
    span: Span,
    for_assignment: bool,
) where
    A: Arena,
{
    let class = context.codebase.get_class_like(class.as_bytes()).map_or(class, |metadata| metadata.original_name);
    let property = display_member(context, class, property);
    let (code, action, direction) = if for_assignment {
        (IssueCode::InvalidPropertyWrite, "write to", "read-only")
    } else {
        (IssueCode::InvalidPropertyRead, "read from", "write-only")
    };
    context.collector.report_with_code(
        code,
        Issue::error(format!("Cannot {action} extension-provided {direction} property `{property}`."))
            .with_annotation(Annotation::primary(span).with_message(format!("This property is {direction}"))),
    );
}

/// Resolves built-in enum properties (`name` and `value`) with literal types.
///
/// For a specific enum case like `Foo::Bar`, returns the literal type:
/// - `name` returns `string('Bar')`
/// - `value` returns `string('bar')` or `int(123)` for backed enums
///
/// For a general enum type like `Color $c`, returns a union of all case literals:
/// - `name` returns `'Red'|'Green'|'Blue'`
/// - `value` returns `'red'|'green'|'blue'`
fn resolve_enum_builtin_property<A>(
    context: &Context<'_, '_, A>,
    enum_type: &TEnum,
    prop_name: Word,
) -> Option<ResolvedProperty>
where
    A: Arena,
{
    let prop_str = trim_start_byte(prop_name.as_bytes(), b'$');
    if prop_str != b"name" && prop_str != b"value" {
        return None;
    }

    let class_metadata = context.codebase.get_class_like(enum_type.name.as_bytes())?;

    // Determine which cases to process: Either it's a specific case, or all cases
    let cases: Vec<_> = if let Some(case_name) = enum_type.case {
        class_metadata.enum_cases.get(&case_name).into_iter().collect()
    } else {
        class_metadata.enum_cases.values().collect()
    };

    if prop_str == b"name" {
        let name_types: Vec<TAtomic> =
            cases.iter().map(|case| TAtomic::Scalar(TScalar::literal_string(case.name))).collect();

        if name_types.is_empty() {
            return None;
        }

        Some(ResolvedProperty {
            property_span: None,
            property_name: prop_name,
            declaring_class_id: Some(word(b"UnitEnum")),
            property_type: TUnion::from_vec(name_types),
            is_magic: false,
            read_type: None,
        })
    } else if prop_str == b"value" {
        let value_types: Vec<TAtomic> = cases.iter().filter_map(|case| case.value_type.clone()).collect();

        if value_types.is_empty() {
            // Unit enum - value property doesn't exist, fall through to normal resolution
            // which will report an appropriate error
            return None;
        }

        Some(ResolvedProperty {
            property_span: None,
            property_name: prop_name,
            declaring_class_id: Some(word(b"BackedEnum")),
            property_type: TUnion::from_vec(value_types),
            is_magic: false,
            read_type: None,
        })
    } else {
        None
    }
}

/// Checks if this is a backing store access: `$this->prop` inside a hook for that property.
fn is_backing_store_access(
    object_expr: &Expression,
    prop_name: Word,
    block_context: &BlockContext,
    resolved_names: &ResolvedNames<'_>,
) -> bool {
    is_this(object_expr, resolved_names)
        && block_context.scope.get_property_hook().is_some_and(|(hook_prop_name, _)| hook_prop_name == prop_name)
}

/// How a declared property resolved at a given call site: through its real declaration, or
/// through a magic `@property`/`@property-read`/`@property-write` annotation.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DeclaredPropertyKind<'ctx> {
    /// A real declaration governs the access.  The annotation of the same name on the accessed
    /// class, when one exists, may refine the declared type (see [`DeclaredProperty::declared_type`]).
    Real { annotation: Option<&'ctx PropertyMetadata> },
    /// A magic annotation governs the access, which goes through `__get`/`__set`.
    Magic,
}

/// The declaration governing a property access, as resolved by [`resolve_declared_property`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct DeclaredProperty<'ctx> {
    /// The class holding the governing declaration: the real property's declaring class, or
    /// the class whose docblock carries the `@property*` tag.
    pub declaring_class: &'ctx ClassLikeMetadata,
    pub property: &'ctx PropertyMetadata,
    pub kind: DeclaredPropertyKind<'ctx>,
}

impl DeclaredProperty<'_> {
    pub(crate) fn is_magic(&self) -> bool {
        matches!(self.kind, DeclaredPropertyKind::Magic)
    }

    /// The type the governing declaration gives the property: for a real declaration whose
    /// annotation narrows the declared type (a wide inherited `public $db` narrowed by
    /// `@property` on a subclass), the annotation's type; otherwise the declared type, or
    /// `mixed` when untyped.  A conflicting (non-narrowing) annotation type is ignored.
    pub(crate) fn declared_type(&self, codebase: &CodebaseMetadata) -> TUnion {
        self.declared_type_impl(codebase, false)
    }

    /// The type the governing declaration accepts when writing to the property: the distinct
    /// `@property-write` type when the declaration splits read and write types, otherwise the
    /// same type [`Self::declared_type`] returns.
    pub(crate) fn declared_write_type(&self, codebase: &CodebaseMetadata) -> TUnion {
        self.declared_type_impl(codebase, true)
    }

    /// [`Self::declared_write_type`] when `for_assignment`, otherwise [`Self::declared_type`].
    pub(crate) fn declared_type_for(&self, codebase: &CodebaseMetadata, for_assignment: bool) -> TUnion {
        self.declared_type_impl(codebase, for_assignment)
    }

    fn declared_type_impl(&self, codebase: &CodebaseMetadata, for_write: bool) -> TUnion {
        fn annotation_type_metadata(annotation: &PropertyMetadata, for_write: bool) -> Option<&TypeMetadata> {
            if for_write { annotation.get_write_type_metadata() } else { annotation.type_metadata.as_ref() }
        }

        if let DeclaredPropertyKind::Real { annotation: Some(annotation) } = self.kind
            && let Some(annotation_type) = annotation_type_metadata(annotation, for_write).map(|tm| &tm.type_union)
        {
            let narrows = match self.property.type_metadata.as_ref().map(|tm| &tm.type_union) {
                None => true,
                Some(real_type) => {
                    real_type.is_mixed()
                        || union_comparator::is_contained_by(
                            codebase,
                            annotation_type,
                            real_type,
                            false,
                            false,
                            false,
                            &mut ComparisonResult::default(),
                        )
                }
            };

            if narrows {
                return annotation_type.clone();
            }
        }

        let declared = annotation_type_metadata(self.property, for_write)
            .or(self.property.type_declaration_metadata.as_ref())
            .map(|tm| tm.type_union.clone())
            .unwrap_or_else(get_mixed);

        if self.declaring_class.flags.is_sharp() { known_type_or_error(declared) } else { declared }
    }
}

/// Resolves which declaration governs an access to `$obj->prop` (or `Class::$prop` with
/// `instance_access` false) on `class_metadata` from `scope`, mirroring PHP's runtime rule for
/// magic properties: a real property applies whenever the access can reach it — for instance
/// access, when it is non-static and visible at the call site; static properties and
/// annotations are irrelevant to static access.  Everywhere else `__get`/`__set` fire, so a
/// magic `@property*` annotation (documenting exactly that interface) applies when present.  A
/// real property that governs no access path still resolves as `Real` so the caller's
/// visibility check reports the inaccessible access.  Returns `None` when the name has neither
/// a real declaration nor an annotation.
pub(crate) fn resolve_declared_property<'ctx>(
    codebase: &'ctx CodebaseMetadata,
    class_metadata: &'ctx ClassLikeMetadata,
    prop_name: Word,
    instance_access: bool,
    scope: Option<Word>,
) -> Option<DeclaredProperty<'ctx>> {
    let annotation = if instance_access {
        class_metadata.magic_property_ids.get(&prop_name).copied().and_then(|tag_class| {
            let tag_metadata = if tag_class == class_metadata.name {
                class_metadata
            } else {
                codebase.get_class_like(tag_class.as_bytes())?
            };

            Some((tag_metadata, tag_metadata.magic_properties.get(&prop_name)?))
        })
    } else {
        None
    };

    let Some((declaring_metadata, real_property)) =
        class_metadata.declaring_property_ids.get(&prop_name).copied().and_then(|declaring_class| {
            let declaring_metadata = codebase.get_class_like(declaring_class.as_bytes())?;
            let real_property = declaring_metadata.properties.get(&prop_name)?;

            Some((declaring_metadata, real_property))
        })
    else {
        return annotation.map(|(tag_metadata, annotation)| DeclaredProperty {
            declaring_class: tag_metadata,
            property: annotation,
            kind: DeclaredPropertyKind::Magic,
        });
    };

    // `->` never reaches a static property, whatever its visibility; otherwise the real
    // property governs exactly where it is visible.  Visibility here means the main (get)
    // visibility for writes as well: PHP calls `__set()` only for a property that is entirely
    // inaccessible in the current scope — writing to one that is readable but not writable
    // (asymmetric visibility, e.g. `public private(set)`) throws instead of falling through.
    let real_property_reachable = || {
        !real_property.flags.is_static()
            && is_visible_from_scope(codebase, real_property.read_visibility, declaring_metadata.name.as_bytes(), scope)
    };

    if let Some((tag_metadata, annotation)) = annotation
        && !real_property_reachable()
    {
        return Some(DeclaredProperty {
            declaring_class: tag_metadata,
            property: annotation,
            kind: DeclaredPropertyKind::Magic,
        });
    }

    Some(DeclaredProperty {
        declaring_class: declaring_metadata,
        property: real_property,
        kind: DeclaredPropertyKind::Real { annotation: annotation.map(|(_, annotation)| annotation) },
    })
}

/// Resolves external access to a property of `class_metadata` where the accessing scope is not
/// a class scope but is certainly outside the class (mixins, intersection members,
/// `array_column()`): a real property governs when it is publicly visible, a magic
/// `@property*` annotation otherwise.  Returns `None` when neither reaches a declaration.
fn get_resolved_declared_property(
    codebase: &CodebaseMetadata,
    resolution: &DeclaredProperty<'_>,
    prop_name: Word,
    for_assignment: bool,
) -> ResolvedProperty {
    let property_metadata = resolution.property;

    ResolvedProperty {
        property_span: property_metadata.name_span.or(property_metadata.span),
        property_name: prop_name,
        declaring_class_id: Some(resolution.declaring_class.name),
        property_type: resolution.declared_type_for(codebase, for_assignment),
        is_magic: resolution.is_magic(),
        read_type: if for_assignment && resolution.is_magic() {
            Some(resolution.declared_type(codebase))
        } else {
            None
        },
    }
}

pub(crate) fn resolve_property_for_external_access<'ctx>(
    codebase: &'ctx CodebaseMetadata,
    class_metadata: &'ctx ClassLikeMetadata,
    prop_name: Word,
) -> Option<DeclaredProperty<'ctx>> {
    let resolution = resolve_declared_property(codebase, class_metadata, prop_name, true, None)?;

    match resolution.kind {
        DeclaredPropertyKind::Magic => Some(resolution),
        DeclaredPropertyKind::Real { .. } if resolution.property.read_visibility.is_public() => Some(resolution),
        DeclaredPropertyKind::Real { .. } => None,
    }
}

/// Whether `object_expr` is the object `new` with an object initializer or `with` creates, spec section 16, which
/// they set properties of while it is created.
fn is_created_object(object_expr: &Expression) -> bool {
    matches!(object_expr, Expression::Variable(Variable::Direct(variable))
        if variable.name.starts_with(CREATED_OBJECT_PREFIX))
}

/// The first method, in declaration order, that the struct `r#struct` declares on its objects and that returns the
/// struct: a method that returns a changed copy, as spec section 10's `moved` does.
fn first_copying_method<A>(context: &Context<'_, '_, A>, r#struct: &ClassLikeMetadata) -> Option<String>
where
    A: Arena,
{
    r#struct
        .methods
        .iter()
        .filter_map(|method| context.codebase.get_method(r#struct.name.as_bytes(), method.as_bytes()))
        .filter(|method| {
            method.method_metadata.as_ref().is_some_and(|metadata| !metadata.is_static && !metadata.is_constructor)
                && method.return_type_metadata.as_ref().is_some_and(|return_type| {
                    return_type.type_union.types.iter().any(|atomic| {
                        matches!(atomic, TAtomic::Object(TObject::Named(object))
                            if object.name.as_bytes().eq_ignore_ascii_case(r#struct.name.as_bytes()))
                    })
                })
        })
        .min_by_key(|method| method.span.start.offset)
        .map(|method| method.original_name.to_string())
}

/// Finds a property in a class, gets its type, and handles template localization. `written_atomic` is the receiver as
/// it is typed, which is `object`'s type parameter when `object` is that type parameter's bound.
fn find_property_in_class<'ctx, 'ast, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    class_id: Word,
    prop_name: Word,
    selector: &'ast ClassLikeMemberSelector<'arena>,
    object_expr: &'ast Expression<'arena>,
    object: &TObject,
    written_atomic: &TAtomic,
    access_span: Span,
    for_assignment: bool,
    result: &mut PropertyResolutionResult,
    has_magic_method: bool,
) -> Option<ResolvedProperty>
where
    A: Arena,
{
    let Some(class_metadata) = context.codebase.get_class_like(class_id.as_bytes()) else {
        report_non_existent_class_like(context, object_expr.span(), class_id);

        return None;
    };

    let method_name = word(trim_start_byte(prop_name.as_bytes(), b'$'));
    let read_span = object_expr.span().join(selector.span());
    let mut resolution = resolve_declared_property(
        context.codebase,
        class_metadata,
        prop_name,
        true, // `instance_access`
        block_context.scope.get_class_like_name(),
    );
    // Spec section 14.3: PHP reads a declared property the reader cannot see through `__get`, as an Eloquent model reads
    // a column named `table`, so in PHP# that read is the `__get` property, `Any?` where no `@property` line types it.
    if context.dialect.is_sharp()
        && !for_assignment
        && has_magic_method
        && let Some(DeclaredProperty { declaring_class, property, kind: DeclaredPropertyKind::Real { .. } }) =
            &resolution
        && (property.flags.is_static()
            || !is_visible_from_scope(
                context.codebase,
                property.read_visibility,
                declaring_class.name.as_bytes(),
                block_context.scope.get_class_like_name(),
            ))
    {
        resolution = None;
    }
    // Spec section 14.3: a class whose plain PHP code defines `__get`, itself or a parent, has the properties it reads,
    // `Any?` where no `@property` line types one. Only plain PHP defines `__get`.
    let property_type = match &resolution {
        Some(declared) => {
            (!declared.declaring_class.flags.is_sharp()).then(|| declared.declared_type(context.codebase))
        }
        None => has_magic_method.then(get_mixed),
    };
    let reads_method = !for_assignment
        && property_type.is_some_and(|property_type| {
            reads_method_over_property(context, artifacts, &property_type, class_id, method_name, read_span)
        });
    if reads_method {
        // The lowering takes the read as the method's value, where the engine would read the property.
        artifacts.record_call_target(
            read_span,
            CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(class_id, method_name), class: None },
        );
        resolution = None;
    }

    let Some(resolution) = resolution else {
        // A PHP# read of a method is its closure where the class has no property of the name, or where the code takes
        // the method over the property.
        let self_parameter = match written_atomic {
            TAtomic::GenericParameter(parameter) => Some(parameter),
            _ => None,
        };
        if !for_assignment
            && (reads_method || !has_magic_method)
            && let Some(method_type) = resolve_method_value(
                context,
                block_context,
                artifacts,
                object,
                self_parameter,
                method_name,
                access_span,
                read_span,
            )
        {
            let method = MethodIdentifier::new(class_id, method_name);
            check_private_method_reach(context, object_expr, &method, selector.span());
            // Spec section 13: a method value runs its method, so through a readonly value it is a call of it.
            if context.dialect.is_sharp() && matches!(object, TObject::Named(named) if named.is_readonly) {
                check_call_through_readonly(
                    context,
                    block_context,
                    &mut artifacts.readonly_calls,
                    (object_expr, selector),
                    &method,
                    class_id,
                );
            }

            return Some(ResolvedProperty {
                property_span: None,
                property_name: prop_name,
                declaring_class_id: None,
                property_type: method_type,
                is_magic: false,
                read_type: None,
            });
        }

        for required_class in class_metadata.require_extends.iter().chain(class_metadata.require_implements.iter()) {
            let Some(required_metadata) = context.codebase.get_class_like(required_class.as_bytes()) else {
                continue;
            };

            // `$this` here is an instance of the required class-like, so the call-site rule
            // applies there.  The required class also provides `__get`/`__set` at runtime,
            // which cannot be checked from here — hence `is_magic: false`.
            if let Some(required_resolution) = resolve_declared_property(
                context.codebase,
                required_metadata,
                prop_name,
                true, // `instance_access`
                block_context.scope.get_class_like_name(),
            ) {
                let prop_meta = required_resolution.property;

                return Some(ResolvedProperty {
                    property_span: prop_meta.name_span.or(prop_meta.span),
                    property_name: prop_name,
                    declaring_class_id: Some(required_resolution.declaring_class.name),
                    property_type: required_resolution.declared_type_for(context.codebase, for_assignment),
                    is_magic: false,
                    read_type: if for_assignment && required_resolution.is_magic() {
                        Some(required_resolution.declared_type(context.codebase))
                    } else {
                        None
                    },
                });
            }
        }

        // Check mixins.
        if !class_metadata.mixins.is_empty() {
            // Try to find property in mixin types
            if let Some(resolved) = find_property_in_mixins(
                context,
                class_metadata,
                object,
                &class_metadata.mixins,
                prop_name,
                for_assignment,
            ) {
                // For an annotation-governed mixin property, the caller reports a missing
                // `__get`/`__set` on the accessed class; the mixin-specific warnings below
                // cover real mixin properties only.
                if has_magic_method || resolved.is_magic {
                    return Some(resolved);
                }

                let magic_method_name = if for_assignment { "__set" } else { "__get" };
                if class_metadata.flags.is_final() {
                    report_non_existent_mixin_property(
                        context,
                        object_expr.span(),
                        selector.span(),
                        class_id,
                        prop_name,
                        resolved.declaring_class_id.unwrap_or(class_id),
                        magic_method_name,
                    );
                    result.has_invalid_path = true;
                } else {
                    report_possibly_non_existent_mixin_property(
                        context,
                        object_expr.span(),
                        selector.span(),
                        class_id,
                        prop_name,
                        resolved.declaring_class_id.unwrap_or(class_id),
                        magic_method_name,
                    );
                }

                return Some(resolved);
            }
        }

        if let TObject::Named(named_object) = object
            && let Some(resolved) =
                find_property_in_intersection_types(context, named_object, prop_name, for_assignment)
        {
            return Some(resolved);
        }

        if has_magic_method
            && !for_assignment
            && let Some(forwarded) = context.plugin_registry.get_forwarded_call(
                context.codebase,
                context.source_file,
                class_metadata.original_name.as_bytes(),
                trim_start_byte(prop_name.as_bytes(), b'$'),
                true,
                &TUnion::from_atomic(TAtomic::Object(object.clone())),
                context.external_analysis_session,
            )
            && let Some(property_type) = run_forwarded_methods(context, forwarded.receiver, &forwarded.methods)
        {
            return Some(ResolvedProperty {
                property_span: None,
                property_name: prop_name,
                declaring_class_id: Some(class_id),
                property_type,
                is_magic: true,
                read_type: None,
            });
        }

        if has_magic_method {
            // Spec section 14.3: what `__get` gives back is `Any?`, whatever PHP# wrote.
            result.has_unreadable_property |= for_assignment && context.dialect.is_sharp();
            report_non_documented_property(
                context,
                object_expr.span(),
                selector.span(),
                class_id,
                prop_name,
                for_assignment,
            );

            return Some(ResolvedProperty {
                property_span: None,
                property_name: prop_name,
                declaring_class_id: Some(class_id),
                property_type: get_mixed(),
                is_magic: true,
                // Undocumented access on a class with `__get`/`__set` (e.g. `stdClass` dynamic
                // properties): the type is unknown, so the written value stays memoizable.
                read_type: None,
            });
        }

        let prop_name_without_dollar = word(trim_start_byte(prop_name.as_bytes(), b'$'));
        if let TObject::Named(named_object) = object
            && type_has_property_assertion(named_object.get_intersection_types(), prop_name_without_dollar)
        {
            record_asserted_descendant_property_references(
                context,
                block_context,
                artifacts,
                named_object,
                prop_name,
                for_assignment,
            );

            return Some(ResolvedProperty {
                property_span: None,
                property_name: prop_name,
                declaring_class_id: None,
                property_type: get_mixed(),
                is_magic: false,
                read_type: None,
            });
        }

        if class_metadata.has_incomplete_hierarchy() {
            result.has_ambiguous_path = true;
            return None;
        }

        result.has_invalid_path = true;

        if !class_metadata.flags.is_final() || class_metadata.kind.is_interface() || class_metadata.kind.is_trait() {
            result.has_possibly_defined_property = true;
        }

        report_non_existent_property(context, class_id, prop_name, selector.span(), object_expr.span(), false);
        return None;
    };

    let DeclaredProperty { declaring_class: declaring_class_metadata, property: property_metadata, .. } = resolution;
    let declaring_class_id = declaring_class_metadata.name;

    let property_display_name = display_member(context, declaring_class_metadata.original_name, prop_name);
    crate::utils::availability::check_property_availability(
        context,
        property_metadata,
        &property_display_name,
        selector.span(),
    );

    // For assignment, use set hook parameter type when not accessing backing store directly
    let mut used_set_hook_param = false;
    let mut property_type = if for_assignment {
        if let Some(set_hook) = property_metadata.hooks.get(&word(b"set"))
            && let Some(param) = &set_hook.parameter
            && let Some(param_type) = param.get_type_metadata()
            && !is_backing_store_access(object_expr, prop_name, block_context, context.resolved_names)
        {
            used_set_hook_param = true;
            param_type.type_union.clone()
        } else {
            resolution.declared_write_type(context.codebase)
        }
    } else {
        resolution.declared_type(context.codebase)
    };

    // When writes diverge from reads — a magic property or a hook with a `set` parameter — the
    // type a read produces regardless of the written value. It is resolved (below) alongside
    // `property_type` so a generic or `self` read type is expanded and localized before it clamps
    // the memoized write value in `property_assignment`.
    let mut read_type = (for_assignment && (resolution.is_magic() || used_set_hook_param))
        .then(|| resolution.declared_type(context.codebase));

    // Plain PHP reads a PHP# property's `Map` or `Set` as the object it runs as, and writes either the object or the
    // PHP array PHP# turns into one.
    if !context.dialect.is_sharp() && resolution.declaring_class.flags.is_sharp() {
        property_type = if for_assignment {
            get_php_passed_type(&property_type, context.codebase)
        } else {
            get_php_received_type(&property_type, context.codebase)
        };
        read_type = read_type.map(|read_type| get_php_received_type(&read_type, context.codebase));
    } else if context.dialect.is_sharp() && !for_assignment && !resolution.declaring_class.flags.is_sharp() {
        property_type = get_sharp_received_type(&property_type);
    }

    let expand_and_localize = |ty: &mut TUnion| {
        expander::expand_union(
            context.codebase,
            ty,
            &TypeExpansionOptions {
                self_class: Some(declaring_class_id),
                static_class_type: StaticClassType::Object(object.clone()),
                ..Default::default()
            },
        );
        localize_member_type(context, ty, object, declaring_class_metadata);
    };

    expand_and_localize(&mut property_type);
    if let Some(read_type) = read_type.as_mut() {
        expand_and_localize(read_type);
    }

    if !for_assignment
        && property_metadata.type_declaration_metadata.is_some()
        && !property_metadata.flags.has_default()
        && !property_metadata.flags.is_promoted_property()
        && (!property_metadata.flags.is_virtual_property() || declaring_class_metadata.kind.is_interface())
    {
        property_type.set_possibly_undefined(true, None);
    }

    // A PHP# `init` property is set only while its object is created, spec section 6.1: on `this`, in a constructor of
    // its class or a subclass, or in a named constructor, where C# allows an `init` accessor. A lambda may run after
    // the object is created, so it cannot set one, as C# refuses with CS8852. The property's own accessor body writes
    // its storage through `field`, on `this` only.
    // A struct never changes, spec section 10: every property is set only while the struct is created, `init` or not.
    // An enum case never changes either, spec section 20: its fields are set only while the case is made.
    let is_struct = declaring_class_metadata.flags.is_struct();
    if for_assignment
        && context.dialect.is_sharp()
        && let Some(case) = display_enum_case(context, declaring_class_metadata)
    {
        context.collector.report_with_code(
            IssueCode::InvalidPropertyWrite,
            Issue::error(format!("An enum case never changes; return a changed copy made with `{case}(…)`."))
                .with_annotation(Annotation::primary(selector.span()).with_message("Written here."))
                .with_note("A case's fields are set only while it is made."),
        );
        result.has_error_path = true;

        return None;
    }
    if for_assignment && context.dialect.is_sharp() && (is_struct || property_metadata.flags.is_init()) {
        let writes_its_storage = is_backing_store_access(object_expr, prop_name, block_context, context.resolved_names);
        let creates_this = is_created_object(object_expr)
            || is_this(object_expr, context.resolved_names)
                && block_context.scope.get_function_like().is_some_and(|function_like| {
                    function_like.flags.is_named_constructor()
                        || function_like.method_metadata.as_ref().is_some_and(|method| method.is_constructor)
                });

        if is_struct && !writes_its_storage && !creates_this {
            // `with` can't set a get-only property, so neither `with` nor a method, which may be a `with` on it, is
            // named as its fix. The main constructor sets every property.
            let message = if !is_set_by_with(property_metadata) {
                let name = display_class_like_name(context, declaring_class_metadata.name);
                let property = display_property_name(context, prop_name);

                format!(
                    "A struct never changes; return a changed copy built with its main constructor, `new {name}(…)`, or declare `{property}` `{{ get; init; }}` and return a changed copy built with `with`."
                )
            } else if let Some(method) = first_copying_method(context, declaring_class_metadata) {
                format!("A struct never changes; return a changed copy from a method, such as `{method}`.")
            } else {
                "A struct never changes; return a changed copy built with `with`.".to_owned()
            };
            context.collector.report_with_code(
                IssueCode::InvalidPropertyWrite,
                Issue::error(message)
                    .with_annotation(Annotation::primary(selector.span()).with_message("Written here."))
                    .with_note("A struct's properties are set only while it is created."),
            );
            result.has_error_path = true;

            return None;
        }

        if !writes_its_storage && !creates_this {
            let property = display_property_name(context, prop_name);
            context.collector.report_with_code(
                IssueCode::InvalidPropertyWrite,
                Issue::error(format!("`{property}` is set only while the object is created."))
                    .with_annotation(Annotation::primary(selector.span()).with_message("Written here."))
                    .with_help("Set it on `this` in a constructor or a named constructor."),
            );
            result.has_error_path = true;

            return None;
        }
    }

    let is_visible = if for_assignment {
        check_resolved_property_write_visibility(
            context,
            block_context,
            &resolution,
            access_span,
            Some(selector.span()),
            is_created_object(object_expr),
        )
    } else {
        check_resolved_property_read_visibility(context, block_context, &resolution, access_span, Some(selector.span()))
    };

    if !is_visible {
        result.has_error_path = true;

        return None;
    }

    check_private_property_reach(
        context,
        object_expr,
        declaring_class_metadata,
        property_metadata,
        for_assignment,
        selector.span(),
    );

    // Spec section 14.3: a read of a property `__get` serves is what `__get` gives back, whatever PHP# wrote.
    result.has_unreadable_property |= for_assignment && context.dialect.is_sharp() && resolution.is_magic();

    Some(ResolvedProperty {
        property_span: property_metadata.name_span.or(property_metadata.span),
        property_name: prop_name,
        declaring_class_id: Some(declaring_class_id),
        property_type,
        is_magic: resolution.is_magic(),
        read_type,
    })
}

/// Records references to child properties that can satisfy a `property_exists()` assertion.
fn record_asserted_descendant_property_references<'ctx, A>(
    context: &Context<'ctx, '_, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    object: &TNamedObject,
    property_name: Word,
    for_assignment: bool,
) where
    A: Arena,
{
    let property_name_without_dollar = word(trim_start_byte(property_name.as_bytes(), b'$'));
    if !type_has_property_assertion(object.get_intersection_types(), property_name_without_dollar) {
        return;
    }

    let mut declaring_classes = Vec::new();
    for descendant in context.codebase.get_all_descendants(object.name.as_bytes()) {
        let Some(declaring_class) =
            context.codebase.get_declaring_property_class(descendant.as_bytes(), property_name.as_bytes())
        else {
            continue;
        };

        if declaring_classes.contains(&declaring_class) {
            continue;
        }

        let Some(declaring_metadata) = context.codebase.get_class_like(declaring_class.as_bytes()) else {
            continue;
        };
        let Some(property_metadata) = declaring_metadata.properties.get(&property_name) else {
            continue;
        };

        if !is_asserted_descendant_property_accessible(
            context,
            block_context,
            declaring_metadata,
            property_metadata,
            for_assignment,
        ) {
            continue;
        }

        declaring_classes.push(declaring_class);
    }

    for declaring_class in declaring_classes {
        if for_assignment {
            artifacts.symbol_references.add_reference_for_property_write(
                &block_context.scope,
                declaring_class,
                property_name,
            );
        } else {
            artifacts.symbol_references.add_reference_for_property_read(
                &block_context.scope,
                declaring_class,
                property_name,
            );
        }
    }
}

/// Checks whether an asserted child property can be reached by the current instance access.
fn is_asserted_descendant_property_accessible<'ctx, A>(
    context: &Context<'ctx, '_, A>,
    block_context: &BlockContext<'ctx>,
    declaring_class: &ClassLikeMetadata,
    property: &PropertyMetadata,
    for_assignment: bool,
) -> bool
where
    A: Arena,
{
    if property.flags.is_static() {
        return false;
    }

    if for_assignment {
        if property.flags.is_virtual_property()
            && property.hooks.contains_key(&word(b"get"))
            && !property.hooks.contains_key(&word(b"set"))
        {
            return false;
        }

        let visibility = effective_write_visibility(property, context.settings.version);

        return is_visible_from_scope(
            context.codebase,
            visibility,
            declaring_class.name.as_bytes(),
            block_context.scope.get_class_like_name(),
        );
    }

    if property.flags.is_virtual_property()
        && property.hooks.contains_key(&word(b"set"))
        && !property.hooks.contains_key(&word(b"get"))
    {
        return false;
    }

    is_visible_from_scope(
        context.codebase,
        property.read_visibility,
        declaring_class.name.as_bytes(),
        block_context.scope.get_class_like_name(),
    )
}

/// Localizes `member_type`, which `declaring_class` declares, for a read through `receiver`: the declaring class's type
/// parameters become the receiver's type arguments, or the ones the receiver's class names in its header.
pub(crate) fn localize_member_type<A>(
    context: &Context<'_, '_, A>,
    member_type: &mut TUnion,
    receiver: &TObject,
    declaring_class: &ClassLikeMetadata,
) where
    A: Arena,
{
    if !declaring_class.template_types.is_empty()
        && let TObject::Named(receiver) = receiver
    {
        *member_type = localize_property_type(
            context,
            member_type,
            receiver.get_type_parameters().unwrap_or_default(),
            context.codebase.get_class_like(receiver.name.as_bytes()).unwrap_or(declaring_class),
            declaring_class,
        );
    }
}

pub fn localize_property_type<A>(
    context: &Context<'_, '_, A>,
    class_property_type: &TUnion,
    object_type_parameters: &[TUnion],
    property_class_metadata: &ClassLikeMetadata,
    property_declaring_class_metadata: &ClassLikeMetadata,
) -> TUnion
where
    A: Arena,
{
    let template_types = get_receiver_type_arguments(
        context,
        object_type_parameters,
        property_class_metadata,
        property_declaring_class_metadata,
    );

    inferred_type_replacer::replace(
        class_property_type,
        &TemplateResult::new(IndexMap::default(), template_types),
        context.codebase,
    )
}

/// The type arguments of `declaring_class`, a generic PHP# class, for its static member reached through `class`,
/// spec section 11: its type parameters as the access gives them, written on the class, as in `Counter<Order>.count`,
/// or by `class`'s header, as in `Orders.count` for `Orders : Counter<Order>`. Each type argument keeps its own
/// statics, and a static method runs with them ahead of its own. None for a member of any other class.
pub(crate) fn static_member_type_arguments<A>(
    context: &Context<'_, '_, A>,
    class: Word,
    declaring_class: Word,
    type_arguments: Option<&[TUnion]>,
) -> Option<Vec<TUnion>>
where
    A: Arena,
{
    let class = context.codebase.get_class_like(class.as_bytes())?;
    let declaring_class = context.codebase.get_class_like(declaring_class.as_bytes())?;
    if !declaring_class.flags.is_sharp() || declaring_class.template_types.is_empty() {
        return None;
    }
    let type_arguments = type_arguments.unwrap_or_default();
    // A header's type argument may hold `class`'s own type parameters, as `Sub<TX> : Base<Box<TX>>` does, which the
    // access's type arguments then fix, so `Sub<Order>.count` reads `Base<Box<Order>>`'s.
    let own = class_type_arguments(class, type_arguments);

    Some(
        declaring_class
            .template_types
            .iter()
            .map(|(name, template)| {
                let parameter = wrap_atomic(TAtomic::GenericParameter(TGenericParameter::new(
                    *name,
                    Arc::new(template.constraint.clone()),
                    template.defining_entity,
                )));
                let localized = localize_property_type(context, &parameter, type_arguments, class, declaring_class);

                inferred_type_replacer::replace(&localized, &own, context.codebase)
            })
            .collect(),
    )
}

/// The type arguments a receiver of `receiver_class` with `receiver_type_parameters` gives its class's type
/// parameters and the ones of `declaring_class`, which declares the member it reads.
fn get_receiver_type_arguments<A>(
    context: &Context<'_, '_, A>,
    receiver_type_parameters: &[TUnion],
    receiver_class: &ClassLikeMetadata,
    declaring_class: &ClassLikeMetadata,
) -> HashMap<Word, HashMap<GenericParent, TUnion>>
where
    A: Arena,
{
    let mut template_types = get_template_types_for_class_member(
        context,
        Some(declaring_class),
        Some(declaring_class.name),
        Some(receiver_class),
        &receiver_class.template_types,
        &IndexMap::default(),
    );

    update_template_types(context, &mut template_types, receiver_class, receiver_type_parameters, declaring_class);

    template_types
}

/// PHP#'s type of a member `declaring_class` declares, called or read as a method value through `receiver`, which finds
/// the member on `calling_class`, spec section 11. `expand_member` expands the member's declared type with the options
/// it is given, which bind `Self` to the receiver's class with its own type parameters, as `get_this_type` builds it,
/// and the intersection parts the receiver carries, or to `self_parameter`, the type parameter a receiver typed by one
/// is written as, whose bound `receiver` is, and keep a derived type over a type parameter. One replacement then
/// gives the receiver's class's type parameters the receiver's type arguments, and the declaring class's the type
/// arguments of the receiver, or of its intersection part that inherits `declaring_class`, beside a call's
/// `type_arguments`. The last expansion binds no `Self`, so a `Self` that a receiver's type argument holds stays the
/// class that wrote it, and evaluates the kept derived types against the replaced types. Where `declaring_class` is a
/// PHP trait, its `self` in what the member gives, or in what its method value returns, is `calling_class`.
pub(crate) fn specialize_member_type<A>(
    context: &Context<'_, '_, A>,
    receiver: &TObject,
    self_parameter: Option<&TGenericParameter>,
    declaring_class: &ClassLikeMetadata,
    calling_class: Word,
    type_arguments: &TemplateResult,
    expand_member: impl FnOnce(&TypeExpansionOptions) -> TUnion,
) -> TUnion
where
    A: Arena,
{
    let intersection_parts = receiver.get_intersection_types().unwrap_or_default();
    let receiver_class = receiver
        .get_name()
        .and_then(|name| context.codebase.get_class_like(name.as_bytes()))
        .unwrap_or(declaring_class);
    let static_class_type = match self_parameter {
        Some(parameter) => StaticClassType::Generic(parameter.clone()),
        None => {
            let mut this_type = get_this_type(context, receiver_class, None);
            for part in intersection_parts {
                this_type.add_intersection_type(part.clone());
            }

            StaticClassType::Object(this_type)
        }
    };

    let member_type = expand_member(&TypeExpansionOptions {
        self_class: Some(declaring_class.name),
        static_class_type,
        keep_generic_derived_types: true,
        ..Default::default()
    });

    let receiver_type_arguments = receiver.get_type_parameters().unwrap_or_default();
    let objects = intersection_parts.iter().filter_map(|part| match part {
        TAtomic::Object(object) => Some(object),
        _ => None,
    });
    let (inheriting_class, inheriting_type_arguments) = std::iter::once(receiver)
        .chain(objects)
        .find_map(|object| {
            let class = context.codebase.get_class_like(object.get_name()?.as_bytes())?;
            context
                .codebase
                .is_instance_of(class.name.as_bytes(), declaring_class.name.as_bytes())
                .then(|| (class, object.get_type_parameters().unwrap_or_default()))
        })
        .unwrap_or((receiver_class, receiver_type_arguments));

    let mut type_arguments = type_arguments.clone();
    for (class, class_type_arguments, member_class) in [
        (receiver_class, receiver_type_arguments, receiver_class),
        (inheriting_class, inheriting_type_arguments, declaring_class),
    ] {
        for (name, bounds) in get_receiver_type_arguments(context, class_type_arguments, class, member_class) {
            for (parent, bound) in bounds {
                type_arguments
                    .lower_bounds
                    .entry(name)
                    .or_default()
                    .insert(parent, vec![TemplateBound::new(bound, 0, None)]);
            }
        }
    }

    let mut member_type = inferred_type_replacer::replace(&member_type, &type_arguments, context.codebase);
    expander::expand_union(context.codebase, &mut member_type, &TypeExpansionOptions::default());

    rename_trait_objects(declaring_class, calling_class, &mut member_type);
    for atomic in member_type.types.to_mut() {
        if let TAtomic::Callable(TCallable::Signature(signature)) = atomic
            && let Some(return_type) = signature.get_return_type_mut()
        {
            rename_trait_objects(declaring_class, calling_class, return_type);
        }
    }

    member_type
}

/// The declared type of the property `property_name` read through `receiver`, from its first object that declares it,
/// localized by `localize_member_type`.
pub(crate) fn get_localized_property_type<A>(
    context: &Context<'_, '_, A>,
    receiver: &TUnion,
    property_name: Word,
) -> Option<TUnion>
where
    A: Arena,
{
    receiver.types.iter().find_map(|atomic| {
        let TAtomic::Object(object) = atomic else {
            return None;
        };
        let class_name = object.get_name()?;
        let mut property_type =
            context.codebase.get_property_type(class_name.as_bytes(), property_name.as_bytes())?.clone();
        let declaring_class_name =
            context.codebase.get_declaring_property_class(class_name.as_bytes(), property_name.as_bytes())?;
        let declaring_class = context.codebase.get_class_like(declaring_class_name.as_bytes())?;
        localize_member_type(context, &mut property_type, object, declaring_class);

        Some(property_type)
    })
}

fn update_template_types<A>(
    context: &Context<'_, '_, A>,
    template_types: &mut HashMap<Word, HashMap<GenericParent, TUnion>>,
    property_class_metadata: &ClassLikeMetadata,
    lhs_type_params: &[TUnion],
    property_declaring_class_metadata: &ClassLikeMetadata,
) where
    A: Arena,
{
    if !template_types.is_empty() && !property_class_metadata.template_types.is_empty() {
        for (param_offset, lhs_param_type) in lhs_type_params.iter().enumerate() {
            let mut i = -1;

            for (calling_param_name, _) in &property_class_metadata.template_types {
                i += 1;

                if i == (param_offset as i32) {
                    template_types.entry(*calling_param_name).or_default().insert(
                        GenericParent::ClassLike(property_class_metadata.name),
                        {
                            let mut lhs_param_type = lhs_param_type.clone();

                            expander::expand_union(
                                context.codebase,
                                &mut lhs_param_type,
                                &TypeExpansionOptions::default(),
                            );

                            lhs_param_type
                        },
                    );
                    break;
                }
            }
        }
    }

    for (type_name, v) in template_types.iter_mut() {
        if let Some(mapped_type) = property_class_metadata
            .template_extended_parameters
            .get(&property_declaring_class_metadata.name)
            .unwrap_or(&IndexMap::default())
            .get(type_name)
        {
            for mapped_type_atomic in mapped_type.types.as_ref() {
                if let TAtomic::GenericParameter(TGenericParameter { parameter_name, .. }) = &mapped_type_atomic {
                    let position = property_class_metadata
                        .template_types
                        .iter()
                        .enumerate()
                        .filter(|(_, (k, _))| *k == parameter_name)
                        .map(|(i, _)| i)
                        .next();

                    if let Some(position) = position
                        && let Some(mapped_param) = lhs_type_params.get(position)
                    {
                        v.insert(
                            GenericParent::ClassLike(property_declaring_class_metadata.name),
                            mapped_param.clone(),
                        );
                    }
                }
            }
        }
    }
}

/// Reports an error for a property access on a `null` or `void` value.
fn report_access_on_null<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    block_context: &BlockContext<'ctx>,
    object_span: Span,
    operator_span: Span,
    is_always_null: bool,
    from_void: bool,
) where
    A: Arena,
{
    match (from_void, is_always_null) {
        (true, true) => {
            context.collector.report_with_code(
                IssueCode::NullPropertyAccess,
                Issue::error("Attempting to access a property on an expression of type `void`.")
                    .with_annotation(
                        Annotation::primary(object_span)
                            .with_message("This expression has type `void`, which is treated as `null` at runtime"),
                    )
                    .with_note("Expressions of type `void` do not produce a value. Accessing a property on this will always result in `null` and raise a warning.")
                    .with_help("This access should be removed. Check the origin of this expression to understand why it results in `void`."),
            );
        }
        (true, false) => {
            context.collector.report_with_code(
                IssueCode::PossiblyNullPropertyAccess,
                Issue::error("Attempting to access a property on an expression that can be `void`.")
                    .with_annotation(
                        Annotation::primary(object_span).with_message("This expression's type includes `void`"),
                    )
                    .with_note("If this expression resolves to `void` at runtime, accessing a property will result in `null` and raise a warning.")
                    .with_note("The `void` type often originates from a function or a method that does not return a value.")
                    .with_help("You must guard this access. Check if the value is an object before accessing the property."),
            );
        }
        (false, true) => {
            context.collector.report_with_code(
                IssueCode::NullPropertyAccess,
                Issue::error("Attempting to access a property on an expression that is always `null`.")
                    .with_annotation(
                        Annotation::primary(object_span)
                            .with_message("This expression is always `null` here"),
                    )
                    .with_note("In PHP, this will raise a warning and the expression will evaluate to `null`.")
                    .with_help("This code path appears to be an error. You should either ensure this expression can be a valid object or remove the property access entirely."),
            );
        }
        (false, false) => {
            let (nullsafe, null_check) =
                if context.dialect.is_sharp() { ("?.", "if (obj != null)") } else { ("?->", "if ($obj !== null)") };

            if !block_context.flags.inside_isset() {
                if block_context.flags.inside_assignment() {
                    context.collector.report_with_code(
                        IssueCode::PossiblyNullPropertyAccess,
                        Issue::error("Attempting to access a property on a possibly `null` value.")
                            .with_annotation(
                                Annotation::primary(object_span)
                                    .with_message("This expression can be `null` here"),
                            )
                            .with_note("If this expression is `null` at runtime, PHP will raise a warning and the property access will result in `null`.")
                            .with_help(format!("Add a check to ensure the value is not `null` (e.g., `{null_check}`)."))
                    );
                } else {
                    context.collector.report_with_code(
                        IssueCode::PossiblyNullPropertyAccess,
                        Issue::error("Attempting to access a property on a possibly `null` value.")
                            .with_annotation(
                                Annotation::primary(object_span)
                                    .with_message("This expression can be `null` here"),
                            )
                            .with_note("If this expression is `null` at runtime, PHP will raise a warning and the property access will result in `null`.")
                            .with_help(format!("Use the nullsafe operator (`{nullsafe}`) to safely access the property, or add a check to ensure the value is not `null` (e.g., `{null_check}`)."))
                            .with_edit(operator_span.file_id, TextEdit::replace(operator_span, nullsafe)),
                    );
                }
            }
        }
    }
}

/// Reports a nullsafe operator on an object that is never `null`. `called` is whether the operator calls the object
/// itself, as PHP#'s `f?.(x)` does, which a plain call replaces.
pub(crate) fn check_redundant_nullsafe<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    operator_span: Span,
    object_expr: &Expression<'arena>,
    object_type: &TUnion,
    called: bool,
) where
    A: Arena,
{
    // Spec section 14.4: `?.` skips the rest of its chain, so the null an earlier `?.` of the same chain gives never
    // reaches a later link.
    let link_type;
    let object_type =
        if context.dialect.is_sharp() && object_type.has_nullsafe_null() && expression_is_nullsafe(object_expr) {
            link_type = object_type.to_non_nullable();
            &link_type
        } else {
            object_type
        };
    let is_nullable = object_type.can_be_null() || object_type.possibly_undefined();
    let is_all_null = object_type.is_null() || object_type.is_void();
    if is_nullable || is_all_null || is_refused_type(object_type) {
        return;
    }

    let (nullsafe, access) = match (context.dialect.is_sharp(), called) {
        (true, true) => ("?.", ""),
        (true, false) => ("?.", "."),
        (false, _) => ("?->", "->"),
    };
    let help = if called {
        "Call the function directly, as in `f(x)`.".to_owned()
    } else {
        format!("Consider using the direct property access operator (`{access}`) for clarity.")
    };

    // A PHP# file that writes PHP's `?->` already has a parse error at the operator.
    let written =
        context.source_file.contents.get(operator_span.start_offset() as usize..operator_span.end_offset() as usize);
    if written != Some(nullsafe.as_bytes()) {
        return;
    }

    let object_type_str = display_type(context, object_type);

    let issue = context.as_null_check_error(
        Issue::help(format!("Redundant nullsafe operator (`{nullsafe}`) used on an expression that is never `null`."))
            .with_annotation(
                Annotation::primary(operator_span).with_message(format!("Nullsafe operator `{nullsafe}` is unnecessary here")),
            )
            .with_annotation(
                Annotation::secondary(object_expr.span())
                    .with_message(format!("This expression (type `{object_type_str}`) is never `null`")),
            )
            .with_note(format!("The nullsafe operator (`{nullsafe}`) short-circuits the access if the object is `null`. Since this expression is guaranteed not to be `null`, this check is unnecessary."))
            .with_help(help),
    );

    context.collector.propose_with_code(IssueCode::RedundantNullsafeOperator, issue, |edits| {
        edits.push(TextEdit::replace(operator_span.to_range(), access));
    });
}

/// Reports a property access on `atomic_type`, which is not an object. A PHP# message names the type the code wrote,
/// `written_type`, such as the type parameter whose bound `atomic_type` is.
fn report_access_on_non_object<A>(
    context: &mut Context<'_, '_, A>,
    atomic_type: &TAtomic,
    written_type: &TAtomic,
    selector: &ClassLikeMemberSelector,
    object_span: Span,
) where
    A: Arena,
{
    let type_str = display_atomic(context, if context.dialect.is_sharp() { written_type } else { atomic_type });
    context.collector.report_with_code(
        if atomic_type.is_mixed() { IssueCode::MixedPropertyAccess } else { IssueCode::InvalidPropertyAccess },
        Issue::error(format!("Attempting to access a property on a non-object type (`{type_str}`)."))
            .with_annotation(Annotation::primary(selector.span()).with_message("Cannot access property here"))
            .with_annotation(
                Annotation::secondary(object_span).with_message(format!("This expression has type `{type_str}`")),
            ),
    );
}

/// The type of the element of the tuple `tuple`, whose `elements` it has, that `name` names, spec section 30, which reads
/// a named element as C# does. A name the tuple does not write, and a write, which a tuple never takes, are errors, and
/// give `None`.
fn resolve_tuple_element<'tuple, A>(
    context: &mut Context<'_, '_, A>,
    tuple: &TAtomic,
    elements: &[(&'tuple TUnion, Option<Word>)],
    name: &LocalIdentifier,
    for_assignment: bool,
) -> Option<&'tuple TUnion>
where
    A: Arena,
{
    let written = display_atomic(context, tuple);
    let named = elements.iter().find(|(_, element)| element.is_some_and(|element| element.as_bytes() == name.value));
    let issue = match named {
        Some(_) if for_assignment => Issue::error(format!(
            "`{written}` never changes: build a new tuple instead of writing `{}`.",
            String::from_utf8_lossy(name.value)
        ))
        .with_annotation(Annotation::primary(name.span).with_message("Written here.")),
        Some((element, _)) => return Some(element),
        None => {
            let written_names: Vec<String> =
                elements.iter().filter_map(|(_, name)| name.map(|name| format!("`{name}`"))).collect();
            let elements = match written_names.as_slice() {
                [] => "its elements have no names, so unpack it, as in `const [first, second] = …;`".to_owned(),
                [only] => format!("its one named element is {only}"),
                [first @ .., last] => format!("its elements are {} and {last}", first.join(", ")),
            };

            Issue::error(format!(
                "`{written}` has no element named `{}`: {elements}.",
                String::from_utf8_lossy(name.value)
            ))
            .with_annotation(Annotation::primary(name.span).with_message("Read here."))
        }
    };
    let code = if for_assignment { IssueCode::InvalidPropertyWrite } else { IssueCode::NonExistentProperty };
    context.collector.report_with_code(code, issue);

    None
}

fn report_ambiguous_access<A>(
    context: &mut Context<'_, '_, A>,
    selector: &ClassLikeMemberSelector,
    object_span: Span,
    object_type: &str,
) where
    A: Arena,
{
    context.collector.report_with_code(
        IssueCode::AmbiguousObjectPropertyAccess,
        Issue::warning(format!("Cannot statically verify property access on a generic `{object_type}` type."))
            .with_annotation(Annotation::primary(selector.span()).with_message("Accessing property here"))
            .with_annotation(
                Annotation::secondary(object_span).with_message(format!("This expression has type `{object_type}`")),
            )
            .with_help("Provide a more specific type hint for the object (e.g., `MyClass`) for robust analysis."),
    );
}

fn report_possibly_non_existent_property<A>(
    context: &mut Context<'_, '_, A>,
    object_type: &TUnion,
    prop_name: Word,
    selector_span: Span,
    object_span: Span,
) where
    A: Arena,
{
    let object_type = display_type(context, object_type);

    context.collector.report_with_code(
        IssueCode::PossiblyNonExistentProperty,
        Issue::error(format!("Property `{prop_name}` might not exist on object `{object_type}`."))
            .with_annotation(Annotation::primary(selector_span).with_message("Property might not exist here"))
            .with_annotation(
                Annotation::secondary(object_span).with_message(format!("On instance of `{object_type}`")),
            )
            .with_note(
                "If this property does not exist at runtime, PHP will raise a warning and the expression will evaluate to `null`.",
            )
            .with_help(
                "To avoid this, ensure the property is defined on the object or check for its existence before accessing it.",
            ),
    );
}

/// Spec section 14.3: a plain PHP class may have a property and a method of one name, as PHP keeps the two apart. The
/// PHP# read `x.name` is the property's, of `property_type`, as in PHP, unless the code takes it as a function the
/// property's type does not fit, where the property's value would be an error: there it is the value of the method
/// `name`.
fn reads_method_over_property<A>(
    context: &Context<'_, '_, A>,
    artifacts: &AnalysisArtifacts,
    property_type: &TUnion,
    class_id: Word,
    method_name: Word,
    read_span: Span,
) -> bool
where
    A: Arena,
{
    context.dialect.is_sharp()
        && context.codebase.method_exists(class_id.as_bytes(), method_name.as_bytes())
        && artifacts.get_expected_type(&read_span).is_some_and(|expected| {
            expected.to_non_nullable().is_callable()
                && !union_comparator::is_contained_by(
                    context.codebase,
                    property_type,
                    expected,
                    false,
                    false,
                    false,
                    &mut ComparisonResult::for_dialect(context.dialect),
                )
        })
}

/// Spec section 14.3: a PHP# read `x.name` of a class with no property `name` gives its method `name` as a closure,
/// as PHP's `$x->name(...)` does, and `Class.name` gives its static method, as `Class::name(...)` does. The engine
/// runs both on the read's missing-member path. Reports a method the read cannot reach, as the call would. Returns
/// the closure's type, as `specialize_member_type` gives a call of the method on `receiver`, or on the intersection
/// part of it that has the method, with `Self` bound to `self_parameter` when the receiver is typed by that type
/// parameter. A generic method's value has the type arguments [`fix_type_arguments`] gives the read at `read_span`.
pub(crate) fn resolve_method_value<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    receiver: &TObject,
    self_parameter: Option<&TGenericParameter>,
    method_name: Word,
    access_span: Span,
    read_span: Span,
) -> Option<TUnion>
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return None;
    }

    let parts = receiver.get_intersection_types().unwrap_or_default().iter().filter_map(|part| match part {
        TAtomic::Object(object) => Some(object),
        _ => None,
    });
    let class_id = std::iter::once(receiver)
        .chain(parts)
        .filter_map(TObject::get_name)
        .find(|class_id| context.codebase.method_exists(class_id.as_bytes(), method_name.as_bytes()))?;

    let method = context.codebase.get_declaring_method_identifier(&MethodIdentifier::new(class_id, method_name));
    let class_name = method.get_class_name();
    if !check_method_visibility(
        context,
        block_context.scope.get_class_like_name(),
        class_name.as_bytes(),
        method.get_method_name().as_bytes(),
        access_span,
        None,
    ) {
        return Some(get_never());
    }

    // Spec section 9.1: a named constructor creates an object, so it is called with `new` and is no function value.
    // The read keeps the closure's type, so the one error is this one.
    if context.codebase.get_method_by_id(&method).is_some_and(|metadata| metadata.flags.is_named_constructor()) {
        let constructor = quoted_source(context, access_span);
        context.collector.report_with_code(
            IssueCode::NamedConstructorWithoutNew,
            Issue::error(format!(
                "`{constructor}` is a constructor, so it is no function value: create the object with `new {constructor}(…)`."
            ))
            .with_annotation(Annotation::primary(access_span).with_message("Read as a value here.")),
        );
    }

    artifacts.symbol_references.add_reference_to_class_member(
        &block_context.scope,
        (ascii_lowercase_word(class_name.as_bytes()), method.get_method_name()),
        false,
    );

    let codebase = context.codebase;
    let declaring_class = codebase.get_class_like(class_name.as_bytes())?;
    let method_metadata = codebase.get_method_by_id(&method)?;

    let value = specialize_member_type(
        context,
        receiver,
        self_parameter,
        declaring_class,
        class_id,
        &TemplateResult::default(),
        |options| {
            // The signature's `source` names the class the value is read through, as a call of the method does, so
            // an `extern` lookup starts there.
            let mut signature = get_signature_of_function_like_metadata(
                &FunctionLikeIdentifier::Method(class_id, method.get_method_name()),
                method_metadata,
                codebase,
                options,
            );
            signature.is_closure = true;

            TUnion::from_atomic(TAtomic::Callable(TCallable::Signature(signature)))
        },
    );
    if method_metadata.template_types.is_empty() {
        return Some(value);
    }

    let method_name = display_sharp_member(display_class_like_name(context, class_name), method_metadata.original_name);
    let template_result = unfixed_type_arguments(method_metadata);

    Some(fix_type_arguments(context, artifacts, value, method_metadata, &method_name, read_span, template_result))
}

/// Spec section 14.3: the value a read without `(…)` gives of the method `name` of a `List`, `Map`, `Set` or
/// `Iterable`, on each of `collections`, the classes [`get_receiver_collections`] finds for the receiver `object`,
/// typed as [`resolve_method_value`] types a method's value. None when one of them has no such method. The value holds
/// the collection as it was when it was read, spec section 12, so a method that changes the collection would change
/// only that copy, and it is refused, as Swift refuses the value of a `mutating` method, with the lambda that changes
/// the collection itself. The read records the method it reads for the lowering.
#[allow(clippy::too_many_arguments)]
fn resolve_collection_method_value<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    collections: &[TObject],
    object: &Expression<'_>,
    name: &LocalIdentifier<'_>,
    access_span: Span,
) -> Option<TUnion>
where
    A: Arena,
{
    let codebase = context.codebase;
    let method_name = ascii_lowercase_word(name.value);
    let methods = collections
        .iter()
        .map(|collection| {
            let class = collection.get_name()?;

            Some((class, codebase.get_method(class.as_bytes(), method_name.as_bytes())?))
        })
        .collect::<Option<Vec<_>>>()?;
    let read_span = object.span().join(name.span());

    if let Some((_, changing)) = methods.iter().find(|(_, metadata)| !metadata.flags.is_mutation_free()) {
        let written = quoted_source(context, read_span);
        let parameters: Vec<String> = changing
            .parameters
            .iter()
            .map(|parameter| parameter.name.0.as_str_lossy().trim_start_matches('$').to_owned())
            .collect();
        let lambda_parameters = if let [parameter] = parameters.as_slice() {
            parameter.clone()
        } else {
            format!("({})", parameters.join(", "))
        };
        context.collector.report_with_code(
            IssueCode::InvalidPropertyAccess,
            Issue::error(format!(
                "`{written}` changes the collection, and a function value holds a copy of it: write a lambda that calls it, as in `{lambda_parameters} => {written}({})`.",
                parameters.join(", ")
            ))
            .with_annotation(Annotation::primary(read_span).with_message("Read as a value here.")),
        );

        return Some(TUnion::from_atomic(TAtomic::Error));
    }

    let mut value: Option<TUnion> = None;
    for (collection, (class, _)) in collections.iter().zip(&methods) {
        artifacts.record_call_target(
            read_span,
            CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(*class, method_name), class: None },
        );
        let method_value = resolve_method_value(
            context,
            block_context,
            artifacts,
            collection,
            None,
            word(name.value),
            access_span,
            read_span,
        )
        .unwrap_or_else(|| unreachable!("a PHP# read of a method the collection's class declares is its value"));
        value = Some(add_optional_union_type(method_value, value.as_ref(), context.codebase));
    }

    value
}

/// Spec section 14.3: the value a read without `(…)` at `read_span` gives of the extension method `extension` on a
/// receiver of type `receiver`, spec section 26: the closure of the static method it runs as, without its first
/// parameter, the receiver. The type parameters of the extension's block take what the receiver gives them, as a call
/// infers them, and the method's own take theirs from the function type the read goes into, as
/// [`fix_type_arguments`] fixes a generic method's.
pub(crate) fn resolve_extension_value<A>(
    context: &mut Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    extension: MethodIdentifier,
    receiver: &TUnion,
    read_span: Span,
) -> TUnion
where
    A: Arena,
{
    let codebase = context.codebase;
    let class = extension.get_class_name();
    let Some(metadata) = codebase.get_method_by_id(&extension) else {
        unreachable!("an extension is a method of its class");
    };
    let mut signature = get_signature_of_function_like_metadata(
        &FunctionLikeIdentifier::Method(class, extension.get_method_name()),
        metadata,
        codebase,
        &TypeExpansionOptions::default(),
    );
    signature.is_closure = true;
    signature.parameters = signature.parameters.into_iter().skip(1).collect();
    let value = TUnion::from_atomic(TAtomic::Callable(TCallable::Signature(signature)));
    if metadata.template_types.is_empty() {
        return value;
    }

    let mut template_result = unfixed_type_arguments(metadata);
    let receiver_parameter = metadata
        .parameters
        .first()
        .and_then(|parameter| parameter.get_type_metadata())
        .map_or_else(get_mixed, |parameter| parameter.type_union.clone());
    infer_unfixed_templates(context, &receiver_parameter, receiver, &mut template_result);
    let method_name = display_sharp_member(display_class_like_name(context, class), metadata.original_name);

    fix_type_arguments(context, artifacts, value, metadata, &method_name, read_span, template_result)
}

/// The plain PHP functions that read the variables of the code that calls them, which PHP refuses to call through a
/// value, as its `zend_forbid_dynamic_call` guards them.
const READS_CALLER_VARIABLES: [&[u8]; 6] =
    [b"compact", b"extract", b"get_defined_vars", b"func_get_args", b"func_get_arg", b"func_num_args"];

/// Spec section 14.3: the value a read without `(…)` at `read_span`, written `written`, gives of the plain PHP function
/// `function` of the full name `name`, as `global.ucfirst` does: the closure PHP's `ucfirst(...)` makes, typed
/// by the function's signature, with the type arguments of a `@template` function from the function type the read goes
/// into, as [`fix_type_arguments`] fixes a generic method's. A function that changes a variable it is given, as
/// `global.sort` does, is no value, since PHP# passes every argument by value, and neither is one that reads the
/// variables of the code that calls it, as `global.compact` does, which PHP runs only when it is named in the call.
pub(crate) fn resolve_function_value<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    name: Word,
    function: &FunctionLikeMetadata,
    written: &str,
    read_span: Span,
) -> TUnion
where
    A: Arena,
{
    artifacts.symbol_references.add_reference_to_symbol(
        &block_context.scope,
        ascii_lowercase_word(name.as_bytes()),
        false,
    );
    // `extract` both reads the caller's variables and takes its array by reference, and the first is what it is for.
    let refusal = if READS_CALLER_VARIABLES.iter().any(|reader| name.as_bytes().eq_ignore_ascii_case(reader)) {
        Some((
            IssueCode::InvalidCallable,
            format!("`{written}` reads the variables of the code that calls it, so it is no function value."),
        ))
    } else if function.parameters.iter().any(|parameter| parameter.flags.is_by_reference()) {
        Some((
            IssueCode::InvalidPassByReference,
            format!(
                "`{written}` changes the variable it is given, and PHP# passes every argument by value, so it is no function value."
            ),
        ))
    } else {
        None
    };
    if let Some((code, message)) = refusal {
        context.collector.report_with_code(
            code,
            Issue::error(message).with_annotation(Annotation::primary(read_span).with_message("Read as a value here.")),
        );

        return TUnion::from_atomic(TAtomic::Error);
    }

    let mut signature = get_signature_of_function_like_metadata(
        &FunctionLikeIdentifier::Function(name),
        function,
        context.codebase,
        &TypeExpansionOptions::default(),
    );
    signature.is_closure = true;
    let value = TUnion::from_atomic(TAtomic::Callable(TCallable::Signature(signature)));
    if function.template_types.is_empty() {
        return value;
    }

    let template_result = unfixed_type_arguments(function);

    fix_type_arguments(context, artifacts, value, function, written, read_span, template_result)
}

/// The type arguments of the generic `method` before anything fixes them: each type parameter with its bound.
fn unfixed_type_arguments(method: &FunctionLikeMetadata) -> TemplateResult {
    TemplateResult::new(
        method.template_types.iter().map(|(name, template)| (*name, vec![template.clone()])).collect(),
        HashMap::default(),
    )
}

/// Spec section 14.3: a generic method's `value`, one closure signature, takes its type arguments from the parameter
/// types of the function type the read at `span` goes into, as C#'s method group conversion does, after those
/// `template_result` already holds. Returns `value` with them. A PHP# method's are recorded at `span` for the
/// lowering, and one nothing fixes is refused, as C#'s CS0411, while one a parameter type fixes outside its bound is
/// left to the bound's own issue. A plain PHP method's `@template` types never reach the engine, so its value stays
/// PHP's first-class callable, and none is recorded.
fn fix_type_arguments<A>(
    context: &mut Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    value: TUnion,
    method: &FunctionLikeMetadata,
    method_name: &str,
    span: Span,
    mut template_result: TemplateResult,
) -> TUnion
where
    A: Arena,
{
    let templates = &method.template_types;
    let mut violated_templates = WordSet::default();
    if let Some(function_type) =
        artifacts.get_expected_type(&span).map(|expected| closure_parameter_types(context, expected))
        && let TAtomic::Callable(TCallable::Signature(signature)) = value.get_single()
    {
        for (index, parameter) in signature.get_parameters().iter().enumerate() {
            if let (Some(parameter_type), Some(function_parameter_type)) =
                (parameter.get_type_signature(), function_type.get(&index))
            {
                violated_templates.extend(infer_parameter_templates_from_argument(
                    context,
                    parameter_type,
                    function_parameter_type,
                    &mut template_result,
                    index,
                    span,
                    false,
                ));
            }
        }
    }
    if !method.flags.is_sharp() {
        return inferred_type_replacer::replace(&value, &template_result, context.codebase);
    }

    let type_arguments: Vec<Option<TUnion>> = templates
        .iter()
        .map(|(name, template)| {
            template_result
                .lower_bounds
                .get(name)
                .and_then(|bounds| bounds.get(&template.defining_entity))
                .filter(|bounds| !bounds.is_empty())
                .map(|bounds| get_most_specific_type_from_bounds(bounds, context.codebase))
        })
        .collect();
    let unfixed: Vec<String> = templates
        .iter()
        .zip(&type_arguments)
        .filter(|((name, _), type_argument)| type_argument.is_none() && !violated_templates.contains(name))
        .map(|((name, _), _)| format!("`{name}`"))
        .collect();
    if !unfixed.is_empty() {
        let arguments = if unfixed.len() == 1 { "argument" } else { "arguments" };
        context.collector.report_with_code(
            IssueCode::MissingTemplateParameter,
            Issue::error(format!(
                "Nothing fixes the type {arguments} {} of the method value `{method_name}`.",
                unfixed.join(", ")
            ))
            .with_annotation(Annotation::primary(span).with_message(format!("`{method_name}` is read as a value here")))
            .with_note(
                "A method value takes its type arguments from the parameter types of the function type it goes into.",
            )
            .with_help("Give the value a function type, as a typed local or a parameter has, or call the method."),
        );

        return value;
    }

    artifacts.record_type_arguments(span, type_arguments.into_iter(), context.codebase);

    inferred_type_replacer::replace(&value, &template_result, context.codebase)
}

fn report_non_existent_property<A>(
    context: &mut Context<'_, '_, A>,
    classname: Word,
    prop_name: Word,
    selector_span: Span,
    object_span: Span,
    is_sealed_object: bool, // `true` if we are accessing undefined prop on `object{foo: string}` type, not an actual class
) where
    A: Arena,
{
    let class_kind_str = context.codebase.get_class_like(classname.as_bytes()).map_or("class", |m| m.kind.as_str());
    let classname = display_class_like_name(context, classname);

    // PHP# writes no sealed object type, so its message names the property of a class.
    if context.dialect.is_sharp() {
        let property = display_sharp_member(classname, prop_name);
        let name = prop_name.as_str_lossy();
        let name = name.trim_start_matches('$');

        context.collector.report_with_code(
            IssueCode::NonExistentProperty,
            Issue::error(format!("Property `{property}` does not exist."))
                .with_annotation(Annotation::primary(selector_span).with_message("Property not found here"))
                .with_annotation(
                    Annotation::secondary(object_span).with_message(format!("On instance of `{classname}`")),
                )
                .with_note(format!("The {class_kind_str} `{classname}` does not define the property `{name}`."))
                .with_help("Define the property in the class or check for its existence before accessing it."),
        );

        return;
    }

    context.collector.report_with_code(
        IssueCode::NonExistentProperty,
        Issue::error(if is_sealed_object {
            format!("Property `{prop_name}` does not exist on sealed object type `{classname}`.")
        } else {
            format!("Property `{prop_name}` does not exist on {class_kind_str} `{classname}`.")
        })
        .with_annotation(Annotation::primary(selector_span).with_message("Property not found here"))
        .with_annotation(Annotation::secondary(object_span).with_message(format!("On instance of `{classname}`")))
        .with_note(if is_sealed_object {
            format!("The type `{classname}` is a sealed object type and does not define the property `{prop_name}`.")
        } else {
            format!("The {class_kind_str} `{classname}` does not define the property `{prop_name}`.")
        })
        .with_help("Define the property in the class or check for its existence before accessing it."),
    );
}

pub(super) fn report_non_documented_property<A>(
    context: &mut Context<'_, '_, A>,
    obj_span: Span,
    selector_span: Span,
    classname: Word,
    property_name: Word,
    for_assignment: bool,
) where
    A: Arena,
{
    if classname.as_bytes().eq_ignore_ascii_case(b"stdClass") {
        // Special case: we don't report undocumented properties on stdClass
        return;
    }

    // Spec section 14.3: in PHP#, a property `__get` serves with no `@property` line to type it is `Any?`. A write of
    // any value to a property of unknown type is safe, so only a read warns.
    if context.dialect.is_sharp() {
        if for_assignment {
            return;
        }
        let access = quoted_source(context, obj_span.join(selector_span));
        let name = trim_start_byte(property_name.as_bytes(), b'$');
        let name = String::from_utf8_lossy(name);
        context.collector.report_with_code(
            IssueCode::NonDocumentedProperty,
            Issue::warning(format!("The type of `{access}` is unknown, so the read is `Any?`."))
                .with_annotation(Annotation::primary(selector_span).with_message("Read here."))
                .with_help(format!(
                    "State its type with `as`, as in `string? {name} = {access} as string;`, or with `is`, as in `if ({access} is string {name})`."
                )),
        );

        return;
    }

    let magic_method = if for_assignment { "__set" } else { "__get" };
    let access_type = if for_assignment { "write to" } else { "read from" };
    let classname = display_class_like_name(context, classname);

    context.collector.report_with_code(
        IssueCode::NonDocumentedProperty,
        Issue::warning(format!("Ambiguous property access: {property_name} on class `{classname}`."))
        .with_annotation(
            Annotation::primary(selector_span).with_message("This property is not explicitly defined"),
        )
        .with_annotation(
            Annotation::secondary(obj_span).with_message(format!("On an object of type `{classname}`")),
        )
        .with_note(
            format!("While this {access_type} might be handled by `{magic_method}()`, the checker cannot determine its type without a corresponding `@property` docblock tag."),
        )
        .with_help(format!(
            "To enable type checking, add a `@property`, `@property-read`, or `@property-write` tag to the docblock of the `{classname}` class. For example: `/** @property string {property_name} */`",
        )),
    );
}

/// Reports a warning when a property is found in a mixin but the target class lacks __get/__set.
/// This is a warning because a subclass might implement __get/__set.
fn report_possibly_non_existent_mixin_property<A>(
    context: &mut Context<'_, '_, A>,
    obj_span: Span,
    selector_span: Span,
    classname: Word,
    prop_name: Word,
    mixin_classname: Word,
    magic_method_name: &str,
) where
    A: Arena,
{
    let mixin_classname = display_class_like_name(context, mixin_classname);
    let classname = display_class_like_name(context, classname);
    context.collector.report_with_code(
        IssueCode::PossiblyNonExistentProperty,
        Issue::warning(format!(
            "Property `{prop_name}` might not exist on type `{classname}` at runtime."
        ))
        .with_annotation(
            Annotation::primary(selector_span).with_message("Property might not exist"),
        )
        .with_annotation(
            Annotation::secondary(obj_span).with_message(format!("On an instance of `{classname}`")),
        )
        .with_note(format!(
            "The property `{prop_name}` is defined in mixin class `{mixin_classname}`, but `{classname}` does not have a `{magic_method_name}` method to forward the access."
        ))
        .with_note(
            "A subclass of this class could implement the magic method to handle this, so the access might succeed at runtime."
        )
        .with_help(format!(
            "Add a `{magic_method_name}` method to `{classname}`, or make `{classname}` final if this should be an error."
        )),
    );
}

fn report_non_existent_mixin_property<A>(
    context: &mut Context<'_, '_, A>,
    obj_span: Span,
    selector_span: Span,
    classname: Word,
    prop_name: Word,
    mixin_classname: Word,
    magic_method_name: &str,
) where
    A: Arena,
{
    let mixin_classname = display_class_like_name(context, mixin_classname);
    let classname = display_class_like_name(context, classname);
    context.collector.report_with_code(
        IssueCode::NonExistentProperty,
        Issue::error(format!(
            "Property `{prop_name}` does not exist on final type `{classname}`."
        ))
        .with_annotation(
            Annotation::primary(selector_span).with_message("Property does not exist"),
        )
        .with_annotation(
            Annotation::secondary(obj_span).with_message(format!("On an instance of final class `{classname}`")),
        )
        .with_note(format!(
            "The property `{prop_name}` is defined in mixin class `{mixin_classname}`, but `{classname}` is final and does not have a `{magic_method_name}` method to forward the access."
        ))
        .with_help(format!(
            "Add a `{magic_method_name}` method to `{classname}` to handle mixin property accesses."
        )),
    );
}

pub(super) fn report_magic_property_without_get_set_method<A>(
    context: &mut Context<'_, '_, A>,
    obj_span: Span,
    selector_span: Span,
    classname: Word,
    property_name: Word,
    for_assignment: bool,
) where
    A: Arena,
{
    let magic_method_name = if for_assignment { "__set" } else { "__get" };
    let access_type = if for_assignment { "write to" } else { "read from" };
    let classname = display_class_like_name(context, classname);

    context.collector.report_with_code(
        IssueCode::MissingMagicMethod,
        Issue::error(format!(
            "Access to documented magic property `{property_name}` on a class that cannot handle it.",
        ))
        .with_annotation(
            Annotation::primary(selector_span)
                .with_message("This magic property is documented but cannot be accessed"),
        )
        .with_annotation(
            Annotation::secondary(obj_span).with_message(format!("Class `{classname}` is missing the `{magic_method_name}` method")),
        )
        .with_note(
            format!("The class `{classname}` has a `@property` tag for `{property_name}` but is missing a `{magic_method_name}` method to handle the {access_type}. This will cause a fatal `Error` at runtime.")
        )
        .with_help(
            format!("Add a `public function {magic_method_name}()` to the `{classname}` class to handle magic property access.")
        ),
    );
}

fn type_has_property_assertion(intersection_types: Option<&[TAtomic]>, property_name: Word) -> bool {
    intersection_types.is_some_and(|types| {
        types.iter().any(|atomic| match atomic {
            TAtomic::Object(TObject::HasProperty(has_property)) => {
                has_property.has_property(property_name)
                    || type_has_property_assertion(has_property.intersection_types.as_deref(), property_name)
            }
            TAtomic::Object(TObject::HasMethod(has_method)) => {
                type_has_property_assertion(has_method.intersection_types.as_deref(), property_name)
            }
            _ => false,
        })
    })
}

/// Searches for a property in mixin types.
/// Returns Some(ResolvedProperty) if the property is found in a mixin, None otherwise.
/// When the mixin type is a generic parameter (e.g., `@mixin T`), tries to resolve it
/// using the outer_object's type_parameters.
fn find_property_in_mixins<A>(
    context: &Context<'_, '_, A>,
    class_metadata: &ClassLikeMetadata,
    outer_object: &TObject,
    mixins: &[TypeMetadata],
    prop_name: Word,
    for_assignment: bool,
) -> Option<ResolvedProperty>
where
    A: Arena,
{
    for mixin_type in mixins {
        for mixin_atomic in mixin_type.type_union.types.as_ref() {
            match mixin_atomic {
                TAtomic::Object(TObject::Named(named)) => {
                    if let Some(result) = find_property_in_single_mixin(context, named.name, prop_name, for_assignment)
                    {
                        return Some(result);
                    }
                }
                TAtomic::Object(TObject::Enum(enum_type)) => {
                    if let Some(result) =
                        find_property_in_single_mixin(context, enum_type.name, prop_name, for_assignment)
                    {
                        return Some(result);
                    }
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
                            let class_name = match atomic {
                                TAtomic::Object(TObject::Named(named)) => named.name,
                                TAtomic::Object(TObject::Enum(enum_type)) => enum_type.name,
                                _ => continue,
                            };

                            if let Some(result) =
                                find_property_in_single_mixin(context, class_name, prop_name, for_assignment)
                            {
                                return Some(result);
                            }
                            resolved = true;
                        }
                    }

                    // Fallback to constraint if we couldn't resolve
                    if !resolved {
                        for constraint_atomic in constraint.types.as_ref() {
                            let constraint_class_name = match constraint_atomic {
                                TAtomic::Object(TObject::Named(named)) => named.name,
                                TAtomic::Object(TObject::Enum(enum_type)) => enum_type.name,
                                _ => continue,
                            };

                            if let Some(result) =
                                find_property_in_single_mixin(context, constraint_class_name, prop_name, for_assignment)
                            {
                                return Some(result);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    None
}

/// Searches for a property in a single mixin class.
fn find_property_in_single_mixin<A>(
    context: &Context<'_, '_, A>,
    mixin_class_name: Word,
    prop_name: Word,
    for_assignment: bool,
) -> Option<ResolvedProperty>
where
    A: Arena,
{
    let mixin_metadata = context.codebase.get_class_like(mixin_class_name.as_bytes())?;
    // Mixin access is always external, so a non-public real property defers to a magic
    // `@property*` annotation when the mixin class documents one.
    let resolution = resolve_property_for_external_access(context.codebase, mixin_metadata, prop_name)?;

    Some(get_resolved_declared_property(context.codebase, &resolution, prop_name, for_assignment))
}

/// Searches for a property in intersection types of a named object.
/// For example, if the type is `Foo&Baz`, this will look for the property on `Baz`.
fn find_property_in_intersection_types<A>(
    context: &Context<'_, '_, A>,
    named_object: &TNamedObject,
    prop_name: Word,
    for_assignment: bool,
) -> Option<ResolvedProperty>
where
    A: Arena,
{
    let intersection_types = named_object.get_intersection_types()?;

    for atomic in intersection_types {
        match atomic {
            TAtomic::Object(TObject::Named(TNamedObject { name, .. }) | TObject::Enum(TEnum { name, .. })) => {
                let Some(class_metadata) = context.codebase.get_class_like(name.as_bytes()) else {
                    continue;
                };

                // Intersection-member access is external, so a non-public real property defers
                // to a magic `@property*` annotation when one is documented.
                let Some(resolution) =
                    resolve_property_for_external_access(context.codebase, class_metadata, prop_name)
                else {
                    continue;
                };

                return Some(get_resolved_declared_property(context.codebase, &resolution, prop_name, for_assignment));
            }
            TAtomic::Object(TObject::WithProperties(shaped)) => {
                let key = mago_word::word(trim_start_byte(prop_name.as_bytes(), b'$'));

                if let Some((_optional, property_type)) = shaped.known_properties.get(&key) {
                    return Some(ResolvedProperty {
                        property_span: None,
                        property_name: prop_name,
                        declaring_class_id: None,
                        property_type: property_type.clone(),
                        is_magic: false,
                        read_type: None,
                    });
                }
            }
            _ => {}
        }
    }

    None
}
