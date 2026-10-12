use std::rc::Rc;

use mago_allocator::Arena;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ObjectInitializer;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::With;
use mago_word::concat_word;
use mago_word::u32_word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::common::synthetic::new_synthetic_variable;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::assignment::PropertyWriteKind;
use crate::expression::assignment::property_assignment;

/// `point with { y: 9 }` is a copy of `point` with the values it names set, spec section 16, so it has the type of
/// `point`, and each value is written to the copy as a property write.
impl<'ast, 'arena> Analyzable<'ast, 'arena> for With<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        self.object.analyze(context, block_context, artifacts)?;

        let object_type = artifacts.get_expression_type(self.object).cloned().unwrap_or_else(get_mixed);
        analyze_object_initializer(
            context,
            block_context,
            artifacts,
            self.span(),
            &object_type,
            &self.initializer,
            "`with`",
        )
    }
}

/// The prefix of the hidden local that holds the object `new` or `with` creates while its initializer is analyzed.
/// A write through it sets a property while its object is created.
pub(crate) const CREATED_OBJECT_PREFIX: &[u8] = b"$-created-";

/// Analyzes each value `initializer` names as a write to the property of its name on the object `new` or `with`
/// creates at `span`, whose type is `object_type`, spec section 16. The object is being created, so an `init` or a
/// `set` property takes the value, at its accessor's access level. A get-only property takes none, and `creator`, as
/// "`with`", names what tried. The creation at `span` has the type `object_type`.
pub(crate) fn analyze_object_initializer<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    span: Span,
    object_type: &TUnion,
    initializer: &ObjectInitializer<'arena>,
    creator: &str,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let created = concat_word!(CREATED_OBJECT_PREFIX, u32_word(span.start.offset));
    block_context.locals.insert(created, Rc::new(object_type.clone()));
    let object = context.arena.alloc(new_synthetic_variable(context.arena, created.as_bytes(), span));

    let mut named = Vec::new();
    for value in initializer.values.iter() {
        value.value.analyze(context, block_context, artifacts)?;

        let name = value.name.value;
        if named.contains(&name) {
            context.collector.report_with_code(
                IssueCode::InvalidPropertyWrite,
                Issue::error(format!("`{}` is named twice.", String::from_utf8_lossy(name)))
                    .with_annotation(Annotation::primary(value.name.span()).with_message("Named again here."))
                    .with_note("Each property takes one value when its object is created."),
            );
            continue;
        }
        named.push(name);

        if let Some(is_sharp) = get_only_class(context, object_type, name) {
            let property = String::from_utf8_lossy(name);
            let mut issue = Issue::error(format!("`{property}` is get-only, so {creator} can't set it."))
                .with_annotation(Annotation::primary(value.name.span()).with_message("Set here."));
            if is_sharp {
                issue = issue.with_help("Declare it `{ get; init; }`.");
            }

            context.collector.report_with_code(IssueCode::InvalidPropertyWrite, issue);
            continue;
        }

        let value_type = artifacts.get_expression_type(value.value).cloned().unwrap_or_else(get_mixed);
        let access = PropertyAccess {
            object,
            arrow: value.name.span(),
            property: ClassLikeMemberSelector::Identifier(value.name),
        };
        property_assignment::analyze(
            context,
            block_context,
            artifacts,
            &access,
            false,
            &value_type,
            Some(value.value),
            PropertyWriteKind::Direct,
        )?;
    }

    // A write through the hidden local types the span from it to the property's name, which is the creation's span.
    block_context.locals.remove(&created);
    artifacts.set_expression_type(&span, object_type.clone());

    Ok(())
}

/// Whether a class of `object_type` has a get-only property named `name`, with no `init` or `set`: a PHP# `{ get; }`
/// or computed property, or a plain PHP `readonly` one. Gives whether that class is PHP#'s, whose property can be
/// declared `init`.
fn get_only_class<A>(context: &Context<'_, '_, A>, object_type: &TUnion, name: &[u8]) -> Option<bool>
where
    A: Arena,
{
    let property_name = concat_word!(b"$", name);

    object_type.types.iter().find_map(|atomic| {
        let TAtomic::Object(TObject::Named(object)) = atomic else {
            return None;
        };
        let class = context.codebase.get_class_like(object.name.as_bytes())?;
        let property = context.codebase.get_declaring_property(class.name.as_bytes(), property_name.as_bytes())?;
        let settable = !property.flags.is_readonly()
            && (!property.flags.is_virtual_property() || property.hooks.contains_key(&mago_word::word("set")));

        (!settable).then(|| class.flags.is_sharp())
    })
}
