//! Spec section 28: a law, whose claim the analysis reads as a static method's `bool` body, and whose `when` condition
//! and body it reads in the same scope. In a body, a call of a law is a statement of its own, and a law calls itself only
//! on a smaller value.

use mago_allocator::Arena;
use mago_codex::context::ScopeContext;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::reference::ReferenceOrigin;
use mago_codex::ttype::template::TemplateResult;
use mago_names::binding::Binding;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Hint;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Law;
use mago_syntax::cst::LawProof;
use mago_syntax::cst::ListPatternElement;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::PatternMatchArm;
use mago_syntax::cst::PatternMatchArmBody;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_word::WordMap;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::context::scope::control_action::ControlActionSet;
use crate::effects;
use crate::effects::Body;
use crate::error::AnalysisError;
use crate::invocation::Invocation;
use crate::invocation::InvocationArgumentsSource;
use crate::invocation::InvocationTarget;
use crate::invocation::analyzer::analyze_invocation;
use crate::statement::analyze_statements;
use crate::statement::function_like::FunctionLikeBody;
use crate::statement::function_like::analyze_function_like;
use crate::utils::names::display_code_member;
use crate::utils::names::display_sharp_type;
use crate::utils::quoted_source;

/// What a law's body may hold, which the error of anything else names.
const BODY: &str =
    "A law's body holds only `if`, `match`, locals, `return;` and calls of laws, each call a statement of its own.";

/// Spec section 28: a law's claim is analyzed as a static method's body that returns `bool`, and records its effects,
/// so the law rule refuses it when it is impure. Its `when` condition and its body are analyzed in the same scope, where
/// its parameters are locals. A law is never one of the class's methods, so no method rule reaches it.
impl<'ast, 'arena> Analyzable<'ast, 'arena> for Law<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let Some(class_like_metadata) = block_context.scope.get_class_like() else {
            tracing::error!(
                "Attempted to analyze law `{}` without class-like context.",
                mago_bytes::BytesDisplay(self.name.value)
            );

            return Ok(());
        };

        let lowercase_law_name = ascii_lowercase_word(self.name.value);
        let Some(law_metadata) = class_like_metadata.laws.get(&lowercase_law_name) else {
            tracing::error!(
                "Failed to find law metadata for `{}` in class `{}`.",
                mago_bytes::BytesDisplay(self.name.value),
                class_like_metadata.original_name
            );

            return Ok(());
        };

        // Skip a second law of one name; semantics reports it.
        if law_metadata.span != self.span() {
            return Ok(());
        }

        let mut scope = ScopeContext::new(ReferenceOrigin::Symbol((class_like_metadata.name, lowercase_law_name)));
        scope.set_class_like(Some(class_like_metadata));
        scope.set_function_like(Some(law_metadata));
        scope.set_static(true);

        let claim = FunctionLikeBody::Expression(self.claim);
        let mut law_block_context = BlockContext::new(scope, context.settings.register_super_globals);
        analyze_function_like(
            context,
            artifacts,
            &mut law_block_context,
            law_metadata,
            &self.parameter_list,
            None,
            claim,
            None,
        )?;

        effects::summary::record(
            context,
            artifacts,
            Body::Method(class_like_metadata.name, lowercase_law_name),
            word(self.name.value),
            self.parameter_list.parameters.iter().map(|parameter| parameter.variable.span).collect(),
            None,
            claim,
        );

        let mut body_context = law_block_context.clone();
        body_context.flags.set_has_returned(false);
        body_context.control_actions = ControlActionSet::new();
        body_context.flags.set_inside_law_body(true);
        if let Some(condition) = &self.condition {
            analyze_condition(context, &mut body_context, artifacts, condition.expression)?;
        }
        if let LawProof::Body(body) = &self.proof {
            let mut walk = BodyWalk { context, law: self, class: class_like_metadata, smaller: Vec::new(), step: None };
            walk.statements(body.statements.as_slice());
            analyze_statements(body.statements.as_slice(), context, &mut body_context, artifacts)?;
        }

        Ok(())
    }
}

/// Analyzes a law's `when` condition, which is a `bool`, as its claim is.
fn analyze_condition<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    condition: &Expression<'arena>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    condition.analyze(context, block_context, artifacts)?;
    if let Some(condition_type) = artifacts.get_expression_type(condition)
        && !condition_type.is_bool()
        && !condition_type.is_never()
    {
        let written = quoted_source(context, condition.span());
        let r#type = display_sharp_type(context, condition_type);
        context.collector.report_with_code(
            IssueCode::InvalidLaw,
            Issue::error(format!("A law's `when` condition is a `bool`, and `{written}` is an `{type}`."))
                .with_annotation(Annotation::primary(condition.span()).with_message("The condition is written here.")),
        );
    }

    Ok(())
}

/// The law `call` names, and the class that states it: `Class.name(…)` names a law of `Class`, as a static method is
/// reached through its class's name, the law's own class included. `None` when the call names no law.
pub(crate) fn called_law<'ctx, A>(
    context: &Context<'ctx, '_, A>,
    call: &Call<'_>,
) -> Option<(&'ctx ClassLikeMetadata, &'ctx FunctionLikeMetadata)>
where
    A: Arena,
{
    let Call::Method(call) = call else {
        return None;
    };
    let class = context.resolved_names.static_call_class(call)?;
    let ClassLikeMemberSelector::Identifier(name) = &call.method else {
        return None;
    };
    let class = context.codebase.get_class_like(context.resolved_names.get(&class.name))?;

    Some((class, class.laws.get(&ascii_lowercase_word(name.value))?))
}

/// Whether `call` is a bare call of a law of `class`, which a law's body writes through the class name, as it writes a
/// static method. The bare call's own analysis names the call to write.
fn is_bare_law_call<A>(context: &Context<'_, '_, A>, class: &ClassLikeMetadata, call: &Call<'_>) -> bool
where
    A: Arena,
{
    let Call::Function(call) = call else {
        return false;
    };
    let Expression::Identifier(name) = call.function else {
        return false;
    };

    !matches!(context.resolved_names.binding(name), Some(Binding::Local(_) | Binding::This))
        && class.laws.contains_key(&ascii_lowercase_word(name.value()))
}

/// The call a statement of a law's body is, when it calls a law.
pub(crate) fn law_call<'ast, 'arena>(statement: &'ast Statement<'arena>) -> Option<&'ast Call<'arena>> {
    match statement {
        Statement::Expression(statement) => match statement.expression {
            Expression::Call(call) => Some(call),
            _ => None,
        },
        _ => None,
    }
}

/// Analyzes `call`, a call statement of the law `law` of `class` in a law's body: its arguments are passed to the law's
/// parameters as any call passes them, and the law it calls is a reference of the body, so a change to it proves the
/// calling law again.
pub(crate) fn analyze_law_call<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    call: &Call<'arena>,
    (class, law): (&'ctx ClassLikeMetadata, &'ctx FunctionLikeMetadata),
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let argument_list = call.get_argument_list();
    artifacts.symbol_references.add_reference_to_class_member(&block_context.scope, (class.name, law.name), false);
    let target = InvocationTarget::FunctionLike {
        identifier: FunctionLikeIdentifier::Method(class.name, law.name),
        metadata: law,
        inferred_return_type: None,
        effective_signature: None,
        method_context: None,
        span: call.span(),
    };
    let mut invocation = Invocation::new(target, InvocationArgumentsSource::ArgumentList(argument_list), call.span());

    analyze_invocation(
        context,
        block_context,
        artifacts,
        &mut invocation,
        None,
        None,
        &mut TemplateResult::default(),
        &mut WordMap::default(),
    )
}

/// The refusal of a call of the law `law` of `class` that is no call statement of a law's body.
pub(crate) fn uncallable_law<A>(
    context: &Context<'_, '_, A>,
    class: &ClassLikeMetadata,
    law: &FunctionLikeMetadata,
    span: Span,
) -> Issue
where
    A: Arena,
{
    let name = display_code_member(context, class.original_name, law.original_name);

    Issue::error(format!("`{name}` is a law, and only a law's body calls a law, as a statement of its own."))
        .with_annotation(Annotation::primary(span).with_message("Called here."))
        .with_annotation(Annotation::secondary(law.span).with_message("The law is stated here."))
}

/// A value a call of the law on itself may pass and be smaller, which the body learned from a pattern or a test.
#[derive(Debug, Clone, Copy)]
enum Smaller<'arena> {
    /// The rest of the list parameter at this place, from a list pattern with an element before the rest.
    Rest(usize, &'arena [u8]),
    /// The int parameter at this place is above the bound, so one less is nearer it.
    Down(usize, Bound),
    /// The int parameter at this place is below the bound, so one more is nearer it.
    Up(usize, Bound),
}

/// The value an int parameter steps towards: an int literal, or the parameter at a place, which every call of the law
/// on itself passes on unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    Literal(i64),
    Parameter(usize),
}

/// How a call of the law on itself makes the parameter at a place smaller, which every such call shares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Rest,
    Down(Bound),
    Up(Bound),
}

/// The walk of a law's body that refuses what a body may not hold, and each call of the law on itself that passes no
/// smaller value.
struct BodyWalk<'walk, 'ctx, 'ast, 'arena, A>
where
    A: Arena,
{
    context: &'walk mut Context<'ctx, 'arena, A>,
    law: &'ast Law<'arena>,
    class: &'ctx ClassLikeMetadata,
    smaller: Vec<Smaller<'arena>>,
    /// The parameter the first call of the law on itself makes smaller, and how.
    step: Option<(usize, Step)>,
}

impl<'ast, 'arena, A> BodyWalk<'_, '_, 'ast, 'arena, A>
where
    A: Arena,
{
    /// Walks `statements` in order. After an `if` whose branch ends the block, the statements that follow know its
    /// condition is false.
    fn statements(&mut self, statements: &'ast [Statement<'arena>]) {
        let known = self.smaller.len();
        for statement in statements {
            self.statement(statement);
            if let Statement::If(r#if) = statement
                && let IfBody::Statement(body) = &r#if.body
                && body.else_clause.is_none()
                && ends(body.statement)
            {
                self.learn(r#if.condition, false);
            }
        }
        self.smaller.truncate(known);
    }

    fn statement(&mut self, statement: &'ast Statement<'arena>) {
        match statement {
            Statement::Block(block) => self.statements(block.statements.as_slice()),
            Statement::Return(r#return) if r#return.value.is_none() => {}
            Statement::LocalDeclaration(_) => {}
            Statement::If(r#if) => {
                let IfBody::Statement(body) = &r#if.body else {
                    return self.refuse(statement.span());
                };
                if !body.else_if_clauses.is_empty() {
                    return self.refuse(statement.span());
                }
                let known = self.smaller.len();
                self.learn(r#if.condition, true);
                self.statement(body.statement);
                self.smaller.truncate(known);
                if let Some(clause) = &body.else_clause {
                    self.learn(r#if.condition, false);
                    self.statement(clause.statement);
                    self.smaller.truncate(known);
                }
            }
            Statement::PatternMatch(r#match) => {
                let subject = self.parameter(r#match.expression);
                for arm in r#match.arms.iter() {
                    let (PatternMatchArm::Pattern(_) | PatternMatchArm::Default(_), PatternMatchArmBody::Block(block)) =
                        (arm, arm.body())
                    else {
                        self.refuse(arm.span());
                        continue;
                    };
                    let known = self.smaller.len();
                    if let (PatternMatchArm::Pattern(arm), Some(subject)) = (arm, subject)
                        && let Pattern::List(list) = arm.pattern
                    {
                        let rest = list.elements.iter().find_map(|element| match element {
                            ListPatternElement::Rest(rest) => rest.variable.as_ref(),
                            ListPatternElement::Pattern(_) => None,
                        });
                        let elements =
                            list.elements.iter().filter(|element| matches!(element, ListPatternElement::Pattern(_)));
                        if let Some(rest) = rest
                            && elements.count() > 0
                        {
                            self.smaller.push(Smaller::Rest(subject, rest.value));
                        }
                    }
                    self.statements(block.statements.as_slice());
                    self.smaller.truncate(known);
                }
            }
            _ => match law_call(statement) {
                Some(call) => match called_law(self.context, call) {
                    Some((class, law)) if class.name == self.class.name && law.span == self.law.span() => {
                        self.call(call.get_argument_list(), call.span());
                    }
                    Some(_) => {}
                    None if is_bare_law_call(self.context, self.class, call) => {}
                    None => self.refuse(statement.span()),
                },
                None => self.refuse(statement.span()),
            },
        }
    }

    /// Checks `arguments`, those of a call statement of this law itself, at `span`: one of them passes a smaller value,
    /// of the parameter every call of the law on itself makes smaller in the same way.
    fn call(&mut self, arguments: &'ast ArgumentList<'arena>, span: Span) {
        let values: Vec<&Expression<'arena>> = arguments.arguments.iter().map(|argument| argument.value()).collect();
        let step =
            values.iter().enumerate().find_map(|(place, value)| Some((place, self.step(&values, place, value)?)));
        let annotation = match (step, self.step) {
            (Some(step), None) => {
                self.step = Some(step);
                return;
            }
            (Some(step), Some(first)) if step == first => return,
            (Some(_), Some(_)) => {
                "This call makes a value smaller other than the one the law's first call of itself does."
            }
            (None, _) => "This call passes no smaller value.",
        };

        let name =
            display_code_member(self.context, self.class.original_name, String::from_utf8_lossy(self.law.name.value));
        self.context.collector.report_with_code(
            IssueCode::LawRecursion,
            Issue::error("a law calls itself only on a smaller value")
                .with_annotation(Annotation::primary(span).with_message(annotation))
                .with_help(format!(
                    "Call {name} on a list's rest from a pattern the body matched, or on an int one step towards a bound the body tested, as `if (n <= 0) {{ return; }}` tests 0 before `{name}(n - 1);`."
                )),
        );
    }

    /// How `value`, the argument at `place` of a call of the law on itself among `values`, makes the parameter at
    /// `place` smaller: it is the rest of its list pattern, or it is one step from it towards a bound, which a parameter
    /// bound's own argument passes on unchanged.
    fn step(&self, values: &[&Expression<'arena>], place: usize, value: &Expression<'arena>) -> Option<Step> {
        let value = unparenthesized(value);
        let passes_on = |bound: Bound| match bound {
            Bound::Literal(_) => true,
            Bound::Parameter(index) => values.get(index).is_some_and(|value| self.parameter(value) == Some(index)),
        };
        let down = match value {
            Expression::ConstantAccess(name) => {
                return self
                    .smaller
                    .iter()
                    .any(|known| matches!(known, Smaller::Rest(parameter, rest) if *parameter == place && name.name.value() == *rest))
                    .then_some(Step::Rest);
            }
            Expression::Binary(binary) => match binary.operator {
                BinaryOperator::Subtraction(_)
                    if literal(binary.rhs) == Some(1) && self.parameter(binary.lhs) == Some(place) =>
                {
                    true
                }
                BinaryOperator::Addition(_)
                    if literal(binary.rhs) == Some(1) && self.parameter(binary.lhs) == Some(place) =>
                {
                    false
                }
                BinaryOperator::Addition(_)
                    if literal(binary.lhs) == Some(1) && self.parameter(binary.rhs) == Some(place) =>
                {
                    false
                }
                _ => return None,
            },
            _ => return None,
        };

        self.smaller.iter().rev().find_map(|known| match *known {
            Smaller::Down(parameter, bound) if down && parameter == place && passes_on(bound) => {
                Some(Step::Down(bound))
            }
            Smaller::Up(parameter, bound) if !down && parameter == place && passes_on(bound) => Some(Step::Up(bound)),
            _ => None,
        })
    }

    /// Learns what `condition` tells of the law's int parameters where it is `holds`: a comparison of one with an int
    /// literal or another int parameter, under `!`, `&&` where it holds, and `||` where it fails.
    fn learn(&mut self, condition: &'ast Expression<'arena>, holds: bool) {
        match unparenthesized(condition) {
            Expression::UnaryPrefix(prefix) if matches!(prefix.operator, UnaryPrefixOperator::Not(_)) => {
                self.learn(prefix.operand, !holds);
            }
            Expression::Binary(binary) => match binary.operator {
                BinaryOperator::And(_) if holds => {
                    self.learn(binary.lhs, true);
                    self.learn(binary.rhs, true);
                }
                BinaryOperator::Or(_) if !holds => {
                    self.learn(binary.lhs, false);
                    self.learn(binary.rhs, false);
                }
                _ => {
                    let learned = match (self.int_parameter(binary.lhs), self.int_parameter(binary.rhs)) {
                        (Some(parameter), _) => self.bound(parameter, &binary.operator, binary.rhs, holds, false),
                        (None, Some(parameter)) => self.bound(parameter, &binary.operator, binary.lhs, holds, true),
                        (None, None) => None,
                    };
                    self.smaller.extend(learned);
                }
            },
            _ => {}
        }
    }

    /// What `parameter OPERATOR other`, or `other OPERATOR parameter` when `swapped`, shows where it is `holds`: that the
    /// int parameter at `parameter` is above or below `other`, an int literal or another int parameter. A literal bound
    /// takes `>=` and `<=` as one step past it, and a parameter only `>` and `<`, since a parameter may be the end of the
    /// range, where one step further leaves it.
    fn bound(
        &self,
        parameter: usize,
        operator: &BinaryOperator,
        other: &Expression<'arena>,
        holds: bool,
        swapped: bool,
    ) -> Option<Smaller<'arena>> {
        let (above, inclusive) = match (operator, swapped) {
            (BinaryOperator::GreaterThan(_), false) | (BinaryOperator::LessThan(_), true) => (true, false),
            (BinaryOperator::GreaterThanOrEqual(_), false) | (BinaryOperator::LessThanOrEqual(_), true) => (true, true),
            (BinaryOperator::LessThan(_), false) | (BinaryOperator::GreaterThan(_), true) => (false, false),
            (BinaryOperator::LessThanOrEqual(_), false) | (BinaryOperator::GreaterThanOrEqual(_), true) => {
                (false, true)
            }
            _ => return None,
        };
        // Where the comparison fails, the opposite one holds: not `>` is `<=`.
        let (above, inclusive) = if holds { (above, inclusive) } else { (!above, !inclusive) };
        let bound = match (self.int_parameter(other), literal(other)) {
            (Some(bound), _) if !inclusive && bound != parameter => Bound::Parameter(bound),
            (_, Some(value)) => Bound::Literal(match (inclusive, above) {
                (false, _) => value,
                (true, true) => value.checked_sub(1)?,
                (true, false) => value.checked_add(1)?,
            }),
            _ => return None,
        };

        Some(if above { Smaller::Down(parameter, bound) } else { Smaller::Up(parameter, bound) })
    }

    /// The place of the law's parameter `expression` names.
    fn parameter(&self, expression: &Expression<'arena>) -> Option<usize> {
        let Expression::ConstantAccess(name) = unparenthesized(expression) else {
            return None;
        };

        self.law.parameter_list.parameters.iter().position(|parameter| {
            parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name) == name.name.value()
        })
    }

    /// The place of the law's `int` parameter `expression` names.
    fn int_parameter(&self, expression: &Expression<'arena>) -> Option<usize> {
        self.parameter(expression).filter(|place| {
            matches!(self.law.parameter_list.parameters.as_slice()[*place].hint, Some(Hint::Integer(_)))
        })
    }

    fn refuse(&mut self, span: Span) {
        self.context.collector.report_with_code(
            IssueCode::InvalidLaw,
            Issue::error(BODY)
                .with_annotation(Annotation::primary(span).with_message("A law's body cannot hold this.")),
        );
    }
}

/// Whether `statement` ends the block it is in: a `return;`, or a block whose last statement does.
fn ends(statement: &Statement<'_>) -> bool {
    match statement {
        Statement::Return(_) => true,
        Statement::Block(block) => block.statements.last().is_some_and(ends),
        _ => false,
    }
}

/// The value of `expression` when it is an int literal, negated or not.
fn literal(expression: &Expression<'_>) -> Option<i64> {
    match unparenthesized(expression) {
        Expression::Literal(Literal::Integer(integer)) => integer.value.and_then(|value| i64::try_from(value).ok()),
        Expression::UnaryPrefix(prefix) if matches!(prefix.operator, UnaryPrefixOperator::Negation(_)) => {
            literal(prefix.operand)?.checked_neg()
        }
        _ => None,
    }
}

fn unparenthesized<'ast, 'arena>(expression: &'ast Expression<'arena>) -> &'ast Expression<'arena> {
    match expression {
        Expression::Parenthesized(parenthesized) => unparenthesized(parenthesized.expression),
        expression => expression,
    }
}
