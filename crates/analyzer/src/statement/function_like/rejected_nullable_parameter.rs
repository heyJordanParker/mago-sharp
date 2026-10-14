use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::union::TUnion;
use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_names::binding::Local;
use mago_names::binding::LocalKind;
use mago_names::binding::php_variable_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::If;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Statement;
use mago_syntax::walker::Walker;
use mago_text_edit::Safety;
use mago_text_edit::TextEdit;

use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::statement::function_like::FunctionLikeBody;
use crate::utils::misc::unwrap_expression;

/// Reports each nullable parameter of a PHP# method that the method rejects on every path before any other use, as
/// in `customer ?? throw …` or `if (customer === null) { throw …; }`. Spec section 14.4 drops the `?` from such a
/// parameter, so the caller checks for null where the value enters.
///
/// A parameter is rejected when every read of it that may still be null is such a check, and it is no longer null
/// where the body ends. `block_context` is the method's context after its body, and `artifacts` holds the types the
/// body's expressions were analyzed with.
pub fn check_rejected_nullable_parameters<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    method: &'ctx FunctionLikeMetadata,
    parameters: &[FunctionLikeParameter<'arena>],
    body: FunctionLikeBody<'_, 'arena>,
    block_context: &BlockContext<'ctx>,
    artifacts: &AnalysisArtifacts,
) where
    A: Arena,
{
    for (parameter, metadata) in parameters.iter().zip(&method.parameters) {
        if parameter.is_promoted_property()
            || parameter.ampersand.is_some()
            || !metadata.get_type_metadata().is_some_and(|type_metadata| writes_null(&type_metadata.type_union))
        {
            continue;
        }

        let mut uses = ParameterUses::new(
            Local { declaration: parameter.variable.span, kind: LocalKind::Parameter },
            context.resolved_names,
        );
        match body {
            FunctionLikeBody::Statements(statements, _) => {
                for statement in statements {
                    ParameterWalker.walk_statement(statement, &mut uses);
                }
            }
            FunctionLikeBody::Expression(expression) | FunctionLikeBody::ExpressionStatement(expression) => {
                ParameterWalker.walk_expression(expression, &mut uses);
            }
        }

        let is_rejected = !uses.assigned
            && !uses.rejections.is_empty()
            && uses.reads.iter().all(|read| {
                uses.rejections.contains(read)
                    || artifacts.get_expression_type(read).is_some_and(|read_type| !read_type.can_be_null())
            })
            && block_context
                .locals
                .get(&php_variable_name(parameter.variable.name))
                .is_some_and(|final_type| !final_type.can_be_null());

        if is_rejected {
            report_rejected_nullable_parameter(context, method, parameter, &uses.rejections);
        }
    }
}

/// Whether `written` lets null in through a type of its own, as `string?` or `Any?` do. A type parameter does not: an
/// unbounded `TItem` holds null only when its type argument does, and the caller picks that type argument.
pub(crate) fn writes_null(written: &TUnion) -> bool {
    written.has_null() || written.has_nullable_mixed() || written.is_void()
}

fn report_rejected_nullable_parameter<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    method: &FunctionLikeMetadata,
    parameter: &FunctionLikeParameter<'arena>,
    rejections: &[Span],
) where
    A: Arena,
{
    let name = BytesDisplay(parameter.variable.name);
    let hint_span = parameter.hint.as_ref().map_or(parameter.variable.span, HasSpan::span);
    let non_null_hint = context
        .source_file
        .contents
        .get(hint_span.start_offset() as usize..hint_span.end_offset() as usize)
        .and_then(|hint| std::str::from_utf8(hint).ok())
        .and_then(|hint| hint.strip_suffix('?'))
        .map(str::to_owned);

    let issue =
        Issue::help(format!("Parameter `{name}` is nullable, but `{}` rejects null on every path.", method.name))
            .with_annotation(Annotation::primary(parameter.span()).with_message(format!("`{name}` may be null here")))
            .with_annotations(
                rejections
                    .iter()
                    .map(|rejection| Annotation::secondary(*rejection).with_message("...but null is rejected here")),
            )
            .with_note("A caller that passes null always fails, so the type should not allow it.")
            .with_help(match &non_null_hint {
                Some(non_null_hint) => {
                    format!("Declare `{name}` as `{non_null_hint}`, and check for null where the value enters.")
                }
                None => format!("Declare `{name}` without null, and check for null where the value enters."),
            });

    let issue = context.as_null_check_error(issue);

    context.collector.propose_with_code(IssueCode::RejectedNullableParameter, issue, |edits| {
        if let Some(non_null_hint) = non_null_hint {
            edits.push(TextEdit::replace(hint_span.to_range(), non_null_hint).with_safety(Safety::PotentiallyUnsafe));
        }
    });
}

/// The reads of one parameter in a method body, and the reads that reject null.
struct ParameterUses<'names, 'arena> {
    local: Local,
    resolved_names: &'names ResolvedNames<'arena>,
    reads: Vec<Span>,
    rejections: Vec<Span>,
    assigned: bool,
}

impl<'names, 'arena> ParameterUses<'names, 'arena> {
    fn new(local: Local, resolved_names: &'names ResolvedNames<'arena>) -> Self {
        Self { local, resolved_names, reads: Vec::new(), rejections: Vec::new(), assigned: false }
    }

    /// The span of `expression` when it is this parameter.
    fn parameter_span(&self, expression: &Expression<'arena>) -> Option<Span> {
        match unwrap_expression(expression) {
            Expression::ConstantAccess(access) if self.is_parameter(access) => Some(access.span()),
            _ => None,
        }
    }

    fn is_parameter(&self, access: &ConstantAccess<'arena>) -> bool {
        self.resolved_names.binding(&access.name) == Some(Binding::Local(self.local))
    }

    /// The span of the parameter that `condition` tests for null, as in `customer === null` or `null == customer`.
    fn null_test_span(&self, condition: &Expression<'arena>) -> Option<Span> {
        let Expression::Binary(Binary { lhs, operator: BinaryOperator::Identical(_) | BinaryOperator::Equal(_), rhs }) =
            unwrap_expression(condition)
        else {
            return None;
        };

        if is_null(rhs) {
            self.parameter_span(lhs)
        } else if is_null(lhs) {
            self.parameter_span(rhs)
        } else {
            None
        }
    }
}

fn is_null(expression: &Expression<'_>) -> bool {
    matches!(unwrap_expression(expression), Expression::Literal(Literal::Null(_)))
}

/// Whether `statement` ends by throwing, as `throw …;` or a block whose last statement throws.
fn always_throws(statement: &Statement<'_>) -> bool {
    match statement {
        Statement::Expression(expression_statement) => {
            matches!(unwrap_expression(expression_statement.expression), Expression::Throw(_))
        }
        Statement::Block(block) => block.statements.last().is_some_and(always_throws),
        _ => false,
    }
}

struct ParameterWalker;

impl<'ast, 'names, 'arena> Walker<'ast, 'arena, ParameterUses<'names, 'arena>> for ParameterWalker {
    fn walk_in_constant_access(&self, access: &'ast ConstantAccess<'arena>, uses: &mut ParameterUses<'names, 'arena>) {
        if uses.is_parameter(access) {
            uses.reads.push(access.span());
        }
    }

    fn walk_in_assignment(&self, assignment: &'ast Assignment<'arena>, uses: &mut ParameterUses<'names, 'arena>) {
        if uses.parameter_span(assignment.lhs).is_some() {
            uses.assigned = true;
        }
    }

    fn walk_in_binary(&self, binary: &'ast Binary<'arena>, uses: &mut ParameterUses<'names, 'arena>) {
        if binary.operator.is_null_coalesce()
            && matches!(unwrap_expression(binary.rhs), Expression::Throw(_))
            && let Some(span) = uses.parameter_span(binary.lhs)
        {
            uses.rejections.push(span);
        }
    }

    fn walk_in_if(&self, r#if: &'ast If<'arena>, uses: &mut ParameterUses<'names, 'arena>) {
        if let IfBody::Statement(body) = &r#if.body
            && body.else_if_clauses.is_empty()
            && body.else_clause.is_none()
            && always_throws(body.statement)
            && let Some(span) = uses.null_test_span(r#if.condition)
        {
            uses.rejections.push(span);
        }
    }
}
