use mago_allocator::Arena;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::TypeOf;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::resolver::class_name::report_non_existent_class_like;

/// `typeof(X)` runs as PHP's `X::class`, so it is the literal class string of `X`.
impl<'ast, 'arena> Analyzable<'ast, 'arena> for TypeOf<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let mut name = word(context.resolved_names.get(&self.class));
        match context.codebase.get_class_like(name.as_bytes()) {
            Some(metadata) => name = metadata.original_name,
            None => report_non_existent_class_like(context, self.class.span(), name),
        }

        artifacts.symbol_references.add_reference_to_symbol(
            &block_context.scope,
            ascii_lowercase_word(name.as_bytes()),
            false,
        );
        artifacts.set_expression_type(
            self,
            TUnion::from_atomic(TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::literal(name)))),
        );

        Ok(())
    }
}
