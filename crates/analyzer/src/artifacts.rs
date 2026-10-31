use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_word::Word;
use mago_word::WordMap;
use mago_word::WordSet;

use mago_algebra::assertion_set::AssertionSet;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::reference::SymbolReferences;
use mago_codex::ttype::combine_union_types;
use mago_codex::ttype::combiner::CombinerOptions;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Node;
use mago_syntax::cst::NodeKind;
use mago_syntax::utils::pattern::PhpShape;

use crate::context::block::BlockContext;
use crate::context::block::ReferenceConstraintSource;
use crate::context::scope::case_scope::CaseScope;
use crate::context::scope::loop_scope::LoopScope;
use crate::effects::EffectSummary;
use crate::effects::FunctionValue;
use crate::effects::ReadonlyCall;
use crate::external::Listing;
use crate::readonly::PendingReadonlyPropertyWrite;
use crate::utils::template::written_type_argument;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum VariableDefinedness {
    Defined = 1,
    PossiblyDefined = 2,
}

/// Represents scope information extracted from a `Closure::bind()` or `Closure::bindTo()` call.
/// This is used to pass the bound class scope to closure/arrow function analysis.
#[derive(Debug, Clone)]
pub struct ClosureBindScope {
    /// The class name for the bound scope (from the newScope argument).
    pub class_name: Option<Word>,
    /// Whether the closure has `$this` bound (newThis argument is non-null object).
    pub has_this: bool,
}

/// One semantic receiver and method target retained for a source-level call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedMethodCall {
    pub span: (u32, u32),
    pub class: Word,
    pub method: Word,
}

/// The type each argument of one call arrives as, keyed by the argument's span, as
/// [`AnalysisArtifacts::get_received_argument_types`] gives it.
pub(crate) type ReceivedArguments = HashMap<(u32, u32), Rc<TUnion>>;

/// What one method gives, of the several a PHP# method call runs, as [`AnalysisArtifacts::get_member_call`] says.
#[derive(Debug, Clone)]
pub struct MemberCall {
    /// The method's type arguments, one per template in declaration order, as a call's own are. None for a method that
    /// isn't generic.
    pub type_arguments: Option<Vec<TUnion>>,
    /// The type the method's call gives.
    pub result: TUnion,
}

/// What a PHP# call runs, as the analysis resolved it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallTarget {
    /// A function or method, and the class the call names, which an `extern` lookup starts from.
    FunctionLike { callee: FunctionLikeIdentifier, class: Option<Word> },
    /// The method `method` of the class `class`, which no class declares, so `callee`, the class's `__call` or
    /// `__callStatic`, serves it.
    MagicMethod { callee: FunctionLikeIdentifier, class: Word, method: Word },
    /// The function the property `property`, named with its `$`, holds, which `class` declares.
    Property { class: Word, property: Word },
}

#[derive(Debug, Clone)]
pub struct AnalysisArtifacts {
    pub expression_types: HashMap<(u32, u32), Rc<TUnion>>,
    /// The type arguments of each generic call and `new`, keyed by its span, one per template in declaration order.
    pub inferred_type_arguments: HashMap<(u32, u32), Vec<TUnion>>,
    pub if_true_assertions: HashMap<(u32, u32), WordMap<AssertionSet>>,
    pub if_false_assertions: HashMap<(u32, u32), WordMap<AssertionSet>>,
    pub true_branch_only_assertions: HashMap<(u32, u32), WordMap<AssertionSet>>,
    pub inferred_return_types: Vec<Rc<TUnion>>,
    pub inferred_yield_key_types: Vec<TUnion>,
    pub inferred_yield_value_types: Vec<TUnion>,
    pub symbol_references: SymbolReferences,
    pub loop_scope: Option<LoopScope>,
    pub case_scopes: Vec<CaseScope>,
    pub fully_matched_switch_offsets: HashSet<u32>,
    /// The declared type the value of the expression at the span goes into: a parameter type, and in PHP# a typed
    /// local or property or a return type too. Read through [`Self::get_expected_type`].
    pub(crate) expected_type: Option<(Span, TUnion)>,
    /// The target of a PHP# write through `?.` that the analysis reads first, as `+=` and `++` read it. The write
    /// happens only where every `?.` receiver on its left is not null, spec section 14.4, so the read there is never
    /// the `null` those links give.
    pub(crate) null_safe_write_target: Option<Span>,
    /// The result type of the function type a PHP# lambda goes into, when these are its body's artifacts and it writes
    /// no result type: its expression body and each `return` take it as their declared type, spec section 11.
    pub(crate) lambda_result: Option<TUnion>,
    pub method_initialized_properties: HashMap<(Word, Word), WordSet>,
    pub method_calls_this_methods: HashMap<(Word, Word), HashSet<Word>>,
    pub method_calls_parent_constructor: HashMap<(Word, Word), bool>,
    pub method_calls_parent_initializer: HashMap<(Word, Word), Word>,
    pub closure_bind_scope: Option<ClosureBindScope>,
    pub resolved_method_calls: Vec<ResolvedMethodCall>,
    /// What each method whose return is taken from its body returned, keyed by class and method.
    pub body_returns: HashMap<(Word, Word), TUnion>,
    /// What each PHP# body of the file does by itself, for [`Effects::solve`](crate::effects::Effects::solve).
    pub effect_summaries: Vec<EffectSummary>,
    /// Each set of names the file's analysis read whole, as a `match` over a sealed interface reads its implementers,
    /// spec section 20.1. A later analysis analyzes the file again when a name joins or leaves one.
    pub listings: HashSet<Listing>,
    /// What each PHP# call runs, keyed by the call's span.
    pub(crate) call_targets: HashMap<(u32, u32), Vec<CallTarget>>,
    /// What each method a PHP# method call runs gives, where the call runs one of several, as on a value that may hold
    /// more than one kind of collection, spec section 12, keyed by the call's span and the method. The call's own type
    /// arguments are the last method's, and its type is the union of every method's. Read through
    /// [`Self::get_member_call`].
    pub(crate) member_calls: HashMap<((u32, u32), FunctionLikeIdentifier), MemberCall>,
    /// The type each argument of a PHP# call to plain PHP arrives as, where it is not the argument's own type, as a
    /// `Map` a parameter that takes `array` receives as its PHP array, spec section 12, keyed by the call's span and
    /// then the argument's. The call's analysis writes its entry anew each time it checks the call, for the target it
    /// checks. Read through [`Self::get_received_argument_types`].
    pub(crate) received_argument_types: HashMap<(u32, u32), ReceivedArguments>,
    /// The type each PHP# `is`, `as`, `match` arm, catch clause or `typeof` reads while the code runs, a type parameter
    /// or a class with type arguments, keyed by the span of the type it writes. Apart from `expression_types`, because the `instanceof`
    /// a test runs as spans that type too when nothing the code writes stands before it.
    pub(crate) tested_types: HashMap<(u32, u32), TUnion>,
    /// The type each declaration's hint writes, a parameter's, a result's, a property's or a typed local's, as the
    /// checker resolved it, each class and type parameter it names included, keyed by the span of the hint, the type a
    /// local declared without a type takes from its first value for its whole life, keyed by the span of its name, and
    /// the collection the place a collection literal that holds elements lands in declares for it, keyed by the span of
    /// the literal. Read through [`Self::get_declared_type`].
    pub(crate) declared_types: HashMap<(u32, u32), TUnion>,
    /// The tuple a list pattern of types alone, as `[int, string]`, narrows the value it tests to, keyed by the span of
    /// its `[`. The test there reads `List<Any?>` while the code runs, and the count and element tests after it check
    /// the rest of the tuple.
    pub(crate) list_pattern_tuples: HashMap<(u32, u32), TUnion>,
    /// The full name of the plain PHP constant each PHP# read through a namespace reaches, keyed by the read's span.
    pub(crate) namespace_constants: HashMap<(u32, u32), Word>,
    /// The parameters of a PHP# method that erase to a narrower type than a parameter of a method it overrides or
    /// implements erases to, as `string value` of `Trimmed.set` against `T value` of `Behavior.set`, keyed by the
    /// span of the parameter's name. The bridge declares each with no PHP type, as Java's bridge method takes the
    /// erased type, and the engine checks a call from plain PHP against its written type (spec section 11).
    pub widened_parameters: HashSet<(u32, u32)>,
    /// The function values the analysis gave places whose function types list effects, until the body that gives them
    /// records its summary.
    pub(crate) function_values: Vec<FunctionValue>,
    /// The calls of PHP# methods without `readonly` through readonly values, until the body that makes them records
    /// its summary.
    pub(crate) readonly_calls: Vec<ReadonlyCall>,
    pub(crate) variable_definedness: HashMap<(u32, u32), WordMap<VariableDefinedness>>,
    variable_definedness_targets: Option<Arc<[bool; NodeKind::COUNT]>>,
    pub(crate) pending_readonly_property_writes: Vec<PendingReadonlyPropertyWrite>,
    pub(crate) static_local_types: Option<WordMap<TUnion>>,
    /// The number each PHP# `is`, `as` and `match` of the body numbers its hidden variables after, keyed by its span.
    /// Read through [`Self::numbered_php_shape`].
    hidden_variable_starts: RefCell<HashMap<(u32, u32), u32>>,
    /// The number of the last hidden variable the body numbered, which a lambda in the body numbers its own after.
    pub(crate) last_hidden_variable: Cell<u32>,
}

impl Default for AnalysisArtifacts {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalysisArtifacts {
    #[must_use]
    pub fn new() -> Self {
        Self {
            expression_types: HashMap::default(),
            inferred_type_arguments: HashMap::default(),
            inferred_return_types: Vec::new(),
            inferred_yield_key_types: Vec::new(),
            inferred_yield_value_types: Vec::new(),
            if_true_assertions: HashMap::default(),
            if_false_assertions: HashMap::default(),
            true_branch_only_assertions: HashMap::default(),
            symbol_references: SymbolReferences::new(),
            case_scopes: Vec::new(),
            loop_scope: None,
            fully_matched_switch_offsets: HashSet::default(),
            expected_type: None,
            null_safe_write_target: None,
            lambda_result: None,
            method_initialized_properties: HashMap::default(),
            method_calls_this_methods: HashMap::default(),
            method_calls_parent_constructor: HashMap::default(),
            method_calls_parent_initializer: HashMap::default(),
            closure_bind_scope: None,
            resolved_method_calls: Vec::new(),
            body_returns: HashMap::default(),
            effect_summaries: Vec::new(),
            listings: HashSet::default(),
            call_targets: HashMap::default(),
            member_calls: HashMap::default(),
            received_argument_types: HashMap::default(),
            tested_types: HashMap::default(),
            declared_types: HashMap::default(),
            list_pattern_tuples: HashMap::default(),
            namespace_constants: HashMap::default(),
            widened_parameters: HashSet::default(),
            function_values: Vec::new(),
            readonly_calls: Vec::new(),
            variable_definedness: HashMap::default(),
            variable_definedness_targets: None,
            pending_readonly_property_writes: Vec::new(),
            static_local_types: None,
            hidden_variable_starts: RefCell::default(),
            last_hidden_variable: Cell::default(),
        }
    }

    /// The PHP of the PHP# `is`, `as` or `match` at `form`, which `build` builds with its hidden variables numbered after
    /// the number it takes. The form's first PHP numbers them after every hidden variable the body numbered before, so
    /// each has a name of its own in the body, and its every later PHP numbers them alike, so a read of one names the
    /// variable its first PHP assigned.
    pub(crate) fn numbered_php_shape<'shape>(
        &self,
        form: Span,
        build: impl FnOnce(u32) -> Option<PhpShape<'shape>>,
    ) -> Option<PhpShape<'shape>> {
        let range = get_expression_range(&form);
        let start = self.hidden_variable_starts.borrow().get(&range).copied();
        if let Some(start) = start {
            return build(start);
        }

        let start = self.last_hidden_variable.get();
        let shape = build(start)?;
        self.hidden_variable_starts.borrow_mut().insert(range, start);
        self.last_hidden_variable.set(start + shape.temporaries);

        Some(shape)
    }

    pub(crate) fn record_static_local_types(
        &mut self,
        block_context: &BlockContext<'_>,
        codebase: &CodebaseMetadata,
        options: CombinerOptions,
    ) {
        let Some(static_local_types) = self.static_local_types.as_mut() else {
            return;
        };

        for variable in &block_context.static_locals {
            if block_context
                .by_reference_constraints
                .get(variable)
                .is_some_and(|constraint| constraint.source == ReferenceConstraintSource::Static)
            {
                continue;
            }

            let Some(variable_type) = block_context.locals.get(variable) else {
                continue;
            };

            if let Some(previous_type) = static_local_types.get_mut(variable) {
                if previous_type != variable_type.as_ref() {
                    *previous_type = combine_union_types(previous_type, variable_type, codebase, options);
                }
            } else {
                static_local_types.insert(*variable, variable_type.as_ref().clone());
            }
        }
    }

    pub(crate) fn with_variable_definedness_targets(mut self, targets: Option<Arc<[bool; NodeKind::COUNT]>>) -> Self {
        self.variable_definedness_targets = targets;
        self
    }

    pub(crate) fn variable_definedness_targets(&self) -> Option<Arc<[bool; NodeKind::COUNT]>> {
        self.variable_definedness_targets.clone()
    }

    #[inline]
    pub(crate) fn record_variable_definedness(&mut self, node: Node<'_, '_>, block_context: &BlockContext<'_>) {
        let Some(targets) = self.variable_definedness_targets.as_deref() else {
            return;
        };

        let span = node.span();
        if !node_or_same_span_descendant_is_targeted(node, span, targets) {
            return;
        }

        let mut variables = WordMap::default();
        for (variable, variable_type) in &block_context.locals {
            if !is_plain_variable(*variable) {
                continue;
            }

            let definedness = if variable_type.possibly_undefined_from_try()
                || variable_type.possibly_undefined()
                    && block_context.possibly_undefined_variable_ids.contains(variable)
            {
                VariableDefinedness::PossiblyDefined
            } else {
                VariableDefinedness::Defined
            };
            variables.insert(*variable, definedness);
        }

        for variable in &block_context.variables_possibly_in_scope {
            if is_plain_variable(*variable) {
                variables.entry(*variable).or_insert(VariableDefinedness::PossiblyDefined);
            }
        }

        self.variable_definedness.insert((span.start.offset, span.end.offset), variables);
    }

    pub(crate) fn set_loop_scope(&mut self, loop_scope: LoopScope) {
        let previous_scope = self.loop_scope.take().map(Box::new);
        self.loop_scope = Some(loop_scope.with_parent_loop(previous_scope));
    }

    /// SAFETY: the caller must ensure that `self.loop_scope` is not `None`.
    pub(crate) unsafe fn take_loop_scope_unchecked(&mut self) -> LoopScope {
        let mut loop_scope = unsafe {
            // SAFETY: the caller must ensure that `self.loop_scope` is not `None`.
            self.loop_scope.take().unwrap_unchecked()
        };

        match loop_scope.parent_loop.take() {
            Some(parent_loop) => {
                self.loop_scope = Some(*parent_loop);
            }
            None => {
                self.loop_scope = None;
            }
        }

        loop_scope
    }

    pub(crate) fn get_loop_scope_mut(&mut self) -> Option<&mut LoopScope> {
        self.loop_scope.as_mut()
    }

    pub(crate) fn record_loop_assignment_target(&mut self, target: Word) {
        let mut loop_scope = self.loop_scope.as_mut();
        while let Some(scope) = loop_scope {
            if scope.tracks_assignment_targets {
                scope.assignment_targets.insert(target);
            }

            loop_scope = scope.parent_loop.as_deref_mut();
        }
    }

    /// Records the type arguments of the generic call or `new` at `span`, one per template in declaration order, each
    /// the type PHP# writes, as [`written_type_argument`] gives it. A template no argument bound is `mixed`.
    pub(crate) fn record_type_arguments(
        &mut self,
        span: Span,
        type_arguments: impl Iterator<Item = Option<TUnion>>,
        codebase: &CodebaseMetadata,
    ) {
        let type_arguments = type_arguments
            .map(|type_argument| match type_argument {
                Some(type_argument) if !type_argument.is_placeholder() => {
                    written_type_argument(&type_argument, codebase)
                }
                _ => get_mixed(),
            })
            .collect();

        self.inferred_type_arguments.insert((span.start.offset, span.end.offset), type_arguments);
    }

    /// Set the type of expression `expression` to `t`.
    #[inline]
    pub fn set_expression_type<T>(&mut self, expression: &T, t: TUnion)
    where
        T: HasSpan,
    {
        self.expression_types.insert(get_expression_range(expression), Rc::new(t));
    }

    /// What the PHP# call `call` runs, as the analysis resolved it when it checked the call.
    pub fn get_callees<T>(&self, call: &T) -> impl Iterator<Item = &CallTarget>
    where
        T: HasSpan,
    {
        self.call_targets.get(&get_expression_range(call)).into_iter().flatten()
    }

    /// Records that the PHP# call at `span` runs `target`, once however often the analysis checks the call.
    pub(crate) fn record_call_target(&mut self, span: Span, target: CallTarget) {
        let recorded = self.call_targets.entry((span.start.offset, span.end.offset)).or_default();
        if !recorded.contains(&target) {
            recorded.push(target);
        }
    }

    /// The type each argument of the call at `call` arrives as, keyed by the argument's span, where the call's analysis
    /// crossed it into plain PHP, spec section 12. None for a call that crossed none.
    pub(crate) fn get_received_argument_types(&self, call: Span) -> Option<&ReceivedArguments> {
        self.received_argument_types.get(&get_expression_range(&call))
    }

    /// The type of `expression`, or, where it is an argument of the call at `call` that the call's analysis crossed
    /// into plain PHP, the type the callee receives, as [`Self::get_received_argument_types`] holds it.
    pub(crate) fn get_received_expression_type<T>(&self, call: Option<Span>, expression: &T) -> Option<&Rc<TUnion>>
    where
        T: HasSpan,
    {
        call.and_then(|call| self.get_received_argument_types(call))
            .and_then(|received| received.get(&get_expression_range(expression)))
            .or_else(|| self.get_rc_expression_type(expression))
    }

    /// What the method `callee` gives, of the several the PHP# method call `call` runs, as the analysis checked it.
    /// None for a call that runs one method.
    pub fn get_member_call<T>(&self, call: &T, callee: &FunctionLikeIdentifier) -> Option<&MemberCall>
    where
        T: HasSpan,
    {
        self.member_calls.get(&(get_expression_range(call), *callee))
    }

    /// Records that the method `callee`, of the several the PHP# method call at `span` runs, gives `result`, with the
    /// type arguments its analysis just recorded for the call. A method the call runs on each part of its receiver
    /// alone, as one that changes it, is recorded once more for each part, and each record joins the earlier ones, so
    /// the method gives what any part gives, with each type argument any part fixes, which are the call's own too.
    pub(crate) fn record_member_call(
        &mut self,
        span: Span,
        callee: FunctionLikeIdentifier,
        mut result: TUnion,
        codebase: &CodebaseMetadata,
    ) {
        let range = (span.start.offset, span.end.offset);
        let mut type_arguments = self.inferred_type_arguments.get(&range).cloned();
        if let Some(earlier) = self.member_calls.get(&(range, callee)) {
            let join = |own: &TUnion, earlier: &TUnion| {
                combine_union_types(own, earlier, codebase, CombinerOptions::default())
            };
            result = join(&result, &earlier.result);
            if let (Some(own), Some(earlier)) = (type_arguments.as_mut(), earlier.type_arguments.as_ref()) {
                for (own, earlier) in own.iter_mut().zip(earlier) {
                    *own = join(own, earlier);
                }
                self.inferred_type_arguments.insert(range, own.clone());
            }
        }
        self.member_calls.insert((range, callee), MemberCall { type_arguments, result });
    }

    /// Records that the PHP# test or `typeof` that writes the type `hint` reads `tested` while the code runs.
    pub(crate) fn record_tested_type(&mut self, hint: &impl HasSpan, tested: TUnion) {
        self.tested_types.insert(get_expression_range(hint), tested);
    }

    /// The type the PHP# test or `typeof` that writes the type `hint` reads while the code runs, a type parameter or a
    /// class with type arguments. `None` for a class without type arguments.
    pub fn get_tested_type(&self, hint: &impl HasSpan) -> Option<&TUnion> {
        self.tested_types.get(&get_expression_range(hint))
    }

    /// Records that the declaration's type `hint` writes `declared`, as the checker resolved it, that the local a
    /// declaration without a type names at `hint` takes `declared`, or that the place a collection literal `hint` lands
    /// in declares it `declared`.
    pub(crate) fn record_declared_type(&mut self, hint: &impl HasSpan, declared: TUnion) {
        self.declared_types.insert(get_expression_range(hint), declared);
    }

    /// The type the declaration's type `hint` writes, as the checker resolved it, each class and type parameter it
    /// names included, the type the local a declaration without a type names at `hint` takes from its first value, or
    /// the collection the place a collection literal `hint` lands in declares for it. `None` for a hint the analysis met
    /// in no declaration, and a literal no declared collection takes.
    pub fn get_declared_type(&self, hint: &impl HasSpan) -> Option<&TUnion> {
        self.declared_types.get(&get_expression_range(hint))
    }

    /// Records that the list pattern whose `[` is `left_bracket` narrows the value it tests to `tuple`, or to the type
    /// its test reads when `tuple` is `None`.
    pub(crate) fn record_list_pattern_tuple(&mut self, left_bracket: &impl HasSpan, tuple: Option<TUnion>) {
        let range = get_expression_range(left_bracket);
        match tuple {
            Some(tuple) => self.list_pattern_tuples.insert(range, tuple),
            None => self.list_pattern_tuples.remove(&range),
        };
    }

    /// The type the test that writes the type `hint` narrows its value to: the tuple of a list pattern of types alone
    /// at its `[`, or else the type the test reads while the code runs.
    pub(crate) fn get_narrowed_type(&self, hint: &impl HasSpan) -> Option<&TUnion> {
        self.list_pattern_tuples.get(&get_expression_range(hint)).or_else(|| self.get_tested_type(hint))
    }

    /// The declared type the value of `expression` goes into, when the expression that set it is `expression` itself
    /// and not one before or around it.
    pub(crate) fn get_expected_type(&self, expression: &impl HasSpan) -> Option<&TUnion> {
        self.expected_type.as_ref().filter(|(span, _)| *span == expression.span()).map(|(_, expected)| expected)
    }

    /// The full name of the plain PHP constant the PHP# read `read` reaches through its namespace, as `Pdf.DEFAULT_DPI`
    /// reads one, spec section 23. None for any other read.
    pub fn get_namespace_constant<T>(&self, read: &T) -> Option<Word>
    where
        T: HasSpan,
    {
        self.namespace_constants.get(&get_expression_range(read)).copied()
    }

    /// Records that the PHP# read at `span` reaches the plain PHP constant `constant`.
    pub(crate) fn record_namespace_constant(&mut self, span: Span, constant: Word) {
        self.namespace_constants.insert((span.start.offset, span.end.offset), constant);
    }

    /// Get the type of expression `expression`.
    #[inline]
    pub fn get_expression_type<T>(&self, expression: &T) -> Option<&TUnion>
    where
        T: HasSpan,
    {
        let t = self.expression_types.get(&get_expression_range(expression))?;

        Some(&**t)
    }

    /// Set the type of expression `expression` to `t`.
    #[inline]
    pub fn set_rc_expression_type<T>(&mut self, expression: &T, t: Rc<TUnion>)
    where
        T: HasSpan,
    {
        self.expression_types.insert(get_expression_range(expression), t);
    }

    /// Get the type of expression `expression`.
    #[inline]
    pub fn get_rc_expression_type<T>(&self, expression: &T) -> Option<&Rc<TUnion>>
    where
        T: HasSpan,
    {
        self.expression_types.get(&get_expression_range(expression))
    }
}

fn node_or_same_span_descendant_is_targeted(node: Node<'_, '_>, span: Span, targets: &[bool; NodeKind::COUNT]) -> bool {
    if targets[node.kind() as usize] {
        return true;
    }

    let mut targeted = false;
    node.visit_children(|child| {
        if !targeted && child.span() == span {
            targeted = node_or_same_span_descendant_is_targeted(child, span, targets);
        }
    });

    targeted
}

fn is_plain_variable(variable: Word) -> bool {
    let bytes = variable.as_bytes();

    bytes.starts_with(b"$")
        && !bytes.contains(&b'[')
        && !bytes.windows(2).any(|window| window == b"->" || window == b"::")
}

#[inline]
pub fn get_expression_range<T>(expression: &T) -> (u32, u32)
where
    T: HasSpan,
{
    let span = expression.span();

    (span.start.offset, span.end.offset)
}
