use mago_allocator::Arena;
use std::rc::Rc;

use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::add_union_type;
use mago_codex::ttype::combiner::CombinerOptions;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_codex::ttype::union::TUnion;
use mago_names::display_sharp_member;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_word::Word;

use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::resolver::static_property::StaticProperty;
use crate::resolver::static_property::resolve_static_properties;
use crate::utils::get_type_diff;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_type;
use crate::utils::names::display_value_type;

pub(crate) fn analyze<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    property_access: StaticProperty<'_, 'arena>,
    assigned_value_type: &TUnion,
    property_access_id: Option<Word>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let property_resolution = resolve_static_properties(context, block_context, artifacts, property_access, true)?;

    let mut resolved_property_type = None;
    let mut matched_all_properties = true;
    let mut widened_assigned_type: Option<TUnion> = None;
    for resolved_property in property_resolution.properties {
        if let Some(declaring_class_id) = resolved_property.declaring_class_id {
            artifacts.symbol_references.add_reference_for_property_write(
                &block_context.scope,
                declaring_class_id,
                resolved_property.property_name,
            );
        }

        let mut union_comparison_result = ComparisonResult::with_strict_nonnull(context.dialect.is_sharp());

        let type_match_found = union_comparator::is_contained_by(
            context.codebase,
            assigned_value_type,
            &resolved_property.property_type,
            assigned_value_type.ignore_nullable_issues(),
            assigned_value_type.ignore_falsable_issues(),
            false,
            &mut union_comparison_result,
        );

        let property_name = resolved_property.property_name;
        // PHP# names the property as it reads, `Order.count`, and PHP keeps the name `php` gives it.
        let display_property =
            |context: &Context<'ctx, 'arena, A>, php: String| match resolved_property.declaring_class_id {
                Some(class_id) if context.dialect.is_sharp() => {
                    display_sharp_member(display_class_like_name(context, class_id), property_name)
                }
                _ => php,
            };

        if !type_match_found && union_comparison_result.type_coerced.is_none() {
            let property = display_property(
                context,
                match resolved_property.declaring_class_id {
                    Some(class_id) => format!("{class_id}::{property_name}"),
                    None => format!("<object>::{property_name}"),
                },
            );
            let mut issue = Issue::error("Invalid property assignment value").with_annotation(
                Annotation::primary(property_access.class.span()).with_message(format!(
                    "{property} with declared type {}, cannot be assigned type {}",
                    display_type(context, &resolved_property.property_type),
                    display_value_type(context, assigned_value_type, &resolved_property.property_type),
                )),
            );

            if let Some(type_diff) = get_type_diff(context, &resolved_property.property_type, assigned_value_type) {
                issue = issue.with_note(type_diff);
            }

            context.collector.report_with_code(IssueCode::InvalidPropertyAssignmentValue, issue);
        }

        if union_comparison_result.type_coerced.is_some() {
            let (code, title) = if union_comparison_result.type_coerced_from_nested_mixed.is_some() {
                (IssueCode::MixedPropertyTypeCoercion, "Mixed property type coercion")
            } else {
                (IssueCode::PropertyTypeCoercion, "Property type coercion")
            };
            let property = display_property(
                context,
                property_access_id.map_or_else(|| "This property".to_string(), |id| id.to_string()),
            );

            context.collector.report_with_code(
                code,
                Issue::error(title).with_annotation(Annotation::primary(property_access.class.span()).with_message(
                    format!(
                        "{property} expects {}, parent type {} provided",
                        display_type(context, &resolved_property.property_type),
                        display_value_type(context, assigned_value_type, &resolved_property.property_type),
                    ),
                )),
            );
        }

        if type_match_found && let Some(replacement) = union_comparison_result.replacement_union_type {
            widened_assigned_type = Some(match widened_assigned_type {
                Some(existing) => add_union_type(existing, &replacement, context.codebase, CombinerOptions::default()),
                None => replacement,
            });
        }

        if let Some(var_id) = property_access_id {
            block_context.locals.insert(var_id, Rc::new(assigned_value_type.clone()));
        }

        resolved_property_type = Some(add_optional_union_type(
            resolved_property.property_type,
            resolved_property_type.as_ref(),
            context.codebase,
        ));

        matched_all_properties &= type_match_found;
    }

    let mut resulting_type = if matched_all_properties && context.settings.memoize_properties {
        Some(widened_assigned_type.unwrap_or_else(|| assigned_value_type.clone()))
    } else {
        resolved_property_type
    };

    if property_resolution.has_ambiguous_path
        || property_resolution.encountered_mixed
        || property_resolution.has_possibly_defined_property
    {
        resulting_type = Some(add_optional_union_type(get_mixed(), resulting_type.as_ref(), context.codebase));
    }

    if property_resolution.has_error_path
        || property_resolution.has_invalid_path
        || property_resolution.encountered_null
    {
        resulting_type = Some(add_optional_union_type(get_never(), resulting_type.as_ref(), context.codebase));
    }

    let resulting_type = Rc::new(resulting_type.unwrap_or_else(get_never));

    if context.settings.memoize_properties
        && let Some(property_access_id) = property_access_id
    {
        block_context.locals.insert(property_access_id, Rc::clone(&resulting_type));
    }

    artifacts.set_rc_expression_type(&property_access.span, resulting_type);

    Ok(())
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use crate::code::IssueCode;
    use crate::test_analysis;

    test_analysis! {
        name = write_public_static_property,
        code = indoc! {r#"
            <?php
            class MyClass { public static string $prop = ""; }

            /** @param string $_s */
            function i_take_string(string $_s): void {}

            MyClass::$prop = "new value";
            i_take_string(MyClass::$prop);
        "#},
    }

    test_analysis! {
        name = write_protected_static_property_from_child,
        code = indoc! {"
            <?php
            class ParentClass { protected static int $prop = 1; }
            class ChildClass extends ParentClass {
                public static function setProp(int $val): void {
                    self::$prop = $val;
                    parent::$prop = $val + 1;
                }
            }
        "},
    }

    test_analysis! {
        name = write_private_static_property_from_same_class,
        code = indoc! {"
            <?php
            class PrivateWriteTest {
                private static int $value = 0;
                public static function setValue(int $new): void {
                    self::$value = $new;
                }
            }
        "},
        issues = [
            IssueCode::WriteOnlyProperty,
        ]
    }

    test_analysis! {
        name = write_wrong_type_to_typed_static_property,
        code = indoc! {r#"
            <?php
            class MyClass { public static string $prop = ""; }
            MyClass::$prop = 123;
        "#},
        issues = [
            IssueCode::InvalidPropertyAssignmentValue,
        ]
    }

    test_analysis! {
        name = write_to_undefined_static_property,
        code = indoc! {"
            <?php
            class MyClass {}
            MyClass::$undefined = 'new';
        "},
        issues = [
            IssueCode::NonExistentProperty,
        ]
    }

    test_analysis! {
        name = write_private_static_property_from_outside,
        code = indoc! {"
            <?php
            class PrivateWrite { private static int $value = 0; }
            PrivateWrite::$value = 1;
        "},
        issues = [
            IssueCode::UnusedProperty,
            IssueCode::InvalidPropertyRead,
        ]
    }

    test_analysis! {
        name = write_protected_static_property_from_outside,
        code = indoc! {"
            <?php
            class MyClass { protected static int $prop = 1; }
            MyClass::$prop = 500;
        "},
        issues = [
            IssueCode::InvalidPropertyRead,
        ]
    }

    test_analysis! {
        name = write_private_set_static_property_from_outside,
        code = indoc! {"
            <?php
            class Counter { public private(set) static int $count = 0; }
            Counter::$count = 1;
        "},
        issues = [
            IssueCode::InvalidPropertyWrite,
        ]
    }

    test_analysis! {
        name = assigning_static_property_with_union_type,
        code = indoc! {r#"
            <?php

            class A {
                public static null|int $x = 1;
                public static null|bool $y = true;
            }

            class B {
                public static null|float $x = 2.5;
                public static null|string $y = "hello";
            }

            /** @param 'x'|'y' $prop */
            function delta(A|B $obj, string $prop): void {
                $obj::${$prop} = null;
                $obj::$$prop = null;
            }
        "#},
    }
}
