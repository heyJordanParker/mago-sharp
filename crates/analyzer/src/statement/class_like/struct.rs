use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::class_like::ParsedField;
use mago_codex::metadata::property::PropertyMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::template::inferred_type_replacer;
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
use crate::utils::names::article;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_sharp_type;
use crate::utils::template::class_type_arguments;
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

        // A plain PHP trait works everywhere an interface does, spec section 22.
        if named.kind.is_interface() || named.kind.is_trait() {
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
                "`{}`'s header names the interfaces it implements and the traits it uses.",
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
/// type, or a class no `parse` builds, also inside a collection or a nested struct or class. A generic type's field
/// reads as the call's `type_arguments` give it, as `int` for `TFirst` of `Pair<int, string>.parse`, and one of a type
/// parameter only through the walk, whose type argument holds no part `parse` builds first. The type itself
/// compiles, so code that never parses it may hold such a field.
pub(crate) fn check_struct_parse<A>(
    context: &mut Context<'_, '_, A>,
    r#struct: &ClassLikeMetadata,
    type_arguments: &[TUnion],
    throws: bool,
    span: Span,
) where
    A: Arena,
{
    let method = if throws { "parse" } else { "tryParse" };
    let class = display_class_like_name(context, r#struct.original_name);
    for field in context.codebase.struct_parse_fields(r#struct.name.as_bytes()) {
        let Some((unreadable, why)) =
            unreadable_field(context.codebase, r#struct, type_arguments, &field, &mut vec![r#struct.name])
        else {
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

/// Whether the struct or class `name` declares `operator ==`, which replaces the `==` that compares a struct property by
/// property and, with the `hash()` it then declares, keys a `Map` and joins a `Set`.
pub(crate) fn declares_equality(codebase: &CodebaseMetadata, name: Word) -> bool {
    // The `op_Equality` the codex writes for a struct that declares none spans the whole struct.
    let Some(r#struct) = codebase.get_class_like(name.as_bytes()) else {
        unreachable!("each caller names a struct or a class the codebase declares, as `is_key_type` requires");
    };

    codebase.get_method(name.as_bytes(), b"op_Equality").is_some_and(|equality| equality.span != r#struct.span)
}

/// The properties a value of `class` compares by with `==`, and is hashed by as a `Map` key or a `Set` element, when
/// it is a value, spec sections 10, 19 and 20: each property of a struct, and each field of a case of an enum with a
/// case that carries data, of every case for the enum itself. None for any other class.
pub(crate) fn value_properties<'codebase>(
    codebase: &'codebase CodebaseMetadata,
    class: &'codebase ClassLikeMetadata,
) -> Option<Vec<&'codebase PropertyMetadata>> {
    if class.flags.is_struct() || class.flags.is_enum_case_class() {
        return Some(class.struct_properties());
    }
    let cases = data_cases(codebase, class);

    (!cases.is_empty()).then(|| cases.into_iter().flat_map(ClassLikeMetadata::struct_properties).collect())
}

/// The enum with a case that carries data, spec section 20, that `class` is or is a case of.
pub(crate) fn data_enum(codebase: &CodebaseMetadata, class: &ClassLikeMetadata) -> Option<Word> {
    if class.flags.is_enum_case_class() {
        return class.direct_parent_class;
    }

    (!data_cases(codebase, class).is_empty()).then_some(class.name)
}

/// The classes the cases of `class` run as, when it is an enum with a case that carries data, spec section 20.
fn data_cases<'codebase>(
    codebase: &'codebase CodebaseMetadata,
    class: &ClassLikeMetadata,
) -> Vec<&'codebase ClassLikeMetadata> {
    class
        .permitted_inheritors
        .iter()
        .flatten()
        .filter_map(|case| codebase.get_class_like(case.as_bytes()))
        .filter(|case| case.flags.is_enum_case_class() && case.direct_parent_class == Some(class.name))
        .collect()
}

/// Reports each struct or enum case in `key_type` that holds a class instance without `==` and `hash()` at any depth,
/// spec sections 10 and 20: a `Map` finds such a key, and a `Set` such an element, by a hash of its fields and their
/// `==`, and a class instance has neither unless its class declares `operator ==`. A struct that declares
/// `operator ==` is held through it and its `hash()`, so it holds what it likes. A class key is `map_key_error`'s to
/// report. Precedent: Rust's `HashMap` needs `Hash` on every field, and Swift's `Dictionary` needs `Hashable`. `in_set`
/// names a `Set` element. Returns whether it reported one.
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
        TAtomic::Object(TObject::Named(object)) if !declares_equality(codebase, object.name) => codebase
            .get_class_like(object.name.as_bytes())
            .filter(|class_like| value_properties(codebase, class_like).is_some()),
        _ => None,
    });
    let mut reported = false;
    for r#struct in structs {
        let Some((property, held, is_direct)) = held_class(codebase, r#struct, &mut vec![r#struct.name]) else {
            continue;
        };

        let class = display_class_like_name(context, r#struct.original_name);
        let held_name = display_sharp_type(context, &TUnion::from_atomic(held.clone()));
        let article = article(&held_name);
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
                .with_note(format!(
                    "{} key is found by a hash of its fields and their `==`, and a class instance has neither unless its class declares `operator ==` with `hash()`.",
                    if r#struct.flags.is_struct() { "A struct" } else { "An enum case" }
                )),
        );
        reported = true;
    }

    reported
}

/// The first property of `value`, a struct, an enum case or an enum with a case that carries data, in declaration
/// order, whose type holds a class instance at any depth, with the part that holds it and whether it is the property's
/// own type: a class, an interface, a function or `Any`, also inside a collection, a struct or a case, which `visiting`
/// holds while it is read. A plain enum case is a value.
fn held_class(
    codebase: &CodebaseMetadata,
    value: &ClassLikeMetadata,
    visiting: &mut Vec<Word>,
) -> Option<(Word, TAtomic, bool)> {
    value_properties(codebase, value)?.into_iter().find_map(|property| {
        let r#type = &property.type_metadata.as_ref()?.type_union;
        let held = class_part(codebase, r#type, visiting)?;
        let is_direct = r#type.types.contains(&held);

        Some((property.name.0, held, is_direct))
    })
}

/// The part of `r#type` that can be a class instance without `==` and `hash()`, or none when every value of it has
/// them. A struct that declares `operator ==` has them whatever it holds, and so does a class that declares or inherits
/// `operator ==`, which the checker requires `hash()` beside. A struct or an enum case has them when its fields do.
fn class_part(codebase: &CodebaseMetadata, r#type: &TUnion, visiting: &mut Vec<Word>) -> Option<TAtomic> {
    r#type.types.iter().find_map(|atomic| match atomic {
        TAtomic::Object(TObject::Named(object)) => {
            // A type that names no class is reported where it is written.
            let class_like = codebase.get_class_like(object.name.as_bytes())?;
            if value_properties(codebase, class_like).is_none() {
                let has_equality = codebase.get_declaring_method(object.name.as_bytes(), b"op_Equality").is_some();

                return (!has_equality).then(|| atomic.clone());
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
            let class = codebase.get_class_like(object.name.as_bytes())?;
            let type_arguments = object.get_type_parameters().unwrap_or_default();
            visiting.push(object.name);
            let found = codebase
                .struct_parse_fields(object.name.as_bytes())
                .iter()
                .find_map(|field| unreadable_field(codebase, class, type_arguments, field, visiting));
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

/// The part of the field `field` of the struct or class `class`, read with `type_arguments`, that `parse` can't read,
/// with why, as [`unreadable_part`] finds it in the field's type as those type arguments give it. A field of a type
/// parameter reads through the walk alone, so a type argument with a part `parse` builds first is refused for it.
fn unreadable_field(
    codebase: &CodebaseMetadata,
    class: &ClassLikeMetadata,
    type_arguments: &[TUnion],
    field: &ParsedField<'_>,
    visiting: &mut Vec<Word>,
) -> Option<(TAtomic, &'static str)> {
    let used = field.r#type.get_template_types();
    let built = class.template_types.iter().zip(type_arguments).find_map(|((name, _), type_argument)| {
        used.iter()
            .any(|used| matches!(used, TAtomic::GenericParameter(parameter) if parameter.parameter_name == *name))
            .then(|| built_part(type_argument))
            .flatten()
    });
    if let Some(part) = built {
        return Some((
            part.clone(),
            "is a type argument `parse` builds, and it reads a field of a type parameter only as its input holds it",
        ));
    }
    let localized =
        inferred_type_replacer::replace(field.r#type, &class_type_arguments(class, type_arguments), codebase);

    unreadable_part(codebase, &localized, visiting)
}

/// The part of `r#type` that `parse` builds before the walk checks the value: a struct or a class through its own
/// `parse`, an enum case from its backing value, or a `Map` or a `Set` from the array plain PHP passes, also inside a
/// `List`. The bridge writes one `parse` for a generic type, which reads a field of a type parameter through the walk
/// alone, so it reads a type argument that holds none of them.
fn built_part(r#type: &TUnion) -> Option<&TAtomic> {
    r#type.types.iter().find_map(|atomic| match atomic {
        TAtomic::Object(_) | TAtomic::Array(TArray::Keyed(_) | TArray::Set(_)) => Some(atomic),
        TAtomic::Array(TArray::List(list)) => built_part(&list.element_type).or_else(|| {
            list.known_elements
                .iter()
                .flat_map(|elements| elements.values())
                .find_map(|(_, element)| built_part(element))
        }),
        _ => None,
    })
}
