//! The effects of PHP# code, spec section 29.
//!
//! Mago analyzes files in parallel against signatures only, so no body knows its callees' effects while it is
//! analyzed. Each body records what it does by itself in an [`EffectSummary`] during its file's analysis, and
//! [`Effects::solve`] combines every summary once all files are analyzed.

use std::cell::OnceCell;
use std::collections::BTreeSet;
use std::sync::Arc;

use foldhash::HashMap;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_names::display_sharp_member;
use mago_names::scope::NamespaceScope;
use mago_reporting::IssueCollection;
use mago_span::Span;
use mago_word::Word;
use mago_word::WordMap;
use mago_word::ascii_lowercase_word;
use mago_word::empty_word;
use mago_word::word;

use crate::code::IssueCode;
use crate::graph::strongly_connected_parts;
use crate::utils::names::sharp_class_like_name;
use crate::utils::names::sharp_effect_name;

pub(crate) mod check;
pub(crate) mod summary;

/// A reason a body is impure other than a change of state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Effect {
    /// An effect an `extern` declaration lists for the plain PHP called, named by its class, such as `Sharp\Http`.
    Foreign(Word),
    /// A call into plain PHP that no `extern` declaration covers, with the callee an `extern` would declare as its full
    /// class name and its member: `Lib\Bag` and `__call`, an empty class and `trim` for a function, and the class and
    /// an empty member for a constructor. A call of a plain PHP property holding a function has none, because an
    /// `extern` declares a class's members, not the code a property holds.
    Unknown(Option<(Word, Word)>),
    /// An `emit`, spec section 15, which has the effect `Sharp\Events` whatever its listeners do. Its cause is the
    /// event's class and an empty member.
    Emit,
}

/// The state a body changes, named by the root of the place it writes. The same roots name the values a call passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Changed {
    /// The object the body runs on.
    This,
    /// What the caller passed as the parameter at this index.
    Parameter(u32),
    /// State every body reaches, such as a static property.
    Shared,
}

/// The roots of a value. A value with no root is fresh: the body made it, so changing it changes nothing given.
pub(crate) type Roots = BTreeSet<Changed>;

/// A PHP# body: a method by its lowercase class and method names, an accessor by its lowercase class, its
/// property's PHP name (`$total`) and `get` or `set`, a lambda by the name its function value's `source` gives it, or
/// an initial value that is not constant, which runs in the constructor, by its lowercase class and its property's PHP
/// name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Body {
    Method(Word, Word),
    Accessor(Word, Word, Word),
    Lambda(Word),
    InitialValue(Word, Word),
}

/// What a function value runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Runs {
    /// A PHP# lambda or method, whose effects only the solve knows.
    Body(Body),
    /// A plain PHP method, as its full class name and its member, with the effects its `extern` declares, or `Unknown`
    /// without one, as a call of it has them.
    Foreign { callee: (Word, Word), effects: Vec<Effect> },
}

/// A lambda or a method value a body gives a place whose PHP# function type lists its effects. Spec section 29 holds
/// it to that list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FunctionValue {
    pub(crate) runs: Runs,
    /// The effects the place's function type lists, by full class name.
    pub(crate) allowed: Vec<Word>,
    pub(crate) span: Span,
    /// The place's type as the value's file writes it, such as `Function<int(int)>`.
    pub(crate) declared: String,
    /// The code a value of the wrong type gets in this place, such as `invalid-argument`.
    pub(crate) code: IssueCode,
}

/// A call from one PHP# body to another, with the roots of what it passes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Call {
    pub(crate) callee: Body,
    pub(crate) span: Span,
    pub(crate) receiver: Roots,
    /// The roots of each argument, by the callee's parameter index.
    pub(crate) arguments: Vec<Roots>,
}

/// What one PHP# body does by itself: the effects of the plain PHP it calls, the PHP# bodies it calls, and the state
/// it changes. A lambda is part of the body that calls it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectSummary {
    pub(crate) body: Body,
    /// The body's member as messages name it: `price`, `total` for an accessor of `total`, or `operator +`.
    pub(crate) member: Word,
    /// Each effect, with the call that has it and that call's callee as its full class name and its member, with an
    /// empty class for a function and an empty member for a constructor.
    pub(crate) effects: Vec<(Effect, Span, (Word, Word))>,
    pub(crate) calls: Vec<Call>,
    /// Each change, with the write and the place written as the source writes it.
    pub(crate) changes: Vec<(Changed, Span, Word)>,
    pub(crate) function_values: Vec<FunctionValue>,
    /// The name each import of the body's file gives its class, and the file's namespace and imports, as
    /// `Context::imported_names` and `Context::scope` hold them, shared by the file's bodies. A message about the body
    /// names classes, and the imports its code needs, as its file does.
    pub(crate) imports: Arc<WordMap<Word>>,
    pub(crate) scope: Arc<NamespaceScope>,
}

/// Why a body is impure, as the first call or write in it that leads there. Each name is a full class name and a
/// member, with an empty class for a function or a place written, and an empty member for a constructor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Impurity {
    /// The call or write in the checked body.
    pub(crate) span: Span,
    /// The PHP# bodies the call reaches, from the one the checked body calls down to the one that is impure by
    /// itself. Empty when the checked body is impure by itself.
    pub(crate) path: Vec<(Word, Word)>,
    /// The plain PHP callee, such as `Lib\Clock` and `now`, or the place written, such as `this.count`.
    pub(crate) cause: (Word, Word),
    /// The effect of the call, or `None` when the cause is a place written.
    pub(crate) effect: Option<Effect>,
}

impl Impurity {
    /// The reason as a sentence's predicate, such as "calls `getenv`, which has no `extern` declaration", with each
    /// class named by `class_name` and each effect by `effect_name`.
    pub(crate) fn describe(&self, class_name: &dyn Fn(Word) -> String, effect_name: &dyn Fn(Word) -> String) -> String {
        let cause = written(self.cause, class_name);
        match (self.path.last().map(|body| written(*body, class_name)), self.effect) {
            (None, Some(Effect::Foreign(effect))) => {
                format!("calls `{cause}`, which has the effect `{}`", effect_name(effect))
            }
            (None, Some(Effect::Unknown(_))) => format!("calls `{cause}`, which has no `extern` declaration"),
            (None, Some(Effect::Emit)) => {
                format!("emits `{cause}`, which has the effect `{}`", effect_name(events()))
            }
            (None, None) => format!("changes `{cause}`"),
            (Some(body), Some(Effect::Foreign(effect))) => {
                format!("reaches `{body}`, which calls `{cause}` with the effect `{}`", effect_name(effect))
            }
            (Some(body), Some(Effect::Unknown(_))) => {
                format!("reaches `{body}`, which calls `{cause}` with no `extern` declaration")
            }
            (Some(body), Some(Effect::Emit)) => {
                format!("reaches `{body}`, which emits `{cause}` with the effect `{}`", effect_name(events()))
            }
            (Some(body), None) => format!("reaches `{body}`, which changes `{cause}`"),
        }
    }
}

/// A class name and a member as a message writes them, the class named by `class_name`: `Clock.now`, `trim` with no
/// class, `Pdf.render` for the function `Vendor\Pdf\render`, named through its namespace as spec section 23 calls it,
/// and `Clock` with no member.
fn written((class, member): (Word, Word), class_name: &dyn Fn(Word) -> String) -> String {
    if class.is_empty() {
        let name = member.as_bytes();
        match name.iter().rposition(|byte| *byte == b'\\') {
            Some(separator) => display_sharp_member(
                class_name(word(&name[..separator])),
                String::from_utf8_lossy(&name[separator + 1..]),
            ),
            None => member.to_string(),
        }
    } else if member.is_empty() {
        class_name(class)
    } else {
        display_sharp_member(class_name(class), member)
    }
}

/// What a body does: an effect, or a change of a root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Item {
    Effect(Effect),
    Changed(Changed),
}

/// Where an item of a body comes from: its own call or write, or a call whose callee has `via`'s item.
#[derive(Clone, Copy, Debug)]
struct Origin {
    span: Span,
    cause: (Word, Word),
    via: Option<(usize, Item)>,
}

/// The solved effects and changes of every PHP# body.
#[derive(Debug)]
pub struct Effects {
    bodies: Vec<Body>,
    /// Each body's lowercase class name and its member as messages name it.
    names: Vec<(Word, Word)>,
    /// The imports and the scope of each body's file, which name the classes of a message about the body.
    imports: Vec<Arc<WordMap<Word>>>,
    scopes: Vec<Arc<NamespaceScope>>,
    index: HashMap<Body, usize>,
    /// Each body's items, its own in source order first, and each once.
    solved: Vec<Vec<(Item, Origin)>>,
    /// Each function value, with the position of the body that gives it, whose file names the classes of a message.
    function_values: Vec<(FunctionValue, usize)>,
}

impl Effects {
    /// Combines `summaries` into what each body does through every call: one solve over the strongly connected
    /// parts of the call graph, callees first, each part iterated until nothing grows.
    #[must_use]
    pub fn solve(codebase: &CodebaseMetadata, summaries: &[EffectSummary]) -> Effects {
        let mut index = HashMap::default();
        for (position, summary) in summaries.iter().enumerate() {
            index.entry(summary.body).or_insert(position);
        }

        // A call to a method that can be overridden runs any override, so it reaches every override's body.
        let mut overrides: HashMap<(Word, Word), Vec<usize>> = HashMap::default();
        for (position, summary) in summaries.iter().enumerate() {
            if let Body::Method(class, method) = summary.body
                && index.get(&summary.body) == Some(&position)
                && let Some(metadata) = codebase.get_class_like(class.as_bytes())
            {
                for parent in &metadata.all_parent_classes {
                    overrides.entry((ascii_lowercase_word(parent.as_bytes()), method)).or_default().push(position);
                }
            }
        }

        let targets: Vec<Vec<Vec<usize>>> = summaries
            .iter()
            .map(|summary| {
                summary
                    .calls
                    .iter()
                    .map(|call| {
                        // A bodiless callee contributes nothing until PR 2 declares what it uses.
                        let Some(&callee) = index.get(&call.callee) else {
                            return Vec::new();
                        };
                        let mut targets = vec![callee];
                        if let Body::Method(class, method) = call.callee {
                            targets.extend(overrides.get(&(class, method)).into_iter().flatten());
                        }

                        targets
                    })
                    .collect()
            })
            .collect();

        let successors: Vec<Vec<usize>> =
            targets.iter().map(|calls| calls.iter().flatten().copied().collect()).collect();

        let mut solved: Vec<Vec<(Item, Origin)>> = summaries.iter().map(own_items).collect();
        for part in strongly_connected_parts(&successors) {
            loop {
                let mut grew = false;
                for &body in &part {
                    for (call, call_targets) in summaries[body].calls.iter().zip(&targets[body]) {
                        for &target in call_targets {
                            let reached: Vec<(Item, Item)> = solved[target]
                                .iter()
                                .flat_map(|(item, _)| through(*item, call).map(move |mapped| (*item, mapped)))
                                .collect();

                            for (callee_item, item) in reached {
                                if !solved[body].iter().any(|(known, _)| *known == item) {
                                    let origin = Origin {
                                        span: call.span,
                                        cause: (empty_word(), empty_word()),
                                        via: Some((target, callee_item)),
                                    };
                                    solved[body].push((item, origin));
                                    grew = true;
                                }
                            }
                        }
                    }
                }

                if !grew {
                    break;
                }
            }
        }

        Effects {
            bodies: summaries.iter().map(|summary| summary.body).collect(),
            names: summaries
                .iter()
                .map(|summary| match summary.body {
                    Body::Method(class, _) | Body::Accessor(class, _, _) | Body::InitialValue(class, _) => {
                        (class, summary.member)
                    }
                    Body::Lambda(_) => (empty_word(), summary.member),
                })
                .collect(),
            imports: summaries.iter().map(|summary| Arc::clone(&summary.imports)).collect(),
            scopes: summaries.iter().map(|summary| Arc::clone(&summary.scope)).collect(),
            index,
            solved,
            function_values: summaries
                .iter()
                .enumerate()
                .flat_map(|(position, summary)| {
                    summary.function_values.iter().map(move |value| (value.clone(), position))
                })
                .collect(),
        }
    }

    /// Why `method` is impure, as a sentence's predicate that names classes as the method's file does, or `None` when
    /// it has no effect and changes neither `this`, a parameter nor shared state. A constructor's changes to `this` do
    /// not count, because its object is new.
    ///
    /// It answers only for a PHP# method with a body. It returns `None` for a function, a closure, a plain PHP method
    /// and a native `extern` method, whose effects no summary records.
    #[must_use]
    pub fn impurity(&self, codebase: &CodebaseMetadata, method: &FunctionLikeIdentifier) -> Option<String> {
        let FunctionLikeIdentifier::Method(class, method) = method else {
            return None;
        };

        let body = Body::Method(ascii_lowercase_word(class.as_bytes()), ascii_lowercase_word(method.as_bytes()));
        let imports = &self.imports[*self.index.get(&body)?];
        let short_name_counts = OnceCell::new();

        self.body_impurity(body).map(|impurity| {
            impurity.describe(&|class| sharp_class_like_name(codebase, imports, &short_name_counts, class), &|effect| {
                sharp_effect_name(codebase, imports, &short_name_counts, effect)
            })
        })
    }

    /// The issues of every effect rule. The getter rule reads only the solve, and the law rule reads which bodies are
    /// laws from `codebase`. Each message names classes as the file of the getter or law it refuses does.
    #[must_use]
    pub fn issues(&self, codebase: &CodebaseMetadata) -> IssueCollection {
        let short_name_counts = OnceCell::new();
        let mut issues = check::getters_must_be_pure(self, codebase, &short_name_counts);
        issues.extend(check::laws_must_be_pure(self, codebase, &short_name_counts));
        issues.extend(check::function_values_must_fit_their_types(self, codebase, &short_name_counts));

        issues
    }

    fn body_impurity(&self, body: Body) -> Option<Impurity> {
        let constructor = matches!(body, Body::Method(_, method) if method.as_bytes() == b"__construct");

        self.first_item(body, |item| !(constructor && item == Item::Changed(Changed::This)))
    }

    /// Why `body` has an effect that `allowed` does not list.
    fn effect_outside(&self, body: Body, allowed: &[Word]) -> Option<Impurity> {
        self.first_item(body, |item| match item {
            Item::Effect(effect) => is_outside(effect, allowed),
            Item::Changed(_) => false,
        })
    }

    /// The first item of `body` that `keep` keeps, as the call or write in `body` that leads to it.
    fn first_item(&self, body: Body, keep: impl Fn(Item) -> bool) -> Option<Impurity> {
        let position = *self.index.get(&body)?;
        let &(mut item, mut origin) = self.solved[position]
            .iter()
            .filter(|(item, _)| keep(*item))
            .min_by_key(|(_, origin)| origin.span.start.offset)?;

        let span = origin.span;
        let mut path = Vec::new();
        // Each item a call brings in was already its callee's, so the chain ends at a body's own call or write.
        while let Some((callee, callee_item)) = origin.via
            && let Some((_, callee_origin)) = self.solved[callee].iter().find(|(known, _)| *known == callee_item)
        {
            path.push(self.names[callee]);
            item = callee_item;
            origin = *callee_origin;
        }

        let effect = match item {
            Item::Effect(effect) => Some(effect),
            Item::Changed(_) => None,
        };

        Some(Impurity { span, path, cause: origin.cause, effect })
    }

    /// The first function value given each place that has an effect its place's function type does not list, with
    /// what it runs as messages name it, a class and a member, the imports and the scope of the file that gives the
    /// value, and the call that has the effect: a call in the body it runs, or the value itself for a plain PHP method.
    fn unfit_function_values(
        &self,
    ) -> impl Iterator<Item = (&FunctionValue, (Word, Word), &WordMap<Word>, &NamespaceScope, Impurity)> + '_ {
        let mut reported: Vec<Span> = Vec::new();
        self.function_values.iter().filter_map(move |(value, giver)| {
            if reported.contains(&value.span) {
                return None;
            }

            let (name, impurity) = match &value.runs {
                Runs::Body(body) => (self.names[*self.index.get(body)?], self.effect_outside(*body, &value.allowed)?),
                Runs::Foreign { callee, effects } => {
                    let effect = effects.iter().find(|effect| is_outside(**effect, &value.allowed))?;

                    (*callee, Impurity { span: value.span, path: Vec::new(), cause: *callee, effect: Some(*effect) })
                }
            };
            reported.push(value.span);

            Some((value, name, &*self.imports[*giver], &*self.scopes[*giver], impurity))
        })
    }

    /// Each body with its member as messages name it, the imports and the scope of its file, and why it is impure.
    fn impure_bodies(&self) -> impl Iterator<Item = (Body, Word, &WordMap<Word>, &NamespaceScope, Impurity)> + '_ {
        self.bodies.iter().enumerate().filter(|(position, body)| self.index.get(body) == Some(position)).filter_map(
            |(position, body)| {
                self.body_impurity(*body).map(|impurity| {
                    (*body, self.names[position].1, &*self.imports[position], &*self.scopes[position], impurity)
                })
            },
        )
    }
}

/// Whether `allowed` does not list `effect`: a foreign effect it does not name, an unknown one, or an `emit` when it
/// does not name `Sharp\Events`.
fn is_outside(effect: Effect, allowed: &[Word]) -> bool {
    let named = match effect {
        Effect::Foreign(effect) => effect,
        Effect::Emit => events(),
        Effect::Unknown(_) => return true,
    };

    !allowed.iter().any(|known| known.as_bytes().eq_ignore_ascii_case(named.as_bytes()))
}

/// The effect an `emit` has, `Sharp\Events`.
fn events() -> Word {
    word(b"Sharp\\Events")
}

/// A body's own effects and changes, in source order, an effect before a change at the same place.
fn own_items(summary: &EffectSummary) -> Vec<(Item, Origin)> {
    let mut items: Vec<(Item, Origin)> = Vec::new();
    let effects = summary.effects.iter().map(|(effect, span, cause)| (Item::Effect(*effect), *span, *cause));
    let changes =
        summary.changes.iter().map(|(changed, span, place)| (Item::Changed(*changed), *span, (empty_word(), *place)));
    for (item, span, cause) in effects.chain(changes) {
        if !items.iter().any(|(known, _)| *known == item) {
            items.push((item, Origin { span, cause, via: None }));
        }
    }

    items.sort_by_key(|(_, origin)| origin.span.start.offset);
    items
}

/// What a callee's item is in the caller: an effect stays, and a change maps through the roots the call passes.
fn through(item: Item, call: &Call) -> impl Iterator<Item = Item> + '_ {
    let roots: Vec<Changed> = match item {
        Item::Effect(_) => Vec::new(),
        Item::Changed(Changed::This) => call.receiver.iter().copied().collect(),
        Item::Changed(Changed::Parameter(index)) => {
            call.arguments.get(index as usize).map(|roots| roots.iter().copied().collect()).unwrap_or_default()
        }
        Item::Changed(Changed::Shared) => vec![Changed::Shared],
    };

    let effect = match item {
        Item::Effect(_) => Some(item),
        Item::Changed(_) => None,
    };

    effect.into_iter().chain(roots.into_iter().map(Item::Changed))
}
