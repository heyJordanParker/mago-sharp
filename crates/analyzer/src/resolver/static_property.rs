use mago_allocator::Arena;
use mago_word::Word;
use mago_word::concat_word;
use mago_word::word;

use mago_codex::ttype::expander;
use mago_codex::ttype::expander::StaticClassType;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::get_mixed;
use mago_names::ResolvedNames;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::StaticPropertyAccess;
use mago_syntax::cst::Variable;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::unary::cast_type_to_string;
use crate::resolver::class_name::resolve_classnames_from_expression;
use crate::resolver::property::PropertyResolutionResult;
use crate::resolver::property::ResolvedProperty;
use crate::resolver::property::resolve_declared_property;
use crate::utils::expression::get_block_expression_id;
use crate::utils::expression::get_variable_id;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_member;
use crate::visibility::check_resolved_property_write_visibility;
use crate::visibility::check_static_property_read_visibility;

/// The parts of a static property access.
///
/// PHP writes it as a [`StaticPropertyAccess`] node, `Class::$name`. PHP# writes it as a [`PropertyAccess`] node,
/// `Class.name`, whose object the binder bound to a class. Both are analyzed as these parts.
#[derive(Debug, Clone, Copy)]
pub struct StaticProperty<'ast, 'arena> {
    pub class: &'ast Expression<'arena>,
    pub name: StaticPropertyName<'ast, 'arena>,
    pub span: Span,
}

/// The name of a static property: PHP's `$name`, `${expression}` or `$$name`, or PHP#'s `name`.
#[derive(Debug, Clone, Copy)]
pub enum StaticPropertyName<'ast, 'arena> {
    Variable(&'ast Variable<'arena>),
    Identifier(&'ast LocalIdentifier<'arena>),
}

impl<'ast, 'arena> StaticProperty<'ast, 'arena> {
    #[must_use]
    pub fn from_static_property_access(access: &'ast StaticPropertyAccess<'arena>) -> Self {
        Self { class: access.class, name: StaticPropertyName::Variable(&access.property), span: access.span() }
    }

    /// Returns the static property a PHP# property access writes, `Class.name`, when
    /// [`ResolvedNames::static_property_class`] finds its class, and `None` for an instance property.
    #[must_use]
    pub fn from_property_access(
        access: &'ast PropertyAccess<'arena>,
        resolved_names: &ResolvedNames<'_>,
    ) -> Option<Self> {
        let ClassLikeMemberSelector::Identifier(name) = &access.property else {
            return None;
        };

        resolved_names.static_property_class(access).map(|_| Self {
            class: access.object,
            name: StaticPropertyName::Identifier(name),
            span: access.span(),
        })
    }
}

impl StaticPropertyName<'_, '_> {
    /// The property's name as the codebase stores it, with its `$`, when the source writes it directly.
    #[must_use]
    pub fn direct_name(&self) -> Option<Word> {
        match self {
            Self::Variable(Variable::Direct(variable)) => Some(word(variable.name)),
            Self::Variable(_) => None,
            Self::Identifier(name) => Some(concat_word!("$", name.value)),
        }
    }
}

impl HasSpan for StaticPropertyName<'_, '_> {
    fn span(&self) -> Span {
        match self {
            Self::Variable(variable) => variable.span(),
            Self::Identifier(name) => name.span,
        }
    }
}

/// Resolves all possible static properties of a static property access, checking that the scope may write each one
/// when `for_assignment` is set.
pub fn resolve_static_properties<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    access: StaticProperty<'_, 'arena>,
    for_assignment: bool,
) -> Result<PropertyResolutionResult, AnalysisError>
where
    A: Arena,
{
    let mut result = PropertyResolutionResult::default();
    let class_expression = access.class;

    let classnames = resolve_classnames_from_expression(context, block_context, artifacts, class_expression, false)?;
    if let Some(class_type) = artifacts.get_expression_type(class_expression)
        && class_type.is_nullable()
    {
        result.encountered_null = true;
    }

    let mut property_names = vec![];

    'resolve_names: {
        let (variable_type, variable_id) = match access.name {
            StaticPropertyName::Identifier(_) | StaticPropertyName::Variable(Variable::Direct(_)) => {
                property_names.extend(access.name.direct_name());

                break 'resolve_names;
            }
            StaticPropertyName::Variable(Variable::Indirect(indirect_variable)) => {
                let was_inside_general_use = block_context.flags.inside_general_use();
                block_context.flags.set_inside_general_use(true);
                indirect_variable.expression.analyze(context, block_context, artifacts)?;
                block_context.flags.set_inside_general_use(was_inside_general_use);

                (
                    artifacts.get_rc_expression_type(indirect_variable.expression).cloned(),
                    get_block_expression_id(indirect_variable.expression, context, block_context),
                )
            }
            StaticPropertyName::Variable(Variable::Nested(nested_variable)) => {
                let was_inside_general_use = block_context.flags.inside_general_use();
                block_context.flags.set_inside_general_use(true);
                nested_variable.variable.analyze(context, block_context, artifacts)?;
                block_context.flags.set_inside_general_use(was_inside_general_use);

                (
                    artifacts.get_rc_expression_type(nested_variable.variable).cloned(),
                    get_variable_id(nested_variable.variable).map(word),
                )
            }
        };

        let Some(variable_type) = variable_type else {
            result.has_invalid_path = true;
            break 'resolve_names;
        };

        let variable_type = cast_type_to_string(
            &variable_type,
            variable_id.as_ref().map(|id| id.as_bytes()),
            context,
            block_context,
            artifacts,
            access.name.span(),
        )?;

        for variable_atomic_type in variable_type.types.as_ref() {
            let Some(property_name) = variable_atomic_type.get_literal_string_value() else {
                if variable_atomic_type.is_any_string() {
                    result.has_ambiguous_path = true;
                } else {
                    result.has_invalid_path = true;
                }

                continue;
            };

            property_names.push(concat_word!("$", property_name));
        }
    };

    for resolved_classname in classnames {
        if resolved_classname.is_from_mixed() {
            result.encountered_mixed = true;
            continue;
        }

        if resolved_classname.is_possibly_invalid() {
            result.has_invalid_path = true;
            continue;
        }

        let Some(fqcn) = resolved_classname.fqcn else {
            result.has_ambiguous_path = true;
            continue;
        };

        for property_name in &property_names {
            if let Some(resolved_property) = find_static_property_in_class(
                context,
                block_context,
                fqcn,
                *property_name,
                access.name.span(),
                class_expression,
                for_assignment,
                &mut result,
            ) {
                result.properties.push(resolved_property);
            }
        }
    }

    Ok(result)
}

/// Finds a static property in a class, gets its type, and handles template localization.
fn find_static_property_in_class<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    class_id: Word,
    property_name: Word,
    name_span: Span,
    class_expr: &Expression<'arena>,
    for_assignment: bool,
    result: &mut PropertyResolutionResult,
) -> Option<ResolvedProperty>
where
    A: Arena,
{
    let Some(class_metadata) = context.codebase.get_class_like(class_id.as_bytes()) else {
        if matches!(class_expr, Expression::Parent(_))
            && block_context.scope.get_class_like().is_some_and(|metadata| metadata.has_incomplete_hierarchy())
        {
            result.has_ambiguous_path = true;
            return None;
        }

        // Error reporting for non-existent class is handled by `resolve_classnames_from_expression`.
        result.has_invalid_path = true;
        return None;
    };

    let declaring_class_id = context
        .codebase
        .get_declaring_property_class(class_id.as_bytes(), property_name.as_bytes())
        .unwrap_or(class_metadata.original_name);

    let Some(declaring_class_metadata) = context.codebase.get_class_like(declaring_class_id.as_bytes()) else {
        // Should not happen if declaring_class_id is valid.
        result.has_error_path = true;
        return None;
    };

    let Some(property_metadata) = declaring_class_metadata.properties.get(&property_name) else {
        for required_class in
            declaring_class_metadata.require_extends.iter().chain(declaring_class_metadata.require_implements.iter())
        {
            let Some(required_metadata) = context.codebase.get_class_like(required_class.as_bytes()) else {
                continue;
            };
            let required_declaring_id = context
                .codebase
                .get_declaring_property_class(required_class.as_bytes(), property_name.as_bytes())
                .unwrap_or(*required_class);
            let required_declaring_metadata =
                context.codebase.get_class_like(required_declaring_id.as_bytes()).unwrap_or(required_metadata);

            if let Some(prop_meta) = required_declaring_metadata.properties.get(&property_name) {
                if !prop_meta.flags.is_static() {
                    continue;
                }

                let mut property_type =
                    prop_meta.type_metadata.as_ref().map_or_else(get_mixed, |metadata| metadata.type_union.clone());

                expander::expand_union(
                    context.codebase,
                    &mut property_type,
                    &TypeExpansionOptions {
                        self_class: Some(required_declaring_id),
                        static_class_type: StaticClassType::Name(class_id),
                        ..Default::default()
                    },
                );

                return Some(ResolvedProperty {
                    property_span: prop_meta.name_span.or(prop_meta.span),
                    property_name,
                    declaring_class_id: Some(required_declaring_id),
                    property_type,
                    is_magic: false,
                    read_type: None,
                });
            }
        }

        if class_metadata.has_incomplete_hierarchy() {
            result.has_ambiguous_path = true;
            return None;
        }

        result.has_invalid_path = true;
        report_non_existent_property(
            context,
            declaring_class_metadata.original_name,
            property_name,
            name_span,
            class_expr.span(),
        );

        return None;
    };

    if !property_metadata.flags.is_static() {
        let classname = declaring_class_metadata.original_name;
        let issue = if context.dialect.is_sharp() {
            let property = display_member(context, classname, property_name);
            let name = property_name.as_str_lossy();
            let name = name.trim_start_matches('$');

            Issue::error(format!("Cannot access instance property `{property}` statically."))
                .with_annotation(Annotation::primary(name_span).with_message("This is an instance property"))
                .with_note("Static properties are declared with the `static` keyword and read through the class, not an instance.")
                .with_help(format!("Read this property through an instance of the class, as `value.{name}`, or declare the property as `static`."))
        } else {
            Issue::error(format!("Cannot access instance property `{classname}::{property_name}` statically."))
                .with_annotation(Annotation::primary(name_span).with_message("This is an instance property"))
                .with_note("Static properties are declared with the `static` keyword and accessed with `::` on a class name, not an instance.")
                .with_help(format!("To access this property, you need an instance of the class (e.g., `$instance->{property_name}`), or declare the property as `static`."))
        };

        context.collector.report_with_code(IssueCode::InvalidStaticPropertyAccess, issue);

        result.has_error_path = true;
        return None;
    }

    if !check_static_property_read_visibility(
        context,
        block_context,
        declaring_class_id.as_bytes(),
        property_name.as_bytes(),
        class_expr.span(),
        Some(name_span),
    ) {
        result.has_error_path = true;
        return None;
    }

    if for_assignment
        && let Some(resolution) = resolve_declared_property(
            context.codebase,
            declaring_class_metadata,
            property_name,
            false,
            block_context.scope.get_class_like_name(),
        )
        && !check_resolved_property_write_visibility(
            context,
            block_context,
            &resolution,
            class_expr.span(),
            Some(name_span),
            false,
        )
    {
        result.has_error_path = true;
        return None;
    }

    let mut property_type =
        property_metadata.type_metadata.as_ref().map_or_else(get_mixed, |metadata| metadata.type_union.clone());

    expander::expand_union(
        context.codebase,
        &mut property_type,
        &TypeExpansionOptions {
            self_class: Some(declaring_class_id),
            static_class_type: StaticClassType::Name(class_id),
            ..Default::default()
        },
    );

    Some(ResolvedProperty {
        property_span: property_metadata.name_span.or(property_metadata.span),
        property_name,
        declaring_class_id: Some(declaring_class_id),
        property_type,
        is_magic: false,
        read_type: None,
    })
}

fn report_non_existent_property<A>(
    context: &mut Context<'_, '_, A>,
    classname: Word,
    property_name: Word,
    selector_span: Span,
    class_like_name_span: Span,
) where
    A: Arena,
{
    let class_kind_str = context.codebase.get_class_like(classname.as_bytes()).map_or("class", |m| m.kind.as_str());

    // PHP# writes a constant, an enum case and a static property alike, `Class.name`, so the read names no kind.
    let issue = if context.dialect.is_sharp() {
        let member = display_member(context, classname, property_name);
        let class = display_class_like_name(context, classname);
        let name = property_name.as_str_lossy();
        let name = name.trim_start_matches('$');

        Issue::error(format!("`{member}` does not exist."))
            .with_annotation(
                Annotation::primary(selector_span).with_message("This names no constant, case or static property"),
            )
            .with_annotation(Annotation::secondary(class_like_name_span).with_message(format!(
                "The {class_kind_str} `{class}` has no constant, case or static property named `{name}`",
            )))
    } else {
        Issue::error(format!("Static property `{property_name}` does not exist on {class_kind_str} `{classname}`."))
            .with_annotation(
                Annotation::primary(selector_span)
                    .with_message("This selector refers to a non-existent static property"),
            )
            .with_annotation(Annotation::secondary(class_like_name_span).with_message(format!(
                "The {class_kind_str} `{classname}` does not have a static property named `{property_name}`",
            )))
    };

    context.collector.report_with_code(IssueCode::NonExistentProperty, issue);
}
