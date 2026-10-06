use mago_allocator::Arena;
use std::rc::Rc;

use mago_codex::context::ScopeContext;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::expander::StaticClassType;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::expander::expand_union;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::wrap_atomic;
use mago_names::binding::php_variable_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::ComputedProperty;
use mago_syntax::cst::Expression;
use mago_syntax::cst::HookedProperty;
use mago_syntax::cst::PlainProperty;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyConcreteItem;
use mago_syntax::cst::PropertyHook;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::PropertyHookConcreteBody;
use mago_syntax::cst::PropertyHookConcreteExpressionBody;
use mago_syntax::cst::PropertyItem;
use mago_word::Word;
use mago_word::concat_word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::statement::analyze_statements;
use crate::statement::attributes::AttributeTarget;
use crate::statement::attributes::analyze_attributes;
use crate::statement::function_like::add_properties_to_context;
use crate::statement::function_like::get_this_type;
use crate::statement::function_like::report_missing_return;
use crate::statement::function_like::report_undefined_type_references;
use crate::statement::r#return::handle_return_value;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Property<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        match self {
            Property::Plain(plain) => plain.analyze(context, block_context, artifacts),
            Property::Hooked(hooked) => hooked.analyze(context, block_context, artifacts),
            Property::Computed(computed) => computed.analyze(context, block_context, artifacts),
        }
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for PlainProperty<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        analyze_attributes(
            context,
            block_context,
            artifacts,
            self.attribute_lists.as_slice(),
            AttributeTarget::Property,
        )?;

        for item in &self.items {
            item.analyze(context, block_context, artifacts)?;
        }

        Ok(())
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for PropertyItem<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        if let PropertyItem::Concrete(property_concrete_item) = self {
            property_concrete_item.analyze(context, block_context, artifacts)?;
        }

        Ok(())
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for PropertyConcreteItem<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        analyze_default_value(self.variable.name, self.value, context, block_context, artifacts)
    }
}

/// Analyzes a property's default value and reports it when its type is not assignable to the property's. A PHP#
/// initial value that is not constant runs in the constructor, and the same check covers it. In PHP# a `null` or
/// `false` that the type does not hold is an error too, as the engine refuses it.
fn analyze_default_value<'ctx, 'arena, A>(
    variable_name: &[u8],
    value: &Expression<'arena>,
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    if value.is_constant(&context.settings.version, false) {
        block_context.in_constant_expression(|block_context| value.analyze(context, block_context, artifacts))?;
    } else {
        value.analyze(context, block_context, artifacts)?;
    }

    if let Some(class_metadata) = block_context.scope.get_class_like()
        && let Some(property_metadata) = class_metadata.properties.get(&php_variable_name(variable_name))
        && let Some(declared_type_metadata) = property_metadata.type_metadata.as_ref()
        && !declared_type_metadata.type_union.is_mixed()
        && !declared_type_metadata.type_union.has_template_types()
        && !declared_type_metadata.type_union.is_generic_parameter()
        && let Some(value_type) = artifacts.get_expression_type(value)
        && !value_type.is_never()
    {
        let mut declared_type = declared_type_metadata.type_union.clone();
        expand_union(
            context.codebase,
            &mut declared_type,
            &TypeExpansionOptions {
                self_class: Some(class_metadata.original_name),
                static_class_type: StaticClassType::Name(class_metadata.original_name),
                ..Default::default()
            },
        );

        let mut comparison_result = ComparisonResult::new();
        if !union_comparator::is_contained_by(
            context.codebase,
            value_type,
            &declared_type,
            !context.dialect.is_sharp(),
            !context.dialect.is_sharp(),
            false,
            &mut comparison_result,
        ) {
            let value_type_str = value_type.get_id();
            let declared_type_str = declared_type.get_id();
            let class_name = class_metadata.original_name;
            let property_name = mago_bytes::BytesDisplay(variable_name);

            let issue = Issue::error(format!(
                    "Default value for property `{class_name}::{property_name}` is not assignable to its declared type."
                ))
                .with_annotation(
                    Annotation::primary(value.span())
                        .with_message(format!("This default value has type `{value_type_str}`")),
                )
                .with_annotation(
                    Annotation::secondary(declared_type_metadata.span)
                        .with_message(format!("Property is declared with type `{declared_type_str}`")),
                )
                .with_note("A property's default value must be assignable to the property's declared type.")
                .with_help("Change the default value to match the declared type, or update the property type to accept the default.");

            context.collector.report_with_code(IssueCode::InvalidPropertyDefaultValue, issue);
        }
    }

    Ok(())
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for HookedProperty<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        analyze_attributes(
            context,
            block_context,
            artifacts,
            self.attribute_lists.as_slice(),
            AttributeTarget::Property,
        )?;
        self.item.analyze(context, block_context, artifacts)?;
        if let Some(initial_value) = &self.initial_value {
            analyze_default_value(self.item.variable().name, initial_value.value, context, block_context, artifacts)?;
        }

        let property_name = php_variable_name(self.item.variable().name);
        for hook in &self.hook_list.hooks {
            analyze_property_hook(hook, property_name, context, block_context, artifacts)?;
        }

        Ok(())
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for PropertyHook<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        analyze_property_hook(self, word(""), context, block_context, artifacts)
    }
}

pub(crate) fn analyze_property_hook<'ctx, 'arena, A>(
    hook: &PropertyHook<'arena>,
    property_name: mago_word::Word,
    context: &mut Context<'ctx, 'arena, A>,
    parent_block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    analyze_attributes(
        context,
        parent_block_context,
        artifacts,
        hook.attribute_lists.as_slice(),
        AttributeTarget::Method,
    )?;

    let PropertyHookBody::Concrete(body) = &hook.body else {
        return Ok(());
    };

    let parameter_name = hook.parameter_list.as_ref().and_then(|p| p.parameters.first()).map(|p| word(p.variable.name));
    let mut hook_block_context =
        hook_block_context(hook.name.value, parameter_name, property_name, context, parent_block_context)?;

    match body {
        PropertyHookConcreteBody::Block(block) => {
            analyze_statements(block.statements.as_slice(), context, &mut hook_block_context, artifacts)?;

            // A PHP# `get` block returns its property's type on every path, as php-src types the hook.
            if context.dialect.is_sharp()
                && hook.name.value == b"get"
                && !hook_block_context.flags.has_returned()
                && let Some(class_like) = parent_block_context.scope.get_class_like()
                && let Some(property_type) =
                    class_like.properties.get(&property_name).and_then(|property| property.type_metadata.as_ref())
            {
                let accessor = concat_word!(class_like.original_name, "::", property_name, "::get");
                report_missing_return(
                    context,
                    "property hook",
                    accessor,
                    hook.name.span,
                    block.span(),
                    &property_type.type_union,
                );
            }
        }
        PropertyHookConcreteBody::Expression(expr_body) => {
            analyze_hook_expression(hook.name.value, expr_body, context, &mut hook_block_context, artifacts)?;
        }
    }

    Ok(())
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for ComputedProperty<'arena> {
    /// A PHP# computed property runs as PHP's `get => expr;` hook, and its expression is analyzed as that hook's.
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        analyze_attributes(
            context,
            block_context,
            artifacts,
            self.attribute_lists.as_slice(),
            AttributeTarget::Property,
        )?;

        let property_name = php_variable_name(self.variable.name);
        let mut hook_block_context = hook_block_context(b"get", None, property_name, context, block_context)?;

        analyze_hook_expression(b"get", &self.body, context, &mut hook_block_context, artifacts)
    }
}

/// The block context a property hook's body runs in: `$this`, the class's properties, and a `set` hook's value.
fn hook_block_context<'ctx, A>(
    hook_name: &[u8],
    parameter_name: Option<Word>,
    property_name: Word,
    context: &mut Context<'ctx, '_, A>,
    parent_block_context: &BlockContext<'ctx>,
) -> Result<BlockContext<'ctx>, AnalysisError>
where
    A: Arena,
{
    let mut scope = ScopeContext::new(parent_block_context.scope.get_reference_origin());
    scope.set_class_like(parent_block_context.scope.get_class_like());
    scope.set_static(false);

    if let Some(class_like) = parent_block_context.scope.get_class_like()
        && let Some(property) = class_like.properties.get(&property_name)
        && let Some(hook_meta) = property.hooks.get(&word(hook_name))
    {
        scope.set_property_hook(Some((property_name, hook_meta)));

        if let Some(param) = &hook_meta.parameter
            && let Some(param_type) = param.get_type_metadata()
        {
            report_undefined_type_references(context, param_type);

            // Only check native declaration if effective type is from docblock
            if param_type.from_docblock
                && let Some(param_type_decl) = param.get_type_declaration_metadata()
            {
                report_undefined_type_references(context, param_type_decl);
            }
        }
    }

    let mut hook_block_context = BlockContext::new(scope, context.settings.register_super_globals);

    if let Some(class_like_metadata) = parent_block_context.scope.get_class_like() {
        hook_block_context.locals.insert(
            Word::from("$this"),
            Rc::new(wrap_atomic(TAtomic::Object(get_this_type(context, class_like_metadata, None)))),
        );

        add_properties_to_context(context, &mut hook_block_context, class_like_metadata, None)?;
    }

    if hook_name == b"set" {
        let value_type = get_value_type_for_set_hook(property_name, parent_block_context);

        hook_block_context.locals.insert(parameter_name.unwrap_or_else(|| word(b"$value")), Rc::new(value_type));
    }

    Ok(hook_block_context)
}

/// Analyzes a hook's `=> expr;` body, which a `get` hook returns.
fn analyze_hook_expression<'ctx, 'arena, A>(
    hook_name: &[u8],
    expr_body: &PropertyHookConcreteExpressionBody<'arena>,
    context: &mut Context<'ctx, 'arena, A>,
    hook_block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    expr_body.expression.analyze(context, hook_block_context, artifacts)?;

    if hook_name == b"get" {
        let value_type =
            artifacts.get_rc_expression_type(&expr_body.expression).cloned().unwrap_or_else(|| Rc::new(get_mixed()));

        handle_return_value(
            context,
            hook_block_context,
            artifacts,
            Some(expr_body.expression),
            value_type,
            expr_body.expression.span(),
        );
    }

    Ok(())
}

fn get_value_type_for_set_hook(
    property_name: mago_word::Word,
    block_context: &BlockContext<'_>,
) -> mago_codex::ttype::union::TUnion {
    let Some(class_like) = block_context.scope.get_class_like() else {
        return get_mixed();
    };
    let Some(property) = class_like.properties.get(&property_name) else {
        return get_mixed();
    };

    if let Some(hook) = property.hooks.get(&word("set"))
        && let Some(param) = &hook.parameter
        && let Some(type_metadata) = param.get_type_metadata()
    {
        return type_metadata.type_union.clone();
    }

    property.type_metadata.as_ref().map_or_else(get_mixed, |t| t.type_union.clone())
}
