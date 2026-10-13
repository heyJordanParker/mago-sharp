use mago_allocator::Arena;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::EnumCase;
use mago_syntax::cst::EnumCaseBackedItem;
use mago_syntax::cst::EnumCaseItem;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::statement::attributes::AttributeTarget;
use crate::statement::attributes::analyze_attributes;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_member;
use crate::utils::names::display_type;
use crate::utils::names::display_value_type;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for EnumCase<'arena> {
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
            AttributeTarget::ClassLikeConstant,
        )?;

        self.item.analyze(context, block_context, artifacts)
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for EnumCaseItem<'arena> {
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
            EnumCaseItem::Unit(_) => Ok(()),
            EnumCaseItem::Backed(item) => item.analyze(context, block_context, artifacts),
        }
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for EnumCaseBackedItem<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let Some(current_enum) = block_context.scope.get_class_like() else {
            return Err(AnalysisError::InternalError(
                "Internal Error: Enum case must be analyzed within an enum scope.".to_string(),
                self.span(),
            ));
        };

        let enum_name = display_class_like_name(context, current_enum.original_name);
        let case_name = mago_bytes::BytesDisplay(self.name.value);
        let qualified_case = display_member(context, enum_name, case_name);
        // PHP names the case alone where its enum follows in the message, and PHP# names a case only as `Enum.case`.
        let case = if context.dialect.is_sharp() { qualified_case.clone() } else { case_name.to_string() };

        let Some(backing_type) = &current_enum.enum_type else {
            context.collector.report_with_code(
                IssueCode::InvalidEnumCaseValue,
                Issue::error(format!(
                    "Case `{case}` in pure enum `{enum_name}` cannot have a value."
                ))
                .with_annotation(Annotation::primary(self.value.span()).with_message("This value is not allowed"))
                .with_annotation(
                    Annotation::secondary(current_enum.name_span.unwrap_or(current_enum.span))
                        .with_message(format!("`{enum_name}` is a pure enum and does not have a backing type")),
                )
                .with_help(format!("Either declare a backing type for the enum (e.g., `enum {enum_name}: int`) or remove the value from this case.")),
            );

            return Ok(());
        };

        block_context.in_constant_expression(|block_context| self.value.analyze(context, block_context, artifacts))?;

        let Some(value_type) = artifacts.get_rc_expression_type(&self.value).cloned() else {
            context.collector.report_with_code(
                IssueCode::InvalidEnumCaseValue,
                Issue::error(format!("Could not infer the type of the value for case `{qualified_case}`."))
                    .with_annotation(Annotation::primary(self.value.span()).with_message("The type of this value could not be determined"))
                    .with_note("The value of a backed enum case must be a constant expression that resolves to either a string or an integer.")
                    .with_help("Please use a literal or a constant expression for the value."),
            );

            return Ok(());
        };

        if (backing_type.is_int() && !value_type.is_int()) || (backing_type.is_string() && !value_type.is_string()) {
            let backing_union = TUnion::from_atomic(backing_type.clone());
            let backing_type_str = display_type(context, &backing_union);
            let value_type_str = display_value_type(context, &value_type, &backing_union);

            context.collector.report_with_code(
                IssueCode::InvalidEnumCaseValue,
                Issue::error(format!(
                    "Invalid case value for `{qualified_case}`. Expected `{backing_type_str}`, but got `{value_type_str}`."
                ))
                .with_annotation(
                    Annotation::primary(self.value.span())
                        .with_message(format!("This value has the type `{value_type_str}`")),
                )
                .with_annotation(
                    Annotation::secondary(current_enum.name_span.unwrap_or(current_enum.span))
                        .with_message(format!("Enum `{enum_name}` is defined here with a `{backing_type_str}` backing type")),
                )
                .with_help(format!("Ensure the case value is a literal {backing_type_str} or a constant expression that resolves to a {backing_type_str}.")),
            );
        }

        Ok(())
    }
}
