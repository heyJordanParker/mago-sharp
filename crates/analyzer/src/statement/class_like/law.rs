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
use mago_syntax::cst::Argument;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
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
            let mut walk = BodyWalk { context, law: self, class: class_like_metadata, smaller: Vec::new() };
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

/// The law `call` names, and the class that states it: a bare name names a law of `class`, the class the call is in,
/// and `Class.name(…)` a law of `Class`. `None` when the call names no law, as a call of a local's function does.
pub(crate) fn called_law<'ctx, A>(
    context: &Context<'ctx, '_, A>,
    class: Option<&'ctx ClassLikeMetadata>,
    call: &Call<'_>,
) -> Option<(&'ctx ClassLikeMetadata, &'ctx FunctionLikeMetadata)>
where
    A: Arena,
{
    let (class, name) = match call {
        Call::Function(call) => {
            let Expression::Identifier(name) = call.function else {
                return None;
            };
            if matches!(context.resolved_names.binding(name), Some(Binding::Local(_) | Binding::This)) {
                return None;
            }

            (class?, name.value())
        }
        Call::Method(call) => {
            let class = context.resolved_names.static_call_class(call)?;
            let ClassLikeMemberSelector::Identifier(name) = &call.method else {
                return None;
            };

            (context.codebase.get_class_like(context.resolved_names.get(&class.name))?, name.value)
        }
        _ => return None,
    };

    Some((class, class.laws.get(&ascii_lowercase_word(name))?))
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
    let argument_list = match call {
        Call::Function(call) => &call.argument_list,
        Call::Method(call) => &call.argument_list,
        _ => return Ok(()),
    };

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
    /// The int parameter at this place is above a bound the body tested, so one less is nearer the bound.
    Above(usize),
    /// The int parameter at this place is below a bound the body tested, so one more is nearer the bound.
    Below(usize),
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
}

impl<'ctx, 'ast, 'arena, A> BodyWalk<'_, 'ctx, 'ast, 'arena, A>
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
                self.learn(r#if.condition, true);
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
                self.learn(r#if.condition, false);
                self.statement(body.statement);
                self.smaller.truncate(known);
                if let Some(clause) = &body.else_clause {
                    self.learn(r#if.condition, true);
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
            _ => match law_call(statement)
                .and_then(|call| Some((call, called_law(self.context, Some(self.class), call)?)))
            {
                Some((call, (class, law))) => {
                    self.call(call, class.name == self.class.name && law.span == self.law.span())
                }
                None => self.refuse(statement.span()),
            },
        }
    }

    /// Checks a call statement of a law: a call of this law itself, when `is_self`, passes a smaller value at one place.
    fn call(&mut self, call: &'ast Call<'arena>, is_self: bool) {
        let arguments = match call {
            Call::Function(function) => &function.argument_list,
            Call::Method(method) => &method.argument_list,
            _ => return,
        };
        if !is_self {
            return;
        }

        let smaller = arguments.arguments.iter().enumerate().any(|(place, argument)| {
            let Argument::Positional(argument) = argument else {
                return false;
            };
            self.smaller.iter().any(|known| match (known, argument.value) {
                (Smaller::Rest(parameter, rest), Expression::ConstantAccess(value)) => {
                    *parameter == place && value.name.value() == *rest
                }
                (Smaller::Above(parameter), Expression::Binary(binary))
                    if matches!(binary.operator, BinaryOperator::Subtraction(_)) =>
                {
                    *parameter == place && self.names_parameter(binary.lhs, place) && is_one(binary.rhs)
                }
                (Smaller::Below(parameter), Expression::Binary(binary))
                    if matches!(binary.operator, BinaryOperator::Addition(_)) =>
                {
                    *parameter == place && self.names_parameter(binary.lhs, place) && is_one(binary.rhs)
                }
                _ => false,
            })
        });
        if smaller {
            return;
        }

        let name = String::from_utf8_lossy(self.law.name.value);
        self.context.collector.report_with_code(
            IssueCode::LawRecursion,
            Issue::error("a law calls itself only on a smaller value")
                .with_annotation(Annotation::primary(call.span()).with_message("This call passes no smaller value."))
                .with_help(format!(
                    "Call {name} on a list's rest from a pattern the body matched, or on an int one step towards a bound the body tested, as `if (n <= 0) {{ return; }}` tests 0 before `{name}(n - 1);`."
                )),
        );
    }

    /// Learns what `condition`, or its negation when `negated`, tells of the law's int parameters.
    fn learn(&mut self, condition: &'ast Expression<'arena>, negated: bool) {
        match condition {
            Expression::Parenthesized(parenthesized) => self.learn(parenthesized.expression, negated),
            Expression::UnaryPrefix(prefix) if matches!(prefix.operator, UnaryPrefixOperator::Not(_)) => {
                self.learn(prefix.operand, !negated);
            }
            Expression::Binary(binary) => match binary.operator {
                BinaryOperator::And(_) if !negated => {
                    self.learn(binary.lhs, false);
                    self.learn(binary.rhs, false);
                }
                BinaryOperator::Or(_) if negated => {
                    self.learn(binary.lhs, true);
                    self.learn(binary.rhs, true);
                }
                BinaryOperator::LessThan(_)
                | BinaryOperator::LessThanOrEqual(_)
                | BinaryOperator::GreaterThan(_)
                | BinaryOperator::GreaterThanOrEqual(_) => {
                    // `n < b` and `n <= b` hold `n` below `b`, strictly for `<`, and a negation flips both.
                    let below =
                        matches!(binary.operator, BinaryOperator::LessThan(_) | BinaryOperator::LessThanOrEqual(_));
                    let strict =
                        matches!(binary.operator, BinaryOperator::LessThan(_) | BinaryOperator::GreaterThan(_));
                    if strict == negated {
                        return;
                    }
                    let (left, right) = (self.parameter(binary.lhs), self.parameter(binary.rhs));
                    let held = [
                        left.filter(|_| right.is_some() || is_bound(binary.rhs)).map(|parameter| (parameter, below)),
                        right.filter(|_| left.is_some() || is_bound(binary.lhs)).map(|parameter| (parameter, !below)),
                    ];
                    for (parameter, below) in held.into_iter().flatten() {
                        self.smaller.push(if below != negated {
                            Smaller::Below(parameter)
                        } else {
                            Smaller::Above(parameter)
                        });
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// The place of the law's parameter `expression` names.
    fn parameter(&self, expression: &Expression<'arena>) -> Option<usize> {
        let Expression::ConstantAccess(name) = expression else {
            return None;
        };

        self.law.parameter_list.parameters.iter().position(|parameter| {
            parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name) == name.name.value()
        })
    }

    fn names_parameter(&self, expression: &Expression<'arena>, place: usize) -> bool {
        self.parameter(expression) == Some(place)
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

fn is_one(expression: &Expression<'_>) -> bool {
    matches!(expression, Expression::Literal(Literal::Integer(integer)) if integer.value == Some(1))
}

/// Whether `expression` is a bound an int parameter steps towards besides another parameter: an int literal.
fn is_bound(expression: &Expression<'_>) -> bool {
    match expression {
        Expression::Literal(Literal::Integer(_)) => true,
        Expression::UnaryPrefix(prefix) => {
            matches!(prefix.operator, UnaryPrefixOperator::Negation(_)) && is_bound(prefix.operand)
        }
        _ => false,
    }
}
