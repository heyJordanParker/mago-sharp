//! What a `match` arm or an `is` tests, as the Lean `match`es, `let`s and conditions that test it, and the choice of
//! branch those tests make.

use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::Hint;
use mago_syntax::cst::ListPattern;
use mago_syntax::cst::ListPatternElement;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::TypePattern;
use mago_word::word;

use crate::issues;
use crate::translate::body::Body;
use crate::translate::body::Branch;
use crate::translate::names;

/// Which branch runs, as Lean writes it: `if`s, `match`es that bind a pattern's locals, and `let`s.
#[derive(Clone)]
pub(super) enum Choice {
    Branch(Branch),
    If(String, Box<Choice>, Box<Choice>),
    /// The term matched, and each pattern with its branch.
    Match(String, Vec<(String, Choice)>),
    Let(String, String, Box<Choice>),
}

/// What a `match` arm or an `is` tests, as Lean tests it: the Lean patterns its subject and parts of the subject match,
/// outermost first, the locals it binds from them, and the conditions it adds.
#[derive(Default)]
pub(super) struct Test {
    matches: Vec<Destructure>,
    lets: Vec<(String, String)>,
    pub(super) conditions: Vec<String>,
}

impl Test {
    /// The test of a pattern that names no value as one condition: each of its matches, outermost first, false for a
    /// value its pattern leaves out, with its conditions inside.
    pub(super) fn condition(self) -> String {
        assert!(self.lets.is_empty(), "a pattern that names no value binds no local");
        let mut condition = if self.conditions.is_empty() { "true".to_owned() } else { self.conditions.join(" && ") };
        for Destructure { term, pattern } in self.matches.into_iter().rev() {
            condition = format!("(match {term} with | {pattern} => {condition} | _ => false)");
        }

        condition
    }
}

/// One Lean `match` of a test: the term, and the pattern.
struct Destructure {
    term: String,
    pattern: String,
}

impl Body<'_, '_> {
    /// What `subject`, a term of `r#type`, matching `pattern` tests, or `None` after recording why Lean cannot test
    /// it. With `written`, the subject as PHP# writes it, a list pattern names each element before its rest that it
    /// leaves unnamed by its place, so a goal of a law's case reads `lines[0]` where the arm `[Line, ...rest]` names
    /// none.
    pub(super) fn test(
        &mut self,
        subject: &str,
        written: Option<&str>,
        r#type: &TUnion,
        pattern: &Pattern,
    ) -> Option<Test> {
        match pattern {
            Pattern::Type(pattern) => self.type_test(subject, r#type, pattern),
            Pattern::List(list) => self.list_test(subject, written, r#type, list),
            Pattern::Parenthesized(parenthesized) => self.test(subject, written, r#type, parenthesized.pattern),
            _ => {
                let condition = self.pattern(subject, r#type, pattern)?;

                Some(Test { conditions: vec![condition], ..Test::default() })
            }
        }
    }

    /// A type pattern holds for a value of the type it names, a case's for a value of that case, whose fields match the
    /// patterns the case's pattern gives them, and an implementer's of a sealed interface for the interface's value its
    /// constructor holds, or a case of an enum that implements it. Lean knows nothing else about a value's type while
    /// the code runs.
    fn type_test(&mut self, subject: &str, r#type: &TUnion, pattern: &TypePattern) -> Option<Test> {
        let atomics: Vec<&TAtomic> = r#type.types.iter().filter(|atomic| !atomic.is_null()).collect();
        let [atomic] = atomics.as_slice() else {
            self.unmodeled(pattern.span(), "a type test".to_owned(), issues::TYPE_TEST);
            return None;
        };

        let name = pattern.variable.map(|variable| names::identifier(variable.value));
        let mut parts = Test::default();
        let constructor = if pattern.properties.is_some() {
            return self.untested(pattern, atomic);
        } else if pattern.fields.is_none() && self.names_type(&pattern.hint, atomic) {
            None
        } else if pattern.fields.is_none()
            && let Some(implementer) = self.implementer(&pattern.hint, atomic)
        {
            Some(named(name.as_deref(), format!("({implementer} _)")))
        } else if let Some((held, case)) = self.case(&pattern.hint, atomic) {
            let arguments = self.fields(pattern, &mut parts)?;
            let case = format!("({case} {arguments})");
            Some(named(
                name.as_deref(),
                match held {
                    Some(constructor) => format!("({constructor} {case})"),
                    None => case,
                },
            ))
        } else {
            return self.untested(pattern, atomic);
        };

        let mut test = Test::default();
        match (constructor, name) {
            (whole, name) if r#type.is_nullable() => {
                let whole = whole.or(name).unwrap_or_else(|| "_".to_owned());
                test.matches.push(Destructure { term: subject.to_owned(), pattern: format!("some {whole}") });
            }
            (Some(constructor), _) => test.matches.push(Destructure { term: subject.to_owned(), pattern: constructor }),
            (None, Some(name)) => test.lets.push((name, subject.to_owned())),
            (None, None) => {}
        }
        test.matches.extend(parts.matches);
        test.lets.extend(parts.lets);
        test.conditions.extend(parts.conditions);

        Some(test)
    }

    /// The arguments of the Lean pattern of the case `pattern` names: `..` when it tests no field, and else each field's
    /// pattern as [`Self::bound`] gives it, whose test `parts` takes.
    fn fields(&mut self, pattern: &TypePattern, parts: &mut Test) -> Option<String> {
        let (Some(fields), Hint::Identifier(identifier)) = (pattern.fields, &pattern.hint) else {
            return Some("..".to_owned());
        };
        let field_types = self.checked.types().constructor_parameters(word(self.checked.names().get(identifier)));
        let mut arguments = Vec::new();
        for (field, field_type) in fields.fields.iter().zip(field_types) {
            let Some(field_type) = field_type.get_type_metadata().map(|metadata| &metadata.type_union) else {
                unreachable!("a field of a case has a type, at {:?}", field.span());
            };
            arguments.push(self.bound(field_type, field, "_".to_owned(), parts)?);
        }

        Some(arguments.join(" "))
    }

    /// Records why Lean cannot test `pattern` on a value of `atomic`: an interface that is not sealed has implementers
    /// a law cannot list, and any other test reads types while the code runs.
    fn untested(&mut self, pattern: &TypePattern, atomic: &TAtomic) -> Option<Test> {
        if let Some(interface) = self.open_interface(atomic) {
            let reason = issues::open_interface(&interface);
            self.unmodeled(pattern.span(), format!("a type test of the interface {interface}"), reason);
        } else {
            self.unmodeled(pattern.span(), "a type test".to_owned(), issues::TYPE_TEST);
        }

        None
    }

    /// A list pattern is Lean's `::` pattern of the elements before its rest. The elements after the rest are the last
    /// ones of the list those leave, once it holds them all, and the rest is what comes before them.
    fn list_test(&mut self, subject: &str, written: Option<&str>, r#type: &TUnion, list: &ListPattern) -> Option<Test> {
        let atomics: Vec<&TAtomic> = r#type.types.iter().filter(|atomic| !atomic.is_null()).collect();
        let [TAtomic::Array(TArray::List(list_type))] = atomics.as_slice() else {
            self.unmodeled(list.span(), "a list pattern on a value that is no List".to_owned(), issues::TYPE_TEST);
            return None;
        };
        let element = &list_type.element_type;
        let rest = list.elements.iter().position(|element| matches!(element, ListPatternElement::Rest(_)));
        let (before, after) = match rest {
            Some(rest) => (&list.elements.as_slice()[..rest], &list.elements.as_slice()[rest + 1..]),
            None => (list.elements.as_slice(), &[][..]),
        };
        let rest = rest.and_then(|rest| match &list.elements.as_slice()[rest] {
            ListPatternElement::Rest(rest) => Some(rest),
            ListPatternElement::Pattern(_) => None,
        });

        let mut parts = Test::default();
        let mut leading = Vec::new();
        for (place, pattern) in before.iter().enumerate() {
            let unnamed = written.map_or_else(|| "_".to_owned(), |written| format!("«{written}[{place}]»"));
            leading.push(self.element(element, pattern, unnamed, &mut parts)?);
        }
        let name = rest.and_then(|rest| rest.variable.map(|variable| names::identifier(variable.value)));

        let some = |pattern: String| if r#type.is_nullable() { format!("some ({pattern})") } else { pattern };
        let mut test = Test::default();
        if after.is_empty() {
            let pattern = match rest {
                None => format!("[{}]", leading.join(", ")),
                Some(_) => {
                    leading.push(name.unwrap_or_else(|| "_".to_owned()));
                    leading.join(" :: ")
                }
            };
            test.matches.push(Destructure { term: subject.to_owned(), pattern: some(pattern) });
        } else {
            // The elements after the rest are the last ones of the list after the leading ones, read by their place
            // once the list holds them all.
            let mut tail = subject.to_owned();
            if !leading.is_empty() || r#type.is_nullable() {
                tail = self.fresh();
                leading.push(tail.clone());
                test.matches.push(Destructure { term: subject.to_owned(), pattern: some(leading.join(" :: ")) });
            }
            let count = after.len();
            for (place, pattern) in after.iter().enumerate() {
                let ListPatternElement::Pattern(pattern) = pattern else {
                    unreachable!("a list pattern has at most one rest");
                };
                let term = format!("({tail}[List.length {tail} - {}]!)", count - place);
                let tested = self.test(&term, None, element, pattern)?;
                parts.matches.extend(tested.matches);
                parts.lets.extend(tested.lets);
                parts.conditions.extend(tested.conditions);
            }
            test.conditions.push(format!("(decide ({count} ≤ List.length {tail}))"));
            if let Some(name) = name {
                test.lets.push((name, format!("(List.take (List.length {tail} - {count}) {tail})")));
            }
        }
        test.matches.extend(parts.matches);
        test.lets.extend(parts.lets);
        test.conditions.extend(parts.conditions);

        Some(test)
    }

    /// The Lean pattern of one element of a list pattern, whose elements are of `r#type`, as [`Self::bound`] gives it.
    fn element(
        &mut self,
        r#type: &TUnion,
        pattern: &ListPatternElement,
        unnamed: String,
        parts: &mut Test,
    ) -> Option<String> {
        let ListPatternElement::Pattern(pattern) = pattern else {
            unreachable!("a list pattern has at most one rest");
        };

        self.bound(r#type, pattern, unnamed, parts)
    }

    /// The Lean pattern that binds a part of a value, of `r#type`, which `pattern` tests: the part's name, or
    /// `unnamed`, when the pattern holds for every value of the type, else a fresh name whose test `parts` takes.
    fn bound(&mut self, r#type: &TUnion, pattern: &Pattern, unnamed: String, parts: &mut Test) -> Option<String> {
        if let Pattern::Type(TypePattern { hint, fields: None, properties: None, variable }) = pattern
            && !r#type.is_nullable()
            && let [atomic] = r#type.types.as_ref()
            && self.names_type(hint, atomic)
        {
            return Some(variable.map_or(unnamed, |variable| names::identifier(variable.value)));
        }

        let name = self.fresh();
        let test = self.test(&name, None, r#type, pattern)?;
        parts.matches.extend(test.matches);
        parts.lets.extend(test.lets);
        parts.conditions.extend(test.conditions);

        Some(name)
    }
}

/// The Lean pattern `pattern`, which also names the whole value `name` when a name is given.
fn named(name: Option<&str>, pattern: String) -> String {
    match name {
        Some(name) => format!("{name}@{pattern}"),
        None => pattern,
    }
}

/// Which branch runs: the first arm whose test holds, else `otherwise`. An arm whose test is one Lean pattern of the
/// same term as the arm after it joins that arm's `match`. An arm whose test fails after a pattern of it matched runs
/// the arms after it there too.
pub(super) fn choice(arms: Vec<(Test, Branch)>, otherwise: Branch) -> Choice {
    let mut next = Choice::Branch(otherwise);
    for (test, branch) in arms.into_iter().rev() {
        let simple = test.matches.len() == 1 && test.lets.is_empty() && test.conditions.is_empty();
        let mut inner = Choice::Branch(branch);
        if !test.conditions.is_empty() {
            inner = Choice::If(test.conditions.join(" && "), Box::new(inner), Box::new(next.clone()));
        }
        for (name, value) in test.lets.into_iter().rev() {
            inner = Choice::Let(name, value, Box::new(inner));
        }
        for destructure in test.matches.into_iter().rev() {
            if simple
                && let Choice::Match(term, alternatives) = &mut next
                && *term == destructure.term
            {
                alternatives.insert(0, (destructure.pattern, inner));
                inner = next.clone();
                continue;
            }
            inner = Choice::Match(destructure.term, vec![(destructure.pattern, inner), ("_".to_owned(), next.clone())]);
        }
        next = inner;
    }

    next
}
