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
use mago_codex::ttype::combiner::combine;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Node;
use mago_syntax::cst::NodeKind;

use crate::context::block::BlockContext;
use crate::context::block::ReferenceConstraintSource;
use crate::context::scope::case_scope::CaseScope;
use crate::context::scope::loop_scope::LoopScope;
use crate::effects::EffectSummary;
use crate::effects::FunctionValue;
use crate::readonly::PendingReadonlyPropertyWrite;

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
    pub inferred_parameter_types: Option<HashMap<usize, TUnion>>,
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
    /// What each PHP# call runs, keyed by the call's span.
    pub(crate) call_targets: HashMap<(u32, u32), Vec<CallTarget>>,
    /// The function values the analysis gave places whose function types list effects, until the body that gives them
    /// records its summary.
    pub(crate) function_values: Vec<FunctionValue>,
    pub(crate) variable_definedness: HashMap<(u32, u32), WordMap<VariableDefinedness>>,
    variable_definedness_targets: Option<Arc<[bool; NodeKind::COUNT]>>,
    pub(crate) pending_readonly_property_writes: Vec<PendingReadonlyPropertyWrite>,
    pub(crate) static_local_types: Option<WordMap<TUnion>>,
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
            inferred_parameter_types: None,
            method_initialized_properties: HashMap::default(),
            method_calls_this_methods: HashMap::default(),
            method_calls_parent_constructor: HashMap::default(),
            method_calls_parent_initializer: HashMap::default(),
            closure_bind_scope: None,
            resolved_method_calls: Vec::new(),
            body_returns: HashMap::default(),
            effect_summaries: Vec::new(),
            call_targets: HashMap::default(),
            function_values: Vec::new(),
            variable_definedness: HashMap::default(),
            variable_definedness_targets: None,
            pending_readonly_property_writes: Vec::new(),
            static_local_types: None,
        }
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

    /// Records the type arguments of the generic call or `new` at `span`, one per template in declaration order. A
    /// template no argument bound is `mixed`, and a literal is its scalar type, as in `int` for `5`.
    pub(crate) fn record_type_arguments(
        &mut self,
        span: Span,
        type_arguments: impl Iterator<Item = Option<TUnion>>,
        codebase: &CodebaseMetadata,
    ) {
        let type_arguments = type_arguments
            .map(|type_argument| match type_argument {
                Some(mut type_argument) if !type_argument.is_placeholder() => {
                    type_argument.widen_scalars();

                    TUnion::from_vec(combine(type_argument.types.into_owned(), codebase, CombinerOptions::default()))
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
