use mago_allocator::Arena;
use mago_codex::ttype::get_int_or_string;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_syntax::cst::DieConstruct;
use mago_syntax::cst::ExitConstruct;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::common::construct::ConstructInput;
use crate::common::construct::analyze_construct_inputs;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::context::scope::control_action::ControlAction;
use crate::error::AnalysisError;
use crate::expression::call::function_call::report_bare_function_call;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for ExitConstruct<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let is_sharp = context.dialect.is_sharp();

        analyze_construct_inputs(
            context,
            block_context,
            artifacts,
            "exit",
            self.exit.span,
            ConstructInput::ArgumentList(self.arguments.as_ref()),
            &if is_sharp { get_mixed() } else { get_int_or_string() },
            true,
            true,
            true,
        )?;

        // Spec section 23: PHP's `exit` is a plain PHP function, so a bare `exit(…)` is refused as any bare call is.
        if is_sharp && let Some(exit) = context.codebase.get_function(b"exit") {
            report_bare_function_call(context, block_context, artifacts, exit, self.arguments.as_ref(), self.exit.span);
        }

        block_context.flags.set_has_returned(true);
        block_context.control_actions.insert(ControlAction::End);

        artifacts.set_expression_type(self, get_never());

        Ok(())
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for DieConstruct<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        analyze_construct_inputs(
            context,
            block_context,
            artifacts,
            "die",
            self.die.span,
            ConstructInput::ArgumentList(self.arguments.as_ref()),
            &get_int_or_string(),
            true,
            true,
            true,
        )?;

        block_context.flags.set_has_returned(true);
        block_context.control_actions.insert(ControlAction::End);

        artifacts.set_expression_type(self, get_never());

        Ok(())
    }
}
