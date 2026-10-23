use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::ttype::get_int_or_string;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_syntax::cst::DieConstruct;
use mago_syntax::cst::ExitConstruct;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::common::construct::ConstructInput;
use crate::common::construct::analyze_construct_inputs;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::context::scope::control_action::ControlAction;
use crate::error::AnalysisError;
use crate::expression::call::report_missing_function;

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

        // PHP's `exit` is a plain PHP function, so a bare `exit(…)` in a `.sharp` file is the error a bare call of one
        // gets, spec section 8.
        if is_sharp {
            report_missing_function(context, &FunctionLikeIdentifier::Function(word(b"exit")), self.exit.span);
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
