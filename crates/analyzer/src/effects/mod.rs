//! The effects of PHP# code, spec section 29.
//!
//! Mago analyzes files in parallel against signatures only, so no body knows its callees' effects while it is
//! analyzed. Each body records what it does by itself in an [`EffectSummary`] during its file's analysis, and
//! [`Effects::solve`] combines every summary once all files are analyzed.

use std::collections::BTreeSet;
use std::sync::Arc;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_codex::graph::strongly_connected_parts;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_database::file::FileId;
use mago_names::ACCEPTS_ANY_EFFECT;
use mago_names::EMIT_EFFECT;
use mago_names::binding::php_variable_name;
use mago_names::display_sharp_member;
use mago_names::scope::NamespaceScope;
use mago_reporting::IssueCollection;
use mago_span::Span;
use mago_word::Word;
use mago_word::WordMap;
use mago_word::empty_word;
use mago_word::word;

use crate::code::IssueCode;
use crate::utils::names::sharp_class_like_name;
use crate::utils::names::sharp_effect_name;

pub(crate) mod check;
pub(crate) mod summary;

/// A reason a body is impure other than a change of state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) enum Effect {
    /// An effect named by its class, such as `Sharp\Http`: one an `extern` line lists for the plain PHP called, or the
    /// effect class of an object code reaches.
    Class(Word),
    /// A call into plain PHP whose `extern` line says `uses any`, which may have any effect, spec section 29.
    Any,
    /// An `emit`, spec section 15, which has the effect `Sharp\Events` whatever its listeners do. Its cause is the
    /// event's class and an empty member.
    Emit,
}

impl Effect {
    /// The effect as a `uses` names it, by full class name: its class, and `Sharp\Events` for an `emit`. `None` for a
    /// call whose `extern` line says `uses any`, which no `uses` names.
    pub(crate) fn named(self) -> Option<Word> {
        match self {
            Effect::Class(effect) => Some(effect),
            Effect::Emit => Some(events()),
            Effect::Any => None,
        }
    }
}

/// The state a body changes, named by the root of the place it writes. The same roots name the values a call passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) enum Changed {
    /// The object the body runs on.
    This,
    /// An object `this` holds in a field, or one such an object holds. Changing it changes no field of `this`, so a
    /// getter may, spec section 6.1, and a `readonly` value may not, since readonly reaches everything read through it,
    /// spec section 13.
    HeldByThis,
    /// What the caller passed as the parameter at this index.
    Parameter(u32),
    /// State every body reaches, such as a static property.
    Shared,
}

impl Changed {
    /// The root of an object read from a place of this root: one `this` holds is held by `this`.
    pub(crate) fn held(self) -> Self {
        match self {
            Changed::This => Changed::HeldByThis,
            root => root,
        }
    }

    /// Whether this root is `this` or an object `this` holds, which a `readonly` value reaches.
    fn is_reached_from_this(self) -> bool {
        matches!(self, Changed::This | Changed::HeldByThis)
    }
}

/// The roots of a value. A value with no root is fresh: the body made it, so changing it changes nothing given.
pub(crate) type Roots = BTreeSet<Changed>;

/// A PHP# body: a method by its lowercase class and method names, an accessor by its lowercase class, its
/// property's PHP name (`$total`) and `get` or `set`, a lambda by the name its function value's `source` gives it, an
/// initial value that is not constant, which runs in the constructor, or a static's on the first use of its class's
/// statics, by its lowercase class and its property's PHP name, a parameter's default that is not constant, which
/// runs each time a call leaves the argument out, by its method's lowercase class and method names and its PHP name, a
/// module binding's expression, by the name of the lambda it is scanned as, or a class constant's value, by its
/// lowercase class and its name. No call reaches a binding or a constant: the container runs one, and a read the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) enum Body {
    Method(Word, Word),
    Accessor(Word, Word, Word),
    Lambda(Word),
    InitialValue(Word, Word),
    Default(Word, Word, Word),
    Binding(Word),
    Constant(Word, Word),
}

/// What a function value runs.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Runs {
    /// A PHP# lambda or method, whose effects only the solve knows.
    Body(Body),
    /// A method whose effects need no solve, as its full class name and its member, with the effects a call of it has:
    /// a plain PHP method's, which its `extern` line declares, an effect class's method's, and a PHP# method's
    /// without a body, which its `uses` lists.
    Declared { callee: (Word, Word), effects: Vec<Effect> },
    /// A plain PHP method or function that no `extern` line declares, as its full class name and its member, and the
    /// line it needs, as [`EffectSummary::undeclared`] holds one.
    Undeclared { callee: (Word, Word), line: (Word, Word) },
}

/// A lambda or a method value a body gives a place whose PHP# function type lists its effects. Spec section 29 holds
/// it to that list.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct FunctionValue {
    pub(crate) runs: Runs,
    /// The effects the place's function type lists, by full class name.
    pub(crate) allowed: Vec<Word>,
    pub(crate) span: Span,
    /// What cannot hold the value, with each type as the value's file writes it, such as "a pure `Function<int(int)>`".
    pub(crate) holder: String,
    /// The code a value of the wrong type gets in this place, such as `invalid-argument`.
    pub(crate) code: IssueCode,
}

/// A call of a PHP# method without `readonly` through a readonly value, spec section 13. Only the solve knows whether
/// the method changes `this`, which its message says. Each text is as the calling file writes it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Call {
    pub(crate) callee: Body,
    pub(crate) span: Span,
    pub(crate) receiver: Roots,
    /// The roots of each argument, by the callee's parameter index.
    pub(crate) arguments: Vec<Roots>,
    /// The call's type arguments, by the callee's own type parameter index, as a read of attributes through one takes
    /// them, which [`through`] substitutes.
    pub(crate) type_arguments: Vec<AttributeArgument>,
}

/// What a call's type argument is to a read of the attributes of the class it names, `attributes<TAttribute>()`, spec
/// sections 1 and 25.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum AttributeArgument {
    /// A class-like by its name, whose attributes a read creates with no effects when [`sharp_attributes`] holds for
    /// it in the codebase the effects are solved against, and with every effect otherwise.
    Class(Word),
    /// The type parameter at this index of the calling body's own method, which its callers give in turn.
    Parameter(u32),
    /// Any other type, whose attributes a read may create with every effect.
    Other,
}

/// Whether a read creates the attributes of `class` with no effects, spec sections 1 and 25: a PHP# class-like that no
/// plain PHP class extends or implements, since the attributes a read creates are of the class or of one that extends
/// or implements it, and whose constructor, each one's own or inherited, PHP# declares.
pub(crate) fn sharp_attributes(codebase: &CodebaseMetadata, class: Word) -> bool {
    let is_sharp = |class: Word| codebase.get_class_like(class).is_some_and(|class| class.flags.is_sharp());
    let written_in_sharp = |class: Word| {
        is_sharp(class)
            && codebase
                .get_declaring_method_class(class, word("__construct"))
                .is_none_or(|constructor| is_sharp(word(constructor.as_bytes())))
    };

    written_in_sharp(class)
        && codebase.get_all_descendants(class).iter().all(|descendant| written_in_sharp(word(descendant.as_bytes())))
}

/// What one PHP# body does by itself: the effects of the plain PHP it calls, the PHP# bodies it calls, and the state
/// it changes. A lambda is part of the body that calls it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffectSummary {
    pub(crate) body: Body,
    /// The body's member as messages name it: `price`, `total` for an accessor of `total`, or `operator +`.
    pub(crate) member: Word,
    /// Each effect, with the call that has it and that call's callee as its full class name and its member, with an
    /// empty class for a function and an empty member for a constructor.
    pub(crate) effects: Vec<(Effect, Span, (Word, Word))>,
    /// Each read of the attributes of the class the body's own type parameter at an index names, as
    /// `type.attributes<TAttribute>()` in `has<TAttribute>`, with the call and its callee: each call of the body gives
    /// it the effects of the class it names.
    pub(crate) attribute_reads: Vec<(u32, Span, (Word, Word))>,
    pub(crate) calls: Vec<Call>,
    /// Each change, with the write and the place written as the source writes it.
    pub(crate) changes: Vec<(Changed, Span, Word)>,
    pub(crate) function_values: Vec<FunctionValue>,
    pub(crate) readonly_calls: Vec<ReadonlyCall>,
    /// Each call, `new` or property read of plain PHP that no `extern` line declares, spec section 29, with the line
    /// it needs: the full name of the class that declares the member reached, and an empty member, or an empty class
    /// and the full name of a function.
    pub(crate) undeclared: Vec<(Span, (Word, Word))>,
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
    /// Whether the path ends at a member of the standard library that may have every effect, spec section 29, as
    /// `Property.get`, whose plain PHP call with `uses any` the caller can't state otherwise.
    pub(crate) every_effect: bool,
}

/// The cause of the effect a member of an effect class has by itself, spec section 29, which no call or write gives:
/// an empty class and an empty member.
pub(crate) fn member_of_effect_class() -> (Word, Word) {
    (empty_word(), empty_word())
}

impl Impurity {
    /// The reason as a sentence's predicate, such as "calls `Pdf.render`, which may have any effect", with each class
    /// named by `class_name` and each effect by `effect_name`.
    pub(crate) fn describe(&self, class_name: &dyn Fn(Word) -> String, effect_name: &dyn Fn(Word) -> String) -> String {
        if self.cause == member_of_effect_class()
            && let Some(Effect::Class(effect)) = self.effect
        {
            let class = format!("a member of the effect class `{}`", effect_name(effect));

            return match self.path.last() {
                Some(body) => format!("reaches `{}`, {class}", written(*body, class_name)),
                None => format!("is {class}"),
            };
        }

        if self.every_effect
            && let Some(body) = self.path.last()
        {
            return format!("reaches `{}`, which may have every effect", written(*body, class_name));
        }

        let cause = written(self.cause, class_name);
        match (self.path.last().map(|body| written(*body, class_name)), self.effect) {
            (None, Some(Effect::Class(effect))) => {
                format!("calls `{cause}`, which has the effect `{}`", effect_name(effect))
            }
            (None, Some(Effect::Any)) => format!("calls `{cause}`, which may have any effect"),
            (None, Some(Effect::Emit)) => {
                format!("emits `{cause}`, which has the effect `{}`", effect_name(events()))
            }
            (_, None) => self.describe_change(class_name),
            (Some(body), Some(Effect::Class(effect))) => {
                format!("reaches `{body}`, which calls `{cause}` with the effect `{}`", effect_name(effect))
            }
            (Some(body), Some(Effect::Any)) => format!("reaches `{body}`, which calls `{cause}` with any effect"),
            (Some(body), Some(Effect::Emit)) => {
                format!("reaches `{body}`, which emits `{cause}` with the effect `{}`", effect_name(events()))
            }
        }
    }

    /// The reason of an impurity that changes a place, which has no effect, as [`Self::describe`] gives it, such as
    /// "changes `this.count`", with each class named by `class_name`.
    pub(crate) fn describe_change(&self, class_name: &dyn Fn(Word) -> String) -> String {
        let cause = written(self.cause, class_name);
        match self.path.last() {
            Some(body) => format!("reaches `{}`, which changes `{cause}`", written(*body, class_name)),
            None => format!("changes `{cause}`"),
        }
    }
}

/// A class name and a member as a message writes them, the class named by `class_name`: `Clock.now`, or a function
/// value's `step` as written with no class, `Pdf.render` for the function `Vendor\Pdf\render`, named through its
/// namespace as spec section 23 calls it, as `Foundation.now` for Laravel's `now()`, and `Clock` with no member.
fn written((class, member): (Word, Word), class_name: &dyn Fn(Word) -> String) -> String {
    if class.is_empty() {
        let name = member;
        match name.as_bytes().iter().rposition(|byte| *byte == b'\\') {
            Some(separator) => display_sharp_member(
                class_name(word(&name.as_bytes()[..separator])),
                String::from_utf8_lossy(&name.as_bytes()[separator + 1..]),
            ),
            None => member.to_string(),
        }
    } else if member.is_empty() {
        class_name(class)
    } else {
        display_sharp_member(class_name(class), member)
    }
}

/// What a body does: an effect, a change of a root, or a read of the attributes of the class the type parameter at an
/// index of the body's own method names, whose effects each call gives, as [`through`] substitutes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Item {
    Effect(Effect),
    Changed(Changed),
    AttributesOf(u32),
}

impl Item {
    /// The effect the item has where it is, `None` for a change. A read of attributes no call has named the class of
    /// may have every effect, as a read of a plain PHP class's does.
    fn effect(self) -> Option<Effect> {
        match self {
            Item::Effect(effect) => Some(effect),
            Item::AttributesOf(_) => Some(Effect::Any),
            Item::Changed(_) => None,
        }
    }
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
    /// Whether each body is a member of the standard library, whose calls a caller sees only through that member.
    standard_library: Vec<bool>,
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
    /// Each use of plain PHP no `extern` line declares, once, with the line it needs and the position of the body that
    /// makes it, whose file names the line's class: the body's own, and a value of the plain PHP method or function a
    /// function value gives a place.
    undeclared: Vec<(Span, (Word, Word), usize)>,
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

        // A call to a method that can be overridden runs any override, so it reaches every override's body. A call
        // that leaves out an argument runs the default of the override it reaches at that place, and a parameter an
        // override adds after the method's own is one every call through the method leaves out.
        let mut overrides: HashMap<Body, Vec<usize>> = HashMap::default();
        for (position, summary) in summaries.iter().enumerate() {
            let (Body::Method(class, method) | Body::Default(class, method, _)) = summary.body else {
                continue;
            };
            let place = |parameter: Word| {
                codebase
                    .get_method(class, method)
                    .and_then(|own| own.parameters.iter().position(|own| own.name.0 == parameter))
            };
            for parent in codebase.get_class_like(class).into_iter().flat_map(|class| &class.all_parent_classes) {
                let parent = codebase.class_key(*parent);
                let overridden = match summary.body {
                    Body::Default(_, _, parameter) => place(parameter)
                        .and_then(|place| codebase.get_method(parent, method)?.parameters.get(place))
                        .map_or(Body::Method(parent, method), |at| Body::Default(parent, method, at.name.0)),
                    _ => Body::Method(parent, method),
                };
                overrides.entry(overridden).or_default().push(position);
            }
        }

        // A callee with no summary contributes nothing but its overrides. A call of a method without a body has the
        // effects its `uses` lists instead of a callee.
        let targets: Vec<Vec<Vec<usize>>> = summaries
            .iter()
            .map(|summary| {
                summary
                    .calls
                    .iter()
                    .map(|call| {
                        index
                            .get(&call.callee)
                            .into_iter()
                            .chain(overrides.get(&call.callee).into_iter().flatten())
                            .copied()
                            .collect()
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
                                .flat_map(|(item, _)| through(codebase, *item, call).map(move |mapped| (*item, mapped)))
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
                    Body::Method(class, _)
                    | Body::Accessor(class, _, _)
                    | Body::InitialValue(class, _)
                    | Body::Default(class, _, _)
                    | Body::Constant(class, _) => (class, summary.member),
                    Body::Lambda(_) | Body::Binding(_) => (empty_word(), summary.member),
                })
                .collect(),
            standard_library: summaries
                .iter()
                .map(|summary| match summary.body {
                    Body::Method(class, _)
                    | Body::Accessor(class, _, _)
                    | Body::InitialValue(class, _)
                    | Body::Default(class, _, _)
                    | Body::Constant(class, _) => codebase
                        .get_class_like(class)
                        .is_some_and(|metadata| codebase.is_standard_library_file(metadata.span.file_id)),
                    Body::Lambda(_) | Body::Binding(_) => false,
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
            readonly_calls: summaries
                .iter()
                .enumerate()
                .flat_map(|(position, summary)| summary.readonly_calls.iter().map(move |call| (call.clone(), position)))
                .collect(),
            undeclared: undeclared(summaries),
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
    /// one can change the effects of the calling file's functions. A method and a parameter's default are in the
    /// method's file, and an accessor and an initial value in its class's.
    #[must_use]
    pub fn callee_files(&self, codebase: &CodebaseMetadata) -> HashMap<FileId, HashSet<FileId>> {
        let file_of = |body: Body| match body {
            Body::Method(class, method) | Body::Default(class, method, _) => {
                codebase.get_method(class, method).map(|method| method.span.file_id)
            }
            Body::Accessor(class, _, _) | Body::InitialValue(class, _) => {
                codebase.get_class_like(class).map(|class| class.span.file_id)
            }
            Body::Lambda(_) | Body::Binding(_) | Body::Constant(..) => {
                unreachable!("a call reaches a method, an accessor, an initial value or a default")
            }
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

        let class = codebase.class_key(*class);
        let body = Body::Method(class, codebase.method_key(class, *method));
        let imports = &self.imports[*self.index.get(&body)?];

        self.body_impurity(body).map(|impurity| {
            impurity.describe(&|class| sharp_class_like_name(codebase, imports, class), &|effect| {
                sharp_effect_name(codebase, imports, effect)
            })
        })
    }

    /// The effects of the PHP# method or lambda `function`, by full class name, each once, as a function type's `uses`
    /// lists them: each effect of the plain PHP and the effect classes it reaches, and `Sharp\Events` when it emits.
    /// A change of state is no effect. A method without a body has the effects its `uses` lists, as a call of it does,
    /// when it holds each implementation to them, spec section 29. `None` when it calls plain PHP whose `extern` line
    /// says `uses any`, which no function type lists, and for a function no summary records and no such declaration
    /// holds, such as a plain PHP one.
    #[must_use]
    pub fn function_effects(
        &self,
        codebase: &CodebaseMetadata,
        function: &FunctionLikeIdentifier,
    ) -> Option<Vec<Word>> {
        let body = match function {
            FunctionLikeIdentifier::Method(class, method) => {
                if let Some(declaration) = holding_declaration(codebase, *class, *method) {
                    return Some(declaration.uses.clone());
                }
                let class = codebase.class_key(*class);
                Body::Method(class, codebase.method_key(class, *method))
            }
            FunctionLikeIdentifier::Closure(name) => Body::Lambda(*name),
            FunctionLikeIdentifier::Function(_) => unreachable!("PHP# declares no function outside a class"),
        };

        self.effects_of(&[*self.index.get(&body)?])
    }

    /// The effects of the accessor `accessor`, `get` or `set`, of the property `property` of the PHP# class `class`, as
    /// [`Self::function_effects`] gives a method's: those of its body and, for a `set`, of the observers that run inside
    /// it, spec section 6.2. An auto accessor runs no body of its own, so it has the effects of the observers alone.
    /// `None` when one of them calls plain PHP whose `extern` line says `uses any`, and for a property the class neither
    /// declares nor inherits.
    #[must_use]
    pub fn accessor_effects(
        &self,
        codebase: &CodebaseMetadata,
        class: Word,
        property: &[u8],
        accessor: &[u8],
    ) -> Option<Vec<Word>> {
        let property = php_variable_name(property);
        let declaring = *codebase.get_class_like(class)?.declaring_property_ids.get(&property)?;
        let declaring = codebase.class_key(declaring);
        let observers: &[&[u8]] = if accessor == b"set" { &[b"willSet", b"didSet"] } else { &[] };
        let positions: Vec<usize> = std::iter::once(accessor)
            .chain(observers.iter().copied())
            .filter_map(|body| self.index.get(&Body::Accessor(declaring, property, word(body))).copied())
            .collect();

        self.effects_of(&positions)
    }

    /// The effects the bodies at `positions` reach, each once, as [`Self::function_effects`] lists them, or `None` when
    /// one calls plain PHP whose `extern` line says `uses any`.
    fn effects_of(&self, positions: &[usize]) -> Option<Vec<Word>> {
        let mut effects = Vec::new();
        for (item, _) in positions.iter().flat_map(|position| &self.solved[*position]) {
            let Some(effect) = item.effect() else {
                continue;
            };
            let effect = effect.named()?;
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
        let mut issues = check::plain_php_has_extern_lines(self, codebase);
        issues.extend(check::getters_never_write_this(self, codebase));
        issues.extend(check::laws_must_be_pure(self, codebase));
        issues.extend(check::function_values_must_fit_their_types(self, codebase));
        issues.extend(check::implementations_must_fit_their_declarations(self, codebase));
        issues.extend(check::readonly_values_call_readonly_methods(self, codebase));
        issues.extend(check::values_have_no_effects(self, codebase));
        issues.extend(check::attributes_have_no_effects(self, codebase));

        issues
    }

    /// Each call of a method without `readonly` through a readonly value, with what the method changes in `this`, the
    /// first write that leads there, or `None` when it changes nothing in `this` and only lacks the marker. The
    /// imports of the calling body's file name the classes of the write.
    fn readonly_calls(&self) -> impl Iterator<Item = (&ReadonlyCall, &WordMap<Word>, Option<Impurity>)> + '_ {
        self.readonly_calls.iter().map(|(call, caller)| {
            let change =
                self.first_item(call.callee, |item| matches!(item, Item::Changed(root) if root.is_reached_from_this()));

            (call, &*self.imports[*caller], change)
        })
    }

    /// Each use of plain PHP no `extern` line declares, with the line it needs, as [`EffectSummary::undeclared`] holds
    /// it, and the imports and the scope of the file that makes it.
    fn undeclared_uses(&self) -> impl Iterator<Item = (Span, (Word, Word), &WordMap<Word>, &NamespaceScope)> + '_ {
        self.undeclared
            .iter()
            .map(|(span, line, position)| (*span, *line, &*self.imports[*position], &*self.scopes[*position]))
    }

    fn body_impurity(&self, body: Body) -> Option<Impurity> {
        let constructor = matches!(body, Body::Method(_, method) if method.as_bytes() == b"__construct");

        self.first_item(body, |item| {
            !(constructor && matches!(item, Item::Changed(root) if root.is_reached_from_this()))
        })
    }

    /// Why `body` has an effect that `allowed` does not list.
    fn effect_outside(&self, codebase: &CodebaseMetadata, body: Body, allowed: &[Word]) -> Option<Impurity> {
        self.first_item(body, |item| item.effect().is_some_and(|effect| is_outside(codebase, effect, allowed)))
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
        let mut every_effect = false;
        // Each item a call brings in was already its callee's, so the chain ends at a body's own call or write.
        while let Some((callee, callee_item)) = origin.via
            && let Some((_, callee_origin)) = self.solved[callee].iter().find(|(known, _)| *known == callee_item)
        {
            path.push(self.names[callee]);
            item = callee_item;
            origin = *callee_origin;
            // A plain PHP call with `uses any` in the standard library is the member's own, as `Property.get` runs any
            // `get`, so for code outside the library the chain ends at the member it calls.
            if !self.standard_library[position]
                && self.standard_library[callee]
                && matches!(item, Item::Effect(Effect::Any))
            {
                every_effect = true;
                break;
            }
        }

        // A read of attributes no call named the class of reaches the standard library member it calls, as
        // `ClassMembers.attributes`, which may have every effect.
        if let Item::AttributesOf(_) = item {
            path.push(origin.cause);
            every_effect = true;
        }

        Some(Impurity { span, path, cause: origin.cause, effect: item.effect(), every_effect })
    }

    /// The first function value given each place that has an effect its place's function type does not list, with
    /// what it runs as messages name it, a class and a member, the imports and the scope of the file that gives the
    /// value, and the call that has the effect: a call in the body it runs, or the value itself for a plain PHP method.
    fn unfit_function_values<'effects>(
        &'effects self,
        codebase: &'effects CodebaseMetadata,
    ) -> impl Iterator<
        Item = (&'effects FunctionValue, (Word, Word), &'effects WordMap<Word>, &'effects NamespaceScope, Impurity),
    > + 'effects {
        let mut reported: Vec<Span> = Vec::new();
        self.function_values.iter().filter_map(move |(value, giver)| {
            if reported.contains(&value.span) {
                return None;
            }

            let (name, impurity) = match &value.runs {
                Runs::Body(body) => {
                    (self.names[*self.index.get(body)?], self.effect_outside(codebase, *body, &value.allowed)?)
                }
                Runs::Declared { callee, effects } => {
                    let effect = effects.iter().find(|effect| is_outside(codebase, **effect, &value.allowed))?;

                    let impurity = Impurity {
                        span: value.span,
                        path: Vec::new(),
                        cause: *callee,
                        effect: Some(*effect),
                        every_effect: false,
                    };

                    (*callee, impurity)
                }
                // Its missing `extern` line is refused on its own.
                Runs::Undeclared { .. } => return None,
            };
            reported.push(value.span);

            Some((value, name, &*self.imports[*giver], &*self.scopes[*giver], impurity))
        })
    }

    /// Each PHP# method, or default of one, with the first method without a body it implements whose `uses`, with the
    /// effect of the abstract effect class that declares it, does not list one of its effects, since a call of the
    /// declaration runs the defaults of the implementation it reaches. An effect class's implementation of its
    /// abstract effect is that effect, spec section 29, so it is held to nothing, and a fake is held to the effect:
    /// its class and member as messages name them, the declaration's class and metadata, the imports and the
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
                let (Body::Method(class, method) | Body::Default(class, method, _)) = *body else {
                    return None;
                };
                let metadata = codebase.get_class_like(class)?;

                metadata.all_parent_classes.iter().chain(&metadata.all_parent_interfaces).find_map(|ancestor| {
                    let declaration = holding_declaration(codebase, *ancestor, method)?;
                    let boundary = codebase
                        .get_class_like(*ancestor)
                        .filter(|ancestor| ancestor.flags.is_abstract_effect())
                        .map(|ancestor| ancestor.original_name);
                    if boundary.is_some() && metadata.flags.is_effect_class() {
                        return None;
                    }
                    let allowed: Vec<Word> = declaration.uses.iter().copied().chain(boundary).collect();
                    let impurity = self.effect_outside(codebase, *body, &allowed)?;

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

    /// Each body that has an effect, a plain PHP call whose `extern` line says `uses any` included, and the first call
    /// that leads to one, as [`Self::bodies_with`] gives them. A change of state is no effect.
    fn bodies_with_effects(
        &self,
    ) -> impl Iterator<Item = (Body, Word, &WordMap<Word>, &NamespaceScope, Impurity)> + '_ {
        self.bodies_with(|effects, body| effects.first_item(body, |item| item.effect().is_some()))
    }

    /// Each body that changes the object it runs on, and the write that leads to it, as [`Self::bodies_with`] gives
    /// them: a `get` body never writes `this`, spec section 6.1.
    fn bodies_changing_this(
        &self,
    ) -> impl Iterator<Item = (Body, Word, &WordMap<Word>, &NamespaceScope, Impurity)> + '_ {
        self.bodies_with(|effects, body| effects.first_item(body, |item| item == Item::Changed(Changed::This)))
    }
}

/// Whether `allowed` does not list `effect`: an effect class it does not name, one of `uses any`, or an `emit` when
/// it does not name `Sharp\Events`. An effect is a class-like, named by its key, spec section 24, so the PHP# `Db` and
/// `DB` are two effects.
fn is_outside(codebase: &CodebaseMetadata, effect: Effect, allowed: &[Word]) -> bool {
    let named = match effect {
        Effect::Class(effect) => effect,
        Effect::Emit => events(),
        Effect::Any => return true,
    };
    let named = codebase.class_key(named);

    !allowed.iter().any(|known| codebase.class_key(*known) == named)
}

/// The effect an `emit` has, the class the standard library marks `EffectOfEmit`.
fn events() -> Word {
    word(EMIT_EFFECT)
}

/// The PHP# declaration without a body of `method` in `ancestor`, which holds each method that overrides it to its
/// `uses`, spec section 29, or `None` when there is none or the standard library marks it
/// [`ACCEPTS_ANY_EFFECT`], as `Dispatcher.dispatch`, which the engine calls with the listeners of every event (spec
/// section 15), and `Behavior<T>`'s `get` and `set`, which run as the accessors of a property that names the behavior
/// (spec section 6.4).
pub(crate) fn holding_declaration(
    codebase: &CodebaseMetadata,
    ancestor: Word,
    method: Word,
) -> Option<&FunctionLikeMetadata> {
    codebase.get_method(ancestor, method).filter(|declaration| {
        declaration.flags.is_sharp()
            && declaration.method_metadata.as_ref().is_some_and(|method| method.is_abstract)
            && !declaration.has_attribute(codebase, word(ACCEPTS_ANY_EFFECT))
    })
}

/// Whether `name`, a class name as the binder resolves it, is an effect, spec section 29: an abstract effect class, the
/// standard library's among them, or `Events`.
fn is_effect(codebase: &CodebaseMetadata, name: Word) -> bool {
    codebase.class_key(name) == codebase.class_key(events())
        || codebase.get_class_like(name).is_some_and(|class| class.flags.is_abstract_effect())
}

/// Each use of plain PHP no `extern` line declares, spec section 29, once, with the line it needs and the position of
/// the body that makes it: each body's own, then each value of an undeclared plain PHP method or function a body gives
/// a place whose function type lists effects. A lambda runs inside each call it is passed to, so its uses are each
/// body's that walks it, and are kept once.
fn undeclared(summaries: &[EffectSummary]) -> Vec<(Span, (Word, Word), usize)> {
    let own = summaries
        .iter()
        .enumerate()
        .flat_map(|(position, summary)| summary.undeclared.iter().map(move |(span, line)| (*span, *line, position)));
    let values = summaries.iter().enumerate().flat_map(|(position, summary)| {
        summary.function_values.iter().filter_map(move |value| match value.runs {
            Runs::Undeclared { line, .. } => Some((value.span, line, position)),
            _ => None,
        })
    });

    // A chain of calls into one class, as `Client.make().next().send()`, starts each call at one place, so the class
    // is reported there once.
    let mut undeclared: Vec<(Span, (Word, Word), usize)> = Vec::new();
    for (span, line, position) in own.chain(values) {
        if !undeclared.iter().any(|(known, known_line, _)| known.start == span.start && *known_line == line) {
            undeclared.push((span, line, position));
        }
    }

    undeclared
}

/// A body's own effects and changes, in source order, an effect before a change at the same place.
fn own_items(summary: &EffectSummary) -> Vec<(Item, Origin)> {
    let mut items: Vec<(Item, Origin)> = Vec::new();
    let effects = summary.effects.iter().map(|(effect, span, cause)| (Item::Effect(*effect), *span, *cause));
    let attribute_reads =
        summary.attribute_reads.iter().map(|(index, span, cause)| (Item::AttributesOf(*index), *span, *cause));
    let changes =
        summary.changes.iter().map(|(changed, span, place)| (Item::Changed(*changed), *span, (empty_word(), *place)));
    for (item, span, cause) in effects.chain(attribute_reads).chain(changes) {
        if !items.iter().any(|(known, _)| *known == item) {
            items.push((item, Origin { span, cause, via: None }));
        }
    }

    items.sort_by_key(|(_, origin)| origin.span.start.offset);
    items
}

/// What a callee's item is in the caller: an effect stays, a change maps through the roots the call passes, and a read
/// of attributes through a type parameter maps through the type argument the call gives it: none for a PHP# attribute
/// class, spec section 1, as `codebase` holds its descendants now, the caller's own type parameter for one it passes on,
/// and every effect for any other type.
fn through<'call>(codebase: &CodebaseMetadata, item: Item, call: &'call Call) -> impl Iterator<Item = Item> + 'call {
    let roots: Vec<Changed> = match item {
        Item::Effect(_) | Item::AttributesOf(_) => Vec::new(),
        Item::Changed(Changed::This) => call.receiver.iter().copied().collect(),
        Item::Changed(Changed::HeldByThis) => call.receiver.iter().map(|root| root.held()).collect(),
        Item::Changed(Changed::Parameter(index)) => {
            call.arguments.get(index as usize).map(|roots| roots.iter().copied().collect()).unwrap_or_default()
        }
        Item::Changed(Changed::Shared) => vec![Changed::Shared],
    };

    let effect = match item {
        Item::Effect(_) => Some(item),
        Item::AttributesOf(index) => match call.type_arguments.get(index as usize) {
            Some(AttributeArgument::Class(class)) => {
                (!sharp_attributes(codebase, *class)).then_some(Item::Effect(Effect::Any))
            }
            Some(AttributeArgument::Parameter(own)) => Some(Item::AttributesOf(*own)),
            Some(AttributeArgument::Other) | None => Some(Item::Effect(Effect::Any)),
        },
        Item::Changed(_) => None,
    };

    effect.into_iter().chain(roots.into_iter().map(Item::Changed))
}
