use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
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
use crate::utils::names::display_sharp_type;
use mago_word::Word;

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
                "`{}`'s header names the interfaces it implements.",
                display_class_like_name(context, class_like_metadata.original_name)
            )),
        );
    }

    // Language rule 10: `parse` reads each key into one field.
    let fields = context.codebase.struct_parse_fields(class_like_metadata.name.as_bytes());
    for (index, field) in fields.iter().enumerate() {
        let Some(first) = fields[..index].iter().find(|earlier| earlier.key == field.key) else {
            continue;
        };

        context.collector.report_with_code(
            IssueCode::InvalidStruct,
            Issue::error(format!(
                "`{}` is read twice, by `{}` and `{}`.",
                BytesDisplay(field.key),
                BytesDisplay(first.name),
                BytesDisplay(field.name)
            ))
            .with_annotation(
                Annotation::primary(field.span.unwrap_or(r#struct.name.span)).with_message("Read again here."),
            )
            .with_note("`parse` reads each key of its input into one field."),
        );
    }

    !inherits
}

/// Reports `Order.parse`, or `Order.tryParse` when it does not `throws`, on a struct with a field `parse` can't read from a value, spec section
/// 10's language rule 11: an enum with no backing value or a function type, also inside a collection or a nested
/// struct. The struct itself compiles, so code that never parses it may hold such a field.
pub(crate) fn check_struct_parse<A>(
    context: &mut Context<'_, '_, A>,
    r#struct: &ClassLikeMetadata,
    throws: bool,
    span: Span,
) where
    A: Arena,
{
    let method = if throws { "parse" } else { "tryParse" };
    let class = display_class_like_name(context, r#struct.original_name);
    for field in context.codebase.struct_parse_fields(r#struct.name.as_bytes()) {
        let Some(unreadable) = unreadable_part(context.codebase, field.r#type, &mut vec![r#struct.name]) else {
            continue;
        };
        let why =
            if let TAtomic::Object(TObject::Enum(_)) = unreadable { "has no backing value" } else { "is a function" };

        context.collector.report_with_code(
            IssueCode::InvalidStruct,
            Issue::error(format!(
                "`{class}.{method}` can't read `{}`: `{}` {why}.",
                BytesDisplay(field.name),
                display_sharp_type(context, &TUnion::from_atomic(unreadable))
            ))
            .with_annotation(Annotation::primary(span).with_message("Called here."))
            .with_note(
                "`parse` reads each field from a value, and that value names an enum case by its backing value.",
            ),
        );
    }
}

/// The part of `r#type` `parse` can't read from a value: an enum with no backing value or a function, also inside a
/// collection or a struct `parse` reads through, which `visiting` holds while it is read.
fn unreadable_part(codebase: &CodebaseMetadata, r#type: &TUnion, visiting: &mut Vec<Word>) -> Option<TAtomic> {
    r#type.types.iter().find_map(|atomic| match atomic {
        TAtomic::Object(TObject::Enum(r#enum)) => codebase
            .get_class_like(r#enum.name.as_bytes())
            .filter(|r#enum| r#enum.enum_type.is_none())
            .map(|_| atomic.clone()),
        TAtomic::Callable(_) => Some(atomic.clone()),
        TAtomic::Object(TObject::Named(object)) if object.name.as_bytes().eq_ignore_ascii_case(b"Closure") => {
            Some(atomic.clone())
        }
        TAtomic::Object(TObject::Named(object))
            if !visiting.contains(&object.name)
                && codebase.get_class_like(object.name.as_bytes()).is_some_and(|class| class.flags.is_struct()) =>
        {
            visiting.push(object.name);
            let found = codebase
                .struct_parse_fields(object.name.as_bytes())
                .iter()
                .find_map(|field| unreadable_part(codebase, field.r#type, visiting));
            visiting.pop();

            found
        }
        TAtomic::Array(array) => {
            let (key, value) = get_array_parameters(array, codebase);

            unreadable_part(codebase, &key, visiting).or_else(|| unreadable_part(codebase, &value, visiting))
        }
        _ => None,
    })
}
