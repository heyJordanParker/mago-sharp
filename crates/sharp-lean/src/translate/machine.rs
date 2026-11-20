//! A PHP# machine's invariants, spec section 20.2, as the Lean statements that prove each one after every entry point
//! and every move that gives its case, and the search for the moves that break it, which the statement's theorem runs.

use mago_codex::metadata::machine::MachineMetadata;
use mago_codex::metadata::machine::MachineMove;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::Enum;
use mago_syntax::cst::EnumCase;
use mago_syntax::cst::EnumCaseItem;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Law as LawNode;
use mago_syntax::cst::Method;
use mago_word::Word;
use mago_word::word;

use crate::package::place_of;
use crate::translate::Declaration;
use crate::translate::Law;
use crate::translate::Role;
use crate::translate::Translator;
use crate::translate::Use;
use crate::translate::binder;
use crate::translate::body::Body;
use crate::translate::names;
use crate::translate::parameter_name;
use crate::translate::proof::Proof;

impl Translator<'_, '_> {
    /// The case of the machine `machine` whose invariant `law` is, spec section 20.2: the case its one parameter names.
    /// None for any other law, and for a law of a class-like that is no machine.
    pub(super) fn invariant_case(&self, machine: &[u8], law: &LawNode) -> Option<Word> {
        let metadata = self.checked.types().machine(word(machine))?;
        let [parameter] = law.parameter_list.parameters.as_slice() else {
            return None;
        };
        let Some(Hint::Identifier(identifier)) = &parameter.hint else {
            return None;
        };
        let class = self.checked.names().get(identifier);

        metadata.cases.iter().map(|case| case.name).find(|case| case_class(machine, case.as_bytes()) == class)
    }

    /// The invariants of the machine `r#enum`, spec section 20.2, each a law whose one parameter is one of its cases.
    /// Each one's claim is `<law>.«$claim»`, which computes whether it holds of a value, and its statement says that the
    /// claim holds of each value of its case an entry point gives, and of each one a move gives from a case whose own
    /// invariants hold. Its theorem first runs `<law>.«$trace»`, the search for the moves that break it.
    pub(super) fn invariants(
        &mut self,
        r#enum: &Enum,
        machine: &MachineMetadata,
        full: &[u8],
        lean: &str,
        short: &str,
    ) {
        let invariants: Vec<(&LawNode, Word)> = r#enum
            .members
            .iter()
            .filter_map(|member| match member {
                ClassLikeMember::Law(law) => Some((law, self.invariant_case(full, law)?)),
                _ => None,
            })
            .collect();
        if invariants.is_empty() {
            return;
        }

        let machine = Machine { r#enum, metadata: machine, full, lean, short };
        for (law, _) in &invariants {
            self.claim(full, lean, short, law);
        }
        let shown = self.shown(&machine);
        let moves = self.moves(&machine);
        let file = self.translation.file;
        let use_of = |key: &str, place: String| Use {
            key: key.to_owned(),
            file: Some(file),
            place,
            smaller: None,
            names_only: false,
        };

        for (law, case) in &invariants {
            let name = String::from_utf8_lossy(law.name.value).into_owned();
            let key = names::member(lean, law.name.value);
            let claim = claim_of(&key);
            let place = format!("{short}.{name}");
            let mut body = Body::new(self.checked, self.reach, full, place.clone());
            let mut uses = vec![use_of(&claim, place.clone())];
            let given = machine.pattern(case.as_bytes());
            let check = |call: &str| {
                [
                    "holds do".to_owned(),
                    format!("    let «$given» ← {call}"),
                    "    match «$given» with".to_owned(),
                    format!("    | {given} => {claim} «$given»"),
                    "    | _ => pure true".to_owned(),
                ]
                .join("\n")
            };

            let mut after = Vec::new();
            let mut parts = Vec::new();
            for entry in machine.metadata.entry_points.iter().filter(|entry| entry.to.contains(case)) {
                let Some(method) = machine.entry_point(entry.name) else {
                    continue;
                };
                let entry_key = names::member(lean, method.name.value);
                uses.push(use_of(&entry_key, format!("{short}.{}", String::from_utf8_lossy(method.name.value))));
                let binders: Vec<String> =
                    method.parameter_list.parameters.iter().map(|parameter| binder(&mut body, parameter)).collect();
                let arguments: Vec<String> = method.parameter_list.parameters.iter().map(parameter_name).collect();
                after.push(written_call(method));
                parts.push(format!(
                    "({}{})",
                    quantified(&binders),
                    check(&std::iter::once(entry_key).chain(arguments).collect::<Vec<_>>().join(" "))
                ));
            }
            for from in machine.cases() {
                for (r#move, method, move_key) in machine.moves_from(from) {
                    if !r#move.to.contains(case) {
                        continue;
                    }
                    uses.push(use_of(&move_key, place_of(&move_key)));
                    let (fields, value) = machine.held(from);
                    let mut binders: Vec<String> = fields
                        .iter()
                        .map(|(field, hint)| format!("({field} : {})", body.declared_type(*hint, from.span())))
                        .collect();
                    binders
                        .extend(method.parameter_list.parameters.iter().map(|parameter| binder(&mut body, parameter)));
                    let mut premises = Vec::new();
                    for (held, _) in invariants.iter().filter(|(_, held)| held.as_bytes() == from.item.name().value) {
                        let held_claim = claim_of(&names::member(lean, held.name.value));
                        uses.push(use_of(&held_claim, place.clone()));
                        premises.push(format!("holds ({held_claim} {value}) → "));
                    }
                    let premises = premises.concat();
                    let arguments: Vec<String> = method.parameter_list.parameters.iter().map(parameter_name).collect();
                    after.push(written_call(method));
                    parts.push(format!(
                        "({}{premises}{})",
                        quantified(&binders),
                        check(&[move_key, value].into_iter().chain(arguments).collect::<Vec<_>>().join(" "))
                    ));
                }
            }
            if parts.is_empty() {
                continue;
            }

            let trace = format!("{key}.«$trace»");
            let broken = format!(
                "(fun «$given» => match «$given» with | {given} => Sharp.Machine.breaks ({claim} «$given») | _ => false)"
            );
            let trace_text = format!(
                "def {trace} : Option (List String) :=\n  Sharp.Machine.search {moves} {shown} {broken} {DEPTH}\n    (Sharp.Machine.next {shown} [] [{}])",
                self.entry_steps(&machine, &mut uses).join(", ")
            );
            let trace_uses =
                [use_of(&moves, place.clone()), use_of(&shown, place.clone()), use_of(&claim, place.clone())];
            self.translation.declarations.push(Declaration {
                key: trace.clone(),
                span: law.name.span,
                text: trace_text,
                uses: trace_uses.into_iter().chain(uses.iter().cloned()).collect(),
                unmodeled: Vec::new(),
                role: Role::Definition,
                measure: None,
                opaque: None,
            });
            uses.push(use_of(&trace, place.clone()));

            let text = format!("@[law] def {key} : Prop :=\n  {}", parts.join(" ∧\n  "));
            let (_, body_uses, unmodeled) = body.finish();
            uses.extend(body_uses);
            let source = &self.checked.file().contents;
            let claim_text = source
                .get(law.claim.span().start.offset as usize..law.claim.span().end.offset as usize)
                .map(|claim| String::from_utf8_lossy(claim).into_owned())
                .unwrap_or_default();
            let span = Span::between(law.law.span(), law.name.span);
            self.translation.laws.push(Law {
                name,
                statement: key.clone(),
                span,
                proof: Proof::invariant(&key, &trace, after, claim_text),
            });
            self.translation.declarations.push(Declaration {
                key,
                span,
                text,
                uses,
                unmodeled,
                role: Role::Definition,
                measure: None,
                opaque: None,
            });
        }
    }

    /// `<law>.«$claim»`, the claim of the invariant `law` of a machine, spec section 20.2: whether it holds of a value,
    /// computing ints exactly, as a law's claim does. A value its `when` condition is false of holds it.
    fn claim(&mut self, full: &[u8], lean: &str, short: &str, law: &LawNode) {
        let place = format!("{short}.{}", String::from_utf8_lossy(law.name.value));
        let mut lines = Vec::new();
        let (mut uses, mut unmodeled) = (Vec::new(), Vec::new());
        if let Some(condition) = &law.condition {
            let mut computed = Body::for_law(self.checked, self.reach, full, place.clone(), law);
            computed.computes_exactly();
            let (condition_lines, terms, condition_uses, condition_unmodeled) =
                computed.terms(&[], &[condition.expression]);
            lines.extend(condition_lines);
            lines.push(format!("unless {} do return true", terms[0]));
            uses.extend(condition_uses);
            unmodeled.extend(condition_unmodeled);
        }
        let mut body = Body::for_law(self.checked, self.reach, full, place, law);
        let binders: Vec<String> =
            law.parameter_list.parameters.iter().map(|parameter| binder(&mut body, parameter)).collect();
        body.computes_exactly();
        let (claim_lines, claim_uses, claim_unmodeled) = body.expression_body(law.claim);
        lines.extend(claim_lines);
        uses.extend(claim_uses);
        unmodeled.extend(claim_unmodeled);

        let key = claim_of(&names::member(lean, law.name.value));
        self.define(key, law.name.span, &binders, "Bool", (lines, uses, unmodeled), None);
    }

    /// `<machine>.«$shown»`, each value of `machine` as a trace writes it, spec section 20.2: its case, and each field
    /// by name with its value, as `Paid(cents: 0)`. A field of a type no candidate value has is written `…`.
    fn shown(&mut self, machine: &Machine<'_, '_, '_>) -> String {
        let key = format!("{}.«$shown»", machine.lean);
        let mut body = Body::new(self.checked, self.reach, machine.full, format!("{}.shown", machine.short));
        let domain = body.class_type(machine.r#enum.name.span, machine.full);
        let mut lines = vec![format!("def {key} : {domain} → String")];
        for case in machine.cases() {
            let name = String::from_utf8_lossy(case.item.name().value);
            let fields = case_fields(case);
            if fields.is_empty() {
                lines.push(format!("  | .{} => {}", names::identifier(case.item.name().value), lean_string(&name)));
                continue;
            }
            let binders: Vec<String> = fields.iter().map(|(field, _)| names::identifier(field)).collect();
            let values: Vec<String> = fields
                .iter()
                .zip(&binders)
                .map(|((field, hint), binder)| {
                    let value = match hint {
                        Some(Hint::Integer(_)) => format!("toString {binder}.toInt"),
                        Some(Hint::Bool(_)) => format!("toString {binder}"),
                        Some(Hint::String(_)) => format!("Sharp.Machine.text {binder}"),
                        _ => lean_string("…"),
                    };
                    format!("{} ++ {value}", lean_string(&format!("{}: ", String::from_utf8_lossy(field))))
                })
                .collect();
            lines.push(format!(
                "  | .{} {} => {} ++ {} ++ \")\"",
                names::identifier(case.item.name().value),
                binders.join(" "),
                lean_string(&format!("{name}(")),
                values.join(" ++ \", \" ++ ")
            ));
        }
        self.declare(body, key.clone(), machine.r#enum.name.span, lines.join("\n"), Role::Definition);

        key
    }

    /// `<machine>.«$moves»`, the moves of each value of `machine` a trace takes, spec section 20.2: each move from the
    /// value's case, on each list of candidate values of its parameters, as written with them and as the step it runs.
    fn moves(&mut self, machine: &Machine<'_, '_, '_>) -> String {
        let key = format!("{}.«$moves»", machine.lean);
        let mut body = Body::new(self.checked, self.reach, machine.full, format!("{}.moves", machine.short));
        let domain = body.class_type(machine.r#enum.name.span, machine.full);
        let mut lines = vec![
            format!("def {key} (this : {domain}) : List (String × M {domain}) :="),
            "  match this with".to_owned(),
        ];
        let mut uses = Vec::new();
        for case in machine.cases() {
            let mut steps = Vec::new();
            for (_, method, move_key) in machine.moves_from(case) {
                for (written, arguments) in self.candidate_calls(machine, method) {
                    steps.push(format!("({}, {move_key} this{arguments})", lean_string(&written)));
                }
                uses.push(Use {
                    key: move_key.clone(),
                    file: Some(self.translation.file),
                    place: place_of(&move_key),
                    smaller: None,
                    names_only: false,
                });
            }
            if !steps.is_empty() {
                lines.push(format!("  | {} => [{}]", machine.pattern(case.item.name().value), steps.join(", ")));
            }
        }
        lines.push("  | _ => []".to_owned());
        let (_, body_uses, unmodeled) = body.finish();
        self.translation.declarations.push(Declaration {
            key: key.clone(),
            span: machine.r#enum.name.span,
            text: lines.join("\n"),
            uses: body_uses.into_iter().chain(uses).collect(),
            unmodeled,
            role: Role::Definition,
            measure: None,
            opaque: None,
        });

        key
    }

    /// The first steps of a trace through `machine`, spec section 20.2: each entry point on each list of candidate
    /// values of its parameters, as written with them and as the step it runs. Each entry point it calls is among
    /// `uses`.
    fn entry_steps(&self, machine: &Machine<'_, '_, '_>, uses: &mut Vec<Use>) -> Vec<String> {
        let mut steps = Vec::new();
        for entry in &machine.metadata.entry_points {
            let Some(method) = machine.entry_point(entry.name) else {
                continue;
            };
            let entry_key = names::member(machine.lean, method.name.value);
            for (written, arguments) in self.candidate_calls(machine, method) {
                steps.push(format!("({}, {entry_key}{arguments})", lean_string(&written)));
            }
            uses.push(Use {
                key: entry_key.clone(),
                file: Some(self.translation.file),
                place: place_of(&entry_key),
                smaller: None,
                names_only: false,
            });
        }

        steps
    }

    /// Each call of `method` a trace through `machine` makes, spec section 20.2, as written and as its Lean arguments,
    /// each after a space: one for each list of candidate values of its parameters, in order, the first parameter's
    /// values varying slowest. None when a parameter's type has no candidate value.
    fn candidate_calls(&self, machine: &Machine<'_, '_, '_>, method: &Method) -> Vec<(String, String)> {
        let mut calls: Vec<(Vec<String>, String)> = vec![(Vec::new(), String::new())];
        for parameter in method.parameter_list.parameters.iter() {
            let name =
                String::from_utf8_lossy(parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name));
            let candidates = self.candidates(machine, parameter.hint.as_ref());
            let mut extended = Vec::new();
            for (written, arguments) in &calls {
                for (term, value) in &candidates {
                    let mut written = written.clone();
                    written.push(format!("{name}: {value}"));
                    extended.push((written, format!("{arguments} {term}")));
                }
            }
            calls = extended;
        }
        let name = String::from_utf8_lossy(method.name.value);

        calls.into_iter().map(|(written, arguments)| (format!("{name}({})", written.join(", ")), arguments)).collect()
    }

    /// The values a trace through `machine` passes for a parameter of the type `hint`, spec section 20.2, each as its
    /// Lean term and as PHP# writes it: the ints `0`, `1` and `-1`, the strings `""` and `"a"`, both bools, and each case
    /// of the machine a parameter of its type takes, with the first candidate of each field. None for any other type.
    fn candidates(&self, machine: &Machine<'_, '_, '_>, hint: Option<&Hint>) -> Vec<(String, String)> {
        let scalars = |hint: Option<&Hint>| -> Vec<(String, String)> {
            match hint {
                Some(Hint::Integer(_)) => {
                    ["0", "1", "-1"].map(|value| (format!("({value} : int)"), value.to_owned())).to_vec()
                }
                Some(Hint::Bool(_)) => ["false", "true"].map(|value| (value.to_owned(), value.to_owned())).to_vec(),
                Some(Hint::String(_)) => [b"".as_slice(), b"a".as_slice()]
                    .map(|value| (names::string(value), format!("\"{}\"", String::from_utf8_lossy(value))))
                    .to_vec(),
                _ => Vec::new(),
            }
        };
        let Some(Hint::Identifier(identifier)) = hint else {
            return scalars(hint);
        };
        let class = self.checked.names().get(identifier);
        let machine_short = names::short_name(machine.full);

        machine
            .cases()
            .filter(|case| class == machine.full || class == case_class(machine.full, case.item.name().value))
            .filter_map(|case| {
                let name = String::from_utf8_lossy(case.item.name().value);
                let fields = case_fields(case);
                if fields.is_empty() {
                    return Some((machine.constructor(case.item.name().value), format!("{machine_short}.{name}")));
                }
                let values: Vec<(String, String)> =
                    fields.iter().map(|(_, hint)| scalars(*hint).into_iter().next()).collect::<Option<_>>()?;
                let terms: Vec<&str> = values.iter().map(|(term, _)| term.as_str()).collect();
                let written: Vec<String> = fields
                    .iter()
                    .zip(&values)
                    .map(|((field, _), (_, value))| format!("{}: {value}", String::from_utf8_lossy(field)))
                    .collect();

                Some((
                    format!("({} {})", machine.constructor(case.item.name().value), terms.join(" ")),
                    format!("{machine_short}.{name}({})", written.join(", ")),
                ))
            })
            .collect()
    }
}

/// How many moves after its entry point a trace through a machine takes at most, spec section 20.2.
const DEPTH: usize = 3;

/// A machine whose invariants a translation proves, spec section 20.2: its declaration, its graph, and its fully
/// qualified PHP name, Lean name and short name.
struct Machine<'machine, 'ast, 'arena> {
    r#enum: &'ast Enum<'arena>,
    metadata: &'machine MachineMetadata,
    full: &'machine [u8],
    lean: &'machine str,
    short: &'machine str,
}

impl<'machine, 'ast, 'arena> Machine<'machine, 'ast, 'arena> {
    /// Its cases, in the order it declares them.
    fn cases(&self) -> impl Iterator<Item = &'ast EnumCase<'arena>> + use<'machine, 'ast, 'arena> {
        self.r#enum.members.iter().filter_map(|member| match member {
            ClassLikeMember::EnumCase(case) => Some(case),
            _ => None,
        })
    }

    /// The Lean constructor of its case `case`: `Shop.OrderStatus.Paid`.
    fn constructor(&self, case: &[u8]) -> String {
        names::member(self.lean, case)
    }

    /// The Lean pattern of its case `case`: `.Draft`, or `.Paid ..` for a case with fields.
    fn pattern(&self, case: &[u8]) -> String {
        let fields =
            self.cases().any(|declared| declared.item.name().value == case && !case_fields(declared).is_empty());

        format!(".{}{}", names::identifier(case), if fields { " .." } else { "" })
    }

    /// The method of its entry point `name`.
    fn entry_point(&self, name: Word) -> Option<&'ast Method<'arena>> {
        self.r#enum.members.iter().find_map(|member| match member {
            ClassLikeMember::Method(method) if method.is_static() && method.name.value == name.as_bytes() => {
                Some(method)
            }
            _ => None,
        })
    }

    /// Each move from its case `from`, with its method and the Lean definition the method is: one of the case's block,
    /// `Shop.OrderStatus.Draft.pay`, or one of the machine's body, `Shop.OrderStatus.cancel`, which a move of the
    /// case's block of its name replaces.
    fn moves_from(&self, from: &'ast EnumCase<'arena>) -> Vec<(&'machine MachineMove, &'ast Method<'arena>, String)> {
        let case = from.item.name().value;
        let own = |name: &[u8]| {
            from.members().iter().find_map(|member| match member {
                ClassLikeMember::Method(method) if method.name.value == name => Some(method),
                _ => None,
            })
        };
        let mut moves = Vec::new();
        for r#move in self.metadata.moves.iter().filter(|r#move| r#move.from.iter().any(|from| from.as_bytes() == case))
        {
            match own(r#move.name.as_bytes()) {
                Some(method) if method.name.span == r#move.span => {
                    moves.push((r#move, method, names::member(&names::member(self.lean, case), method.name.value)));
                }
                Some(_) => {}
                None => {
                    let declared = self.r#enum.members.iter().find_map(|member| match member {
                        ClassLikeMember::Method(method) if method.name.span == r#move.span => Some(method),
                        _ => None,
                    });
                    if let Some(method) = declared {
                        moves.push((r#move, method, names::member(self.lean, method.name.value)));
                    }
                }
            }
        }

        moves
    }

    /// The binders of the fields of its case `case`, by name and type, and the value of the case they hold:
    /// `Shop.OrderStatus.Draft`, or `(Shop.OrderStatus.Paid «this.cents»)`.
    fn held<'case>(&self, case: &'case EnumCase<'arena>) -> (Vec<(String, Option<&'case Hint<'arena>>)>, String) {
        let fields: Vec<(String, Option<&Hint>)> = case_fields(case)
            .into_iter()
            .map(|(field, hint)| (format!("«this.{}»", String::from_utf8_lossy(field)), hint))
            .collect();
        let constructor = self.constructor(case.item.name().value);
        let value = if fields.is_empty() {
            constructor
        } else {
            let binders: Vec<&str> = fields.iter().map(|(binder, _)| binder.as_str()).collect();
            format!("({constructor} {})", binders.join(" "))
        };

        (fields, value)
    }
}

/// `<law>.«$claim»`, the claim of the invariant whose Lean name is `law`, spec section 20.2.
fn claim_of(law: &str) -> String {
    format!("{law}.«$claim»")
}

/// The full name of the class the case `case` of the enum `full` runs as, spec section 20: `Shop\OrderStatus\Paid`.
pub(super) fn case_class(full: &[u8], case: &[u8]) -> Vec<u8> {
    [full, b"\\", case].concat()
}

/// The fields of `case`, each by its name without `$` and its type: none for a case that carries no data.
fn case_fields<'case, 'arena>(case: &'case EnumCase<'arena>) -> Vec<(&'case [u8], Option<&'case Hint<'arena>>)> {
    match &case.item {
        EnumCaseItem::Data(item) => item
            .parameter_list
            .parameters
            .iter()
            .map(|field| (field.variable.name.strip_prefix(b"$").unwrap_or(field.variable.name), field.hint.as_ref()))
            .collect(),
        _ => Vec::new(),
    }
}

/// A call of `method` as an error names it, with its parameters by name: `pay(cents)`.
fn written_call(method: &Method) -> String {
    let parameters: Vec<String> = method
        .parameter_list
        .parameters
        .iter()
        .map(|parameter| {
            String::from_utf8_lossy(parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name))
                .into_owned()
        })
        .collect();

    format!("{}({})", String::from_utf8_lossy(method.name.value), parameters.join(", "))
}

/// `∀ binders, `, or nothing when there are no binders.
fn quantified(binders: &[String]) -> String {
    if binders.is_empty() { String::new() } else { format!("∀ {}, ", binders.join(" ")) }
}

/// `text` as a Lean string literal.
fn lean_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}
