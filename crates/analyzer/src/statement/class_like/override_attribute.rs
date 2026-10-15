use mago_allocator::Arena;
use mago_codex::defaults_trait_name;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::property::PropertyMetadata;
use mago_names::binding::php_method_name;
use mago_names::binding::php_variable_name;
use mago_names::display_sharp_member;
use mago_php_version::PHPVersion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::Attribute;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Property;
use mago_text_edit::TextEdit;
use mago_word::Word;
use mago_word::ascii_lowercase_word;

use crate::code::IssueCode;
use crate::context::Context;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_member;

/// Checks the `#[Override]` attribute when the `check-missing-override` setting is on, and PHP#'s `override`
/// modifier always: spec section 22 requires `override` to replace a parent class's method.
pub fn check_override_attribute<'ctx, 'arena, A>(
    metadata: &'ctx ClassLikeMetadata,
    members: &[ClassLikeMember<'arena>],
    context: &mut Context<'ctx, 'arena, A>,
) where
    A: Arena,
{
    if context.settings.version < PHPVersion::PHP83 {
        // Override attribute not supported before PHP 8.3
        return;
    }

    let is_sharp = context.dialect.is_sharp();
    if !is_sharp && !context.settings.check_missing_override {
        return;
    }

    if is_sharp {
        check_override_modifier_on_properties(metadata, members, context);
    }

    // PHP# writes `#[\Override]` as the `override` modifier, so its messages name the modifier and propose no edit.
    let (marker, noun) = if is_sharp { ("`override`", "modifier") } else { ("`#[Override]`", "attribute") };

    let class_name = metadata.original_name;
    for member in members {
        let ClassLikeMember::Method(method) = member else {
            continue;
        };

        let (override_attribute, attribute_list_index) = 'outer: {
            for (index, attribute_list) in method.attribute_lists.iter().enumerate() {
                for attribute in &attribute_list.attributes {
                    let fqcn = context.resolved_names.get(&attribute.name);

                    if fqcn.eq_ignore_ascii_case(b"Override") {
                        break 'outer (Some(attribute), index);
                    }
                }
            }

            (None, 0)
        };

        let override_modifier = method.modifiers.iter().find(|modifier| matches!(modifier, Modifier::Override(_)));
        let override_span = override_modifier.map(HasSpan::span).or_else(|| override_attribute.map(HasSpan::span));

        let name_bytes = php_method_name(method).to_ascii_lowercase();
        let name = mago_bytes::BytesDisplay(&name_bytes);
        if name_bytes.eq_ignore_ascii_case(b"__construct") {
            if let Some(override_span) = override_span {
                let issue = Issue::error(format!("Invalid {marker} {noun} on constructor."))
                    .with_code(IssueCode::InvalidOverrideAttribute)
                    .with_annotation(
                        Annotation::primary(override_span)
                            .with_message(format!("Constructors cannot be marked with {marker}.")),
                    )
                    .with_note("PHP constructors don't override parent constructors.")
                    .with_help(format!("Remove the {marker} {noun} from the constructor."));

                propose_removal(
                    context,
                    is_sharp,
                    issue,
                    method.attribute_lists.as_slice(),
                    attribute_list_index,
                    override_attribute,
                );
            }

            continue;
        }

        let lowercase_name = ascii_lowercase_word(php_method_name(method));
        let Some(parent_class_names) = metadata.overridden_method_ids.get(&lowercase_name) else {
            if metadata.has_incomplete_hierarchy() {
                continue;
            }

            if let Some(override_span) = override_span {
                // PHP keeps the lowercase name Mago looks the method up by, and PHP# names it as it is declared.
                let method_name = if is_sharp { mago_bytes::BytesDisplay(method.name.value) } else { name };
                let method_display = display_member(context, class_name, method_name);
                let mut issue = Issue::error(format!("Invalid {marker} {noun} on `{method_display}`."))
                    .with_code(IssueCode::InvalidOverrideAttribute)
                    .with_annotation(
                        Annotation::primary(override_span)
                            .with_message("This method doesn't override any parent method."),
                    )
                    .with_note(format!("The {noun} should only be used when explicitly overriding a parent method."))
                    .with_help(format!("Remove the {marker} {noun} from `{name}` or verify inheritance."));

                if metadata.kind.is_trait() {
                    issue = issue.with_note(
                        "If this method is intended to override an interface method, add a `@require-implements` annotation to the trait."
                    );
                }

                propose_removal(
                    context,
                    is_sharp,
                    issue,
                    method.attribute_lists.as_slice(),
                    attribute_list_index,
                    override_attribute,
                );
            }

            continue;
        };

        // Spec section 22: a sub-interface's body that replaces a parent interface's default body takes `override`, as
        // a class's replacement does.
        let replaces_default_body = |parent_method_id: &MethodIdentifier| {
            metadata.kind.is_interface()
                && !matches!(method.body, MethodBody::Abstract(_))
                && context
                    .codebase
                    .get_interface(parent_method_id.get_class_name().as_bytes())
                    .and_then(|parent| {
                        context.codebase.get_trait(defaults_trait_name(parent.original_name.as_bytes()).as_bytes())
                    })
                    .is_some_and(|defaults| {
                        defaults.declaring_method_ids.contains_key(&parent_method_id.get_method_name())
                    })
        };

        // Spec section 22: implementing an abstract interface method replaces no body, so it takes no `override`.
        if is_sharp
            && let Some(override_span) = override_span
            && !parent_class_names.values().any(replaces_default_body)
            && let Some(interface_method) = parent_class_names.values().find(|parent_method_id| {
                context
                    .codebase
                    .get_class_like(parent_method_id.get_class_name().as_bytes())
                    .is_some_and(|parent| parent.kind.is_interface())
            })
            && parent_class_names.values().all(|parent_method_id| {
                context
                    .codebase
                    .get_class_like(parent_method_id.get_class_name().as_bytes())
                    .is_some_and(|parent| parent.kind.is_interface())
            })
        {
            let method_display = display_member(context, class_name, mago_bytes::BytesDisplay(method.name.value));
            let interface = context
                .codebase
                .get_class_like(interface_method.get_class_name().as_bytes())
                .map_or(interface_method.get_class_name(), |interface| interface.original_name);
            let interface_display = display_member(context, interface, mago_bytes::BytesDisplay(method.name.value));
            context.collector.report(
                Issue::error(format!(
                    "`{method_display}` implements `{interface_display}`, which takes no `override`."
                ))
                .with_code(IssueCode::InvalidOverrideAttribute)
                .with_annotation(Annotation::primary(override_span).with_message("Remove `override`."))
                .with_note(
                    "`override` marks a method that replaces a body: a base class's, or an interface's default body.",
                ),
            );

            continue;
        }

        if override_span.is_some() || metadata.kind.is_trait() {
            continue;
        }

        // PHP# implements an interface method without `override`, as it only replaces a parent class's method, and a
        // parent's named constructor is not inherited, so no method replaces it.
        let overridden_class = |parent_method_id: &MethodIdentifier| {
            context
                .codebase
                .get_class_like(parent_method_id.get_class_name().as_bytes())
                .filter(|parent_metadata| {
                    !is_sharp || !parent_metadata.kind.is_interface() || replaces_default_body(parent_method_id)
                })
                .filter(|_| {
                    !context
                        .codebase
                        .get_method_by_id(parent_method_id)
                        .is_some_and(|parent_method| parent_method.flags.is_named_constructor())
                })
        };

        let has_non_pseudo_parent_method = parent_class_names.values().any(|parent_method_id| {
            let method_name = parent_method_id.get_method_name();

            overridden_class(parent_method_id).is_some_and(|parent_metadata| {
                !parent_metadata.pseudo_methods.contains(&method_name)
                    && !parent_metadata.static_pseudo_methods.contains(&method_name)
            })
        });

        if !has_non_pseudo_parent_method {
            continue;
        }

        // A closed PHP# method's replacement already reports that no subclass replaces it, which `override` can't fix.
        if is_sharp
            && parent_class_names.values().any(|parent_method_id| {
                overridden_class(parent_method_id).is_some()
                    && context
                        .codebase
                        .get_method_by_id(parent_method_id)
                        .and_then(|parent_method| parent_method.method_metadata.as_ref())
                        .is_some_and(|parent_method| parent_method.is_final)
            })
        {
            continue;
        }

        let Some(parents_metadata) = parent_class_names.values().find_map(overridden_class) else {
            continue;
        };

        // An interface's trait of default bodies is named as its interface, which PHP# code writes.
        let parent_classname =
            context.codebase.get_defaults_interface(parents_metadata).unwrap_or(parents_metadata).original_name;

        let original_method_name = mago_bytes::BytesDisplay(method.name.value);
        let method_display = display_member(context, class_name, original_method_name);
        let parent_method_display = display_member(context, parent_classname, original_method_name);

        let issue = Issue::error(format!("Missing {marker} {noun} on overriding method `{method_display}`."))
            .with_code(IssueCode::MissingOverrideAttribute)
            .with_annotation(
                Annotation::primary(method.name.span)
                    .with_message(format!("This method overrides `{parent_method_display}`.")),
            )
            .with_note(format!("The {marker} {noun} clarifies intent and prevents accidental signature mismatches."))
            .with_help(format!("Add {marker} {noun} to method declaration."));

        if is_sharp {
            context.collector.report(issue);

            continue;
        }

        context.collector.propose(issue, |edits| {
            let offset = method.span().start.offset;
            let line_start_offset =
                context.source_file.get_line_start_offset(context.source_file.line_number(offset)).unwrap_or(offset);

            let line_slice = &context.source_file.contents[line_start_offset as usize..offset as usize];
            let indent_end = line_slice.iter().take_while(|b| b.is_ascii_whitespace()).count();
            let indent = std::str::from_utf8(&line_slice[..indent_end]).map(str::to_string).unwrap_or_default();

            edits.push(TextEdit::insert(method.start_offset(), format!("#[\\Override]\n{indent}")));
        });
    }
}

/// Checks PHP#'s `override` on properties. Spec section 6.1 requires it on a property that replaces a parent's
/// property or field, plain PHP or PHP#, and `check_class_like_properties` checks the type and access level against
/// the parent's declaration. An override is a property, so a field that replaces one names the property to write,
/// and semantics refuses `override` on a field.
fn check_override_modifier_on_properties<'ctx, 'arena, A>(
    metadata: &'ctx ClassLikeMetadata,
    members: &[ClassLikeMember<'arena>],
    context: &mut Context<'ctx, 'arena, A>,
) where
    A: Arena,
{
    let class_name = display_class_like_name(context, metadata.original_name);
    for member in members {
        let ClassLikeMember::Property(property) = member else {
            continue;
        };

        let variable = property.first_variable();
        let name = php_variable_name(variable.name);
        let property_name = mago_bytes::BytesDisplay(mago_bytes::trim_start_byte(variable.name, b'$'));
        let property_display = display_sharp_member(class_name, name);
        let is_field = matches!(property, Property::Plain(_));
        let override_modifier = property.modifiers().iter().find(|modifier| matches!(modifier, Modifier::Override(_)));

        // Spec section 22: an interface's computed property is a default body, which a class replaces with `override`,
        // as it replaces a base class's property.
        let replaced = overridden_properties(metadata, name, context.codebase)
            .next()
            .or_else(|| defaulted_properties(metadata, name, context.codebase).next());
        match (replaced, override_modifier) {
            (None, Some(override_modifier)) if !is_field && !metadata.has_incomplete_hierarchy() => {
                context.collector.report(
                    Issue::error(format!("Invalid `override` modifier on `{property_display}`."))
                        .with_code(IssueCode::InvalidOverrideAttribute)
                        .with_annotation(
                            Annotation::primary(override_modifier.span())
                                .with_message("This property doesn't override any parent property."),
                        )
                        .with_help(format!(
                            "Remove the `override` modifier from `{property_name}` or verify inheritance."
                        )),
                );
            }
            (Some((parent, _)), None) => {
                let parent_display = display_sharp_member(display_class_like_name(context, parent.original_name), name);
                let issue = if is_field {
                    Issue::error(format!(
                        "The field `{property_display}` replaces `{parent_display}`: an override is a property marked `override`."
                    ))
                    .with_help(format!("Declare `{property_name}` as a property, as in `override {property_name} {{ get; set; }}`."))
                } else {
                    Issue::error(format!("Missing `override` modifier on overriding property `{property_display}`."))
                        .with_help("Add the `override` modifier to the property declaration.")
                };

                context.collector.report(issue.with_code(IssueCode::MissingOverrideAttribute).with_annotation(
                    Annotation::primary(variable.span).with_message(format!("This overrides `{parent_display}`.")),
                ));
            }
            _ => {}
        }
    }
}

/// The parent classes that declare a property named `name` its children inherit, with their declarations, nearest
/// first and up to the root, the farthest one before a parent that declares it private.
pub(super) fn overridden_properties<'ctx>(
    metadata: &ClassLikeMetadata,
    name: Word,
    codebase: &'ctx CodebaseMetadata,
) -> impl Iterator<Item = (&'ctx ClassLikeMetadata, &'ctx PropertyMetadata)> {
    let parent_of = move |class: &ClassLikeMetadata| {
        class.direct_parent_class.and_then(|parent| codebase.get_class_like(parent.as_bytes()))
    };

    std::iter::successors(parent_of(metadata), move |parent| parent_of(parent))
        .filter_map(move |parent| Some((parent, parent.properties.get(&name)?)))
        .take_while(|(_, property)| !property.read_visibility.is_private())
}

/// The interfaces `metadata` implements that give the property named `name` a default body, spec section 22, with
/// their declarations: a computed property, whose `get` is not abstract.
fn defaulted_properties<'ctx>(
    metadata: &ClassLikeMetadata,
    name: Word,
    codebase: &'ctx CodebaseMetadata,
) -> impl Iterator<Item = (&'ctx ClassLikeMetadata, &'ctx PropertyMetadata)> {
    metadata
        .all_parent_interfaces
        .iter()
        .filter_map(move |interface| codebase.get_interface(interface.as_bytes()))
        .filter_map(move |interface| Some((interface, interface.properties.get(&name)?)))
        .filter(|(_, property)| property.hooks.values().any(|accessor| !accessor.is_abstract))
}

/// Reports `issue` about a stray override marker, with the edit that deletes a PHP `#[Override]` attribute.
fn propose_removal<A>(
    context: &mut Context<'_, '_, A>,
    is_sharp: bool,
    issue: Issue,
    attribute_lists: &[AttributeList<'_>],
    attribute_list_index: usize,
    override_attribute: Option<&Attribute<'_>>,
) where
    A: Arena,
{
    let Some(attribute) = override_attribute.filter(|_| !is_sharp) else {
        context.collector.report(issue);

        return;
    };

    context.collector.propose(issue, |edits| {
        let attribute_list = &attribute_lists[attribute_list_index];
        if attribute_list.attributes.len() == 1 {
            edits.push(TextEdit::delete(attribute_list.span()));
        } else {
            edits.push(TextEdit::delete(attribute.span()));
        }
    });
}
