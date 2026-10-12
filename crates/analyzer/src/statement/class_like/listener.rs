use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::Method;
use mago_syntax::cst::Modifier;
use mago_word::Word;

use crate::code::IssueCode;
use crate::context::Context;
use crate::statement::function_like::report_undefined_type;
use crate::statement::is_event_class;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_sharp_type;
use crate::utils::names::display_variable_name;

/// Checks each PHP# listener of a class-like, spec section 15: a method with an `on` clause. It lists events, or
/// interfaces an event implements. It runs on an object the container creates, so in a class that is not abstract,
/// returns nothing, and takes no parameter or one that every event it lists is.
pub fn check_listener<'ctx, 'arena, A>(
    metadata: &'ctx ClassLikeMetadata,
    members: &[ClassLikeMember<'arena>],
    context: &mut Context<'ctx, 'arena, A>,
) where
    A: Arena,
{
    let mut listeners = members.iter().filter_map(|member| match member {
        ClassLikeMember::Method(method) => method.on.as_ref().map(|on| (method, on)),
        _ => None,
    });
    let Some((first, first_on)) = listeners.next() else {
        return;
    };

    let class_name = display_class_like_name(context, metadata.original_name);
    let first_name = BytesDisplay(first.name.value);
    let constructor = context.codebase.get_declaring_method(metadata.name.as_bytes(), b"__construct");
    if let Some(visibility) = constructor
        .and_then(|constructor| constructor.method_metadata.as_ref())
        .map(|constructor| constructor.visibility)
        .filter(|visibility| !visibility.is_public())
    {
        context.collector.report_with_code(
            IssueCode::InvalidEvent,
            Issue::error(format!(
                "`{class_name}`'s constructor is {visibility}, so PHP# can't create a `{class_name}` to run `{first_name}`."
            ))
            .with_annotation(Annotation::primary(first_on.span()).with_message(format!("`{first_name}` listens here."))),
        );
    }
    if constructor.is_some_and(|constructor| !constructor.parameters.is_empty()) {
        context.collector.report_with_code(
            IssueCode::InvalidEvent,
            Issue::error("A listener whose class takes constructor parameters is not supported yet in PHP#.")
                .with_annotation(
                    Annotation::primary(first_on.span()).with_message(format!("`{first_name}` listens to events.")),
                )
                .with_note(format!("PHP# creates `{class_name}` with no arguments to run its listeners.")),
        );
    }

    for (method, on) in std::iter::once((first, first_on)).chain(listeners) {
        // A second method of the same name has no metadata of its own: semantics reports it.
        let Some(method_metadata) = context
            .codebase
            .get_method(metadata.name.as_bytes(), method.name.value)
            .filter(|method_metadata| method_metadata.span == method.span())
        else {
            continue;
        };

        check_listed_events(method, method_metadata, context);
        check_listener_shape(metadata, class_name, method, method_metadata, on.span(), context);
    }
}

/// Reports each name the `on` clause lists that names no event: an unknown type, a type that is not an event, a
/// built-in type included, or an interface no event implements.
fn check_listed_events<'arena, A>(
    method: &Method<'arena>,
    method_metadata: &FunctionLikeMetadata,
    context: &mut Context<'_, 'arena, A>,
) where
    A: Arena,
{
    let codebase = context.codebase;
    let events = method.on.iter().flat_map(|on| on.events.iter());
    for (event, listed) in events.zip(&method_metadata.listens) {
        let message = match codebase.get_class_like(listed.as_bytes()) {
            Some(class_like) if is_heard(codebase, class_like) => continue,
            Some(class_like) if class_like.kind.is_interface() => format!(
                "No event implements `{}`, so `{}` never runs.",
                display_class_like_name(context, class_like.original_name),
                BytesDisplay(method.name.value)
            ),
            Some(class_like) => {
                format!("`{}` is not an event.", display_class_like_name(context, class_like.original_name))
            }
            None if BUILT_IN_TYPES.iter().any(|built_in| built_in.eq_ignore_ascii_case(event.value)) => {
                format!("`{}` is not an event.", BytesDisplay(event.value))
            }
            None => {
                report_undefined_type(context, *listed, event.span);

                continue;
            }
        };

        context.collector.report_with_code(
            IssueCode::InvalidEvent,
            Issue::error(message)
                .with_annotation(Annotation::primary(event.span).with_message("Listed here."))
                .with_note(
                    "An `on` clause lists events, which `event` or `extern event` declares, or interfaces an event implements.",
                ),
        );
    }
}

/// Reports a listener that cannot run on an object the container creates, that returns a value, or whose parameters
/// are not one that every listed event is.
fn check_listener_shape<'arena, A>(
    metadata: &ClassLikeMetadata,
    class_name: Word,
    method: &Method<'arena>,
    method_metadata: &FunctionLikeMetadata,
    on: Span,
    context: &mut Context<'_, 'arena, A>,
) where
    A: Arena,
{
    let name = BytesDisplay(method.name.value);
    let mut refusals = Vec::new();
    if let Some(r#static) = method.modifiers.iter().find(|modifier| matches!(modifier, Modifier::Static(_))) {
        refusals.push(
            Issue::error(format!("A listener runs on an object, so `{name}` cannot be `static`."))
                .with_annotation(Annotation::primary(r#static.span()).with_message("Declared `static` here.")),
        );
    }
    if let (Some(hint), Some(returned)) = (&method.return_type_hint, &method_metadata.return_type_declaration_metadata)
        && !returned.type_union.is_void()
    {
        refusals.push(
            Issue::error(format!(
                "A listener returns nothing, and `{name}` returns `{}`.",
                display_sharp_type(context, &returned.type_union)
            ))
            .with_annotation(Annotation::primary(hint.span()).with_message("Returned type."))
            .with_help("Return `void`."),
        );
    }
    let unconstructible = if metadata.kind.is_interface() {
        Some("an interface")
    } else if metadata.kind.is_enum() {
        Some("an enum")
    } else if metadata.flags.is_abstract() {
        Some("abstract")
    } else {
        None
    };
    if let Some(unconstructible) = unconstructible {
        refusals.push(
            Issue::error(format!(
                "`{class_name}` is {unconstructible}, so the container cannot create it to run `{name}`."
            ))
            .with_annotation(Annotation::primary(on).with_message(format!("`{name}` listens here."))),
        );
    }

    match method.parameter_list.parameters.as_slice() {
        [] => {}
        [parameter] => {
            if let Some(taken) = method_metadata.parameters.first().and_then(|taken| taken.type_metadata.as_ref()) {
                refusals.extend(unshared_events(method, method_metadata, &taken.type_union, parameter.span(), context));
            }
        }
        [_, extra, ..] => refusals.push(
            Issue::error(format!(
                "A listener takes only its event, and `{name}` also takes `{}`.",
                display_variable_name(context, extra.variable.name)
            ))
            .with_annotation(Annotation::primary(extra.span()).with_message("Not the event."))
            .with_help("A listener gets everything else through its class's constructor."),
        ),
    }

    for issue in refusals {
        context.collector.report_with_code(IssueCode::InvalidEvent, issue);
    }
}

/// The refusal of each listed event that is not the type the listener takes, with the help naming the most specific
/// interface every listed event implements.
fn unshared_events<'arena, A>(
    method: &Method<'arena>,
    method_metadata: &FunctionLikeMetadata,
    taken: &TUnion,
    parameter: Span,
    context: &Context<'_, 'arena, A>,
) -> Vec<Issue>
where
    A: Arena,
{
    let codebase = context.codebase;
    let events = method.on.iter().flat_map(|on| on.events.iter());
    let unshared: Vec<_> = events
        .zip(&method_metadata.listens)
        .filter_map(|(event, listed)| Some((event, codebase.get_class_like(listed.as_bytes())?)))
        .filter(|(_, class_like)| {
            is_heard(codebase, class_like)
                && !union_comparator::is_contained_by(
                    codebase,
                    &TUnion::from_atomic(TAtomic::Object(TObject::new_named(class_like.original_name))),
                    taken,
                    false,
                    false,
                    false,
                    &mut ComparisonResult::new(),
                )
        })
        .collect();
    if unshared.is_empty() {
        return Vec::new();
    }

    let taken = display_sharp_type(context, taken);
    let article = if taken.starts_with(['A', 'E', 'I', 'O', 'U', 'a', 'e', 'i', 'o', 'u']) { "an" } else { "a" };
    let help = match shared_interface(codebase, &method_metadata.listens) {
        Some(shared) => format!(
            "Take a type every listed event implements, such as `{}`, or take no parameter.",
            display_class_like_name(context, shared)
        ),
        None => "Take a type every listed event implements, or take no parameter.".to_owned(),
    };

    unshared
        .into_iter()
        .map(|(event, class_like)| {
            Issue::error(format!(
                "`{}` takes {article} `{taken}`, and it listens to `{}`, which is not one.",
                BytesDisplay(method.name.value),
                display_class_like_name(context, class_like.original_name),
            ))
            .with_annotation(Annotation::primary(event.span).with_message("Listed here."))
            .with_annotation(Annotation::secondary(parameter).with_message("The event it takes."))
            .with_help(help.clone())
        })
        .collect()
}

/// The most specific interface every listed type is or implements: the one that extends the most others, then the
/// first by name. None when there is no such interface, or a listed type names nothing.
fn shared_interface(codebase: &CodebaseMetadata, listed: &[Word]) -> Option<Word> {
    let mut shared: Option<Vec<&ClassLikeMetadata>> = None;
    for name in listed {
        let class_like = codebase.get_class_like(name.as_bytes())?;
        let interfaces: Vec<Word> = std::iter::once(class_like)
            .filter(|class_like| class_like.kind.is_interface())
            .map(|interface| interface.name)
            .chain(class_like.all_parent_interfaces.iter().copied())
            .collect();
        shared = Some(match shared {
            None => interfaces.iter().filter_map(|name| codebase.get_class_like(name.as_bytes())).collect(),
            Some(shared) => shared.into_iter().filter(|interface| interfaces.contains(&interface.name)).collect(),
        });
    }

    shared?
        .into_iter()
        .max_by(|a, b| {
            a.all_parent_interfaces.len().cmp(&b.all_parent_interfaces.len()).then_with(|| b.name.cmp(&a.name))
        })
        .map(|interface| interface.original_name)
}

/// The names PHP reserves for its own types (`reserved_class_names` in `zend_compile.c`), and `Any`, PHP#'s `mixed`. Each
/// names a type that exists, and none is an event, so an `on` clause that lists one is told that, not "Cannot find type".
const BUILT_IN_TYPES: [&[u8]; 18] = [
    b"array",
    b"bool",
    b"callable",
    b"false",
    b"float",
    b"int",
    b"iterable",
    b"mixed",
    b"never",
    b"null",
    b"object",
    b"parent",
    b"self",
    b"static",
    b"string",
    b"true",
    b"void",
    b"Any",
];

/// Whether an `on` clause may list `class_like`: an event, or an interface an event implements.
fn is_heard(codebase: &CodebaseMetadata, class_like: &ClassLikeMetadata) -> bool {
    if !class_like.kind.is_interface() {
        return is_event_class(codebase, class_like);
    }

    codebase
        .class_likes
        .values()
        .any(|class| is_event_class(codebase, class) && class.all_parent_interfaces.contains(&class_like.name))
}
