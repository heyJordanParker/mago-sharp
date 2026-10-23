//! What a `match` arm or an `is` tests, as the Lean `match`es, `let`s and conditions that test it, and the choice of
//! branch those tests make.

use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::ListPattern;
use mago_syntax::cst::ListPatternElement;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::TypePattern;

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
                let condition = self.pattern(subject, self.kind(r#type), pattern)?;

                Some(Test { conditions: vec![condition], ..Test::default() })
            }
        }
    }

    /// A type pattern holds for a value of the type it names, and a case's for a value of that case. Lean knows nothing
    /// else about a value's type while the code runs.
    fn type_test(&mut self, subject: &str, r#type: &TUnion, pattern: &TypePattern) -> Option<Test> {
        let atomics: Vec<&TAtomic> = r#type.types.iter().filter(|atomic| !atomic.is_null()).collect();
        let [atomic] = atomics.as_slice() else {
            self.unmodeled(pattern.span(), "a type test".to_owned(), issues::TYPE_TEST);
            return None;
        };
        let case = if pattern.fields.is_some() || pattern.properties.is_some() {
            None
        } else if self.names_type(&pattern.hint, atomic) {
            Some(None)
        } else {
            self.case(&pattern.hint, atomic).map(Some)
        };
        let Some(case) = case else {
            self.unmodeled(pattern.span(), "a type test".to_owned(), issues::TYPE_TEST);
            return None;
        };

        let name = pattern.variable.map(|variable| names::identifier(variable.value));
        let whole = match (&case, &name) {
            (None, Some(name)) => name.clone(),
            (None, None) => "_".to_owned(),
            (Some(case), Some(name)) => format!("{name}@({case} ..)"),
            (Some(case), None) => format!("({case} ..)"),
        };
        let mut test = Test::default();
        if r#type.is_nullable() {
            test.matches.push(Destructure { term: subject.to_owned(), pattern: format!("some {whole}") });
        } else if case.is_some() {
            test.matches.push(Destructure { term: subject.to_owned(), pattern: whole });
        } else if let Some(name) = name {
            test.lets.push((name, subject.to_owned()));
        }

        Some(test)
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

    /// The Lean pattern of one element of a list pattern, whose elements are of `r#type`: the element's name, or
    /// `unnamed`, when the pattern holds for every element, else a fresh name whose test `parts` takes.
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
