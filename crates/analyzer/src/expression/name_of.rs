use mago_allocator::Arena;
use mago_codex::ttype::get_literal_string;
use mago_names::display_sharp_member;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::NameOf;
use mago_word::ascii_lowercase_word;
use mago_word::concat_word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::resolver::class_name::report_non_existent_class_like;
use crate::resolver::property::resolve_declared_property;
use crate::utils::names::display_class_like_name;
use crate::visibility::is_visible_from_scope;

/// `nameof(X.y)` is the string `'y'`, spec section 6.3, checked against the class `X`: `y` is a property or a field
/// of `X`, its own or inherited, instance or static, visible where `nameof` is written, as C#'s `nameof` is.
impl<'ast, 'arena> Analyzable<'ast, 'arena> for NameOf<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        artifacts.set_expression_type(self, get_literal_string(word(self.property.value)));

        let name = word(context.resolved_names.get(&self.class));
        artifacts.symbol_references.add_reference_to_symbol(
            &block_context.scope,
            ascii_lowercase_word(name.as_bytes()),
            false,
        );

        let Some(class) = context.codebase.get_class_like(name.as_bytes()) else {
            report_non_existent_class_like(context, self.class.span(), name);

            return Ok(());
        };

        let scope = block_context.scope.get_class_like_name();
        let property_name = concat_word!("$", self.property.value);
        let class_name = display_class_like_name(context, class.original_name);
        let property = display_sharp_member(class_name, property_name);

        let Some(resolution) = resolve_declared_property(context.codebase, class, property_name, true, scope) else {
            if !class.has_incomplete_hierarchy() {
                context.collector.report_with_code(
                    IssueCode::NonExistentProperty,
                    Issue::error(format!("Property `{property}` does not exist."))
                        .with_annotation(Annotation::primary(self.property.span).with_message("Named here."))
                        .with_note(format!(
                            "`nameof` names a property or field that `{class_name}` declares or inherits."
                        ))
                        .with_help("Check the property's name, or declare it in the class."),
                );
            }

            return Ok(());
        };

        let visibility = resolution.property.read_visibility;
        if !resolution.is_magic()
            && !is_visible_from_scope(context.codebase, visibility, resolution.declaring_class.name.as_bytes(), scope)
        {
            context.collector.report_with_code(
                IssueCode::InvalidPropertyRead,
                Issue::error(format!("`{property}` is `{visibility}`, so `nameof` can't name it here."))
                    .with_annotation(Annotation::primary(self.property.span).with_message("Named here."))
                    .with_help("Name a property this code can read, or widen the property's access level."),
            );
        }

        artifacts.symbol_references.add_reference_to_class_member(
            &block_context.scope,
            (ascii_lowercase_word(resolution.declaring_class.name.as_bytes()), property_name),
            false,
        );

        Ok(())
    }
}
