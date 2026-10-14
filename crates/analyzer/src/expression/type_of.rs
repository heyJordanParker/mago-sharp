use std::sync::Arc;

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
/// Of any other type, as `int`, `List<Order>` or `Self`, it is the `Class` of that type. A class value of a type the
/// code runs with, a type parameter's or a generic PHP# class's, records it as a tested type, which the engine spells.
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
        let class = match &self.r#type {
            Hint::Identifier(name) if context.resolved_names.is_type_parameter(name) => {
                let parameter = get_type_from_hint(context, block_context, artifacts, &self.r#type);
                artifacts.record_tested_type(&self.r#type, parameter.clone());
                let Ok(class) = get_class_strings_of(TClassLikeStringKind::Class, parameter, self.span()) else {
                    unreachable!("a type parameter's class values are those of its bound, which takes any type");
                };

                class
            }
            Hint::Identifier(name) => class_value(context, block_context, artifacts, name, None),
            Hint::Generic(generic) if context.resolved_names.resolve(&generic.name).is_some() => {
                class_value(context, block_context, artifacts, &generic.name, Some(&self.r#type))
            }
            // `Self` is the class a static method is called on, which PHP's `static::class` names.
            Hint::Self_(_) => class_of(&get_type_from_hint(context, block_context, artifacts, &self.r#type)),
            r#type => {
                let held = get_type_from_hint(context, block_context, artifacts, r#type);
                artifacts.record_tested_type(r#type, held.clone());

                class_of(&held)
            }
        };
        artifacts.set_expression_type(self, class);

        Ok(())
    }
}

/// The class value type of the one type `held`, a union or a nullable type included: `typeof(int|string)` is a
/// `Class<int|string>`.
fn class_of(held: &TUnion) -> TUnion {
    wrap_atomic(TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::OfType {
        kind: TClassLikeStringKind::Class,
        constraint: Arc::new(held.clone()),
    })))
}

/// The class value of the class `name` names, with the type arguments of `generic`, its generic type, when written:
/// `typeof(PaginatedList<Order>)` is a `Class<PaginatedList<Order>>`, spec section 25. A PHP# generic class's value
/// carries its type arguments while the code runs, so its type is recorded as a tested type's is. A class that does
/// not exist is reported, and its value is the literal class string of the name.
fn class_value<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    name: &impl HasSpan,
    generic: Option<&Hint<'_>>,
) -> TUnion
where
    A: Arena,
{
    let mut class = word(context.resolved_names.get(name));
    let metadata = context.codebase.get_class_like(class.as_bytes());
    match metadata {
        Some(metadata) => class = metadata.original_name,
        None => report_non_existent_class_like(context, name.span(), class),
    }

    artifacts.symbol_references.add_reference_to_symbol(
        &block_context.scope,
        ascii_lowercase_word(class.as_bytes()),
        false,
    );

    if let Some(hint @ Hint::Generic(generic)) = generic
        && let Some(metadata) = metadata
    {
        let type_arguments = seed_type_arguments(
            context,
            block_context,
            artifacts,
            &generic.type_arguments,
            display_class_like_name(context, class),
            metadata.name_span.unwrap_or(metadata.span),
            &metadata.template_types,
            &mut TemplateResult::new(IndexMap::with_hasher(RandomState::default()), HashMap::default()),
        );
        let class = TAtomic::Object(TObject::Named(TNamedObject::new_with_type_parameters(class, type_arguments)));
        if metadata.flags.is_sharp() && !metadata.template_types.is_empty() {
            artifacts.record_tested_type(hint, wrap_atomic(class.clone()));
        }

        return wrap_atomic(TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::of_type(
            TClassLikeStringKind::Class,
            class,
        ))));
    }

    TUnion::from_atomic(TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::literal(class))))
}
