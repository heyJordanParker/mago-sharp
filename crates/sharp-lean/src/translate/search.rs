//! A law's claim, `<law>.«$claim»`, and the search for the small values that break a law whose proof stays open, spec
//! section 28: `<law>.«$breaking»`, `Sharp.Law.breaking` of the claim on each list of candidate values of the law's
//! parameters, which the law checker's runner evaluates once the law's theorem leaves a goal open. What it finds is a
//! run of the code, so it proves nothing, and finding none proves nothing either.

use mago_codex::metadata::parameter::FunctionLikeParameterMetadata;
use mago_codex::scanner::get_union_from_hint;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::reference::TReference;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::resolution::TypeResolutionContext;
use mago_codex::ttype::union::TUnion;
use mago_span::Span;
use mago_syntax::cst::Law as LawNode;
use mago_syntax::dialect::Dialect;
use mago_word::word;

use crate::reach::Reach;
use crate::translate::Declaration;
use crate::translate::Role;
use crate::translate::Translator;
use crate::translate::binder;
use crate::translate::body::Body;
use crate::translate::names;

/// How many classes deep a candidate value of a law's parameter builds: a class whose constructor takes a class takes
/// values of that class, whose constructors take no class.
const DEPTH: usize = 2;

/// How many lists of candidate values the search runs the claim on at most.
const LIMIT: usize = 256;

/// How many values of one class, or of one case of an enum, the search builds at most.
const VALUES: usize = 9;

impl Translator<'_, '_> {
    /// `<law>.«$claim»`, the claim of `law` of the class-like `full`: whether it holds of its parameters' values,
    /// computing ints exactly, as a law's claim does. A value its `when` condition is false of holds it.
    pub(super) fn claim(&mut self, full: &[u8], lean: &str, short: &str, law: &LawNode) {
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

    /// Writes `<law>.«$breaking»` for `law` of the class-like `full`, with its claim, and gives its key: the first values
    /// of the law's parameters its claim finishes with `false` on, written as PHP# writes them, `a = new Account(0),
    /// amount = -1`, as `Sharp.Law.first` finds them, the first parameter's values varying slowest. A parameter takes
    /// its first candidate value alone once the lists of values before it number more than [`LIMIT`] with its others.
    /// None for a law without parameters, whose claim takes no value, and for one with a parameter whose type has no
    /// candidate value.
    pub(super) fn breaking(&mut self, full: &[u8], lean: &str, short: &str, law: &LawNode) -> Option<String> {
        if law.parameter_list.parameters.is_empty() {
            return None;
        }
        let place = format!("{short}.{}", String::from_utf8_lossy(law.name.value));
        let mut body = Body::new(self.checked, self.reach, full, place.clone());
        let mut candidates = Candidates { body: &mut body, reach: self.reach, span: law.name.span, bound: 0 };
        let mut lines = Vec::new();
        let mut lists = 1;
        for (index, parameter) in law.parameter_list.parameters.iter().enumerate() {
            let r#type = get_union_from_hint(
                parameter.hint.as_ref()?,
                Some(word(full)),
                self.checked.names(),
                &TypeResolutionContext::new(),
                Dialect::Sharp,
            );
            let mut values = candidates.of(&r#type, DEPTH);
            if values.is_empty() {
                return None;
            }
            if lists * values.len() > LIMIT {
                values.truncate(1);
            }
            lists *= values.len();
            let name = parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name);
            let values: Vec<String> =
                values.iter().map(|value| format!("({}, {})", lean_string(&value.written), value.term)).collect();
            lines.push(format!(
                "  Sharp.Law.first {} [{}] fun «$argument{index}» =>",
                lean_string(&String::from_utf8_lossy(name)),
                values.join(", ")
            ));
        }

        self.claim(full, lean, short, law);
        let key = names::member(lean, law.name.value);
        let claim = claim_of(&key);
        let arguments: Vec<String> =
            (0..law.parameter_list.parameters.len()).map(|index| format!("«$argument{index}»")).collect();
        lines.push(format!("  Sharp.Law.broken ({claim} {})", arguments.join(" ")));
        let breaking = format!("{key}.«$breaking»");
        let text = format!("protected def {breaking} : Option String :=\n{}", lines.join("\n"));
        let (_, mut uses, unmodeled) = body.finish();
        uses.push(self.own_use(&claim, place));
        self.translation.declarations.push(Declaration {
            key: breaking.clone(),
            span: law.name.span,
            text,
            uses,
            unmodeled,
            role: Role::Definition,
            measure: None,
            opaque: None,
        });

        Some(breaking)
    }
}

/// `<law>.«$claim»`, the claim of the law whose Lean name is `law`.
pub(super) fn claim_of(law: &str) -> String {
    format!("{law}.«$claim»")
}

/// `text` as a Lean string literal.
pub(super) fn lean_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A candidate value of a type: its Lean term, a computation in `M` that gives it, and the value as PHP# writes it.
#[derive(Clone)]
struct Candidate {
    term: String,
    written: String,
}

/// The candidate values the search for the small values that break a law passes, of the classes `reach` holds: each
/// term built in `body`, which records each declaration a term uses and each construct it cannot write, at `span`, and
/// each value a term binds named apart by `bound`.
struct Candidates<'candidates, 'body, 'program> {
    body: &'candidates mut Body<'body, 'program>,
    reach: &'body Reach,
    span: Span,
    bound: usize,
}

impl Candidates<'_, '_, '_> {
    /// The candidate values of `r#type`, a class building at most `depth` classes deep: the ints `0`, `1` and `-1`, both
    /// bools, the strings `""` and `"a"`, each case of an enum, each implementer of a sealed interface, the values of a
    /// class `new` builds from its constructor's candidate values, `[]` and a list of one and of two elements, the
    /// empty `Map`, and `null` last for a type that takes it. None for a type of more than one kind of value and for
    /// any other type.
    fn of(&mut self, r#type: &TUnion, depth: usize) -> Vec<Candidate> {
        let values: Vec<&TAtomic> = r#type.types.iter().filter(|atomic| !atomic.is_null()).collect();
        let [value] = values.as_slice() else {
            return Vec::new();
        };
        let mut values = match value {
            TAtomic::Scalar(TScalar::Integer(_)) => ["0", "1", "-1"]
                .map(|int| Candidate { term: format!("(pure ({int} : int))"), written: int.to_owned() })
                .to_vec(),
            TAtomic::Scalar(TScalar::Bool(_)) => ["false", "true"]
                .map(|bool| Candidate { term: format!("(pure {bool})"), written: bool.to_owned() })
                .to_vec(),
            TAtomic::Scalar(TScalar::String(_)) => [b"".as_slice(), b"a".as_slice()]
                .map(|string| Candidate {
                    term: format!("(pure {})", names::string(string)),
                    written: format!("\"{}\"", String::from_utf8_lossy(string)),
                })
                .to_vec(),
            TAtomic::Object(TObject::Enum(r#enum)) => self.cases(r#enum.name.as_bytes(), None, depth),
            TAtomic::Object(TObject::Named(named)) => self.objects(named.name.as_bytes(), depth),
            TAtomic::Reference(TReference::Symbol { name, parameters: None, .. }) => {
                self.objects(name.as_bytes(), depth)
            }
            TAtomic::Array(TArray::List(list)) => self.lists(&list.element_type, depth),
            TAtomic::Array(TArray::Keyed(_)) => {
                vec![Candidate { term: "(pure Sharp.Map.empty)".to_owned(), written: "[:]".to_owned() }]
            }
            _ => Vec::new(),
        };
        if r#type.is_nullable() && !values.is_empty() {
            for value in &mut values {
                value.term = format!("(Option.some <$> {})", value.term);
            }
            values.push(Candidate { term: "(pure Option.none)".to_owned(), written: "null".to_owned() });
        }

        values
    }

    /// The candidate values of the class-like `class`: the one case of an enum a case's class is, spec section 20, each
    /// case of an enum, each implementer of a sealed interface, and else one value of the class `new` builds.
    fn objects(&mut self, class: &[u8], depth: usize) -> Vec<Candidate> {
        let reach = self.reach;
        if let Some(at) = class.iter().rposition(|&byte| byte == b'\\') {
            let (r#enum, case) = (&class[..at], &class[at + 1..]);
            if reach.cases(r#enum).iter().any(|known| known.as_bytes() == case) {
                return self.cases(r#enum, Some(case), depth);
            }
        }
        if !reach.cases(class).is_empty() {
            return self.cases(class, None, depth);
        }
        if let Some(implementers) = reach.implementers(class) {
            return implementers.iter().flat_map(|implementer| self.built(implementer.as_bytes(), depth)).collect();
        }

        self.built(class, depth)
    }

    /// Each case of the enum `r#enum`, spec section 20, or its case `only` alone, a case that carries data with the
    /// values of its fields [`Candidates::applied`] gives: `Shape.Dot`, `Shape.Circle(0)`. A case whose field has no
    /// candidate value is left out.
    fn cases(&mut self, r#enum: &[u8], only: Option<&[u8]>, depth: usize) -> Vec<Candidate> {
        let reach = self.reach;
        let lean = self.body.class_type(self.span, r#enum);
        let name = reach.class(r#enum).map_or_else(|| word(r#enum), |reached| reached.name);
        let short = names::short_name(name.as_bytes());
        let mut values = Vec::new();
        for case in reach.cases(r#enum).iter().filter(|case| only.is_none_or(|only| case.as_bytes() == only)) {
            let constructor = names::member(&lean, case.as_bytes());
            let written = format!("{short}.{case}");
            let Some(class) = self.body.checked.types().enum_case_class(word(r#enum), *case) else {
                values.push(Candidate { term: format!("(pure {constructor})"), written });
                continue;
            };
            let parameters = self.body.checked.types().constructor_parameters(class);
            values.extend(self.applied(parameters, depth, &constructor, false, &written));
        }

        values
    }

    /// The values of the class `class` `new` builds from the values of its constructor's parameters
    /// [`Candidates::applied`] gives, as values of the sealed interface it implements when it implements one:
    /// `new Account(0)`. None for a class `new` cannot build, one past `depth`, and one whose constructor takes a value
    /// of no candidate.
    fn built(&mut self, class: &[u8], depth: usize) -> Vec<Candidate> {
        let Some(constructor) = self.body.constructor(class).filter(|_| depth > 0) else {
            return Vec::new();
        };
        let parameters = self.body.checked.types().constructor_parameters(word(class));
        let name = self.reach.class(class).map_or_else(|| word(class), |reached| reached.name);
        let written = format!("new {}", names::short_name(name.as_bytes()));
        let mut values = self.applied(parameters, depth - 1, &constructor, true, &written);
        if let Some((_, held)) = self.body.sealed(self.span, class) {
            for value in &mut values {
                value.term = format!("({held} <$> {})", value.term);
            }
        }

        values
    }

    /// `function` applied to each list of candidate values of `parameters`, the first parameter's varying slowest,
    /// building classes at most `depth` deep: a computation in `M` when `monadic`, and else a value. A parameter takes
    /// its first candidate alone once the lists before it number more than [`VALUES`] with its others. Written
    /// `name(…)`, or `name` alone for a value of no parameter that is no `new`. None when a parameter's type has no
    /// candidate value.
    fn applied(
        &mut self,
        parameters: &[FunctionLikeParameterMetadata],
        depth: usize,
        function: &str,
        monadic: bool,
        name: &str,
    ) -> Vec<Candidate> {
        let mut lists: Vec<Vec<(String, Candidate)>> = vec![Vec::new()];
        for parameter in parameters {
            let Some(declared) = &parameter.type_metadata else {
                return Vec::new();
            };
            let mut values = self.of(&declared.type_union, depth);
            if lists.len() * values.len() > VALUES {
                values.truncate(1);
            }
            self.bound += 1;
            let bound = format!("«$value{}»", self.bound);
            lists = lists
                .iter()
                .flat_map(|list| {
                    values.iter().map(|value| [list.as_slice(), &[(bound.clone(), value.clone())]].concat())
                })
                .collect();
        }

        lists
            .iter()
            .map(|arguments| {
                let applied: Vec<&str> =
                    std::iter::once(function).chain(arguments.iter().map(|(bound, _)| bound.as_str())).collect();
                let last = if monadic {
                    format!("({})", applied.join(" "))
                } else {
                    format!("(pure ({}))", applied.join(" "))
                };
                let term = arguments
                    .iter()
                    .rev()
                    .fold(last, |inner, (bound, value)| format!("({} >>= fun {bound} => {inner})", value.term));
                let written: Vec<&str> = arguments.iter().map(|(_, value)| value.written.as_str()).collect();
                let written = if written.is_empty() && !monadic {
                    name.to_owned()
                } else {
                    format!("{name}({})", written.join(", "))
                };

                Candidate { term, written }
            })
            .collect()
    }

    /// `[]`, and the lists of the first candidate element and of the first two, of a list whose elements are
    /// `element`.
    fn lists(&mut self, element: &TUnion, depth: usize) -> Vec<Candidate> {
        let elements = self.of(element, depth);
        let mut values = vec![Candidate { term: "(pure [])".to_owned(), written: "[]".to_owned() }];
        for count in 1..=elements.len().min(2) {
            let taken = &elements[..count];
            let bound: Vec<String> = taken
                .iter()
                .map(|_| {
                    self.bound += 1;
                    format!("«$value{}»", self.bound)
                })
                .collect();
            let term = taken
                .iter()
                .zip(&bound)
                .rev()
                .fold(format!("(pure [{}])", bound.join(", ")), |inner, (value, bound)| {
                    format!("({} >>= fun {bound} => {inner})", value.term)
                });
            let written: Vec<&str> = taken.iter().map(|value| value.written.as_str()).collect();
            values.push(Candidate { term, written: format!("[{}]", written.join(", ")) });
        }

        values
    }
}
