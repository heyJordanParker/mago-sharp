use mago_allocator::Arena;
use mago_codex::scanner::inference::get_literal_constant_type;
use mago_codex::scanner::inference::get_platform_constant_type;
use mago_codex::ttype::expander;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::get_mixed;
use mago_names::binding::Binding;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::Access;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Expression;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::Variable;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::variable::read_variable;
use crate::utils::expression::get_bare_name_variable_id;
use mago_bytes::BytesDisplay;

impl<'arena> Analyzable<'_, 'arena> for ConstantAccess<'arena> {
    fn analyze<'ctx, A>(
        &self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        if let Some(variable_id) = get_bare_name_variable_id(&self.name, context.resolved_names) {
            let resulting_type = read_variable(context, block_context, artifacts, variable_id.as_bytes(), self.span());
            artifacts.set_rc_expression_type(self, resulting_type);

            return Ok(());
        }

        if context.resolved_names.binding(&self.name) == Some(Binding::Field) {
            match field_storage(self, context, block_context) {
                Some(storage) => storage.analyze(context, block_context, artifacts)?,
                None => artifacts.set_expression_type(self, get_mixed()),
            }

            return Ok(());
        }

        // The semantic checks report a bare member, and a class is only ever the object of a member access.
        if let Some(Binding::Class | Binding::Member) = context.resolved_names.binding(&self.name) {
            artifacts.set_expression_type(self, get_mixed());

            return Ok(());
        }

        let name_bytes = context.resolved_names.get(self);
        let name = BytesDisplay(name_bytes);
        let unqualified_name = self.name.value();

        let constant_metadata = context.codebase.get_constant_or_global(name_bytes, unqualified_name);

        let Some(constant_metadata) = constant_metadata else {
            if let Some(literal_type) = get_literal_constant_type(name_bytes) {
                artifacts.set_expression_type(self, literal_type);
                return Ok(());
            }

            let is_known = block_context.known_constants.contains(&word(name_bytes));

            if is_known {
                artifacts.set_expression_type(self, get_mixed());
            } else {
                context.collector.report_with_code(
                    IssueCode::NonExistentConstant,
                    Issue::error(format!(
                        "Undefined constant: `{name}`."
                    ))
                    .with_annotation(
                        Annotation::primary(self.span())
                            .with_message(format!("Constant `{name}` is not defined."))
                    )
                    .with_note(
                        "The constant might be misspelled, not defined, or not imported."
                    )
                    .with_help(
                        format!(
                            "Define the constant `{name}` using `define()` or `const`, or check for typos and ensure it's available in this scope."
                        )
                    ),
                );
            }

            return Ok(());
        };

        if constant_metadata.flags.is_deprecated() {
            context.collector.report_with_code(
                IssueCode::DeprecatedConstant,
                Issue::warning(format!("Using deprecated constant: `{name}`."))
                    .with_annotation(Annotation::primary(self.span()).with_message("This constant is deprecated."))
                    .with_note("Consider using an alternative constant or variable.")
                    .with_help("Check `{name}` documentation for alternatives or updates."),
            );
        }

        crate::utils::experimental::check_experimental_constant(
            context,
            block_context,
            &constant_metadata.flags,
            name_bytes,
            self.span(),
        );

        crate::utils::availability::check_constant_availability(context, constant_metadata, &name, self.span());

        let mut constant_type = if let Some(t) = get_platform_constant_type(name_bytes) {
            t
        } else {
            match &constant_metadata.type_metadata {
                Some(t) => t.type_union.clone(),
                None => match &constant_metadata.inferred_type {
                    Some(t) => t.clone(),
                    _ => get_mixed(),
                },
            }
        };

        expander::expand_union(context.codebase, &mut constant_type, &TypeExpansionOptions::default());

        artifacts.set_expression_type(self, constant_type);

        Ok(())
    }
}

/// The property access that `field` runs as in its accessor: `$this->name`, which PHP reads and writes as the storage
/// inside the property's own hook. It has the span of `field`, so its type is the type of `field`. Returns `None`
/// outside an accessor, where the semantic checks report `field`.
pub(crate) fn field_storage<'arena, A>(
    field: &ConstantAccess<'arena>,
    context: &Context<'_, 'arena, A>,
    block_context: &BlockContext<'_>,
) -> Option<&'arena Expression<'arena>>
where
    A: Arena,
{
    let (property, _) = block_context.scope.get_property_hook()?;
    let name = context.arena.alloc_slice_copy(property.as_bytes().strip_prefix(b"$")?);
    let span = field.span();
    let this = context.arena.alloc(Expression::Variable(Variable::Direct(DirectVariable { span, name: b"$this" })));

    Some(context.arena.alloc(Expression::Access(Access::Property(PropertyAccess {
        object: this,
        arrow: span,
        property: ClassLikeMemberSelector::Identifier(LocalIdentifier { span, value: name }),
    }))))
}
