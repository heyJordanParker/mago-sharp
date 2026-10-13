use foldhash::HashMap;
use foldhash::fast::RandomState;
use indexmap::IndexMap;

use mago_allocator::Arena;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::named::TNamedObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeStringKind;
use mago_codex::ttype::builder::get_class_strings_of;
use mago_codex::ttype::template::TemplateResult;
use mago_codex::ttype::union::TUnion;
use mago_codex::ttype::wrap_atomic;
use mago_span::HasSpan;
use mago_syntax::cst::Hint;
use mago_syntax::cst::TypeOf;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::invocation::template_result::seed_type_arguments;
use crate::resolver::class_name::report_non_existent_class_like;
use crate::statement::get_type_from_hint;
use crate::utils::names::display_class_like_name;

/// `typeof(X)` of a class runs as PHP's `X::class`, so it is the literal class string of `X`, and `typeof(X<…>)` the
/// class value with type arguments. Of a type parameter, it is the `Class<T>` of the type argument the code runs with.
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
        if context.resolved_names.is_type_parameter(&self.class) {
            let parameter = get_type_from_hint(context, block_context, artifacts, &Hint::Identifier(self.class));
            artifacts.record_tested_type(&self.class, parameter.clone());
            let class =
                get_class_strings_of(TClassLikeStringKind::Class, parameter, self.span()).unwrap_or_else(|_| {
                    TUnion::from_atomic(TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::any(
                        TClassLikeStringKind::Class,
                    ))))
                });
            artifacts.set_expression_type(self, class);

            return Ok(());
        }

        let mut name = word(context.resolved_names.get(&self.class));
        let metadata = context.codebase.get_class_like(name.as_bytes());
        match metadata {
            Some(metadata) => name = metadata.original_name,
            None => report_non_existent_class_like(context, self.class.span(), name),
        }

        artifacts.symbol_references.add_reference_to_symbol(
            &block_context.scope,
            ascii_lowercase_word(name.as_bytes()),
            false,
        );

        // `typeof(PaginatedList<Order>)` is a `Class<PaginatedList<Order>>`, spec section 25. A PHP# generic class's
        // value carries its type arguments while the code runs, so its type is recorded as a tested type's is.
        if let Some(type_arguments) = &self.type_arguments
            && let Some(metadata) = metadata
        {
            let type_arguments = seed_type_arguments(
                context,
                block_context,
                artifacts,
                type_arguments,
                display_class_like_name(context, name),
                metadata.name_span.unwrap_or(metadata.span),
                &metadata.template_types,
                &mut TemplateResult::new(IndexMap::with_hasher(RandomState::default()), HashMap::default()),
            );
            let class = TAtomic::Object(TObject::Named(TNamedObject::new_with_type_parameters(name, type_arguments)));
            if metadata.flags.is_sharp() && !metadata.template_types.is_empty() {
                artifacts.record_tested_type(&self.class, wrap_atomic(class.clone()));
            }
            artifacts.set_expression_type(
                self,
                wrap_atomic(TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::of_type(
                    TClassLikeStringKind::Class,
                    class,
                )))),
            );

            return Ok(());
        }

        artifacts.set_expression_type(
            self,
            TUnion::from_atomic(TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::literal(name)))),
        );

        Ok(())
    }
}
