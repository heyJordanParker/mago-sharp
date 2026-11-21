use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::machine::MachineMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::php_variable_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_word::Word;
use mago_word::word;

use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_member;
use crate::utils::quoted_source;

/// Checks the graph of a PHP# machine, spec section 20.2: an entry point or a move reaches every case, a case with no
/// move out is `final` and a `final` case has none, and every case reaches a `final` one when the machine has one.
/// Each refusal names the case, at its name.
pub(crate) fn check_machine<A>(context: &mut Context<'_, '_, A>, machine: &ClassLikeMetadata)
where
    A: Arena,
{
    let Some(graph) = &machine.machine else {
        return;
    };

    let machine_name = display_class_like_name(context, machine.original_name);
    let reached = reachable(graph, graph.entry_points.iter().flat_map(|entry_point| entry_point.to.iter().copied()));
    let has_final = graph.cases.iter().any(|case| case.is_final);
    for case in &graph.cases {
        let case_name = BytesDisplay(case.name.as_bytes());
        let shown = format!("{machine_name}.{case_name}");
        let mut moves_out = graph.moves.iter().filter(|r#move| r#move.from.contains(&case.name));
        let refusal = match (case.is_final, moves_out.next()) {
            (true, Some(r#move)) => Some((
                format!("The `final` case `{shown}` has a move out, `{}`.", BytesDisplay(r#move.name.as_bytes())),
                "Remove `final`, or remove the move.".to_owned(),
            )),
            (false, None) => Some((
                format!("The case `{shown}` has no move out, so it is `final`."),
                format!("Write `final case {case_name}`, or give it a move."),
            )),
            (false, Some(_))
                if has_final
                    && !reachable(graph, [case.name])
                        .iter()
                        .any(|reached| graph.cases.iter().any(|case| case.name == *reached && case.is_final)) =>
            {
                Some((
                    format!("The case `{shown}` reaches no `final` case."),
                    format!("Give `{shown}` a move that leads to a `final` case."),
                ))
            }
            _ => None,
        };
        if let Some((message, help)) = refusal {
            report(context, case.span, message, help);
        }

        if !reached.contains(&case.name) {
            report(
                context,
                case.span,
                format!("No entry point or move reaches the case `{shown}`."),
                format!("Give `{machine_name}` an entry point or a move that gives `{shown}`, or remove it."),
            );
        }
    }
}

/// The cases `start` names and every case a move reaches from one of them.
fn reachable(graph: &MachineMetadata, start: impl IntoIterator<Item = Word>) -> Vec<Word> {
    let mut reached: Vec<Word> = start.into_iter().collect();
    let mut next = 0;
    while let Some(&case) = reached.get(next) {
        next += 1;
        for r#move in graph.moves.iter().filter(|r#move| r#move.from.contains(&case)) {
            for &to in &r#move.to {
                if !reached.contains(&to) {
                    reached.push(to);
                }
            }
        }
    }

    reached
}

/// Checks each property of `class` that holds a PHP# machine, spec section 20.2: its `set` is `private`, and its
/// initial value, when it has one, calls an entry point of the machine. A property its constructor takes has the
/// caller's value, which no entry point gave, so it is refused there. An interface's property has no storage and no
/// `set`, so nothing writes it, and each implementer's own property is checked instead.
pub(crate) fn check_machine_properties<A>(
    context: &mut Context<'_, '_, A>,
    class: &ClassLikeMetadata,
    members: &[ClassLikeMember<'_>],
) where
    A: Arena,
{
    if class.kind.is_interface() {
        return;
    }
    let mut properties: Vec<_> = class.properties.values().collect();
    properties.sort_by_key(|property| property.name_span.map(|span| span.start.offset));
    for property in properties {
        let (Some(name_span), Some(type_metadata)) = (property.name_span, &property.type_metadata) else {
            continue;
        };
        let Some(machine) = machine_of(context, &type_metadata.type_union) else {
            continue;
        };
        let Some(graph) = &machine.machine else {
            continue;
        };

        let machine_name = display_class_like_name(context, machine.original_name);
        let property_name = property.name.0.as_bytes().strip_prefix(b"$").unwrap_or(property.name.0.as_bytes());
        let shown = display_member(context, class.original_name, BytesDisplay(property_name));
        let r#move = example_move(graph);
        if !property.write_visibility.is_private() || property.flags.is_readonly() {
            let property_name = BytesDisplay(property_name);
            report(
                context,
                name_span,
                format!("`{shown}` holds the machine `{machine_name}`, so its `set` is `private`."),
                format!(
                    "Write `{{ get; private set; }}`, and change it only by a move, as `this.{property_name} = this.{property_name}.{move}(…)`."
                ),
            );
        }

        let initial_value = members.iter().find_map(|member| match member {
            ClassLikeMember::Property(declared)
                if php_variable_name(declared.first_variable().name) == property.name.0 =>
            {
                declared.initial_value()
            }
            _ => None,
        });
        let refused_at = match initial_value {
            Some(value) if !calls_entry_point(context, machine, value) => value.span(),
            None if property.flags.is_promoted_property() => name_span,
            _ => continue,
        };
        let entry_point = example_entry_point(graph);
        report(
            context,
            refused_at,
            format!(
                "`{shown}` holds the machine `{machine_name}`, so its initial value is an entry point of `{machine_name}`, as `{machine_name}.{entry_point}()`."
            ),
            "Start it with an entry point.".to_owned(),
        );
    }
}

/// Checks `target = value` where `target` holds a PHP# machine, spec section 20.2: it changes only by a move of its
/// own, as `this.status = this.status.pay(5)`, and a constructor may start it with an entry point of the machine.
pub(crate) fn check_machine_write<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    target: &Expression<'_>,
    declared: &TUnion,
    value: &Expression<'_>,
) where
    A: Arena,
{
    let Some(machine) = machine_of(context, declared) else {
        return;
    };
    let Some(graph) = &machine.machine else {
        return;
    };

    let target_source = quoted_source(context, target.span());
    if let Expression::Call(Call::Method(call)) = value.unparenthesized()
        && let ClassLikeMemberSelector::Identifier(name) = &call.method
        && graph.moves.iter().any(|r#move| r#move.name.as_bytes() == name.value)
        && quoted_source(context, call.object.span()) == target_source
    {
        return;
    }

    // A case built here is refused where it is built.
    if machine_call(context, machine, value)
        .is_some_and(|called| graph.cases.iter().any(|case| case.name.as_bytes() == called))
    {
        return;
    }

    let in_constructor = block_context
        .scope
        .get_function_like()
        .and_then(|function_like| function_like.method_metadata.as_ref())
        .is_some_and(|method| method.is_constructor);
    if in_constructor && calls_entry_point(context, machine, value) {
        return;
    }

    let machine_name = display_class_like_name(context, machine.original_name);
    report(
        context,
        value.span(),
        format!(
            "`{target_source}` holds the machine `{machine_name}`, so it changes only by a move of its own, as `{target_source} = {target_source}.{}(…)`.",
            example_move(graph)
        ),
        format!("Call a move of `{target_source}`."),
    );
}

/// Refuses a case of a PHP# machine built outside the machine, spec section 20.2: only its entry points and moves
/// build a case that carries data, through the static method of the case's name, as `OrderStatus.Paid(5)`.
pub(crate) fn check_case_built<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    class: &ClassLikeMetadata,
    method: Word,
    span: Span,
) where
    A: Arena,
{
    let Some(graph) = &class.machine else {
        return;
    };
    if !graph.cases.iter().any(|case| case.name == method) {
        return;
    }
    let inside = block_context.scope.get_class_like().is_some_and(|scope| {
        scope.name == class.name || scope.direct_parent_class.is_some_and(|parent| parent == class.name)
    });
    if inside {
        return;
    }

    let machine_name = display_class_like_name(context, class.original_name);
    report(
        context,
        span,
        format!(
            "Only the entry points and moves of `{machine_name}` build its case `{machine_name}.{}`.",
            BytesDisplay(method.as_bytes())
        ),
        format!(
            "Call an entry point, as `{machine_name}.{}()`, or a move of a value of `{machine_name}`.",
            example_entry_point(graph)
        ),
    );
}

/// The machine whose value `union` holds: the machine itself, or the machine of a case of it.
fn machine_of<'ctx, A>(context: &Context<'ctx, '_, A>, union: &TUnion) -> Option<&'ctx ClassLikeMetadata>
where
    A: Arena,
{
    union.types.iter().find_map(|atomic| {
        let TAtomic::Object(TObject::Named(object)) = atomic else {
            return None;
        };
        let class = context.codebase.get_class_like(object.name)?;
        if class.machine.is_some() {
            return Some(class);
        }

        class
            .direct_parent_class
            .and_then(|parent| context.codebase.get_class_like(parent))
            .filter(|parent| class.flags.is_enum_case_class() && parent.machine.is_some())
    })
}

/// The method `value` calls on `machine` itself, as `open` in `OrderStatus.open()`, the static call PHP# writes as a
/// method call on the class's name.
fn machine_call<'ast, A>(
    context: &Context<'_, '_, A>,
    machine: &ClassLikeMetadata,
    value: &'ast Expression<'_>,
) -> Option<&'ast [u8]>
where
    A: Arena,
{
    let Expression::Call(Call::Method(call)) = value.unparenthesized() else {
        return None;
    };
    let ClassLikeMemberSelector::Identifier(name) = &call.method else {
        return None;
    };
    let class = context.resolved_names.static_call_class(call)?;

    context
        .codebase
        .get_class_like(word(context.resolved_names.get(&class.name)))
        .is_some_and(|class| class.name == machine.name)
        .then_some(name.value)
}

/// Whether `value` calls an entry point of `machine`, as `OrderStatus.open()`.
fn calls_entry_point<A>(context: &Context<'_, '_, A>, machine: &ClassLikeMetadata, value: &Expression<'_>) -> bool
where
    A: Arena,
{
    machine_call(context, machine, value).is_some_and(|called| {
        machine
            .machine
            .as_ref()
            .is_some_and(|graph| graph.entry_points.iter().any(|entry_point| entry_point.name.as_bytes() == called))
    })
}

/// The name of the machine's first move, which a message shows as the way to change a value of it.
fn example_move(graph: &MachineMetadata) -> BytesDisplay<'_> {
    BytesDisplay(graph.moves.first().map_or(b"move".as_slice(), |r#move| r#move.name.as_bytes()))
}

/// The name of the machine's first entry point, which a message shows as the way to start a value of it.
fn example_entry_point(graph: &MachineMetadata) -> BytesDisplay<'_> {
    BytesDisplay(graph.entry_points.first().map_or(b"start".as_slice(), |entry_point| entry_point.name.as_bytes()))
}

fn report<A>(context: &mut Context<'_, '_, A>, span: Span, message: String, help: String)
where
    A: Arena,
{
    context.collector.report_with_code(
        IssueCode::InvalidMachine,
        Issue::error(message).with_annotation(Annotation::primary(span)).with_help(help),
    );
}
