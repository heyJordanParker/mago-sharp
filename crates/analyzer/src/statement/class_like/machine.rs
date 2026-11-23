use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::machine::MachineMetadata;
use mago_codex::metadata::machine::MachineMove;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::Binding;
use mago_names::binding::Local;
use mago_names::binding::LocalKind;
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
/// initial value, when it has one, calls an entry point of the machine. A struct's property is `{ get; init; }`, and
/// a `with` that sets it is checked as a write. A property its constructor takes has the caller's value, which no
/// entry point gave, so it is refused there. An interface's property has no storage and no `set`, so nothing writes
/// it, and each implementer's own property is checked instead. A computed property stores nothing, so it holds no
/// machine a write could change, and the stored properties it reads are checked instead.
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
    let mut properties: Vec<_> =
        class.properties.values().filter(|property| !property.flags.is_virtual_property()).collect();
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
        if !class.flags.is_struct() && (!property.write_visibility.is_private() || property.flags.is_readonly()) {
            let place = format!("this.{}", BytesDisplay(property_name));
            report(
                context,
                name_span,
                format!("`{shown}` holds the machine `{machine_name}`, so its `set` is `private`."),
                format!(
                    "Write `{{ get; private set; }}`, and change it only by a move: `{}`.",
                    move_example(machine_name, graph, &place)
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

/// Checks `place = value`, where `place`, as the file writes it, holds a PHP# machine, spec section 20.2: it changes
/// only by a move of the value it holds, called on `place` itself or on a name a type pattern bound from `place`, as
/// `this.status = draft.pay(5)` after `this.status is OrderStatus.Draft draft`. A constructor may start it with an
/// entry point of the machine, and so may any write `starts` names: one to a `late` property, and `new`'s initializer.
pub(crate) fn check_machine_write<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    place: &str,
    declared: &TUnion,
    value: &Expression<'_>,
    starts: bool,
) where
    A: Arena,
{
    let Some(machine) = machine_of(context, declared) else {
        return;
    };
    let Some(graph) = &machine.machine else {
        return;
    };

    if let Expression::Call(Call::Method(call)) = value.unparenthesized()
        && let ClassLikeMemberSelector::Identifier(name) = &call.method
        && graph.moves.iter().any(|r#move| r#move.name.as_bytes() == name.value)
        && holds_value_of(context, call.object, place)
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
    if (starts || in_constructor) && calls_entry_point(context, machine, value) {
        return;
    }

    let machine_name = display_class_like_name(context, machine.original_name);
    report(
        context,
        value.span(),
        format!(
            "`{place}` holds the machine `{machine_name}`, so it changes only by a move of its own, as `{}`.",
            move_example(machine_name, graph, place)
        ),
        format!("Call a move of the case `{place}` holds."),
    );
}

/// Whether `receiver` holds the value of `place`, as the file writes both: it is `place`, or a name a type pattern
/// bound from `place` that nothing assigns after, spec section 21. A call never changes the name, as `held.remove()`
/// the binder takes as a write from its name alone, since spec section 26 lets only a collection's extension take its
/// receiver by `ref`, and a case's value is no collection.
fn holds_value_of<A>(context: &Context<'_, '_, A>, receiver: &Expression<'_>, place: &str) -> bool
where
    A: Arena,
{
    let receiver = receiver.unparenthesized();
    if quoted_source(context, receiver.span()) == place {
        return true;
    }
    let Expression::ConstantAccess(name) = receiver else {
        return false;
    };
    let Some(Binding::Local(local @ Local { kind: LocalKind::Pattern { test, .. }, .. })) =
        context.resolved_names.binding(&name.name)
    else {
        return false;
    };

    !context.resolved_names.is_assigned(&local)
        && context.pattern_tests.get(&test).is_some_and(|tested| quoted_source(context, *tested) == place)
}

/// A member a call or a read names on a value of a machine, spec section 20.2.
#[derive(Clone, Copy)]
pub(crate) enum CaseMember {
    /// A move, which belongs to the cases it moves from.
    Move(Word),
    /// A property, which belongs to the class of each case that has it.
    Property(Word),
}

/// Spec section 20.2: a member of a case's block belongs to that case's class alone, and a move of the machine's body
/// to every case that isn't `final`, so `member` at `span` on `object`, whose type `receiver` is the machine or a union
/// of its cases, is refused when a case the value may be lacks it, with the test that narrows it to a case that has
/// it. Returns whether it refused the member.
pub(crate) fn check_case_member<A>(
    context: &mut Context<'_, '_, A>,
    object: &Expression<'_>,
    receiver: &TUnion,
    member: CaseMember,
    span: Span,
) -> bool
where
    A: Arena,
{
    let mut machine: Option<&ClassLikeMetadata> = None;
    let mut held: Vec<Word> = Vec::new();
    for atomic in receiver.types.iter().filter(|atomic| !atomic.is_null()) {
        let TAtomic::Object(TObject::Named(object)) = atomic else {
            return false;
        };
        let Some(class) = context.codebase.get_class_like(object.name) else {
            return false;
        };
        let (of, cases) = match &class.machine {
            Some(graph) => (class, graph.cases.iter().map(|case| case.name).collect()),
            None => {
                let Some(parent) = class
                    .direct_parent_class
                    .and_then(|parent| context.codebase.get_class_like(parent))
                    .filter(|parent| class.flags.is_enum_case_class() && parent.machine.is_some())
                else {
                    return false;
                };
                let short = class.original_name.as_bytes().rsplit(|byte| *byte == b'\\').next().unwrap_or_default();

                (parent, vec![word(short)])
            }
        };
        if machine.is_some_and(|machine| machine.name != of.name) {
            return false;
        }
        machine = Some(of);
        held.extend(cases);
    }
    let Some(machine) = machine else {
        return false;
    };
    let Some(graph) = &machine.machine else {
        return false;
    };
    let cases = graph.cases.iter().map(|case| case.name);
    let from: Vec<Word> = match member {
        CaseMember::Move(name) => {
            let moves: Vec<&MachineMove> = graph.moves.iter().filter(|r#move| r#move.name == name).collect();

            cases.clone().filter(|case| moves.iter().any(|r#move| r#move.from.contains(case))).collect()
        }
        CaseMember::Property(name) => {
            let property = php_variable_name(name.as_bytes());
            let class = |case: &Word| word([machine.original_name.as_bytes(), b"\\", case.as_bytes()].concat());

            cases.clone().filter(|case| context.codebase.property_exists(class(case), property)).collect()
        }
    };
    let lacking: Vec<Word> = cases.filter(|case| held.contains(case) && !from.contains(case)).collect();
    let Some(first) = from.first() else {
        return false;
    };
    if lacking.is_empty() {
        return false;
    }

    let machine_name = display_class_like_name(context, machine.original_name);
    let receiver = quoted_source(context, object.span());
    let bound = bound_name(first.as_bytes());
    let from = listed(from.iter().map(|case| format!("`{machine_name}.{}`", BytesDisplay(case.as_bytes()))).collect());
    let lacking = listed(lacking.iter().map(|case| format!("`{}`", BytesDisplay(case.as_bytes()))).collect());
    let first = BytesDisplay(first.as_bytes());
    let test = format!("test it first, as in `if ({receiver} is {machine_name}.{first} {bound})`");
    let (message, fix) = match member {
        CaseMember::Move(name) => {
            let name = BytesDisplay(name.as_bytes());
            (
                format!("`{name}` moves from {from}, and `{receiver}` may be {lacking}: {test}."),
                format!("Call `{name}` on the name the test binds, as `{bound}.{name}(…)`."),
            )
        }
        CaseMember::Property(name) => {
            let name = BytesDisplay(name.as_bytes());
            (
                format!("`{name}` belongs to {from}, and `{receiver}` may be {lacking}: {test}."),
                format!("Read `{name}` on the name the test binds, as `{bound}.{name}`."),
            )
        }
    };
    report(context, span, message, fix);

    true
}

/// `items` joined as a sentence lists them: `a`, `a or b`, `a, b or c`.
fn listed(mut items: Vec<String>) -> String {
    let Some(last) = items.pop() else {
        return String::new();
    };
    if items.is_empty() {
        return last;
    }

    format!("{} or {last}", items.join(", "))
}

/// The name a message binds a case to, as `draft` for `Draft`.
fn bound_name(case: &[u8]) -> String {
    let case = String::from_utf8_lossy(case);
    let mut letters = case.chars();

    letters.next().map(|first| first.to_lowercase().chain(letters).collect()).unwrap_or_default()
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

/// The write a message shows as the way to change `place`, a value of the machine `machine_name` with the graph
/// `graph`: a test of the case its first move moves from, and that move called on the name the test binds.
fn move_example(machine_name: Word, graph: &MachineMetadata, place: &str) -> String {
    let (name, from) = graph.moves.first().map_or((b"move".as_slice(), b"Case".as_slice()), |r#move| {
        (r#move.name.as_bytes(), r#move.from.first().map_or(b"Case".as_slice(), Word::as_bytes))
    });
    let bound = bound_name(from);

    format!(
        "if ({place} is {machine_name}.{} {bound}) {{ {place} = {bound}.{}(…); }}",
        BytesDisplay(from),
        BytesDisplay(name)
    )
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
