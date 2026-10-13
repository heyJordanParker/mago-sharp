use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::class_like::ParsedField;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
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
        // The scanner registers every struct, or the class-like declared first under its name.
        let name = context.resolved_names.get(&self.name);

        context
            .codebase
            .get_class_like(name)
            .map_or(Ok(()), |class_like_metadata| analyze_struct(self, class_like_metadata, context, artifacts))
    }
}

fn analyze_struct<'ctx, 'arena, A>(
    r#struct: &Struct<'arena>,
    class_like_metadata: &'ctx ClassLikeMetadata,
    context: &mut Context<'ctx, 'arena, A>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    analyze_class_like_attributes(context, artifacts, r#struct.attribute_lists.as_slice(), class_like_metadata)?;

    // A class-like declared first in the file under the struct's name keeps it.
    if class_like_metadata.span != r#struct.span() {
        let name = context.resolved_names.get(&r#struct.name);
        report_duplicate_definition(context, "Struct", "struct", name, r#struct.span(), class_like_metadata.span);
        return Ok(());
    }

    // The class the header names is the struct's parent, which every other check would read.
    if !check_struct(r#struct, class_like_metadata, context) {
        return Ok(());
    }

    analyze_class_like(
        context,
        artifacts,
        Some(r#struct.name.span),
        r#struct.span(),
        None,
        None,
        r#struct.inheritance.as_ref(),
        class_like_metadata,
        r#struct.members.as_slice(),
    )?;

    override_attribute::check_override_attribute(class_like_metadata, r#struct.members.as_slice(), context);

    Ok(())
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

    if context.codebase.parses(class_like_metadata.name.as_bytes()) {
        report_keys_read_twice(context, class_like_metadata, r#struct.name.span);
    }

    !inherits
}

/// Reports each key the `parse` of `class_like_metadata`, a struct or a PHP# class, would read into two fields,
/// spec section 10's language rule 10, at the second field, or at `name` where the codex has no span for it. The
/// parameters come first, so the first reader of a key is a main constructor parameter, the one `[Key]` can rename.
pub(crate) fn report_keys_read_twice<A>(
    context: &mut Context<'_, '_, A>,
    class_like_metadata: &ClassLikeMetadata,
    name: Span,
) where
    A: Arena,
{
    let fields = context.codebase.struct_parse_fields(class_like_metadata.name.as_bytes());
    let reader = |field: &ParsedField<'_>| {
        let kind = if field.is_parameter { "the main constructor parameter" } else { "the property" };

        format!("{kind} `{}`", BytesDisplay(field.name))
    };
    for (index, field) in fields.iter().enumerate() {
        let Some(first) = fields[..index].iter().find(|earlier| earlier.key == field.key) else {
            continue;
        };

        context.collector.report_with_code(
            IssueCode::InvalidStruct,
            Issue::error(format!(
                "`{}` is read twice: by {} and {}.",
                BytesDisplay(field.key),
                reader(first),
                reader(field)
            ))
            .with_annotation(Annotation::primary(field.span.unwrap_or(name)).with_message("Read again here."))
            .with_note("`parse` reads each key of its input into one field.")
            .with_help(format!("Give `{}` another key with `[Key(\"…\")]`.", BytesDisplay(first.name))),
        );
    }
}

/// Reports `Order.parse`, or `Order.tryParse` when it does not `throws`, on a struct or a PHP# class with a field
/// `parse` can't read from a value, spec section 10's language rule 11: an enum with no backing value, a function
/// type, or a class no `parse` builds, also inside a collection or a nested struct or class. The type itself compiles,
/// so code that never parses it may hold such a field.
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
        let Some((unreadable, why)) = unreadable_part(context.codebase, field.r#type, &mut vec![r#struct.name]) else {
            continue;
        };

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

/// Whether the struct `name` declares `operator ==`, which replaces the `==` that compares it property by property and,
/// with the `hash()` it then declares, keys a `Map` and joins a `Set`.
pub(crate) fn declares_equality(codebase: &CodebaseMetadata, name: Word) -> bool {
    // The `op_Equality` the codex writes for a struct that declares none spans the whole struct.
    let Some(r#struct) = codebase.get_class_like(name.as_bytes()) else {
        return false;
    };

    codebase.get_method(name.as_bytes(), b"op_Equality").is_some_and(|equality| equality.span != r#struct.span)
}

/// Reports each struct in `key_type` that holds a class instance at any depth, spec section 10: a `Map` holds a struct
/// key, and a `Set` a struct element, as its key text, built from its values, while a class instance compares through
/// its own `==`, which no text matches. A struct that declares `operator ==` is held through it and its `hash()`, so
/// it holds what it likes. Precedent: Rust's `HashMap` needs `Hash` on every field, and Swift's `Dictionary` needs
/// `Hashable`. `key_type` runs as an array key, so each class it names is a struct. `in_set` names a `Set` element.
/// Returns whether it reported one.
pub(crate) fn report_class_held_by_key<A>(
    context: &mut Context<'_, '_, A>,
    key_type: &TUnion,
    in_set: bool,
    span: Span,
) -> bool
where
    A: Arena,
{
    let codebase = context.codebase;
    let structs = key_type.types.iter().filter_map(|atomic| match atomic {
        TAtomic::Object(TObject::Named(object)) if !declares_equality(codebase, object.name) => {
            codebase.get_class_like(object.name.as_bytes())
        }
        _ => None,
    });
    let mut reported = false;
    for r#struct in structs {
        let Some((property, held, is_direct)) = held_class(codebase, r#struct, &mut vec![r#struct.name]) else {
            continue;
        };

        let class = display_class_like_name(context, r#struct.original_name);
        let held_name = display_sharp_type(context, &TUnion::from_atomic(held.clone()));
        let article = if held_name.starts_with(['A', 'E', 'I', 'O', 'U']) { "an" } else { "a" };
        let what = match &held {
            TAtomic::Object(TObject::Named(named)) => {
                match context.codebase.get_class_like(named.name.as_bytes()).map(|class_like| class_like.kind) {
                    Some(kind) if kind.is_interface() => format!("{article} `{held_name}`, an interface"),
                    _ => format!("{article} `{held_name}`, a class"),
                }
            }
            TAtomic::Callable(_) => format!("a `{held_name}`, a function"),
            _ => format!("`{held_name}`, which can be a class instance"),
        };
        let verb = if is_direct { "is" } else { "holds" };
        let message = if in_set {
            format!(
                "`{class}` can't be in a `Set`: `{}` {verb} {what}, and a `Set` holds only values.",
                BytesDisplay(property.as_bytes().strip_prefix(b"$").unwrap_or(property.as_bytes()))
            )
        } else {
            format!(
                "`{class}` can't key a `Map`: `{}` {verb} {what}, and a key holds only values.",
                BytesDisplay(property.as_bytes().strip_prefix(b"$").unwrap_or(property.as_bytes()))
            )
        };

        context.collector.report_with_code(
            IssueCode::TemplateConstraintViolation,
            Issue::error(message)
                .with_annotation(Annotation::primary(span).with_message(if in_set {
                    format!("`{class}` is this `Set`'s element.")
                } else {
                    format!("`{class}` keys this `Map`.")
                }))
                .with_note(
                    "A struct key is stored as text built from its values, and a class instance compares through its own `==`, which no text matches.",
                ),
        );
        reported = true;
    }

    reported
}

/// The first property of `r#struct`, in declaration order, whose type holds a class instance at any depth, with the
/// part that holds it and whether it is the property's own type: a class, an interface, a function or `Any`, also
/// inside a collection or a struct, which `visiting` holds while it is read. An enum case is a value.
fn held_class(
    codebase: &CodebaseMetadata,
    r#struct: &ClassLikeMetadata,
    visiting: &mut Vec<Word>,
) -> Option<(Word, TAtomic, bool)> {
    let mut properties: Vec<_> = r#struct
        .properties
        .iter()
        .filter(|(_, property)| !property.flags.is_static() && !property.flags.is_virtual_property())
        .collect();
    properties.sort_by_key(|(_, property)| property.name_span.map(|span| span.start.offset));

    properties.into_iter().find_map(|(name, property)| {
        let r#type = &property.type_metadata.as_ref()?.type_union;
        let held = class_part(codebase, r#type, visiting)?;
        let is_direct = r#type.types.contains(&held);

        Some((*name, held, is_direct))
    })
}

/// The part of `r#type` that can be a class instance, or none when every value of it is a value. A struct that
/// declares `operator ==` is a value whatever it holds.
fn class_part(codebase: &CodebaseMetadata, r#type: &TUnion, visiting: &mut Vec<Word>) -> Option<TAtomic> {
    r#type.types.iter().find_map(|atomic| match atomic {
        TAtomic::Object(TObject::Named(object)) => {
            // A type that names no class is reported where it is written.
            let class_like = codebase.get_class_like(object.name.as_bytes())?;
            if !class_like.flags.is_struct() {
                return Some(atomic.clone());
            }
            if visiting.contains(&class_like.name) || declares_equality(codebase, class_like.name) {
                return None;
            }

            visiting.push(class_like.name);
            let found = held_class(codebase, class_like, visiting).map(|(_, held, _)| held);
            visiting.pop();

            found
        }
        TAtomic::Object(TObject::Enum(_)) => None,
        TAtomic::Object(_) | TAtomic::Callable(_) | TAtomic::Mixed(_) => Some(atomic.clone()),
        TAtomic::Array(array) => {
            let (key, value) = get_array_parameters(array, codebase);

            class_part(codebase, &key, visiting).or_else(|| class_part(codebase, &value, visiting))
        }
        _ => None,
    })
}

/// The part of `r#type` `parse` can't read from a value, with why: an enum with no backing value, a function, a class
/// no `parse` builds, or any other type but `Any`, `int`, `float`, `string`, `bool` and a collection, which the bridge
/// writes no `parse` for, also inside a collection or a struct or class `parse` reads through, which `visiting` holds
/// while it is read.
fn unreadable_part(
    codebase: &CodebaseMetadata,
    r#type: &TUnion,
    visiting: &mut Vec<Word>,
) -> Option<(TAtomic, &'static str)> {
    r#type.types.iter().find_map(|atomic| match atomic {
        TAtomic::Object(TObject::Enum(r#enum)) => codebase
            .get_class_like(r#enum.name.as_bytes())
            .filter(|r#enum| r#enum.enum_type.is_none())
            .map(|_| (atomic.clone(), "has no backing value")),
        TAtomic::Callable(_) => Some((atomic.clone(), "is a function")),
        TAtomic::Object(TObject::Named(object)) if visiting.contains(&object.name) => None,
        TAtomic::Object(TObject::Named(object)) if !codebase.parses(object.name.as_bytes()) => {
            // A type that names no class is reported where it is written.
            let class = codebase.get_class_like(object.name.as_bytes())?;
            let why = if class.kind.is_interface() {
                "is an interface"
            } else if class.flags.is_abstract() {
                "is abstract"
            } else if !class.flags.is_sharp() {
                "is a plain PHP class"
            } else {
                "has no `parse` that reads a value"
            };

            Some((atomic.clone(), why))
        }
        TAtomic::Object(TObject::Named(object)) => {
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
        TAtomic::Mixed(_)
        | TAtomic::Null
        | TAtomic::Scalar(TScalar::Integer(_) | TScalar::Float(_) | TScalar::String(_) | TScalar::Bool(_)) => None,
        _ => Some((atomic.clone(), "is no type `parse` reads")),
    })
}
