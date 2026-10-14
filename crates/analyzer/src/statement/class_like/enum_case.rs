use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::context::ScopeContext;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::reference::ReferenceOrigin;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::EnumCase;
use mago_syntax::cst::EnumCaseBackedItem;
use mago_syntax::cst::EnumCaseItem;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Node;
use mago_syntax::cst::Program;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::statement::attributes::AttributeTarget;
use crate::statement::attributes::analyze_attributes;
use crate::statement::function_like::FunctionLikeBody;
use crate::statement::function_like::analyze_function_like;
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

        if let Some(enum_metadata) = block_context.scope.get_class_like()
            && !enum_metadata.kind.is_enum()
        {
            return analyze_case_class(self, enum_metadata, context, artifacts);
        }

        self.item.analyze(context, block_context, artifacts)
    }
}

/// Refuses a dotted type in a PHP# file that names no case of an enum whose cases carry data, spec section 20, as
/// `Shop.Order` for the class `App.Shop.Order`: PHP# writes a class by its short name, and its full name only in an
/// `import` line. A name that names no class at all has its own issue where the type is read.
pub(crate) fn report_dotted_types<A>(program: &Program<'_>, context: &mut Context<'_, '_, A>)
where
    A: Arena,
{
    for node in Node::Program(program).filter_map(|node| match node {
        Node::Hint(Hint::Identifier(identifier @ Identifier::Dotted(_))) => Some(*identifier),
        _ => None,
    }) {
        let Some(class) = context.codebase.get_class_like(context.resolved_names.get(&node)) else {
            continue;
        };
        if class.flags.is_enum_case_class() {
            continue;
        }

        context.collector.report_with_code(
            IssueCode::NonExistentClassLike,
            Issue::error(format!("`{}` is no case of an enum.", BytesDisplay(node.value())))
                .with_annotation(Annotation::primary(node.span()).with_message("Written here."))
                .with_note("A dotted type names a case of an enum whose cases carry data, as `PaymentResult.Paid`.")
                .with_help(format!(
                    "Import `{}` and write `{}`.",
                    display_class_like_name(context, class.original_name),
                    BytesDisplay(node.value().rsplit(|&byte| byte == b'.').next().unwrap_or_default()),
                )),
        );
    }
}

/// Analyzes a case of a PHP# enum with a case that carries data, spec section 20, which runs as a class named in the
/// enum's namespace: a class of that name another file declares clashes with the case, and a case's fields have their
/// types and defaults checked as its constructor's parameters.
fn analyze_case_class<'ctx, 'arena, A>(
    case: &EnumCase<'arena>,
    enum_metadata: &'ctx ClassLikeMetadata,
    context: &mut Context<'ctx, 'arena, A>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let name = case.item.name();
    let class_name = [enum_metadata.original_name.as_bytes(), b"\\", name.value].concat();
    let Some(case_metadata) = context.codebase.get_class_like(&class_name) else {
        return Ok(());
    };
    if case_metadata.span != case.span() {
        context.collector.report_with_code(
            IssueCode::DuplicateDefinition,
            Issue::error(format!(
                "The case `{}` runs as a class of that name, which another file declares.",
                display_member(context, enum_metadata.original_name, BytesDisplay(name.value)),
            ))
            .with_annotation(Annotation::primary(name.span).with_message("Case declared here."))
            .with_annotation(Annotation::secondary(case_metadata.span).with_message("Class declared here."))
            .with_help("Rename the case or the class."),
        );

        return Ok(());
    }
    let EnumCaseItem::Data(item) = &case.item else {
        return Ok(());
    };
    let Some(constructor) = context.codebase.get_method(&class_name, b"__construct") else {
        return Ok(());
    };

    let mut scope = ScopeContext::new(ReferenceOrigin::Symbol((case_metadata.name, constructor.name)));
    scope.set_class_like(Some(case_metadata));
    scope.set_function_like(Some(constructor));
    let mut block_context = BlockContext::new(scope, context.settings.register_super_globals);

    analyze_function_like(
        context,
        artifacts,
        &mut block_context,
        constructor,
        &item.parameter_list,
        None,
        FunctionLikeBody::Statements(&[], item.parameter_list.span()),
        None,
    )
    .map(drop)
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
            EnumCaseItem::Unit(_) | EnumCaseItem::Data(_) => Ok(()),
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
