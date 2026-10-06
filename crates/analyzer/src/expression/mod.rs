use indexmap::IndexMap;

use mago_algebra::clause::Clause;
use mago_algebra::find_satisfying_assignments;
use mago_algebra::saturate_clauses;
use mago_allocator::Arena;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::r#enum::TEnum;
use mago_codex::ttype::get_literal_string;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_named_object;
use mago_codex::ttype::get_never;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_span::HasPosition;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Call;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Node;
use mago_syntax::cst::Parenthesized;
use mago_syntax::cst::PatternMatch;
use mago_syntax::cst::PatternMatchArm;
use mago_syntax::cst::PropertiesPattern;
use mago_syntax::cst::Statement;
use mago_syntax::utils::pattern::PhpShape;
use mago_syntax::walker::Walker;
use mago_syntax_core::stack::ensure_sufficient_stack;
use mago_word::Word;
use mago_word::WordSet;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::context::scope::var_has_root;
use crate::error::AnalysisError;
use crate::expression::instantiation::analyze_anonymous_class_constructor;
use crate::formula::get_formula;
use crate::formula::negate_or_synthesize;
use crate::plugin::ExpressionHookResult;
use crate::plugin::context::HookContext;
use crate::reconciler::reconcile_keyed_types;
use crate::statement::attributes::analyze_class_like_attributes;
use crate::statement::class_like::analyze_class_like;
use crate::statement::class_like::override_attribute;
use crate::utils::misc::check_for_paradox;

pub mod access;
pub mod argument_list;
pub mod array;
pub mod array_access;
pub mod arrow_function;
pub mod assignment;
pub mod binary;
pub mod call;
pub mod clone;
pub mod closure;
pub mod composite_string;
pub mod conditional;
pub mod constant_access;
pub mod construct;
pub mod instantiation;
pub mod literal;
pub mod magic_constant;
pub mod r#match;
pub mod partial_application;
pub mod throw;
pub mod type_of;
pub mod unary;
pub mod variable;
pub mod r#yield;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Expression<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        ensure_sufficient_stack(|| {
            artifacts.record_variable_definedness(Node::Expression(self), block_context);

            if context.plugin_registry.has_expression_hooks() {
                let mut hook_context = HookContext::new(context, block_context, artifacts);
                let expression_hook_result = context.plugin_registry.before_expression(self, &mut hook_context)?;
                for reported in hook_context.take_issues() {
                    context.collector.report_with_code(reported.code, reported.issue);
                }

                match expression_hook_result {
                    ExpressionHookResult::Continue => {}
                    ExpressionHookResult::Skip => {
                        return Ok(());
                    }
                    ExpressionHookResult::SkipWithType(ty) => {
                        artifacts.set_expression_type(self, ty);
                        return Ok(());
                    }
                }
            }

            let result = match self {
                Expression::Parenthesized(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Literal(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Binary(expr) => expr.analyze(context, block_context, artifacts),
                Expression::UnaryPrefix(expr) => expr.analyze(context, block_context, artifacts),
                Expression::UnaryPostfix(expr) => expr.analyze(context, block_context, artifacts),
                Expression::CompositeString(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Assignment(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Conditional(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Array(expr) => expr.analyze(context, block_context, artifacts),
                Expression::LegacyArray(expr) => expr.analyze(context, block_context, artifacts),
                Expression::ArrayAccess(expr) => expr.analyze(context, block_context, artifacts).map(|()| {
                    if context.dialect.is_sharp() {
                        array_access::check_sharp_map_read(expr, context, block_context, artifacts);
                    }
                }),
                Expression::ArrayAppend(_) => {
                    context.collector.report_with_code(
                    IssueCode::ArrayAppendInReadContext,
                    Issue::error("Array append syntax `[]` cannot be used in a read context.")
                    .with_annotation(
                        Annotation::primary(self.span()).with_message("This syntax is for appending elements, not for reading a value.")
                    )
                    .with_note("The `[]` syntax after an array (e.g., `$array[]`) is used exclusively on the left-hand side of an assignment to append a new element (e.g., `$array[] = $value;`). It does not represent a readable value itself.")
                    .with_help("If you intended to access an array element, provide an index (e.g., `$array[0]`, `$array['key']`). If you intended to append, use this syntax on the left side of an assignment."),
                );

                    Ok(())
                }
                Expression::AnonymousClass(anonymous_class) => {
                    let Some(class_like_metadata) =
                        context.codebase.get_anonymous_class(context.source_file, self.span())
                    else {
                        return Ok(());
                    };

                    analyze_class_like_attributes(
                        context,
                        artifacts,
                        anonymous_class.attribute_lists.as_slice(),
                        class_like_metadata,
                    )?;

                    analyze_anonymous_class_constructor(
                        context,
                        block_context,
                        artifacts,
                        class_like_metadata,
                        anonymous_class.argument_list.as_ref(),
                        anonymous_class.span(),
                    )?;

                    analyze_class_like(
                        context,
                        artifacts,
                        None,
                        anonymous_class.span(),
                        anonymous_class.extends.as_ref(),
                        anonymous_class.implements.as_ref(),
                        None,
                        class_like_metadata,
                        anonymous_class.members.as_slice(),
                    )?;

                    override_attribute::check_override_attribute(
                        class_like_metadata,
                        anonymous_class.members.as_slice(),
                        context,
                    );

                    artifacts.set_expression_type(&self, get_named_object(class_like_metadata.name, None));

                    Ok(())
                }
                Expression::Closure(expr) => expr.analyze(context, block_context, artifacts),
                Expression::ArrowFunction(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Variable(expr) => expr.analyze(context, block_context, artifacts),
                Expression::ConstantAccess(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Match(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Yield(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Construct(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Throw(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Clone(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Error(_) | Expression::Access(_) | Expression::Call(_) if is_refused(self) => {
                    artifacts.set_expression_type(&self, get_never());

                    Ok(())
                }
                Expression::Call(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Access(expr) => expr.analyze(context, block_context, artifacts),
                Expression::PartialApplication(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Instantiation(expr) => expr.analyze(context, block_context, artifacts),
                Expression::MagicConstant(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Pipe(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Is(_) | Expression::As(_) | Expression::PatternMatch(_) => {
                    analyze_php_shape(Node::Expression(self), context, block_context, artifacts)
                }
                Expression::TypeOf(expr) => expr.analyze(context, block_context, artifacts),
                Expression::List(list_expr) => {
                    context.collector.report_with_code(
                    IssueCode::ListUsedInReadContext,
                    Issue::error("`list()` construct cannot be used as a value.")
                        .with_annotation(
                            Annotation::primary(list_expr.span())
                                .with_message("`list()` used here in a read context"),
                        )
                        .with_note(
                            "`list()` is a language construct for destructuring an array on the left side of an assignment."
                        )
                        .with_help(
                            "It is not a function and does not return a value. To create an array, use `[]` or `array()`."
                        ),
                );

                    artifacts.set_expression_type(&list_expr, get_never());

                    Ok(())
                }
                Expression::Self_(keyword) | Expression::Static(keyword) | Expression::Parent(keyword) => {
                    let keyword_str = mago_bytes::BytesDisplay(keyword.value);

                    context.collector.report_with_code(
                    IssueCode::InvalidScopeKeywordContext,
                    Issue::error(format!("The `{keyword_str}` keyword cannot be used as a standalone value."))
                        .with_annotation(
                            Annotation::primary(keyword.span)
                                .with_message(format!("`{keyword_str}` used as a value here")),
                        )
                        .with_note(
                            format!("The `{keyword_str}` keyword is used to refer to a class scope and must be used with the `::` operator.")
                        )
                        .with_help(
                            format!("Use `{keyword_str}::CONSTANT`, `{keyword_str}::method()`, or `new {keyword_str}()` instead.")
                        ),
                );

                    artifacts.set_expression_type(&self, get_never());

                    Ok(())
                }
                Expression::Identifier(identifier) => {
                    if !identifier.is_local() {
                        unreachable!(
                            "Parser should not produce a bare `Identifier` as a standalone expression in this context. \nIf you see this, it indicates a bug in the parser or the analysis logic. \nPlease report this issue with the following identifier: `{}` line `{}`, column `{}`.",
                            mago_bytes::BytesDisplay(&context.source_file.name),
                            context.source_file.line_number(self.offset()),
                            context.source_file.column_number(self.offset()),
                        );
                    }

                    artifacts.set_expression_type(&self, get_literal_string(word(identifier.value())));

                    Ok(())
                }
                #[allow(clippy::unreachable)]
                _ => unreachable!("An expression variant was not handled in analyzer: {self:?}"),
            };

            result?;

            if context.plugin_registry.has_expression_hooks() {
                let mut hook_context = HookContext::new(context, block_context, artifacts);
                context.plugin_registry.after_expression(self, &mut hook_context)?;
                for reported in hook_context.take_issues() {
                    context.collector.report_with_code(reported.code, reported.issue);
                }
            }

            if context.check_throws() && context.plugin_registry.has_expression_throw_providers() {
                let exceptions = context.plugin_registry.get_expression_thrown_exceptions(
                    context.codebase,
                    context.source_file,
                    block_context,
                    artifacts,
                    self,
                );

                for exception in exceptions {
                    block_context.possibly_thrown_exceptions.entry(exception).or_default().insert(self.span());
                }
            }

            artifacts.record_static_local_types(block_context, context.codebase, context.settings.combiner_options());

            Ok(())
        })
    }
}

/// Analyzes a PHP# `is`, `as` or `match` as the PHP it runs as, which the engine's bridge lowers too, so the analyzer
/// narrows on the code that runs. An expression's type is its PHP's. A form the slice refuses has no PHP, and its
/// error is the semantic check's.
pub(crate) fn analyze_php_shape<'ctx, 'arena, A>(
    node: Node<'_, 'arena>,
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let Some(PhpShape { php, temporaries, tests }) =
        context.get_assertion_context_from_block(block_context).php_shape(node)
    else {
        if let Node::Expression(expression) = node {
            artifacts.set_expression_type(expression, get_mixed());
        }

        return Ok(());
    };

    context.temporaries += temporaries;
    let (result, issues) = context.record(|context| match php {
        Node::Expression(expression) => expression.analyze(context, block_context, artifacts),
        Node::Statement(statement) => statement.analyze(context, block_context, artifacts),
        _ => Ok(()),
    });
    context.temporaries -= temporaries;

    // A properties pattern's `is_object` is its null check, which a value that cannot be null makes redundant. C#
    // reports nothing there, and no source the user wrote can drop the check.
    let mut object_checks = Vec::new();
    match node {
        Node::Expression(expression) => ObjectChecks.walk_expression(expression, &mut object_checks),
        Node::Statement(statement) => ObjectChecks.walk_statement(statement, &mut object_checks),
        _ => {}
    }
    // The last arm of a `match` without `default` takes every value the arms before it leave, as `default` does, so its
    // test is always true and not redundant. An arm before it that is always true leaves nothing for the arms after it.
    let last_arm_test: Option<Span> = match node {
        Node::Expression(Expression::PatternMatch(pattern_match))
        | Node::Statement(Statement::PatternMatch(pattern_match))
            if !pattern_match.arms.iter().any(PatternMatchArm::is_default) =>
        {
            pattern_match.arms.last().and_then(|arm| match arm {
                PatternMatchArm::Pattern(arm) => {
                    tests.iter().find(|(pattern, _)| *pattern == arm.pattern.span()).map(|(_, test)| test.span())
                }
                PatternMatchArm::Default(_) => None,
            })
        }
        _ => None,
    };
    let is_code = |issue: &Issue, code: IssueCode| issue.code.as_deref() == Some(code.as_str());
    let issues: Vec<Issue> = issues
        .into_iter()
        .filter(|issue| {
            let span = issue.primary_span();
            let redundant = is_code(issue, IssueCode::RedundantTypeComparison)
                || is_code(issue, IssueCode::RedundantLogicalOperation);
            let always_true = redundant
                || is_code(issue, IssueCode::RedundantComparison)
                || is_code(issue, IssueCode::RedundantCondition);

            // PHP's report on `match (true)` names `true`, so a PHP# `match` reports what it misses itself.
            !is_code(issue, IssueCode::MatchNotExhaustive)
                && (!redundant || span.is_none_or(|span| !object_checks.contains(&span.start.offset)))
                && (!always_true
                    || span.is_none_or(|span| !last_arm_test.is_some_and(|test| test.contains(&span.start))))
        })
        .collect();

    if let Node::Expression(Expression::PatternMatch(pattern_match))
    | Node::Statement(Statement::PatternMatch(pattern_match)) = node
        && !pattern_match.arms.iter().any(PatternMatchArm::is_default)
    {
        // A `when` condition may be false for any value, so only the tests of arms without one handle a case.
        let handled: Vec<&Expression<'_>> = pattern_match
            .arms
            .iter()
            .filter_map(|arm| match arm {
                PatternMatchArm::Pattern(arm) if arm.guard.is_none() => {
                    tests.iter().find(|(pattern, _)| *pattern == arm.pattern.span()).map(|(_, test)| *test)
                }
                _ => None,
            })
            .collect();

        report_unhandled(pattern_match, &handled, context, block_context, artifacts);
    }

    // Reading (g) of spec section 21: a pattern that can never match is an error, as C#'s CS8121 is. Its test is then
    // `false`, unless an error at the pattern already says so.
    for (pattern, test) in tests {
        let reported = issues.iter().any(|issue| {
            issue.level == Level::Error
                && is_code(issue, IssueCode::ImpossibleTypeComparison)
                && issue.primary_span().is_some_and(|span| pattern.contains(&span.start))
        });
        if reported || !artifacts.get_rc_expression_type(test).is_some_and(|test_type| test_type.is_false()) {
            continue;
        }

        context.collector.report_with_code(
            IssueCode::ImpossibleTypeComparison,
            Issue::error("This pattern never matches the value it tests.")
                .with_annotation(Annotation::primary(pattern).with_message("Never matches."))
                .with_note("Spec section 21 makes a pattern that can never match an error, as C# does (CS8121).")
                .with_help("Remove the pattern, or test a value that can match it."),
        );
    }
    context.collector.extend(issues);
    result?;

    if let (Node::Expression(expression), Node::Expression(php)) = (node, php)
        && let Some(php_type) = artifacts.get_rc_expression_type(php).cloned()
    {
        artifacts.set_rc_expression_type(expression, php_type);
    }

    Ok(())
}

/// Reports what a PHP# `match` without `default` leaves unhandled. Spec section 21 gives `default` to every `match` on
/// a value that is not an enum, and section 20 makes a `match` on an enum handle each of its cases. The value left is
/// the value once every `handled` test is false, which Mago's reconciler narrows as it narrows the arms of PHP's
/// `match`.
fn report_unhandled<'ctx, 'arena, A>(
    pattern_match: &PatternMatch<'arena>,
    handled: &[&Expression<'arena>],
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &AnalysisArtifacts,
) where
    A: Arena,
{
    // Each arm reads a local in its own test, never where `match` writes it, so a local's type is the variable's, and
    // any other value's is its own, as `MatchAnalyzer` reads a subject.
    let value_type = context
        .get_assertion_context_from_block(block_context)
        .get_expression_id(pattern_match.expression)
        .and_then(|id| block_context.locals.get(&id).map(|value| (**value).clone()))
        .or_else(|| artifacts.get_expression_type(pattern_match.expression).cloned())
        .unwrap_or_else(get_mixed);
    let is_enum = value_type
        .types
        .iter()
        .all(|atomic| matches!(atomic, TAtomic::Null) || enum_value(atomic, context.codebase).is_some());

    let missing = if is_enum {
        missing_cases(pattern_match.span(), &value_type, handled, context, block_context, artifacts)
    } else {
        vec![]
    };
    let message = match missing.as_slice() {
        _ if !is_enum => "A `match` needs a `default` arm.".to_owned(),
        [] => return,
        [only] => format!("This `match` misses `{only}`."),
        [rest @ .., last] => {
            let rest: Vec<String> = rest.iter().map(|case| format!("`{case}`")).collect();

            format!("This `match` misses {} and `{last}`.", rest.join(", "))
        }
    };

    context.collector.report_with_code(
        IssueCode::MatchNotExhaustive,
        Issue::error(message)
            .with_annotation(
                Annotation::primary(pattern_match.r#match.span).with_message("This `match` has no `default`."),
            )
            .with_note(
                "Spec section 21: only a `match` on an enum may leave out `default`, when its arms cover every case.",
            )
            .with_help("Add an arm for each value it misses, or add `default => …` as the last arm."),
    );
}

/// The cases of the enum `value_type` that no `handled` test of the `match` at `span` matches, as PHP# writes them,
/// `Status.Open`, in the order the enum declares them, and `null` last.
fn missing_cases<'ctx, 'arena, A>(
    span: Span,
    value_type: &TUnion,
    handled: &[&Expression<'arena>],
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &AnalysisArtifacts,
) -> Vec<String>
where
    A: Arena,
{
    let thresholds = context.settings.algebra_thresholds();
    let size_threshold = context.settings.formula_size_threshold;
    let assertion_context = context.get_assertion_context_from_block(block_context);
    let mut clauses = Vec::new();
    for test in handled {
        let formula =
            get_formula(test.span(), test.span(), test, assertion_context, artifacts, &thresholds, size_threshold)
                .unwrap_or_default();
        clauses.extend(negate_or_synthesize(formula, test, assertion_context, artifacts, &thresholds, size_threshold));
    }
    let (assertions, _) =
        find_satisfying_assignments(&saturate_clauses(clauses.iter(), &thresholds), None, &mut WordSet::default());

    let left = if assertions.is_empty() {
        vec![value_type.clone()]
    } else {
        let mut unhandled_context = block_context.clone();
        reconcile_keyed_types(
            context,
            &assertions,
            IndexMap::new(),
            &mut unhandled_context,
            &mut WordSet::default(),
            &WordSet::default(),
            &span,
            false,
            false,
        );

        assertions.keys().filter_map(|id| unhandled_context.locals.get(id)).map(|left| (**left).clone()).collect()
    };

    let atomics = || left.iter().flat_map(|left| left.types.iter());
    let cases: Vec<(Word, Option<Word>)> =
        atomics().filter_map(|atomic| enum_value(atomic, context.codebase)).collect();

    let mut missing = Vec::new();
    let mut enums: Vec<Word> = Vec::new();
    for (name, _) in &cases {
        if !enums.contains(name) {
            enums.push(*name);
        }
    }
    for name in enums {
        let Some(metadata) = context.codebase.get_enum(name.as_bytes()) else {
            continue;
        };
        let short_name = metadata.original_name.as_bytes().rsplit(|byte| *byte == b'\\').next().unwrap_or_default();
        let mut declared: Vec<_> = metadata.enum_cases.values().collect();
        declared.sort_by_key(|case| case.span.start.offset);
        for case in declared {
            if cases.iter().any(|(enum_name, left)| *enum_name == name && left.is_none_or(|left| left == case.name)) {
                missing.push(format!("{}.{}", String::from_utf8_lossy(short_name), case.name));
            }
        }
    }
    if atomics().any(|atomic| matches!(atomic, TAtomic::Null)) {
        missing.push("null".to_owned());
    }

    missing
}

/// The enum and the case a type holds: one case, or every case of the enum when the case is `None`. `None` for a type
/// that is not an enum.
fn enum_value(atomic: &TAtomic, codebase: &CodebaseMetadata) -> Option<(Word, Option<Word>)> {
    match atomic {
        TAtomic::Object(TObject::Enum(TEnum { name, case })) => Some((*name, *case)),
        TAtomic::Object(TObject::Named(named)) if codebase.get_enum(named.name.as_bytes()).is_some() => {
            Some((named.name, None))
        }
        _ => None,
    }
}

/// Collects the offset of each properties pattern's `{`, where its PHP's `is_object` check starts.
struct ObjectChecks;

impl<'ast, 'arena> Walker<'ast, 'arena, Vec<u32>> for ObjectChecks {
    fn walk_in_properties_pattern(&self, properties: &'ast PropertiesPattern<'arena>, offsets: &mut Vec<u32>) {
        offsets.push(properties.left_brace.start.offset);
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Parenthesized<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        self.expression.analyze(context, block_context, artifacts)?;
        if let Some(u) = artifacts.get_expression_type(&self.expression) {
            artifacts.set_expression_type(&self, u.clone());
        }

        Ok(())
    }
}

/// Whether an error already refuses `expression`: it failed to parse, or it reads or calls a member of `typeof(X)`
/// through any chain of property reads, which `check_slice` refuses. Its type is `never`, and it adds no issue.
pub(crate) fn is_refused(expression: &Expression<'_>) -> bool {
    let mut object = match expression {
        Expression::Error(_) => return true,
        Expression::Access(Access::Property(access)) => access.object,
        Expression::Call(Call::Method(call)) => call.object,
        _ => return false,
    };

    while let Expression::Access(Access::Property(access)) = object {
        object = access.object;
    }

    matches!(object, Expression::TypeOf(_))
}

pub fn find_expression_logic_issues<'ctx, 'arena, A>(
    expression: &Expression<'arena>,
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &AnalysisArtifacts,
) where
    A: Arena,
{
    let mut if_block_context = block_context.clone();
    let mut cond_referenced_var_ids = if_block_context.conditionally_referenced_variable_ids.clone();

    let Some(mut expression_clauses) = get_formula(
        expression.span(),
        expression.span(),
        expression,
        context.get_assertion_context_from_block(block_context),
        artifacts,
        &context.settings.algebra_thresholds(),
        context.settings.formula_size_threshold,
    ) else {
        context.collector.report_with_code(
           IssueCode::ExpressionIsTooComplex,
           Issue::warning("Expression is too complex for complete logical analysis.")
               .with_annotation(
                   Annotation::primary(expression.span())
                       .with_message("This expression is too complex for the analyzer to fully understand its logical implications"),
               )
               .with_note(
                   "To prevent performance issues, the analyzer limits how many logical paths it explores for a single expression."
               )
               .with_note(
                   "As a result, some logical paradoxes or redundant checks within this expression may not be detected."
               )
               .with_help(
                   "Consider refactoring this expression into smaller, intermediate variables to improve analysis and readability.",
               ),
        );

        return;
    };

    let mut mixed_var_ids = Vec::new();
    for (var_id, var_type) in &block_context.locals {
        if var_type.is_mixed() && block_context.locals.contains_key(var_id) {
            mixed_var_ids.push(*var_id);
        }
    }

    expression_clauses = expression_clauses
        .into_iter()
        .map(|c| {
            let keys: WordSet = c.possibilities.keys().copied().collect();

            mixed_var_ids.retain(|i| !keys.contains(i));

            for key in keys {
                for mixed_var_id in &mixed_var_ids {
                    if var_has_root(key, *mixed_var_id) {
                        return Clause::new(
                            IndexMap::default(),
                            expression.span(),
                            expression.span(),
                            Some(true),
                            None,
                            None,
                        );
                    }
                }
            }

            c
        })
        .collect::<Vec<Clause>>();

    let expression_span = expression.span();

    // this will see whether any of the clauses in set A conflict with the clauses in set B
    check_for_paradox(
        &mut context.collector,
        &block_context.clauses,
        &expression_clauses,
        expression_span,
        &context.settings.algebra_thresholds(),
    );

    expression_clauses.extend(block_context.clauses.iter().map(|v| (**v).clone()));

    let (reconcilable_if_types, active_if_types) = find_satisfying_assignments(
        expression_clauses.iter().as_slice(),
        Some(expression.span()),
        &mut cond_referenced_var_ids,
    );

    reconcile_keyed_types(
        context,
        &reconcilable_if_types,
        active_if_types,
        &mut if_block_context,
        &mut mago_word::WordSet::default(),
        &cond_referenced_var_ids,
        &expression_span,
        true,
        false,
    );
}
