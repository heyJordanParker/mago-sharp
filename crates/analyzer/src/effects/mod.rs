//! The effects of PHP# code, spec section 29.
//!
//! Mago analyzes files in parallel against signatures only, so no body knows its callees' effects while it is
//! analyzed. Each body records what it does by itself in an [`EffectSummary`] during its file's analysis, and
//! [`Effects::solve`] combines every summary once all files are analyzed.

use std::cell::OnceCell;
use std::collections::BTreeSet;
use std::sync::Arc;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_database::file::FileId;
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
    /// The effect `effect` of a call into a plain PHP class without an `extern`, which the class carries because `by`,
    /// a call an `extern` declares, returns it: a full class name and a member, or an empty class and a function.
    Carried { effect: Word, by: (Word, Word) },
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
/// an initial value that is not constant, which runs in the constructor, or a static's on the first use of its class's
/// statics, by its lowercase class and its property's PHP name.
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
    /// A method whose effects need no solve, as its full class name and its member, with the effects a call of it has:
    /// a plain PHP method's, which its `extern` declares or `Unknown` without one, a `foreign` class's method's, and a
    /// PHP# method's without a body, which its `uses` lists.
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

/// A call of a PHP# method without `readonly` through a readonly value, spec section 13. Only the solve knows whether
/// the method changes `this`, which its message says. Each text is as the calling file writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReadonlyCall {
    pub(crate) callee: Body,
    pub(crate) span: Span,
    /// The code that calls: `render`, or `This lambda`.
    pub(crate) caller: String,
    /// The call, `order.markPaid`, and its readonly value, `order`.
    pub(crate) call: String,
    pub(crate) receiver: String,
    /// The method's name, its class and its declaration up to its parameters: `totalIn`, `Order` and
    /// `totalIn(string currency)`.
    pub(crate) method: String,
    pub(crate) class: String,
    pub(crate) declaration: String,
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
    pub(crate) readonly_calls: Vec<ReadonlyCall>,
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

/// The cause of the effect a member of a `foreign` class has by itself, spec section 29, which no call or write gives:
/// an empty class and an empty member.
pub(crate) fn member_of_foreign_class() -> (Word, Word) {
    (empty_word(), empty_word())
}

impl Impurity {
    /// The reason as a sentence's predicate, such as "calls `global.getenv`, which has no `extern` declaration", with each
    /// class named by `class_name` and each effect by `effect_name`.
    pub(crate) fn describe(&self, class_name: &dyn Fn(Word) -> String, effect_name: &dyn Fn(Word) -> String) -> String {
        if self.cause == member_of_foreign_class()
            && let Some(Effect::Foreign(effect)) = self.effect
        {
            let class = format!("a member of the `foreign` class `{}`", effect_name(effect));

            return match self.path.last() {
                Some(body) => format!("reaches `{}`, {class}", written(*body, class_name)),
                None => format!("is {class}"),
            };
        }

        let cause = written(self.cause, class_name);
        match (self.path.last().map(|body| written(*body, class_name)), self.effect) {
            (None, Some(Effect::Foreign(effect))) => {
                format!("calls `{cause}`, which has the effect `{}`", effect_name(effect))
            }
            (None, Some(Effect::Carried { effect, by })) => {
                format!(
                    "calls `{cause}`, which has the effect `{}` {}",
                    effect_name(effect),
                    self.carried_by(by, class_name)
                )
            }
            (Some(body), Some(Effect::Carried { effect, by })) => format!(
                "reaches `{body}`, which calls `{cause}` with the effect `{}` {}",
                effect_name(effect),
                self.carried_by(by, class_name)
            ),
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

    /// Where a carried effect comes from, as "that `Desk.open` declares for the `Session` it returns", for the class of
    /// the cause, which the declared call `by` returns.
    pub(crate) fn carried_by(&self, by: (Word, Word), class_name: &dyn Fn(Word) -> String) -> String {
        format!("that `{}` declares for the `{}` it returns", written(by, class_name), class_name(self.cause.0))
    }
}

/// A class name and a member as a message writes them, the class named by `class_name`: `Clock.now`, `global.trim` or
/// a function value's `step` as written with no class, `Pdf.render` for the function `Vendor\Pdf\render`, named
/// through its namespace as spec section 23 calls it, and `Clock` with no member.
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
    /// Each call of a method without `readonly` through a readonly value, with the position of the body that makes it.
    readonly_calls: Vec<(ReadonlyCall, usize)>,
    /// The file of each call, with the position of each body it reaches.
    calls: Vec<(FileId, Vec<usize>)>,
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
                        // A callee with no summary contributes nothing. A call of a method without a body has the
                        // effects its `uses` lists instead of a callee.
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

        let carried = carried_effects(codebase);
        let mut solved: Vec<Vec<(Item, Origin)>> =
            summaries.iter().map(|summary| own_items(summary, &carried)).collect();
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
                .flat_map(|(position, summary)| summary.function_values.iter().map(move |value| (value, position)))
                .map(|(value, position)| {
                    let runs = match &value.runs {
                        Runs::Foreign { callee, effects } => Runs::Foreign {
                            callee: *callee,
                            effects: effects.iter().flat_map(|effect| carried_through(*effect, &carried)).collect(),
                        },
                        runs => runs.clone(),
                    };

                    (FunctionValue { runs, ..value.clone() }, position)
                })
                .collect(),
            readonly_calls: summaries
                .iter()
                .enumerate()
                .flat_map(|(position, summary)| summary.readonly_calls.iter().map(move |call| (call.clone(), position)))
                .collect(),
            calls: summaries
                .iter()
                .zip(targets)
                .flat_map(|(summary, targets)| {
                    summary.calls.iter().zip(targets).map(|(call, targets)| (call.span.file_id, targets))
                })
                .collect(),
        }
    }

    /// The files whose PHP# bodies a call in each file reaches, an override a call can run included, so an edit of
    /// one can change the effects of the calling file's functions. A method is in its file, and an accessor and a
    /// static's initial value in its class's.
    #[must_use]
    pub fn callee_files(&self, codebase: &CodebaseMetadata) -> HashMap<FileId, HashSet<FileId>> {
        let file_of = |body: Body| match body {
            Body::Method(class, method) => {
                codebase.get_method(class.as_bytes(), method.as_bytes()).map(|method| method.span.file_id)
            }
            Body::Accessor(class, _, _) | Body::InitialValue(class, _) => {
                codebase.get_class_like(class.as_bytes()).map(|class| class.span.file_id)
            }
            Body::Lambda(_) => unreachable!("a call reaches a method, an accessor or a static's initial value"),
        };

        let mut files: HashMap<FileId, HashSet<FileId>> = HashMap::default();
        for (caller, targets) in &self.calls {
            for &target in targets {
                if let Some(callee) = file_of(self.bodies[target])
                    && callee != *caller
                {
                    files.entry(*caller).or_default().insert(callee);
                }
            }
        }

        files
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

    /// The effects of the PHP# method or lambda `function`, by full class name, each once, as a function type's `uses`
    /// lists them: each effect of the plain PHP and the `foreign` classes it reaches, and `Sharp\Events` when it emits.
    /// A change of state is no effect. `None` when it calls plain PHP no `extern` declares, which no function type lists,
    /// and for a function no summary records, such as a plain PHP one or a method without a body.
    #[must_use]
    pub fn function_effects(&self, function: &FunctionLikeIdentifier) -> Option<Vec<Word>> {
        let body = match function {
            FunctionLikeIdentifier::Method(class, method) => {
                Body::Method(ascii_lowercase_word(class.as_bytes()), ascii_lowercase_word(method.as_bytes()))
            }
            FunctionLikeIdentifier::Closure(name) => Body::Lambda(*name),
            FunctionLikeIdentifier::Function(_) => unreachable!("PHP# declares no function outside a class"),
        };

        let mut effects = Vec::new();
        for (item, _) in &self.solved[*self.index.get(&body)?] {
            let effect = match item {
                Item::Effect(Effect::Foreign(effect) | Effect::Carried { effect, .. }) => *effect,
                Item::Effect(Effect::Emit) => events(),
                Item::Effect(Effect::Unknown(_)) => return None,
                Item::Changed(_) => continue,
            };
            if !effects.contains(&effect) {
                effects.push(effect);
            }
        }

        Some(effects)
    }

    /// The issues of every effect rule. The getter rule reads only the solve, and the law rule reads which bodies are
    /// laws from `codebase`. Each message names classes as the file of the getter or law it refuses does.
    #[must_use]
    pub fn issues(&self, codebase: &CodebaseMetadata) -> IssueCollection {
        let short_name_counts = OnceCell::new();
        let mut issues = check::getters_never_write_this(self, codebase, &short_name_counts);
        issues.extend(check::laws_must_be_pure(self, codebase, &short_name_counts));
        issues.extend(check::function_values_must_fit_their_types(self, codebase, &short_name_counts));
        issues.extend(check::implementations_must_fit_their_declarations(self, codebase, &short_name_counts));
        issues.extend(check::readonly_values_call_readonly_methods(self, codebase, &short_name_counts));

        issues
    }

    /// Each call of a method without `readonly` through a readonly value, with what the method changes in `this`, the
    /// first write that leads there, or `None` when it changes nothing in `this` and only lacks the marker. The
    /// imports of the calling body's file name the classes of the write.
    fn readonly_calls(&self) -> impl Iterator<Item = (&ReadonlyCall, &WordMap<Word>, Option<Impurity>)> + '_ {
        self.readonly_calls.iter().map(|(call, caller)| {
            let change = self.first_item(call.callee, |item| item == Item::Changed(Changed::This));

            (call, &*self.imports[*caller], change)
        })
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

    /// Each PHP# method with the first method without a body it implements whose `uses` does not list one of its
    /// effects: its class and member as messages name them, the declaration's class and metadata, the imports and the
    /// scope of its file, and the call that has the effect. An implementation of `Sharp\Dispatcher::dispatch` is held
    /// to nothing, spec section 15: it runs the listeners of every event, which may reach any effect.
    fn unfit_implementations<'effects>(
        &'effects self,
        codebase: &'effects CodebaseMetadata,
    ) -> impl Iterator<
        Item = (
            (Word, Word),
            Word,
            &'effects FunctionLikeMetadata,
            &'effects WordMap<Word>,
            &'effects NamespaceScope,
            Impurity,
        ),
    > + 'effects {
        self.bodies.iter().enumerate().filter(|(position, body)| self.index.get(body) == Some(position)).filter_map(
            move |(position, body)| {
                let Body::Method(class, method) = *body else {
                    return None;
                };
                let metadata = codebase.get_class_like(class.as_bytes())?;

                metadata.all_parent_classes.iter().chain(&metadata.all_parent_interfaces).find_map(|ancestor| {
                    let declaration = holding_declaration(codebase, *ancestor, method)?;
                    let impurity = self.effect_outside(*body, &declaration.uses)?;

                    Some((
                        self.names[position],
                        *ancestor,
                        declaration,
                        &*self.imports[position],
                        &*self.scopes[position],
                        impurity,
                    ))
                })
            },
        )
    }

    /// Each body that `find` gives a reason for, with its member as messages name it, the imports and the scope of its
    /// file, and that reason.
    fn bodies_with<'effects>(
        &'effects self,
        find: impl Fn(&Self, Body) -> Option<Impurity> + 'effects,
    ) -> impl Iterator<Item = (Body, Word, &'effects WordMap<Word>, &'effects NamespaceScope, Impurity)> + 'effects
    {
        self.bodies.iter().enumerate().filter(|(position, body)| self.index.get(body) == Some(position)).filter_map(
            move |(position, body)| {
                find(self, *body).map(|impurity| {
                    (*body, self.names[position].1, &*self.imports[position], &*self.scopes[position], impurity)
                })
            },
        )
    }

    /// Each body that is impure, and why, as [`Self::bodies_with`] gives them.
    fn impure_bodies(&self) -> impl Iterator<Item = (Body, Word, &WordMap<Word>, &NamespaceScope, Impurity)> + '_ {
        self.bodies_with(Self::body_impurity)
    }

    /// Each body that changes the object it runs on, and the write that leads to it, as [`Self::bodies_with`] gives
    /// them: a `get` body never writes `this`, spec section 6.1.
    fn bodies_changing_this(
        &self,
    ) -> impl Iterator<Item = (Body, Word, &WordMap<Word>, &NamespaceScope, Impurity)> + '_ {
        self.bodies_with(|effects, body| effects.first_item(body, |item| item == Item::Changed(Changed::This)))
    }
}

/// Whether `allowed` does not list `effect`: a foreign effect it does not name, an unknown one, or an `emit` when it
/// does not name `Sharp\Events`.
fn is_outside(effect: Effect, allowed: &[Word]) -> bool {
    let named = match effect {
        Effect::Foreign(effect) | Effect::Carried { effect, .. } => effect,
        Effect::Emit => events(),
        Effect::Unknown(_) => return true,
    };

    !allowed.iter().any(|known| known.as_bytes().eq_ignore_ascii_case(named.as_bytes()))
}

/// The effect an `emit` has, `Sharp\Events`.
fn events() -> Word {
    word(b"Sharp\\Events")
}

/// Whether the declaration of `method` in `class` lets an implementation have any effect, because the code that calls
/// it carries the implementation's effects: `Sharp\Dispatcher::dispatch`, which the engine calls with the listeners of
/// every event (spec section 15), and `Sharp\Behavior`'s `get` and `set`, which run as the accessors of a property
/// that names the behavior (spec section 6.4).
fn accepts_any_effect(class: Word, method: Word) -> bool {
    let (class, method) = (class.as_bytes(), method.as_bytes());

    (class.eq_ignore_ascii_case(b"Sharp\\Dispatcher") && method.eq_ignore_ascii_case(b"dispatch"))
        || (class.eq_ignore_ascii_case(b"Sharp\\Behavior")
            && (method.eq_ignore_ascii_case(b"get") || method.eq_ignore_ascii_case(b"set")))
}

/// The PHP# declaration without a body of `method` in `ancestor`, which holds each method that overrides it to its
/// `uses`, spec section 29, or `None` when there is none or it accepts any effect.
pub(crate) fn holding_declaration(
    codebase: &CodebaseMetadata,
    ancestor: Word,
    method: Word,
) -> Option<&FunctionLikeMetadata> {
    if accepts_any_effect(ancestor, method) {
        return None;
    }

    codebase.get_method(ancestor.as_bytes(), method.as_bytes()).filter(|declaration| {
        declaration.flags.is_sharp() && declaration.method_metadata.as_ref().is_some_and(|method| method.is_abstract)
    })
}

/// Whether `name`, a class name as the binder resolves it, is an effect, spec section 29: a `foreign` class, the
/// standard library's among them, or `Events`.
fn is_effect(codebase: &CodebaseMetadata, name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(events().as_bytes())
        || codebase.get_class_like(name).is_some_and(|class| class.flags.is_foreign())
}

/// An effect a plain PHP class carries, with the declared call that returns the class: a full class name and a member,
/// or an empty class and a function.
type CarriedEffect = (Word, (Word, Word));

/// The effects each plain PHP class carries because a call an `extern` declares returns it, spec section 29, by the
/// class's lowercase name: the effects of every such call, and through a chain, those of every class whose method
/// returns it, as `Builder.where(): static` keeps the builder's, each with the declared call that starts its chain. A
/// call into the class finds no `extern`, so it reads these in place of an unknown effect, and an `extern` on the class
/// wins because its calls are never unknown.
fn carried_effects(codebase: &CodebaseMetadata) -> HashMap<Word, Vec<CarriedEffect>> {
    let mut carried: HashMap<Word, Vec<CarriedEffect>> = HashMap::default();
    let mut pending: Vec<(&FunctionLikeMetadata, Vec<CarriedEffect>)> = Vec::new();
    for declaration in codebase.externs.values().filter_map(|declarations| declarations.first()) {
        let (class, member) = declaration.target;
        let returning: Vec<(Word, &FunctionLikeMetadata)> = if class.is_empty() {
            codebase.get_function(member.as_bytes()).map(|function| (empty_word(), function)).into_iter().collect()
        } else if member.is_empty() {
            methods_of(codebase, class)
        } else {
            let declaring = codebase.get_declaring_method_identifier(&MethodIdentifier::new(class, member));
            codebase
                .get_method_by_id(&declaring)
                .map(|method| (declaring.get_class_name(), method))
                .into_iter()
                .collect()
        };
        for (class, function) in returning {
            let by = (class, function.original_name);
            pending.push((function, declaration.effects.iter().map(|effect| (*effect, by)).collect()));
        }
    }

    while let Some((function, effects)) = pending.pop() {
        for returned in returned_classes(function) {
            let known = carried.entry(returned).or_default();
            let before = known.len();
            for (effect, by) in &effects {
                if !known.iter().any(|(known, _)| known == effect) {
                    known.push((*effect, *by));
                }
            }
            if known.len() != before {
                let known = known.clone();
                pending.extend(methods_of(codebase, returned).into_iter().map(|(_, method)| (method, known.clone())));
            }
        }
    }

    carried
}

/// Every method a call through `class` can reach, with the class that declares it: its own, the ones it inherits,
/// and the ones its docblock gives it.
fn methods_of(codebase: &CodebaseMetadata, class: Word) -> Vec<(Word, &FunctionLikeMetadata)> {
    codebase
        .get_class_like(class.as_bytes())
        .into_iter()
        .flat_map(|metadata| {
            metadata
                .declaring_method_ids
                .values()
                .filter_map(|method| codebase.get_method_by_id(method).map(|found| (method.get_class_name(), found)))
                .chain(
                    metadata
                        .pseudo_methods
                        .iter()
                        .filter_map(|method| codebase.get_method(class.as_bytes(), method.as_bytes()))
                        .map(|found| (metadata.original_name, found)),
                )
        })
        .collect()
}

/// The lowercase name of each class `function` declares it returns.
fn returned_classes(function: &FunctionLikeMetadata) -> impl Iterator<Item = Word> + '_ {
    function
        .return_type_metadata
        .iter()
        .flat_map(|returned| returned.type_union.types.iter())
        .filter_map(|atomic| match atomic {
            TAtomic::Object(object) => object.get_name(),
            _ => None,
        })
        .map(|name| ascii_lowercase_word(name.as_bytes()))
}

/// `effect`, or the effects its class carries when it is an unknown call into a class a declared call returns.
fn carried_through(effect: Effect, carried: &HashMap<Word, Vec<CarriedEffect>>) -> Vec<Effect> {
    match effect {
        Effect::Unknown(Some((class, _))) if !class.is_empty() => {
            match carried.get(&ascii_lowercase_word(class.as_bytes())) {
                Some(effects) => effects.iter().map(|&(effect, by)| Effect::Carried { effect, by }).collect(),
                None => vec![effect],
            }
        }
        effect => vec![effect],
    }
}

/// A body's own effects and changes, in source order, an effect before a change at the same place, with an unknown call
/// into a class a declared call returns read as the effects it carries.
fn own_items(summary: &EffectSummary, carried: &HashMap<Word, Vec<CarriedEffect>>) -> Vec<(Item, Origin)> {
    let mut items: Vec<(Item, Origin)> = Vec::new();
    let effects = summary.effects.iter().flat_map(|(effect, span, cause)| {
        carried_through(*effect, carried).into_iter().map(move |effect| (Item::Effect(effect), *span, *cause))
    });
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
