use mago_allocator::Arena;
use mago_names::binding::php_variable_name;
use mago_names::scope::NamespaceScope;
use mago_phpdoc_syntax::cst::Document;
use mago_phpdoc_syntax::cst::TagValue;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::*;
use mago_syntax::walker::MutWalker;
use mago_word::Word;
use mago_word::word;

use crate::issue::ScanningIssueKind;
use crate::metadata::class_like::ClassLikeMetadata;
use crate::metadata::flags::MetadataFlags;
use crate::metadata::parameter::FunctionLikeParameterMetadata;
use crate::metadata::property::PropertyMetadata;
use crate::metadata::property_hook::PropertyHookMetadata;
use crate::metadata::ttype::TypeMetadata;
use crate::misc::VariableIdentifier;
use crate::scanner::Context;
use crate::scanner::attribute::scan_attribute_lists;
use crate::scanner::docblock::HookParamTag;
use crate::scanner::docblock::apply_common_metadata_flag;
use crate::scanner::docblock::find_most_trusted_tag;
use crate::scanner::docblock::parse_docblock;
use crate::scanner::inference::infer;
use crate::scanner::ttype::get_type_metadata_from_hint;
use crate::scanner::ttype::get_type_metadata_from_type;
use crate::scanner::ttype::merge_type_preserving_nullability;
use crate::scanner::typing_error_issue;
use crate::scanner::version_claim::TypeOverride;
use crate::scanner::version_claim::evaluate_version_attributes;
use crate::ttype::resolution::TypeResolutionContext;
use crate::visibility::Visibility;

#[inline]
pub fn scan_promoted_property<'arena, A>(
    parameter: &'arena FunctionLikeParameter<'arena>,
    parameter_metadata: &mut FunctionLikeParameterMetadata,
    class_like_metadata: &mut ClassLikeMetadata,
    classname: Word,
    type_context: &TypeResolutionContext,
    context: &Context<'_, 'arena, A>,
    scope: &NamespaceScope,
) -> PropertyMetadata
where
    A: Arena,
{
    debug_assert!(parameter.is_promoted_property(), "Parameter is not a promoted property");

    let name = parameter_metadata.get_name();
    let name_span = parameter_metadata.get_name_span();

    let mut flags = MetadataFlags::PROMOTED_PROPERTY;
    flags |= MetadataFlags::origin_flags(context.file.file_type);

    if parameter_metadata.flags.has_default() {
        flags |= MetadataFlags::HAS_DEFAULT;
    }

    if parameter.modifiers.contains_readonly() {
        flags |= MetadataFlags::READONLY;
    }

    if parameter.modifiers.contains_abstract() {
        flags |= MetadataFlags::ABSTRACT;
    }

    if parameter.modifiers.contains_static() {
        flags |= MetadataFlags::STATIC;
    }

    let default_type_metadata = parameter_metadata.get_default_type().cloned();

    let is_sharp = context.program.dialect.is_sharp();
    if is_sharp {
        // Spec section 9: a member a parameter declares without accessors is a field.
        let accessors = parameter.hooks.as_ref();
        flags |= sharp_flags(&parameter.modifiers, accessors, accessors.is_none(), class_like_metadata, context);
    }

    let read_visibility = context.member_visibility(&parameter.modifiers, class_like_metadata);

    let write_visibility = match (parameter.modifiers.get_first_write_visibility(), &parameter.hooks) {
        (Some(visibility), _) => Visibility::try_from(visibility).unwrap_or(Visibility::Public),
        (None, Some(accessors)) if is_sharp => sharp_write_visibility(accessors, read_visibility),
        (None, _) => {
            if parameter.modifiers.contains_readonly() {
                Visibility::Protected
            } else {
                read_visibility
            }
        }
    };

    let mut property_metadata = PropertyMetadata::new(*name, flags);

    property_metadata.attributes.clone_from(&parameter_metadata.attributes);
    property_metadata.set_default_type_metadata(default_type_metadata);
    property_metadata.set_name_span(Some(name_span));
    property_metadata.set_span(Some(parameter.span()));
    property_metadata.set_visibility(read_visibility, write_visibility);
    property_metadata.set_type_declaration_metadata(
        parameter
            .hint
            .as_ref()
            .map(|hint| get_type_metadata_from_hint(hint, Some(class_like_metadata.name), type_context, context)),
    );

    if let Some(hook_list) = &parameter.hooks {
        let has_behaviors = parameter.behaviors.is_some();
        scan_hooks(hook_list, has_behaviors, &mut property_metadata, class_like_metadata, type_context, context, scope);
    }

    let mut used_parameter_type_from_docblock = false;
    if let Some(type_metadata) = parameter_metadata.type_metadata.as_ref()
        && type_metadata.from_docblock
    {
        used_parameter_type_from_docblock = true;
        property_metadata.type_metadata = Some(type_metadata.clone());
    }

    // Check for inline @var docblock comment on the parameter
    if let Some(document) = parse_docblock(context, parameter) {
        for parse_error in document.errors {
            class_like_metadata.issues.push(
                Issue::error("Failed to parse promoted property docblock comment.")
                    .with_code(ScanningIssueKind::MalformedDocblockComment)
                    .with_annotation(Annotation::primary(parse_error.span()).with_message(parse_error.to_string()))
                    .with_note(parse_error.note())
                    .with_help(parse_error.help()),
            );
        }

        update_property_metadata_from_docblock(
            &mut property_metadata,
            &document,
            classname,
            type_context,
            scope,
            class_like_metadata,
            true,
        );

        if let Some(type_metadata) = property_metadata.type_metadata.as_ref()
            && type_metadata.from_docblock
            && !used_parameter_type_from_docblock
        {
            parameter_metadata.type_metadata = Some(type_metadata.clone());
        }
    }

    property_metadata
}

#[inline]
pub fn scan_properties<'arena, A>(
    property: &'arena Property<'arena>,
    class_like_metadata: &mut ClassLikeMetadata,
    classname: Word,
    type_context: &TypeResolutionContext,
    context: &Context<'_, 'arena, A>,
    scope: &NamespaceScope,
) -> Vec<PropertyMetadata>
where
    A: Arena,
{
    let document = parse_docblock(context, property);

    if let Some(document) = document.as_ref() {
        for parse_error in document.errors {
            class_like_metadata.issues.push(
                Issue::error("Failed to parse property docblock comment.")
                    .with_code(ScanningIssueKind::MalformedDocblockComment)
                    .with_annotation(Annotation::primary(parse_error.span()).with_message(parse_error.to_string()))
                    .with_note(parse_error.note())
                    .with_help(parse_error.help()),
            );
        }
    }

    let mut flags = MetadataFlags::origin_flags(context.file.file_type);
    let is_sharp = context.program.dialect.is_sharp();
    let type_parameters: Vec<&[u8]> = class_like_metadata.template_types.keys().map(|name| name.as_bytes()).collect();
    flags.set(
        MetadataFlags::RUNTIME_INITIAL_VALUE,
        is_sharp && property.initial_value_runs_after_class(&type_parameters),
    );

    match property {
        Property::Plain(plain_property) => {
            let verdict = evaluate_version_attributes(&plain_property.attribute_lists, context, context.php_version);
            let attributes = scan_attribute_lists(
                &plain_property.attribute_lists,
                context,
                scope,
                Some(class_like_metadata.original_name),
            );

            plain_property
                .items
                .iter()
                .map(|item| {
                    let (name, name_span, has_default, default_type) =
                        scan_property_item(item, classname, context, scope);
                    let is_required = is_sharp && is_required(&plain_property.modifiers);

                    let mut item_flags = flags;
                    if is_sharp {
                        // Spec section 6: a PHP# property without accessors is a field.
                        item_flags |= sharp_flags(&plain_property.modifiers, None, true, class_like_metadata, context);
                    }

                    if is_required {
                        item_flags |= MetadataFlags::REQUIRED;
                    }

                    if has_default {
                        item_flags |= MetadataFlags::HAS_DEFAULT;
                    }

                    if plain_property.modifiers.contains_readonly() {
                        item_flags |= MetadataFlags::READONLY;
                    }

                    if plain_property.modifiers.contains_abstract() {
                        item_flags |= MetadataFlags::ABSTRACT;
                    }

                    if plain_property.modifiers.contains_static() {
                        item_flags |= MetadataFlags::STATIC;
                    }

                    if plain_property.modifiers.contains_final() {
                        item_flags |= MetadataFlags::FINAL;
                    }

                    let read_visibility = context.member_visibility(&plain_property.modifiers, class_like_metadata);

                    let write_visibility = match plain_property.modifiers.get_first_write_visibility() {
                        Some(visibility) => Visibility::try_from(visibility).unwrap_or(Visibility::Public),
                        None => {
                            if plain_property.modifiers.contains_readonly() {
                                Visibility::Protected
                            } else {
                                read_visibility
                            }
                        }
                    };

                    let mut metadata = PropertyMetadata::new(name, item_flags);

                    metadata.attributes.clone_from(&attributes);
                    metadata.set_name_span(Some(name_span));
                    metadata.set_default_type_metadata(default_type);
                    metadata.set_visibility(read_visibility, write_visibility);
                    metadata.set_type_declaration_metadata(plain_property.hint.as_ref().map(|hint| {
                        get_type_metadata_from_hint(hint, Some(class_like_metadata.name), type_context, context)
                    }));

                    if let Some(document) = document.as_ref() {
                        update_property_metadata_from_docblock(
                            &mut metadata,
                            document,
                            classname,
                            type_context,
                            scope,
                            class_like_metadata,
                            false,
                        );
                    }

                    if matches!(verdict.type_override, Some(TypeOverride::Untyped)) {
                        metadata.type_declaration_metadata = None;
                        metadata.type_metadata = None;
                    }

                    metadata.version_constraint = verdict.constraint.clone();

                    metadata
                })
                .collect()
        }
        Property::Hooked(hooked_property) => {
            let verdict = evaluate_version_attributes(&hooked_property.attribute_lists, context, context.php_version);
            let attributes = scan_attribute_lists(
                &hooked_property.attribute_lists,
                context,
                scope,
                Some(class_like_metadata.original_name),
            );

            let (name, name_span, mut has_default, mut default_type) =
                scan_property_item(&hooked_property.item, classname, context, scope);
            if let Some(initial_value) = &hooked_property.initial_value {
                has_default = true;
                default_type = infer(context, scope, initial_value.value, Some(classname)).map(|u| {
                    let mut type_metadata = TypeMetadata::new(u, initial_value.value.span());
                    type_metadata.inferred = true;
                    type_metadata
                });
            }
            if is_sharp && is_required(&hooked_property.modifiers) {
                flags |= MetadataFlags::REQUIRED;
            }

            let read_visibility = context.member_visibility(&hooked_property.modifiers, class_like_metadata);

            let write_visibility = match hooked_property.modifiers.get_first_write_visibility() {
                Some(visibility) => Visibility::try_from(visibility).unwrap_or(Visibility::Public),
                None if is_sharp => sharp_write_visibility(&hooked_property.hook_list, read_visibility),
                None => read_visibility,
            };

            if has_default {
                flags |= MetadataFlags::HAS_DEFAULT;
            }

            if is_sharp {
                let accessors = Some(&hooked_property.hook_list);
                flags |= sharp_flags(&hooked_property.modifiers, accessors, false, class_like_metadata, context);
            }

            if hooked_property.modifiers.contains_static() {
                flags |= MetadataFlags::STATIC;
            }

            if hooked_property.modifiers.contains_abstract() {
                flags |= MetadataFlags::ABSTRACT;
            }

            if hooked_property.modifiers.contains_final() {
                flags |= MetadataFlags::FINAL;
            }

            let mut metadata = PropertyMetadata::new(name, flags);

            metadata.attributes = attributes;
            metadata.set_name_span(Some(name_span));
            metadata.set_default_type_metadata(default_type);
            metadata.set_span(Some(hooked_property.span()));
            metadata.set_visibility(read_visibility, write_visibility);
            metadata.set_type_declaration_metadata(
                hooked_property.hint.as_ref().map(|hint| {
                    get_type_metadata_from_hint(hint, Some(class_like_metadata.name), type_context, context)
                }),
            );

            if let Some(document) = document.as_ref() {
                update_property_metadata_from_docblock(
                    &mut metadata,
                    document,
                    classname,
                    type_context,
                    scope,
                    class_like_metadata,
                    false,
                );
            }

            let has_behaviors = hooked_property.behaviors.is_some();
            scan_hooks(
                &hooked_property.hook_list,
                has_behaviors,
                &mut metadata,
                class_like_metadata,
                type_context,
                context,
                scope,
            );

            if matches!(verdict.type_override, Some(TypeOverride::Untyped)) {
                metadata.type_declaration_metadata = None;
                metadata.type_metadata = None;
            }

            metadata.version_constraint = verdict.constraint;

            vec![metadata]
        }
        // A PHP# computed property runs as PHP's virtual property with one `get => expr;` hook.
        Property::Computed(computed_property) => {
            let read_visibility = context.member_visibility(&computed_property.modifiers, class_like_metadata);

            if is_sharp {
                flags |= sharp_flags(&computed_property.modifiers, None, false, class_like_metadata, context);
            }

            if computed_property.modifiers.contains_static() {
                flags |= MetadataFlags::STATIC;
            }

            let mut metadata =
                PropertyMetadata::new(VariableIdentifier(php_variable_name(computed_property.variable.name)), flags);

            metadata.attributes = scan_attribute_lists(
                &computed_property.attribute_lists,
                context,
                scope,
                Some(class_like_metadata.original_name),
            );
            metadata.set_name_span(Some(computed_property.variable.span));
            metadata.set_span(Some(computed_property.span()));
            metadata.set_visibility(read_visibility, read_visibility);
            metadata.set_type_declaration_metadata(
                computed_property.hint.as_ref().map(|hint| {
                    get_type_metadata_from_hint(hint, Some(class_like_metadata.name), type_context, context)
                }),
            );

            if let Some(document) = document.as_ref() {
                update_property_metadata_from_docblock(
                    &mut metadata,
                    document,
                    classname,
                    type_context,
                    scope,
                    class_like_metadata,
                    false,
                );
            }

            let get = word("get");
            metadata.hooks.insert(
                get,
                PropertyHookMetadata {
                    name: get,
                    span: computed_property.body.span(),
                    flags: MetadataFlags::empty(),
                    parameter: None,
                    returns_by_ref: false,
                    is_abstract: false,
                    attributes: Vec::new(),
                    return_type_metadata: None,
                    has_docblock: false,
                    issues: Vec::new(),
                },
            );
            metadata.set_is_virtual(true);

            vec![metadata]
        }
    }
}

/// The flags a PHP# property takes from its modifiers and its `accessors`, whichever form declares it: a field, a
/// property in the class body, a computed one, or one a constructor promotes. A field and a computed property have no
/// accessors.
///
/// A property is closed unless it is `virtual`, `abstract` or an `override` that is not `final`, spec sections 6.1 and
/// 22, as a method is, so the analyzer reads a closed one as final. A private property is never inherited, an
/// interface's is always open, and a field has no `virtual`, so it is always open. The engine runs a property as an
/// open PHP property unless it is `final`.
///
/// A get-only property with storage is set once, in the constructor, as `readonly` is, and one without storage is
/// computed on each read. An `init` property is written only while its object is created.
///
/// `INTERNAL_REACH` when the property is `internal`, and `INTERNAL_SET` when its `set` or `init` is, spec section 5. A
/// `set` reaches its namespace at most, spec section 6.1, so a `set` with no access modifier of a `public` or
/// `protected` property is `internal` too, unless it overrides a plain PHP parent's, which the populator reads, and an
/// `init` keeps the property's level. The property runs as PHP's `public`, so its visibility is `public`.
fn sharp_flags<A>(
    modifiers: &Sequence<'_, Modifier<'_>>,
    accessors: Option<&PropertyHookList<'_>>,
    is_field: bool,
    class_like_metadata: &ClassLikeMetadata,
    context: &Context<'_, '_, A>,
) -> MetadataFlags
where
    A: Arena,
{
    let is_open = is_field
        || class_like_metadata.kind.is_interface()
        || context.member_visibility(modifiers, class_like_metadata) == Visibility::Private
        || (!modifiers.contains_final()
            && modifiers.iter().any(|modifier| {
                matches!(modifier, Modifier::Abstract(_) | Modifier::Virtual(_) | Modifier::Override(_))
            }));
    let is_get_only = !modifiers.contains_abstract()
        && accessors.is_some_and(|accessors| accessors.is_get_only() && context.resolved_names.has_storage(accessors));
    let setter = accessors.and_then(PropertyHookList::setter);
    let is_internal_set = setter.is_some_and(|setter| {
        setter.modifiers.contains_internal()
            || (setter.name.value == b"set" && setter.modifiers.is_empty() && !modifiers.contains_private())
    });

    let mut flags = MetadataFlags::empty();
    flags.set(MetadataFlags::FIELD, is_field);
    flags.set(MetadataFlags::FINAL, !is_open);
    flags.set(MetadataFlags::READONLY, is_get_only);
    flags.set(MetadataFlags::INIT, setter.is_some_and(|setter| setter.name.value == b"init"));
    flags.set(MetadataFlags::LATE, modifiers.contains_late());
    flags.set(MetadataFlags::INTERNAL_REACH, modifiers.contains_internal());
    flags.set(MetadataFlags::INTERNAL_SET, is_internal_set);

    flags
}

/// The write visibility of a PHP# property: its `set` or `init` accessor's access modifier, or the property's own for
/// an accessor without one. An `init` property is written at that level only while its object is created, which the
/// analyzer checks, as an object initializer and `with` write it, spec section 16. A get-only property runs as
/// `readonly`, or as `protected(set)` when its `get` has a body, so it is written at `protected` at most.
fn sharp_write_visibility(accessors: &PropertyHookList, read_visibility: Visibility) -> Visibility {
    let setter = accessors.setter();
    match setter.and_then(|setter| setter.modifiers.first()).and_then(|modifier| Visibility::try_from(modifier).ok()) {
        Some(visibility) => visibility,
        None if setter.is_some() => read_visibility,
        None if read_visibility == Visibility::Public => Visibility::Protected,
        None => read_visibility,
    }
}

/// Scans a property's hooks and whether it is virtual. A PHP# auto accessor is storage, as an auto-property's is, so
/// only the accessors that [run as hooks](PropertyHook::runs_as_hook) are hooks, and every accessor of a property
/// that `has_behaviors`, whose behaviors run around its storage, spec section 6.4. A PHP# property is virtual when no
/// accessor is auto and no body uses `field`, as the engine lowers it, and a PHP one when no hook reaches
/// `$this->name`. A PHP# interface property and an abstract one run as PHP's, so their accessors are abstract hooks.
fn scan_hooks<'arena, A>(
    hook_list: &'arena PropertyHookList<'arena>,
    has_behaviors: bool,
    property: &mut PropertyMetadata,
    class_like_metadata: &mut ClassLikeMetadata,
    type_context: &TypeResolutionContext,
    context: &Context<'_, 'arena, A>,
    scope: &NamespaceScope,
) where
    A: Arena,
{
    let is_sharp =
        context.program.dialect.is_sharp() && !class_like_metadata.kind.is_interface() && !property.flags.is_abstract();
    for hook in &hook_list.hooks {
        if is_sharp && !has_behaviors && !hook.runs_as_hook() {
            continue;
        }

        let mut hook_metadata =
            scan_property_hook(hook, property, type_context, context, scope, Some(class_like_metadata.original_name));
        class_like_metadata.issues.extend(hook_metadata.take_issues());
        // An observer is the `set` hook only when the property has no `set` body, which is the hook otherwise.
        if hook.is_observer() {
            property.hooks.entry(hook_metadata.name).or_insert(hook_metadata);
        } else {
            property.hooks.insert(hook_metadata.name, hook_metadata);
        }
    }

    let is_virtual = if is_sharp {
        !context.resolved_names.has_storage(hook_list)
    } else {
        let name_bytes = property.name.0.as_bytes();
        let prop_name = name_bytes.strip_prefix(b"$").unwrap_or(name_bytes);

        !hooks_reference_backing_store(&hook_list.hooks, prop_name)
    };
    property.set_is_virtual(is_virtual);
}

/// Whether a PHP# property is written `required`, spec section 6.1: each creation of its object sets it, so it takes
/// no `null` default.
fn is_required(modifiers: &Sequence<Modifier>) -> bool {
    modifiers.iter().any(|modifier| matches!(modifier, Modifier::Required(_)))
}

fn scan_property_hook<'arena, A>(
    hook: &'arena PropertyHook<'arena>,
    property_metadata: &PropertyMetadata,
    type_context: &TypeResolutionContext,
    context: &Context<'_, 'arena, A>,
    scope: &NamespaceScope,
    classname: Option<Word>,
) -> PropertyHookMetadata
where
    A: Arena,
{
    let is_get = hook.name.value == b"get";
    // A PHP# `init` accessor's body runs as the `set` hook it lowers to, and the property's `INIT` flag keeps that it
    // writes only while the object is created. An observer runs inside that `set` hook too, spec section 6.2.
    let is_set = if context.program.dialect.is_sharp() {
        hook.is_setter() || hook.is_observer()
    } else {
        hook.name.value == b"set"
    };
    let name = if is_set { word(b"set") } else { word(hook.name.value) };
    let is_abstract = matches!(hook.body, PropertyHookBody::Abstract(_));
    let has_explicit_parameter = hook.parameter_list.is_some();

    let mut flags = MetadataFlags::empty();
    if hook.modifiers.contains_final() {
        flags |= MetadataFlags::FINAL;
    }

    let mut parameter = if is_set {
        if let Some(param_list) = &hook.parameter_list {
            param_list.parameters.first().map(|p| scan_hook_parameter(p, property_metadata, type_context, context))
        } else {
            Some(create_implicit_value_parameter(property_metadata, hook.span()))
        }
    } else {
        None
    };

    let attributes = scan_attribute_lists(&hook.attribute_lists, context, scope, classname);

    let mut has_docblock = false;
    let mut return_type_metadata = None;
    let mut issues = Vec::new();

    if let Some(document) = parse_docblock(context, hook) {
        has_docblock = true;

        for parse_error in document.errors {
            issues.push(
                Issue::error("Failed to parse property hook docblock comment.")
                    .with_code(ScanningIssueKind::MalformedDocblockComment)
                    .with_annotation(Annotation::primary(parse_error.span()).with_message(parse_error.to_string()))
                    .with_note(parse_error.note())
                    .with_help(parse_error.help()),
            );
        }

        let parameter_tag = find_most_trusted_tag(&document, |tag| match &tag.value {
            TagValue::Param(param) => Some(HookParamTag::Typed(*param)),
            TagValue::TypelessParam(param) => Some(HookParamTag::Typeless(*param)),
            _ => None,
        });

        if let Some(param_tag) = &parameter_tag {
            if !has_explicit_parameter {
                issues.push(
                    Issue::error("The `@param` tag cannot be used on a set hook without an explicit parameter.")
                        .with_code(ScanningIssueKind::InvalidParamTag)
                        .with_annotation(
                            Annotation::primary(param_tag.span())
                                .with_message("This @param cannot be applied to implicit `$value` parameter"),
                        )
                        .with_note("Set hooks without an explicit parameter use an implicit `$value` parameter that inherits the property type.")
                        .with_help("Either add an explicit parameter `set(Type $value) {}` or remove the @param tag."),
                );
            } else if let Some(ref mut param) = parameter
                && let Some(param_type) = param_tag.get_type()
            {
                let type_context = TypeResolutionContext::new();
                match get_type_metadata_from_type(param_type, None, &type_context, scope) {
                    Ok(docblock_type) => {
                        let native_type = param.type_declaration_metadata.as_ref();
                        let merged = merge_type_preserving_nullability(docblock_type, native_type);
                        param.set_type_metadata(Some(merged));
                    }
                    Err(typing_error) => {
                        issues.push(typing_error_issue(
                            "Could not resolve the type for the @param tag.",
                            ScanningIssueKind::InvalidParamTag,
                            &typing_error,
                        ));
                    }
                }
            }
        }

        let return_type_tag = find_most_trusted_tag(&document, |tag| match &tag.value {
            TagValue::Return(return_tag) => Some(*return_tag),
            _ => None,
        });

        if let Some(return_tag) = &return_type_tag
            && is_get
        {
            let type_context = TypeResolutionContext::new();
            match get_type_metadata_from_type(return_tag.r#type, None, &type_context, scope) {
                Ok(docblock_type) => {
                    return_type_metadata = Some(docblock_type);
                }
                Err(typing_error) => {
                    issues.push(typing_error_issue(
                        "Could not resolve the type for the @return tag.",
                        ScanningIssueKind::InvalidReturnTag,
                        &typing_error,
                    ));
                }
            }
        }
    }

    PropertyHookMetadata {
        name,
        span: hook.span(),
        flags,
        parameter,
        returns_by_ref: hook.ampersand.is_some(),
        is_abstract,
        attributes,
        return_type_metadata,
        has_docblock,
        issues,
    }
}

fn scan_hook_parameter<'arena, A>(
    param: &'arena FunctionLikeParameter<'arena>,
    property_metadata: &PropertyMetadata,
    type_context: &TypeResolutionContext,
    context: &Context<'_, 'arena, A>,
) -> FunctionLikeParameterMetadata
where
    A: Arena,
{
    let name = VariableIdentifier(word(param.variable.name));
    let name_span = param.variable.span;

    let mut flags = MetadataFlags::empty();
    if param.ampersand.is_some() {
        flags |= MetadataFlags::BY_REFERENCE;
    }

    let mut param_metadata = FunctionLikeParameterMetadata::new(name, param.span(), name_span, flags);

    if let Some(hint) = &param.hint {
        let type_meta = get_type_metadata_from_hint(hint, None, type_context, context);
        param_metadata.set_type_declaration_metadata(Some(type_meta));
    } else if let Some(prop_type) = &property_metadata.type_metadata {
        param_metadata.set_type_declaration_metadata(Some(prop_type.clone()));
    }

    param_metadata
}

fn create_implicit_value_parameter(property_metadata: &PropertyMetadata, span: Span) -> FunctionLikeParameterMetadata {
    let name = VariableIdentifier(word("$value"));
    let mut param = FunctionLikeParameterMetadata::new(name, span, span, MetadataFlags::empty());

    if let Some(type_meta) = &property_metadata.type_metadata {
        param.set_type_declaration_metadata(Some(type_meta.clone()));
    }

    param
}

#[inline]
pub fn scan_property_item<'arena, A>(
    property_item: &'arena PropertyItem<'arena>,
    classname: Word,
    context: &Context<'_, 'arena, A>,
    scope: &NamespaceScope,
) -> (VariableIdentifier, Span, bool, Option<TypeMetadata>)
where
    A: Arena,
{
    // A PHP# property is named without `$` and runs as the PHP property of the same name.
    match property_item {
        PropertyItem::Abstract(property_abstract_item) => {
            let name = VariableIdentifier(php_variable_name(property_abstract_item.variable.name));
            let name_span = property_abstract_item.variable.span;
            let has_default = false;
            let default_type = None;

            (name, name_span, has_default, default_type)
        }
        PropertyItem::Concrete(property_concrete_item) => {
            let name = VariableIdentifier(php_variable_name(property_concrete_item.variable.name));
            let name_span = property_concrete_item.variable.span;
            let has_default = true;
            let default_type = infer(context, scope, property_concrete_item.value, Some(classname)).map(|u| {
                let mut type_metadata = TypeMetadata::new(u, property_concrete_item.value.span());
                type_metadata.inferred = true;
                type_metadata
            });

            (name, name_span, has_default, default_type)
        }
    }
}

fn update_property_metadata_from_docblock(
    property_metadata: &mut PropertyMetadata,
    document: &Document<'_>,
    classname: Word,
    type_context: &TypeResolutionContext,
    scope: &NamespaceScope,
    class_like_metadata: &mut ClassLikeMetadata,
    allow_param_tag: bool,
) {
    for tag in document.tags() {
        if apply_common_metadata_flag(&mut property_metadata.flags, &tag.value) {
            continue;
        }

        match &tag.value {
            TagValue::Readonly(_) => {
                property_metadata.flags |= MetadataFlags::READONLY;
            }
            _ => {}
        }
    }

    let var_type = find_most_trusted_tag(document, |tag| match &tag.value {
        TagValue::Var(var) => Some(var.r#type),
        _ => None,
    });

    let docblock_type = var_type.or_else(|| {
        if !allow_param_tag {
            return None;
        }

        find_most_trusted_tag(document, |tag| match &tag.value {
            TagValue::Param(param) => Some(param.r#type),
            _ => None,
        })
    });

    if let Some(docblock_type) = docblock_type {
        match get_type_metadata_from_type(docblock_type, Some(classname), type_context, scope) {
            Ok(property_type_metadata) => {
                let real_type = property_metadata.type_declaration_metadata.as_ref();
                let property_type_metadata = merge_type_preserving_nullability(property_type_metadata, real_type);

                property_metadata.set_type_metadata(Some(property_type_metadata));
            }
            Err(typing_error) => class_like_metadata.issues.push(typing_error_issue(
                "Could not resolve the property type from its docblock.",
                ScanningIssueKind::InvalidVarTag,
                &typing_error,
            )),
        }
    }
}

/// Checks if any hook references the property's backing store, either by
/// explicitly writing or reading `$this->propertyName`, or implicitly via the
/// `set => expr` shorthand desugaring (which expands to `set { $this->propertyName = expr; }`
/// when the shorthand body does not itself contain any assignment).
fn hooks_reference_backing_store<'arena>(
    hooks: impl IntoIterator<Item = &'arena PropertyHook<'arena>>,
    property_name: &[u8],
) -> bool {
    struct Walker<'arena> {
        property_name: &'arena [u8],
        found: bool,
        assignment_seen: bool,
    }

    impl<'arena> Walker<'arena> {
        fn new(property_name: &'arena [u8]) -> Self {
            Self { property_name, found: false, assignment_seen: false }
        }

        fn check_access<'ast>(
            &mut self,
            object: &'ast Expression<'arena>,
            property: &'ast ClassLikeMemberSelector<'arena>,
        ) {
            if self.found {
                return;
            }

            let Expression::Variable(Variable::Direct(direct_variable)) = object else {
                return;
            };

            if direct_variable.name != b"$this" {
                return;
            }

            let ClassLikeMemberSelector::Identifier(identifier) = property else {
                return;
            };

            if identifier.value == self.property_name {
                self.found = true;
            }
        }
    }

    impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for Walker<'arena> {
        fn walk_in_property_access(&mut self, access: &'ast PropertyAccess<'arena>, _: &mut ()) {
            self.check_access(access.object, &access.property);
        }

        fn walk_in_null_safe_property_access(&mut self, access: &'ast NullSafePropertyAccess<'arena>, _: &mut ()) {
            self.check_access(access.object, &access.property);
        }

        fn walk_in_assignment(&mut self, _: &'ast Assignment<'arena>, _: &mut ()) {
            self.assignment_seen = true;
        }
    }

    let mut walker = Walker::new(property_name);
    for hook in hooks {
        walker.assignment_seen = false;

        walker.walk_property_hook_body(&hook.body, &mut ());
        if walker.found {
            return true;
        }

        if hook.name.value == b"set"
            && matches!(hook.body, PropertyHookBody::Concrete(PropertyHookConcreteBody::Expression(_)))
            && !walker.assignment_seen
        {
            return true;
        }
    }

    false
}
