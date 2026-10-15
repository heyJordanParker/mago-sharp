use std::rc::Rc;

use indexmap::IndexMap;

use mago_algebra::clause::Clause;
use mago_algebra::find_satisfying_assignments;
use mago_algebra::saturate_clauses;
use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::r#enum::TEnum;
use mago_codex::ttype::get_literal_string;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_named_object;
use mago_codex::ttype::get_never;
use mago_codex::ttype::union::TUnion;
use mago_codex::ttype::wrap_atomic;
use mago_names::binding::php_variable_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_span::HasPosition;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::As;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Leave;
use mago_syntax::cst::ListPattern;
use mago_syntax::cst::ListPatternElement;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Node;
use mago_syntax::cst::Parenthesized;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::PatternMatch;
use mago_syntax::cst::PatternMatchArm;
use mago_syntax::cst::PropertiesPattern;
use mago_syntax::cst::RestPattern;
use mago_syntax::cst::Statement;
use mago_syntax::cst::TypePattern;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::utils::pattern::PatternNames;
use mago_syntax::utils::pattern::PhpShape;
use mago_syntax::utils::pattern::called_functions;
use mago_syntax::utils::pattern::has_type_test;
use mago_syntax::walker::Walker;
use mago_syntax_core::stack::ensure_sufficient_stack;
use mago_word::Word;
use mago_word::WordSet;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::assertion::CodebasePatternNames;
use crate::context::block::BlockContext;
use crate::context::scope::var_has_root;
use crate::error::AnalysisError;
use crate::expression::instantiation::analyze_anonymous_class_constructor;
use crate::external::Listing;
use crate::formula::get_formula;
use crate::formula::negate_or_synthesize;
use crate::plugin::ExpressionHookResult;
use crate::plugin::context::HookContext;
use crate::reconciler::reconcile_keyed_types;
use crate::statement::attributes::analyze_class_like_attributes;
use crate::statement::class_like::analyze_class_like;
use crate::statement::class_like::override_attribute;
use crate::statement::get_type_from_hint;
use crate::statement::report_erased_type_test;
use crate::statement::report_hint_type_arguments;
use crate::utils::misc::check_for_paradox;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_code_member;
use crate::utils::names::display_member;
use crate::utils::names::display_missing_imports;
use crate::utils::names::display_type;
use crate::utils::quoted_source;

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
pub mod with;
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
                // Spec section 14.4: `a ?? return`, `a ?? continue` and `a ?? break` run their statement, which leaves,
                // so the expression has no value.
                Expression::Leave(leave) => {
                    match leave {
                        Leave::Return(r#return) => r#return.analyze(context, block_context, artifacts)?,
                        Leave::Continue(r#continue) => r#continue.analyze(context, block_context, artifacts)?,
                        Leave::Break(r#break) => r#break.analyze(context, block_context, artifacts)?,
                    }
                    artifacts.set_expression_type(&self, get_never());

                    Ok(())
                }
                Expression::Clone(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Error(_)
                | Expression::Access(_)
                | Expression::Call(_)
                | Expression::TypeOf(_)
                | Expression::Instantiation(_)
                | Expression::Is(_)
                | Expression::As(_)
                | Expression::PatternMatch(_)
                    if is_refused(self, context) =>
                {
                    if let Expression::Is(is) = self {
                        for (hint, variable) in Node::Pattern(is.pattern).filter_map(|node| match node {
                            Node::TypePattern(TypePattern { hint, variable: Some(variable), .. }) => {
                                Some((hint, variable))
                            }
                            _ => None,
                        }) {
                            let variable_id = php_variable_name(variable.value);
                            let variable_type = Rc::new(get_type_from_hint(context, block_context, artifacts, hint));
                            block_context.local_types.insert(variable_id, (Rc::clone(&variable_type), hint.span()));
                            block_context.locals.insert(variable_id, variable_type);
                        }
                    }
                    let refused_type =
                        if context.dialect.is_sharp() { TUnion::from_atomic(TAtomic::Error) } else { get_never() };
                    artifacts.set_expression_type(&self, refused_type);

                    Ok(())
                }
                Expression::Call(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Access(expr) => expr.analyze(context, block_context, artifacts),
                Expression::PartialApplication(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Instantiation(expr) => expr.analyze(context, block_context, artifacts),
                Expression::MagicConstant(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Pipe(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Is(is) => analyze_php_shape(
                    Node::Expression(self),
                    type_pattern_hints(is.pattern),
                    context,
                    block_context,
                    artifacts,
                ),
                Expression::As(r#as) => {
                    analyze_php_shape(Node::Expression(self), vec![r#as.hint], context, block_context, artifacts)
                }
                Expression::PatternMatch(pattern_match) => analyze_php_shape(
                    Node::Expression(self),
                    match_arm_hints(pattern_match),
                    context,
                    block_context,
                    artifacts,
                ),
                Expression::TypeOf(expr) => expr.analyze(context, block_context, artifacts),
                Expression::With(expr) => expr.analyze(context, block_context, artifacts),
                Expression::GenericClass(expr) => expr.analyze(context, block_context, artifacts),
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
                    let keyword_name = word(keyword.value);
                    let operator = if context.dialect.is_sharp() { "." } else { "::" };
                    let constant = display_member(context, keyword_name, "CONSTANT");
                    let method = display_member(context, keyword_name, "method()");

                    context.collector.report_with_code(
                    IssueCode::InvalidScopeKeywordContext,
                    Issue::error(format!("The `{keyword_str}` keyword cannot be used as a standalone value."))
                        .with_annotation(
                            Annotation::primary(keyword.span)
                                .with_message(format!("`{keyword_str}` used as a value here")),
                        )
                        .with_note(
                            format!("The `{keyword_str}` keyword is used to refer to a class scope and must be used with the `{operator}` operator.")
                        )
                        .with_help(
                            format!("Use `{constant}`, `{method}`, or `new {keyword_str}()` instead.")
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
/// narrows on the code that runs. `hints` are the types it tests, whose type arguments it checks. An expression's type
/// is its PHP's. A form the slice refuses has no PHP, and its error is the semantic check's.
pub(crate) fn analyze_php_shape<'ctx, 'arena, A>(
    node: Node<'_, 'arena>,
    hints: Vec<&Hint<'_>>,
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let tests_function_type = hints.iter().any(|hint| names_function_type(hint));
    let mut is_refused = false;
    for hint in hints {
        is_refused |= report_hint_type_arguments(context, block_context, artifacts, hint);
        is_refused |= report_erased_type_test(context, hint);

        // A test of a type parameter, of a generic class with its type arguments, of a collection, of `Any`, of a
        // function type or of a union is `instanceof` a name spanning the whole type, and narrows to the type written,
        // which the analysis records as the type the test reads.
        let reads_type_arguments = match hint {
            Hint::Identifier(name) => context.resolved_names.is_type_parameter(name),
            Hint::Generic(_) | Hint::Mixed(_) | Hint::Function(_) | Hint::Union(_) => has_type_test(hint),
            _ => false,
        };
        if reads_type_arguments {
            let tested = get_type_from_hint(context, block_context, artifacts, hint);
            artifacts.record_tested_type(hint, tested);
        }
    }

    let Some(PhpShape { php, temporaries, tests }) =
        context.get_assertion_context_from_block(block_context).php_shape(node)
    else {
        // A refused form is the error type, so nothing that reads it reports again.
        let refused = report_case_patterns_without_php(node, context);
        if let Node::Expression(expression) = node {
            let refused_type = if refused { TUnion::from_atomic(TAtomic::Error) } else { get_mixed() };
            artifacts.set_expression_type(expression, refused_type);
        }

        return Ok(());
    };

    let patterns = context.patterns.len();
    match node {
        Node::Expression(Expression::Is(is)) => context.patterns.push((Some(is.is), is.pattern)),
        Node::Expression(Expression::PatternMatch(pattern_match))
        | Node::Statement(Statement::PatternMatch(pattern_match)) => {
            context.patterns.extend(pattern_match.arms.iter().filter_map(|arm| match arm {
                PatternMatchArm::Pattern(arm) => Some((None, arm.pattern)),
                PatternMatchArm::Default(_) => None,
            }));
        }
        _ => {}
    }
    context.temporaries += temporaries;
    let (result, issues) = context.record(|context| match php {
        Node::Expression(expression) => expression.analyze(context, block_context, artifacts),
        Node::Statement(statement) => statement.analyze(context, block_context, artifacts),
        _ => Ok(()),
    });
    context.temporaries -= temporaries;
    context.patterns.truncate(patterns);

    // The PHP of a pattern calls a type test that no source the user wrote calls. It is the PHP# syntax itself, so it is
    // never a bare call to refuse. Some calls are the shape of the pattern, which no source the user wrote can drop, so
    // C# reports nothing there when they always hold: a properties pattern's `is_object` and a list pattern's
    // `is_array`, their null checks, a list pattern's `count`, and the type test of a list's element or rest, which the
    // list's own type can make always true, as `[int, ...List<int> rest]` writes "any element".
    let mut pattern_calls = PatternCalls::default();
    match node {
        Node::Expression(expression) => PatternCallWalker.walk_expression(expression, &mut pattern_calls),
        Node::Statement(statement) => PatternCallWalker.walk_statement(statement, &mut pattern_calls),
        _ => {}
    }
    let issues = refuse_list_patterns_on_other_values(php, &pattern_calls.lists, context, artifacts, issues);
    let pattern_call = |span: Span| {
        pattern_calls.calls.iter().find(|call: &&LocalIdentifier| call.span.start.offset == span.start.offset)
    };
    let is_shape = |call: &LocalIdentifier| {
        matches!(call.value, b"is_object" | b"count" | b"is_array") || pattern_calls.element_tests.contains(&call.span)
    };
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
    // A `match` on a list has `default` even when its list patterns cover every list, as `[]` and `[Line first,
    // ...List<Line> rest]` do, because spec section 21 skips `default` only on an enum or a sealed interface. The last
    // list arm before that `default` is then always true, which is the shape the spec asks for, not a mistake.
    let covering_list_arm: Option<u32> = match node {
        Node::Expression(Expression::PatternMatch(pattern_match))
        | Node::Statement(Statement::PatternMatch(pattern_match)) => {
            let last = pattern_match.arms.iter().rposition(|arm| !arm.is_default());
            last.and_then(|last| match &pattern_match.arms.as_slice()[last] {
                PatternMatchArm::Pattern(arm) if arm.guard.is_none() && holds_list_pattern(arm.pattern) => {
                    Some(arm.pattern.span().start.offset)
                }
                _ => None,
            })
        }
        _ => None,
    };
    // An `as`, or an `is` of a type, whose value is always of the type tests nothing. PHP# says so in the words the user
    // wrote, in place of PHP's report on the conditional it runs as. A type with fields or properties tests them too,
    // and each reports its own redundant test.
    let redundant_test = match node {
        Node::Expression(Expression::Is(is))
            if let Pattern::Type(TypePattern { hint, fields: None, properties: None, .. }) = is.pattern =>
        {
            Some((is.value, hint.span(), is.is.span.join(is.pattern.span())))
        }
        Node::Expression(Expression::As(r#as)) => {
            Some((r#as.value, r#as.hint.span(), r#as.r#as.span.join(r#as.hint.span())))
        }
        _ => None,
    }
    .filter(|(value, hint, _)| {
        // A value whose expression is refused has no type to be already of.
        artifacts.get_expression_type(*value).is_some_and(|value| !is_refused_type(value))
            && tests.iter().any(|(pattern, test)| {
                pattern.contains(&hint.start)
                    && artifacts.get_rc_expression_type(*test).is_some_and(|test| test.is_true())
            })
    });
    let is_code = |issue: &Issue, code: IssueCode| issue.code.as_deref() == Some(code.as_str());
    let issues: Vec<Issue> = issues
        .into_iter()
        .filter(|issue| {
            let span = issue.primary_span();
            let called = span.and_then(pattern_call);
            let redundant = is_code(issue, IssueCode::RedundantTypeComparison)
                || is_code(issue, IssueCode::RedundantLogicalOperation);
            let always_true = redundant
                || is_code(issue, IssueCode::RedundantComparison)
                || is_code(issue, IssueCode::RedundantCondition);

            // PHP's report on `match (true)` names `true`, so a PHP# `match` reports what it misses itself.
            !is_code(issue, IssueCode::MatchNotExhaustive)
                && (!is_code(issue, IssueCode::MatchArmAlwaysTrue)
                    || span.is_none_or(|span| Some(span.start.offset) != covering_list_arm))
                && (!(is_code(issue, IssueCode::ReplacedBySyntax) || is_code(issue, IssueCode::BareFunctionCall))
                    || called.is_none())
                && (!always_true || called.is_none_or(|call| !is_shape(call)))
                && (!always_true || span.is_none_or(|span| !pattern_calls.case_patterns.contains(&span.start.offset)))
                && (!always_true
                    || span.is_none_or(|span| {
                        !last_arm_test.is_some_and(|test| test.contains(&span.start))
                            && !redundant_test.is_some_and(|(value, _, test)| {
                                test.contains(&span.start)
                                    || (span.start == value.span().start && span.end > value.span().end)
                            })
                    }))
        })
        .collect();

    if let Some((value, hint, test)) = redundant_test {
        let value_text = quoted_source(context, value.span());
        let hint_text = quoted_source(context, hint);
        let article =
            if hint_text.starts_with(['A', 'E', 'I', 'O', 'U', 'a', 'e', 'i', 'o', 'u']) { "an" } else { "a" };
        let (consequence, help) = if let Node::Expression(Expression::As(_)) = node {
            ("changes nothing", format!("Write `{value_text}` alone."))
        } else {
            ("is always true", "Remove the test.".to_owned())
        };

        context.collector.report_with_code(
            IssueCode::RedundantTypeComparison,
            Issue::help(format!(
                "`{value_text}` is already {article} `{hint_text}`, so `{}` {consequence}.",
                quoted_source(context, test)
            ))
            .with_annotation(Annotation::primary(test).with_message("Tests nothing."))
            .with_help(help),
        );
    }

    // Spec section 21: a function value from plain PHP keeps no PHP# type while the code runs, so a function-type test
    // of a value whose source declares none, such as the `Closure` a plain PHP method returns, is an error.
    if tests_function_type
        && let Some(value) = tested_value(node)
        && artifacts.get_expression_type(value).is_some_and(|value_type| has_undeclared_function(value_type, context))
    {
        let value_text = quoted_source(context, value.span());
        context.collector.report_with_code(
            IssueCode::UnknownFunctionType,
            Issue::error(format!(
                "The type of `{value_text}`'s function is unknown: its source declares no parameter or result types."
            ))
            .with_annotation(Annotation::primary(value.span()).with_message("Tested here."))
            .with_help("Call it from a PHP# lambda that declares its types, and test the lambda."),
        );
    }

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
            .map(|test| handled_test(test, context.arena, artifacts))
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

        context.collector.report(never_matches(pattern));
    }
    context.collector.extend(issues);
    result?;

    // A refused type, as `value as Box` writes, gives the expression no type, so where its value goes reports nothing
    // again.
    if let Node::Expression(expression) = node
        && is_refused
    {
        artifacts.set_expression_type(expression, wrap_atomic(TAtomic::Error));
    } else if let (Node::Expression(expression), Node::Expression(php)) = (node, php)
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
    artifacts: &mut AnalysisArtifacts,
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
    let is_closed = value_type.types.iter().all(|atomic| {
        matches!(atomic, TAtomic::Null)
            || enum_value(atomic, context.codebase).is_some()
            || closed_class(atomic, context.codebase).is_some()
    });

    let (missing, enums) = if is_closed {
        missing_cases(pattern_match.span(), &value_type, handled, context, block_context, artifacts)
    } else {
        (vec![], vec![])
    };
    let message = match missing.as_slice() {
        _ if !is_closed => "A `match` needs a `default` arm.".to_owned(),
        [] => return,
        [only] => format!("This `match` misses `{only}`."),
        [rest @ .., last] => {
            let rest: Vec<String> = rest.iter().map(|case| format!("`{case}`")).collect();

            format!("This `match` misses {} and `{last}`.", rest.join(", "))
        }
    };
    let message = match display_missing_imports(context, enums) {
        Some(imports) => format!("{message} {imports}"),
        None => message,
    };

    context.collector.report_with_code(
        IssueCode::MatchNotExhaustive,
        Issue::error(message)
            .with_annotation(
                Annotation::primary(pattern_match.r#match.span).with_message("This `match` has no `default`."),
            )
            .with_note("Only a `match` on an enum may leave out `default`, when its arms cover every case.")
            .with_help("Add an arm for each value it misses, or add `default => …` as the last arm."),
    );
}

/// The cases of the enum `value_type` that no `handled` test of the `match` at `span` matches, as PHP# writes them,
/// `Status.Open`, in the order the enum declares them, and `null` last, with the enums those cases name.
fn missing_cases<'ctx, 'arena, A>(
    span: Span,
    value_type: &TUnion,
    handled: &[&Expression<'arena>],
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> (Vec<String>, Vec<Word>)
where
    A: Arena,
{
    // A new implementer of a sealed interface joins its set without changing the interface, so the file is analyzed
    // again when the classes that implement it change, spec section 20.1. The `match` reads the set whether or not its
    // arms leave a class of it.
    for atomic in value_type.types.iter() {
        if let Some(interface) = closed_class(atomic, context.codebase).filter(|class| class.flags.is_sealed()) {
            artifacts.listings.insert(Listing::DirectDescendants(interface.name));
        }
    }

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
    let mut named = Vec::new();
    for name in enums {
        let Some(metadata) = context.codebase.get_enum(name.as_bytes()) else {
            continue;
        };
        let mut declared: Vec<_> = metadata.enum_cases.values().collect();
        declared.sort_by_key(|case| case.span.start.offset);
        for case in declared {
            if cases.iter().any(|(enum_name, left)| *enum_name == name && left.is_none_or(|left| left == case.name)) {
                missing.push(display_code_member(context, metadata.original_name, case.name));
                if !named.contains(&metadata.original_name) {
                    named.push(metadata.original_name);
                }
            }
        }
    }
    // A closed set of classes leaves each class no arm tests, of all the cases of an enum with data or all the
    // implementers of a sealed interface while the value is still the set's type, and else each class left.
    let tested: Vec<&[u8]> = handled
        .iter()
        .filter_map(|test| match test.unparenthesized() {
            Expression::Binary(Binary {
                operator: BinaryOperator::Instanceof(_),
                rhs: Expression::Identifier(class),
                ..
            }) => context.resolved_names.resolve(class),
            _ => None,
        })
        .collect();
    let mut left_classes: Vec<&ClassLikeMetadata> = Vec::new();
    for class in atomics().filter_map(|atomic| closed_class(atomic, context.codebase)) {
        // A member of the set inherits the set's `permitted_inheritors` from its parent, so only a class outside its
        // own set, the enum or the sealed interface, stands for the whole set.
        let members: Vec<&ClassLikeMetadata> = match &class.permitted_inheritors {
            Some(inheritors) if !inheritors.contains(&class.name) => inheritors
                .iter()
                .filter_map(|inheritor| context.codebase.get_class_like(inheritor.as_bytes()))
                .collect(),
            _ => vec![class],
        };
        for member in members {
            let is_tested = tested.iter().any(|tested| tested.eq_ignore_ascii_case(member.name.as_bytes()));
            if !is_tested && !left_classes.iter().any(|known| known.name == member.name) {
                left_classes.push(member);
            }
        }
    }
    left_classes.sort_by_key(|class| (class.span.file_id, class.span.start.offset));
    for class in left_classes {
        // A case is written through its enum, as `PaymentResult.Paid`.
        let enum_name = class.direct_parent_class.filter(|_| class.flags.is_enum_case_class());
        missing.push(match enum_name.and_then(|name| context.codebase.get_class_like(name.as_bytes())) {
            Some(r#enum) => {
                let case = class.original_name.as_bytes().rsplit(|&byte| byte == b'\\').next().unwrap_or_default();

                display_code_member(context, r#enum.original_name, word(case))
            }
            None => display_class_like_name(context, class.original_name).to_string(),
        });
    }

    if atomics().any(|atomic| matches!(atomic, TAtomic::Null)) {
        missing.push("null".to_owned());
    }

    (missing, named)
}

/// What an arm's `test` handles of the value a `match` tests, spec section 20. A type pattern that names its value is
/// `($p = s) instanceof C`, which narrows `$p`, and a case pattern adds the tests of its fields and properties, so a
/// test whose first part is `instanceof` and whose other parts are always true, as a field unpacked with the type the
/// case declares, handles the class: it is `s instanceof C`. Any other test handles what it tests.
fn handled_test<'arena, A>(
    test: &'arena Expression<'arena>,
    arena: &'arena A,
    artifacts: &AnalysisArtifacts,
) -> &'arena Expression<'arena>
where
    A: Arena,
{
    let mut parts = Vec::new();
    let mut rest = test.unparenthesized();
    while let Expression::Binary(Binary { lhs, operator: BinaryOperator::And(_), rhs }) = rest {
        parts.push(*rhs);
        rest = lhs.unparenthesized();
    }
    let Expression::Binary(Binary { lhs, operator: operator @ BinaryOperator::Instanceof(_), rhs }) = rest else {
        return test;
    };
    if !parts.iter().all(|part| artifacts.get_expression_type(part).is_some_and(TUnion::is_true)) {
        return test;
    }
    let subject = match lhs.unparenthesized() {
        Expression::Assignment(assignment) => assignment.rhs,
        subject => subject,
    };

    arena.alloc(Expression::Binary(Binary { lhs: subject, operator: *operator, rhs }))
}

/// The class a value of `atomic` is in a closed set of, spec section 20: a case of an enum with data, an enum with
/// data, a sealed interface, or a class its permitted inheritors name.
fn closed_class<'codebase>(
    atomic: &TAtomic,
    codebase: &'codebase CodebaseMetadata,
) -> Option<&'codebase ClassLikeMetadata> {
    let TAtomic::Object(TObject::Named(object)) = atomic else {
        return None;
    };
    let class = codebase.get_class_like(object.name.as_bytes())?;

    (class.flags.is_enum_case_class() || class.permitted_inheritors.is_some()).then_some(class)
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

/// The error at a pattern that can never match the value it tests.
fn never_matches(pattern: Span) -> Issue {
    Issue::error("This pattern never matches the value it tests.")
        .with_code(IssueCode::ImpossibleTypeComparison.as_str())
        .with_annotation(Annotation::primary(pattern).with_message("Never matches."))
        .with_note("PHP# makes a pattern that can never match an error, as C# does (CS8121).")
        .with_help("Remove the pattern, or test a value that can match it.")
}

/// Checks the value each list pattern tests, as its PHP's first read, the argument of the `\is_array` at its `[`, holds
/// it. A list pattern tests a `List`: a value that holds no `List` never matches it, and a value that may be a `List` or
/// another array, as `Any` may, waits for the type test of `List<T>`. Either one replaces the issues inside the list
/// pattern and inside its value's read, which count and index the value as a list.
fn refuse_list_patterns_on_other_values<A>(
    php: Node<'_, '_>,
    lists: &[(Span, Span)],
    context: &Context<'_, '_, A>,
    artifacts: &AnalysisArtifacts,
    issues: impl IntoIterator<Item = Issue>,
) -> Vec<Issue>
where
    A: Arena,
{
    let mut reads = ListReads::default();
    match php {
        Node::Expression(expression) => ListReadWalker.walk_expression(expression, &mut reads),
        Node::Statement(statement) => ListReadWalker.walk_statement(statement, &mut reads),
        _ => {}
    }

    let mut refused = Vec::new();
    let mut refusals = Vec::new();
    for &(list, left_bracket) in lists {
        let Some((read, read_type)) = reads
            .0
            .iter()
            .find(|(function, _)| function.start.offset == left_bracket.start.offset)
            .and_then(|(_, read)| Some((read.span(), artifacts.get_expression_type(*read)?)))
        else {
            continue;
        };

        let kinds: Vec<ListKind> = read_type.types.iter().map(list_kind).collect();
        let refusal = if kinds.contains(&ListKind::Unknown)
            || (kinds.contains(&ListKind::List) && kinds.contains(&ListKind::OtherArray))
        {
            let read_type = display_type(context, read_type);

            Issue::error(format!("A list pattern on a `{read_type}` is not supported yet in PHP#."))
                .with_code(IssueCode::NotSupportedYet.as_str())
                .with_annotation(Annotation::primary(list).with_message("Not supported yet."))
                .with_note("A list pattern tests a `List`, and a value that may be another array needs the type test of `List<T>` first.")
        } else if kinds.contains(&ListKind::List) || kinds.contains(&ListKind::Error) {
            continue;
        } else {
            never_matches(list)
        };

        refused.extend([list, read]);
        refusals.push(refusal);
    }

    issues
        .into_iter()
        .filter(|issue| issue.primary_span().is_none_or(|span| !refused.iter().any(|at| at.contains(&span.start))))
        .chain(refusals)
        .collect()
}

/// What a value's type is to a list pattern.
#[derive(PartialEq)]
enum ListKind {
    List,
    OtherArray,
    /// A type that is never an array, as `int`, `string`, an object or null.
    NotArray,
    /// A type that may be any array, as `Any` or a type parameter.
    Unknown,
    /// The type of an expression a parse error refused.
    Error,
}

fn list_kind(atomic: &TAtomic) -> ListKind {
    match atomic {
        TAtomic::Array(TArray::List(_)) => ListKind::List,
        TAtomic::Array(_) => ListKind::OtherArray,
        TAtomic::Scalar(_)
        | TAtomic::Object(_)
        | TAtomic::Callable(_)
        | TAtomic::Resource(_)
        | TAtomic::Null
        | TAtomic::Void
        | TAtomic::Never => ListKind::NotArray,
        TAtomic::Error => ListKind::Error,
        _ => ListKind::Unknown,
    }
}

/// The value each `\is_array` call of a pattern's PHP tests, by the span of its function name.
#[derive(Default)]
struct ListReads<'ast, 'arena>(Vec<(Span, &'ast Expression<'arena>)>);

struct ListReadWalker;

impl<'ast, 'arena> Walker<'ast, 'arena, ListReads<'ast, 'arena>> for ListReadWalker {
    fn walk_in_function_call(&self, call: &'ast FunctionCall<'arena>, reads: &mut ListReads<'ast, 'arena>) {
        if let Expression::Identifier(Identifier::Local(function)) = call.function
            && function.value == b"is_array"
            && let Some(argument) = call.argument_list.arguments.first()
        {
            reads.0.push((function.span, argument.value()));
        }
    }
}

/// Whether a pattern holds a list pattern.
fn holds_list_pattern(pattern: &Pattern<'_>) -> bool {
    match pattern {
        Pattern::List(_) => true,
        Pattern::Not(not) => holds_list_pattern(not.pattern),
        Pattern::Binary(binary) => holds_list_pattern(binary.left) || holds_list_pattern(binary.right),
        Pattern::Parenthesized(parenthesized) => holds_list_pattern(parenthesized.pattern),
        Pattern::Type(_) | Pattern::Value(_) | Pattern::Comparison(_) | Pattern::Properties(_) => false,
    }
}

/// Reports each case pattern of a PHP# `is` or `match` that `php_shape` builds no PHP for, spec section 21: one whose
/// type is no case of an enum with data, or whose fields are not the case's in number. Returns whether it reported one.
fn report_case_patterns_without_php<A>(node: Node<'_, '_>, context: &mut Context<'_, '_, A>) -> bool
where
    A: Arena,
{
    let names = CodebasePatternNames { names: context.resolved_names, codebase: context.codebase };
    let case_patterns = node.filter_map(|node| match node {
        Node::TypePattern(TypePattern { hint, fields: Some(fields), .. }) => Some((hint.span(), *fields)),
        _ => None,
    });
    let mut reported = false;
    for (hint, fields) in case_patterns {
        let written = BytesDisplay(&context.source_file.contents[hint.start.offset as usize..hint.end.offset as usize]);
        let message = match names.case_fields(hint) {
            None => format!("`{written}` is no case of an enum whose cases carry data, so it has no fields to unpack."),
            Some(names) if names.len() != fields.fields.len() => {
                let count = |count: usize, word: &str| {
                    if count == 1 { format!("1 {word}") } else { format!("{count} {word}s") }
                };

                format!(
                    "`{written}` has {}, and this pattern unpacks {}.",
                    count(names.len(), "field"),
                    fields.fields.len()
                )
            }
            Some(_) => continue,
        };

        context.collector.report_with_code(
            IssueCode::InvalidArgument,
            Issue::error(message).with_annotation(Annotation::primary(fields.span()).with_message("Unpacked here.")),
        );
        reported = true;
    }

    reported
}

/// Each function a pattern's PHP calls to test a value, at the span `called_functions` gives it, the type a list
/// pattern's element or a case pattern's field is tested against, each list pattern with its `[`, and the start of
/// each case pattern with fields, whose tests the case's own field types can make always true, as
/// `PaymentResult.Paid(string transactionId)` unpacks the field the case declares a `string`.
#[derive(Default)]
struct PatternCalls {
    calls: Vec<LocalIdentifier<'static>>,
    element_tests: Vec<Span>,
    lists: Vec<(Span, Span)>,
    case_patterns: Vec<u32>,
}

/// Collects the `PatternCalls` of a PHP# `is`, `as` or `match`.
struct PatternCallWalker;

impl<'ast, 'arena> Walker<'ast, 'arena, PatternCalls> for PatternCallWalker {
    fn walk_in_type_pattern(&self, type_pattern: &'ast TypePattern<'arena>, calls: &mut PatternCalls) {
        calls.calls.extend(called_functions(Node::TypePattern(type_pattern)));
        if let Some(fields) = type_pattern.fields {
            calls.case_patterns.push(type_pattern.hint.span().start.offset);
            calls.element_tests.extend(fields.fields.iter().filter_map(|field| match field {
                Pattern::Type(TypePattern { hint, .. }) => Some(hint.span()),
                _ => None,
            }));
        }
    }

    fn walk_in_as(&self, r#as: &'ast As<'arena>, calls: &mut PatternCalls) {
        calls.calls.extend(called_functions(Node::As(r#as)));
    }

    fn walk_in_properties_pattern(&self, properties: &'ast PropertiesPattern<'arena>, calls: &mut PatternCalls) {
        calls.calls.extend(called_functions(Node::PropertiesPattern(properties)));
    }

    fn walk_in_list_pattern(&self, list: &'ast ListPattern<'arena>, calls: &mut PatternCalls) {
        calls.calls.extend(called_functions(Node::ListPattern(list)));
        calls.lists.push((list.span(), list.left_bracket));
        calls.element_tests.extend(list.elements.iter().filter_map(|element| match element {
            ListPatternElement::Pattern(Pattern::Type(TypePattern { hint, .. })) => Some(hint.span()),
            ListPatternElement::Rest(rest) => rest.hint.map(HasSpan::span),
            ListPatternElement::Pattern(_) => None,
        }));
    }

    fn walk_in_rest_pattern(&self, rest: &'ast RestPattern<'arena>, calls: &mut PatternCalls) {
        calls.calls.extend(called_functions(Node::RestPattern(rest)));
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

/// Whether an error already refuses `expression`. `check_slice` refuses it when it failed to parse, it is `!x is T`, it
/// calls a named constructor of a type parameter, or it tests or converts a value to a type with no
/// [type test](mago_syntax::utils::pattern::has_type_test) in `is`, `as` or a `match` arm. Its type is the error type
/// in PHP# and `never` in PHP, a variable its `is` pattern names holds the type written beside it, and it adds no other
/// issue.
pub(crate) fn is_refused<A>(expression: &Expression<'_>, context: &Context<'_, '_, A>) -> bool
where
    A: Arena,
{
    match expression {
        Expression::Error(_) => true,
        Expression::Instantiation(instantiation) => {
            instantiation.constructor.is_some()
                && matches!(instantiation.class, Expression::Identifier(class) if context.resolved_names.is_type_parameter(class))
        }
        Expression::Is(is) => {
            matches!(is.value, Expression::UnaryPrefix(prefix) if matches!(prefix.operator, UnaryPrefixOperator::Not(_)))
                || type_pattern_hints(is.pattern).into_iter().any(|hint| !has_type_test(hint))
        }
        Expression::As(r#as) => !has_type_test(r#as.hint),
        Expression::PatternMatch(pattern_match) => is_refused_match(pattern_match),
        _ => false,
    }
}

/// Whether `union` is the type of a refused PHP# expression: one that failed to parse, or an index read of a container
/// that gives no value, such as an `int`. A check that judges an operand's kind skips it, and the expression that reads
/// it gets the same type, because the expression's own error already reports it.
pub(crate) fn is_refused_type(union: &TUnion) -> bool {
    union.types.iter().any(|atomic| matches!(atomic, TAtomic::Error))
}

/// Whether an arm of `pattern_match`, an expression or a statement, tests its value against a type no test can run
/// on yet.
pub(crate) fn is_refused_match(pattern_match: &PatternMatch<'_>) -> bool {
    match_arm_hints(pattern_match).into_iter().any(|hint| !has_type_test(hint))
}

/// Whether `hint` is a function type, or a union with one.
fn names_function_type(hint: &Hint<'_>) -> bool {
    match hint {
        Hint::Function(_) => true,
        Hint::Union(union) => names_function_type(union.left) || names_function_type(union.right),
        _ => false,
    }
}

/// The value a PHP# `is`, `as` or `match` tests.
fn tested_value<'ast, 'arena>(node: Node<'ast, 'arena>) -> Option<&'ast Expression<'arena>> {
    match node {
        Node::Expression(Expression::Is(is)) => Some(is.value),
        Node::Expression(Expression::As(r#as)) => Some(r#as.value),
        Node::Expression(Expression::PatternMatch(pattern_match))
        | Node::Statement(Statement::PatternMatch(pattern_match)) => Some(pattern_match.expression),
        _ => None,
    }
}

/// Whether `value_type` has a function whose source declares no PHP# type: a `Closure` or a function signature plain
/// PHP wrote, or a plain PHP function or method named as a value. The engine finds no type on such a function.
fn has_undeclared_function<A>(value_type: &TUnion, context: &Context<'_, '_, A>) -> bool
where
    A: Arena,
{
    value_type.types.iter().any(|atomic| match atomic {
        TAtomic::Callable(TCallable::Signature(signature)) => signature.effects.is_none(),
        TAtomic::Callable(TCallable::Alias(identifier)) => {
            context.codebase.get_function_like(identifier).is_none_or(|function| !function.flags.is_sharp())
        }
        TAtomic::Object(TObject::Named(object)) => object.name.as_bytes().eq_ignore_ascii_case(b"Closure"),
        _ => false,
    })
}

/// The type of each type pattern in `pattern`, itself or a pattern inside it, and of each list pattern's named rest,
/// which tests the elements the others leave as a typed element is tested.
fn type_pattern_hints<'ast>(pattern: &'ast Pattern<'ast>) -> Vec<&'ast Hint<'ast>> {
    let properties_hints = |properties: &'ast PropertiesPattern<'ast>| -> Vec<&'ast Hint<'ast>> {
        properties.properties.iter().flat_map(|property| type_pattern_hints(property.pattern)).collect()
    };

    match pattern {
        Pattern::Type(type_pattern) => {
            let mut hints = vec![&type_pattern.hint];
            hints.extend(
                type_pattern
                    .fields
                    .iter()
                    .flat_map(|fields| fields.fields.iter())
                    .flat_map(|field| type_pattern_hints(field)),
            );
            hints.extend(type_pattern.properties.into_iter().flat_map(properties_hints));

            hints
        }
        Pattern::Not(not) => type_pattern_hints(not.pattern),
        Pattern::Binary(binary) => [type_pattern_hints(binary.left), type_pattern_hints(binary.right)].concat(),
        Pattern::Parenthesized(parenthesized) => type_pattern_hints(parenthesized.pattern),
        Pattern::Properties(properties) => properties_hints(properties),
        Pattern::List(list) => list
            .elements
            .iter()
            .flat_map(|element| match element {
                ListPatternElement::Pattern(pattern) => type_pattern_hints(pattern),
                ListPatternElement::Rest(rest) => rest.hint.into_iter().collect(),
            })
            .collect(),
        Pattern::Value(_) | Pattern::Comparison(_) => Vec::new(),
    }
}

/// The type of each type pattern in the arms of `pattern_match`.
pub(crate) fn match_arm_hints<'ast>(pattern_match: &'ast PatternMatch<'ast>) -> Vec<&'ast Hint<'ast>> {
    pattern_match
        .arms
        .iter()
        .flat_map(|arm| match arm {
            PatternMatchArm::Pattern(arm) => type_pattern_hints(arm.pattern),
            PatternMatchArm::Default(_) => Vec::new(),
        })
        .collect()
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
    check_for_paradox(context, &block_context.clauses, &expression_clauses, expression_span);

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
