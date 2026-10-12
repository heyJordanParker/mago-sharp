use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::Struct;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::statement::attributes::analyze_class_like_attributes;
use crate::statement::class_like::analyze_class_like;
use crate::statement::class_like::override_attribute;
use crate::statement::class_like::report_duplicate_definition;
use crate::utils::names::display_class_like_name;

/// A PHP# struct, spec section 10, analyzes as the final class it runs as. Its header names interfaces only, because
/// a struct inherits nothing.
impl<'ast, 'arena> Analyzable<'ast, 'arena> for Struct<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        _block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let name = context.resolved_names.get(&self.name);
        let Some(class_like_metadata) = context.codebase.get_class_like(name) else {
            tracing::warn!("Struct {} not found in codebase", BytesDisplay(name));

            return Ok(());
        };

        analyze_class_like_attributes(context, artifacts, self.attribute_lists.as_slice(), class_like_metadata)?;

        if class_like_metadata.span != self.span() {
            report_duplicate_definition(context, "Struct", "struct", name, self.span(), class_like_metadata.span);
            return Ok(());
        }

        // The class the header names is the struct's parent, which every other check would read.
        if !check_struct(self, class_like_metadata, context) {
            return Ok(());
        }

        analyze_class_like(
            context,
            artifacts,
            Some(self.name.span),
            self.span(),
            None,
            None,
            self.inheritance.as_ref(),
            class_like_metadata,
            self.members.as_slice(),
        )?;

        override_attribute::check_override_attribute(class_like_metadata, self.members.as_slice(), context);

        Ok(())
    }
}

/// Reports what a struct's declaration may not hold, spec section 10, and whether the rest of it can be analyzed as
/// the class it runs as: a struct's header names no class, since a struct inherits nothing.
fn check_struct<A>(
    r#struct: &Struct<'_>,
    class_like_metadata: &ClassLikeMetadata,
    context: &mut Context<'_, '_, A>,
) -> bool
where
    A: Arena,
{
    let mut inherits = false;
    for type_name in r#struct.inheritance.iter().flat_map(|inheritance| inheritance.types.iter()) {
        let Some(named) = context.codebase.get_class_like(context.resolved_names.get(type_name)) else {
            continue;
        };

        if named.kind.is_interface() {
            continue;
        }

        inherits = true;
        context.collector.report_with_code(
            IssueCode::InvalidStruct,
            Issue::error(format!(
                "A struct can't inherit: `{}` is a {}.",
                display_class_like_name(context, named.original_name),
                named.kind.as_str()
            ))
            .with_annotation(Annotation::primary(type_name.span()).with_message("Named here."))
            .with_note(format!(
                "`{}`'s header names the interfaces it implements, after its backing type if it has one.",
                display_class_like_name(context, class_like_metadata.original_name)
            )),
        );
    }

    !inherits
}
