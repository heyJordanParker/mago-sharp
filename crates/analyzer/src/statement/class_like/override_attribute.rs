use mago_allocator::Arena;
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
        check_override_modifier_on_fields(metadata, members, context);
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

        if override_span.is_some() || metadata.kind.is_trait() {
            continue;
        }

        // PHP# implements an interface method without `override`, as it only replaces a parent class's method.
        let overridden_class = |parent_class_name: &[u8]| {
            context
                .codebase
                .get_class_like(parent_class_name)
                .filter(|parent_metadata| !is_sharp || !parent_metadata.kind.is_interface())
        };

        let has_non_pseudo_parent_method = parent_class_names.values().any(|parent_method_id| {
            let method_name = parent_method_id.get_method_name();

            overridden_class(parent_method_id.get_class_name().as_bytes()).is_some_and(|parent_metadata| {
                !parent_metadata.pseudo_methods.contains(&method_name)
                    && !parent_metadata.static_pseudo_methods.contains(&method_name)
            })
        });

        if !has_non_pseudo_parent_method {
            continue;
        }

        let Some(parents_metadata) = parent_class_names
            .values()
            .find_map(|parent_method_id| overridden_class(parent_method_id.get_class_name().as_bytes()))
        else {
            continue;
        };

        let parent_classname = parents_metadata.original_name;

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

/// Checks PHP#'s `override` on fields. Spec section 6.1 requires it on a field that replaces a parent's property, plain
/// PHP or PHP#, and `check_class_like_properties` checks the type and access level against the parent's declaration.
fn check_override_modifier_on_fields<'ctx, 'arena, A>(
    metadata: &'ctx ClassLikeMetadata,
    members: &[ClassLikeMember<'arena>],
    context: &mut Context<'ctx, 'arena, A>,
) where
    A: Arena,
{
    let class_name = display_class_like_name(context, metadata.original_name);
    for member in members {
        let ClassLikeMember::Property(property @ Property::Plain(field)) = member else {
            continue;
        };

        let variable = property.first_variable();
        let name = php_variable_name(variable.name);
        let field_name = mago_bytes::BytesDisplay(mago_bytes::trim_start_byte(variable.name, b'$'));
        let field_display = display_sharp_member(class_name, name);
        let override_modifier = field.modifiers.iter().find(|modifier| matches!(modifier, Modifier::Override(_)));

        match (overridden_properties(metadata, name, context.codebase).next(), override_modifier) {
            (None, Some(override_modifier)) if !metadata.has_incomplete_hierarchy() => {
                context.collector.report(
                    Issue::error(format!("Invalid `override` modifier on `{field_display}`."))
                        .with_code(IssueCode::InvalidOverrideAttribute)
                        .with_annotation(
                            Annotation::primary(override_modifier.span())
                                .with_message("This field doesn't override any parent property."),
                        )
                        .with_help(format!(
                            "Remove the `override` modifier from `{field_name}` or verify inheritance."
                        )),
                );
            }
            (Some((parent, parent_property)), Some(override_modifier))
                if parent.flags.is_sharp() && !parent_property.hooks.is_empty() =>
            {
                let parent_display = display_sharp_member(display_class_like_name(context, parent.original_name), name);

                context.collector.report(
                    Issue::error(format!("Overriding the PHP# property `{parent_display}` is not supported yet."))
                        .with_code(IssueCode::NotSupportedYet)
                        .with_annotation(
                            Annotation::primary(override_modifier.span()).with_message("Not supported yet."),
                        )
                        .with_note("A PHP# parent's property is overridden as a property.")
                        .with_note("PHP keeps the parent's accessors on a field that overrides it."),
                );
            }
            (Some((parent, _)), None) => {
                let parent_display = display_sharp_member(display_class_like_name(context, parent.original_name), name);

                context.collector.report(
                    Issue::error(format!("Missing `override` modifier on overriding field `{field_display}`."))
                        .with_code(IssueCode::MissingOverrideAttribute)
                        .with_annotation(
                            Annotation::primary(variable.span)
                                .with_message(format!("This field overrides `{parent_display}`.")),
                        )
                        .with_help("Add the `override` modifier to the field declaration."),
                );
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
