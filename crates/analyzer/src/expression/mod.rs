use std::rc::Rc;

use foldhash::HashMap;
use indexmap::IndexMap;

use mago_algebra::clause::Clause;
use mago_algebra::find_satisfying_assignments;
use mago_algebra::saturate_clauses;
use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeKind;
use mago_codex::misc::GenericParent;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::array::list::TList;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::r#enum::TEnum;
use mago_codex::ttype::atomic::object::named::TNamedObject;
use mago_codex::ttype::combine_union_types;
use mago_codex::ttype::combiner::CombinerOptions;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::get_false;
use mago_codex::ttype::get_list;
use mago_codex::ttype::get_literal_string;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_mixed_list;
use mago_codex::ttype::get_named_object;
use mago_codex::ttype::get_never;
use mago_codex::ttype::get_null;
use mago_codex::ttype::get_specialized_template_type;
use mago_codex::ttype::get_true;
use mago_codex::ttype::get_void;
use mago_codex::ttype::template::TemplateResult;
use mago_codex::ttype::template::inferred_type_replacer;
use mago_codex::ttype::union::TUnion;
use mago_codex::ttype::wrap_atomic;
use mago_names::binding::Binding;
use mago_names::binding::Local;
use mago_names::binding::LocalKind;
use mago_names::binding::php_variable_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_span::HasPosition;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::As;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Leave;
use mago_syntax::cst::ListPattern;
use mago_syntax::cst::ListPatternElement;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Node;
use mago_syntax::cst::Parenthesized;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::PatternMatch;
use mago_syntax::cst::PatternMatchArm;
use mago_syntax::cst::PatternMatchPatternArm;
use mago_syntax::cst::Propagate;
use mago_syntax::cst::PropertiesPattern;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::RestPattern;
use mago_syntax::cst::Statement;
use mago_syntax::cst::TypePattern;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Variable;
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
use crate::statement::class_like::property::accessor_property_type;
use crate::statement::class_like::r#struct::data_cases;
use crate::statement::class_like::r#struct::data_enum;
use crate::statement::function_like::HintPlace;
use crate::statement::function_like::body_return_type;
use crate::statement::function_like::expect_type;
use crate::statement::function_like::known_type_or_error;
use crate::statement::get_type_from_hint;
use crate::statement::report_erased_type_test;
use crate::statement::report_hint_type_arguments;
use crate::utils::expression::get_bare_name_variable_id;
use crate::utils::misc::check_for_paradox;
use crate::utils::names::article;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_code_member;
use crate::utils::names::display_enum_case;
use crate::utils::names::display_member;
use crate::utils::names::display_missing_imports;
use crate::utils::names::display_sharp_type;
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
pub mod name_of;
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
                Expression::Propagate(propagate) => {
                    analyze_propagate(self, propagate, context, block_context, artifacts)
                }
                Expression::Clone(expr) => expr.analyze(context, block_context, artifacts),
                Expression::Error(_)
                | Expression::Access(_)
                | Expression::Call(_)
                | Expression::TypeOf(_)
                | Expression::Is(_)
                | Expression::As(_)
                | Expression::PatternMatch(_)
                    if is_refused(self) =>
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
                Expression::NameOf(expr) => expr.analyze(context, block_context, artifacts),
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
                // The semantics checker refuses `super<HasLabel>` anywhere but as the object of a call.
                Expression::SuperOf(_) => {
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

            if artifacts.null_safe_write_target == Some(self.span())
                && let Some(read) = artifacts.get_expression_type(self).filter(|read| read.has_nullsafe_null())
            {
                artifacts.set_expression_type(self, read.to_non_nullable());
            }

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
    // Its PHP would read such a name as a class, so the form is refused as a whole.
    if report_pattern_reads(node, context, block_context, artifacts) {
        if let Node::Expression(expression) = node {
            artifacts.set_expression_type(expression, TUnion::from_atomic(TAtomic::Error));
        }

        return Ok(());
    }

    let tests_function_type = hints.iter().any(|hint| names_function_type(hint));
    let mut case_types = generic_case_types(node, &hints, context, block_context, artifacts)?;
    let mut is_refused = false;
    for hint in hints {
        // Spec section 31: a case of a generic enum takes the type arguments of the value it tests, as `Result.Err e`
        // takes a `Result<Order, PaymentError>`'s, so it writes none, and the test reads them.
        if let Some(case_type) = case_types.remove(&hint.span()) {
            artifacts.record_tested_type(hint, case_type);
            continue;
        }
        is_refused |= report_hint_type_arguments(context, block_context, artifacts, hint, HintPlace::TypeTest);
        is_refused |= report_erased_type_test(context, hint);

        // A test of a type parameter, of a generic class with its type arguments, of a collection, of a class value, of
        // `Any`, of `Object`, of a tuple, of a function type, of a union or of an intersection is `instanceof` a name
        // spanning the whole type, and narrows to the type written, which the analysis records as the type the test
        // reads. So is a test of a plain PHP trait, spec section 22, since no value is `instanceof` one.
        let reads_type_arguments = match hint {
            Hint::Identifier(name) => {
                context.resolved_names.is_type_parameter(name)
                    || context.codebase.get_trait(context.resolved_names.get(name)).is_some()
            }
            Hint::Generic(_)
            | Hint::Mixed(_)
            | Hint::Object(_)
            | Hint::Tuple(_)
            | Hint::Function(_)
            | Hint::Union(_)
            | Hint::Intersection(_) => has_type_test(hint),
            _ => false,
        };
        if reads_type_arguments {
            let tested = get_type_from_hint(context, block_context, artifacts, hint);
            artifacts.record_tested_type(hint, tested);
        }
    }
    // A list pattern's first test is `instanceof` a name spanning its `[`, which reads `List<Any?>`. A list pattern of
    // types alone, as `[int, string]`, matches the values of that tuple, so once its PHP is analyzed, the code after
    // the pattern narrows to the tuple. Its PHP itself narrows to `List<Any?>`, so an element that is never of its
    // type reports in PHP#'s words, and the elements after it report nothing more. A loop analyzes its body again, so
    // the tuple an earlier pass recorded is forgotten first.
    let mut tuples = Vec::new();
    for list in node.filter_map(|node| match node {
        Node::ListPattern(list) => Some(*list),
        _ => None,
    }) {
        artifacts.record_tested_type(&list.left_bracket, get_mixed_list());
        artifacts.record_list_pattern_tuple(&list.left_bracket, None);
        let element_types: Option<Vec<(TUnion, Option<Word>)>> = list
            .elements
            .iter()
            .map(|element| match element {
                ListPatternElement::Pattern(Pattern::Type(TypePattern {
                    hint,
                    fields: None,
                    properties: None,
                    variable: None,
                })) => Some((get_type_from_hint(context, block_context, artifacts, hint), None)),
                _ => None,
            })
            .collect();
        if let Some(element_types) = element_types.filter(|element_types| !element_types.is_empty()) {
            tuples.push((list.left_bracket, wrap_atomic(TAtomic::Array(TArray::List(TList::tuple(element_types))))));
        }
    }

    // Whether the index a pattern tests reads quietly depends on what it indexes, as the analysis types it, so that
    // value is typed first, in a copy of the block whose issues the analysis of the PHP reports.
    if let Expression::ArrayAccess(access) = tested_value(node).unparenthesized() {
        let mut typing = block_context.clone();
        let (typed, _) = context.record(|context| access.array.analyze(context, &mut typing, artifacts));
        typed?;
    }
    // Each arm of a `match` reads a local it tests anew, so the PHP never reads the local where the `match` writes it,
    // and the translation to Lean reads its type there: it is typed there too, in the same way.
    if let Expression::ConstantAccess(_) = tested_value(node).unparenthesized() {
        let mut typing = block_context.clone();
        let (typed, _) = context.record(|context| tested_value(node).analyze(context, &mut typing, artifacts));
        typed?;
    }

    let Some(PhpShape { php, temporaries, tests }) =
        context.get_assertion_context_from_block(block_context).php_shape(node, artifacts)
    else {
        // A refused form is the error type, so nothing that reads it reports again.
        let refused = report_case_patterns_without_php(node, context, artifacts);
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
    // The declared type a `match`'s value goes into is the one its PHP's value goes into, which gives it to each arm.
    if let (Node::Expression(expression), Node::Expression(php)) = (node, php)
        && let Some(expected) = artifacts.get_expected_type(expression).cloned()
    {
        expect_type(context, artifacts, php, Some(&expected));
    }
    context.temporaries += temporaries;
    let hidden_variables = context.hidden_variables.len();
    let mut assigned = HiddenVariables::default();
    match php {
        Node::Expression(expression) => HiddenVariableWalker.walk_expression(expression, &mut assigned),
        Node::Statement(statement) => HiddenVariableWalker.walk_statement(statement, &mut assigned),
        _ => {}
    }
    context.hidden_variables.extend(assigned.0);
    let (result, issues) = context.record(|context| match php {
        Node::Expression(expression) => expression.analyze(context, block_context, artifacts),
        Node::Statement(statement) => statement.analyze(context, block_context, artifacts),
        _ => Ok(()),
    });
    context.temporaries -= temporaries;
    context.hidden_variables.truncate(hidden_variables);
    context.patterns.truncate(patterns);
    for (left_bracket, tuple) in tuples {
        artifacts.record_list_pattern_tuple(&left_bracket, Some(tuple));
    }

    // The PHP of a pattern calls a type test that no source the user wrote calls. Some calls are the shape of the
    // pattern, which no source the user wrote can drop, so C# reports nothing there when they always hold: a properties pattern's `is_object` and a list pattern's
    // `is List<Any?>`, their null checks, a list pattern's `count`, and the type test of a list's element or rest, which
    // the list's own type can make always true, as `[int, ...List<int> rest]` writes "any element".
    let mut pattern_calls = PatternCalls::default();
    match node {
        Node::Expression(expression) => PatternCallWalker.walk_expression(expression, &mut pattern_calls),
        Node::Statement(statement) => PatternCallWalker.walk_statement(statement, &mut pattern_calls),
        _ => {}
    }
    let list_reads = list_reads(php, &pattern_calls.lists);
    let issues = refuse_list_patterns_on_other_values(&list_reads, artifacts, issues);
    let pattern_call = |span: Span| {
        pattern_calls.calls.iter().find(|call: &&LocalIdentifier| call.span.start.offset == span.start.offset)
    };
    let is_shape = |call: &LocalIdentifier| {
        matches!(call.value, b"is_object" | b"count") || pattern_calls.element_tests.contains(&call.span)
    };
    // The last arm of a `match` without `default` takes every value the arms before it leave, as `default` does, so its
    // test is always true and not redundant. Before `default`, the last arm with a pattern that is always true is
    // reported once, as the arm, and not again for each part of its test. An arm before it that is always true leaves
    // nothing for the arms after it.
    let last_arm_test: Option<Span> = match node {
        Node::Expression(Expression::PatternMatch(pattern_match))
        | Node::Statement(Statement::PatternMatch(pattern_match)) => {
            let defaulted = pattern_match.arms.iter().any(PatternMatchArm::is_default);
            pattern_match.arms.iter().rev().find_map(|arm| match arm {
                PatternMatchArm::Pattern(arm) => Some((arm.pattern.span(), defaulted)),
                PatternMatchArm::Default(_) => None,
            })
        }
        _ => None,
    }
    .filter(|(last, defaulted)| {
        !defaulted
            || issues.iter().any(|issue| {
                issue.code.as_deref() == Some(IssueCode::MatchArmAlwaysTrue.as_str())
                    && issue.primary_span().is_some_and(|span| span.start == last.start)
            })
    })
    .and_then(|(last, _)| tests.iter().find(|(pattern, _)| *pattern == last).map(|(_, test)| test.span()));
    // A type pattern with fields tests the type and then each field, whose test the type makes always true, as
    // `ChargeError.Declined(string reason)` binds a field that is always a `string`, so its `&&` is the pattern's shape.
    let tested_patterns: Vec<&Pattern> = match node {
        Node::Expression(Expression::PatternMatch(pattern_match))
        | Node::Statement(Statement::PatternMatch(pattern_match)) => pattern_match
            .arms
            .iter()
            .filter_map(|arm| match arm {
                PatternMatchArm::Pattern(arm) => Some(arm.pattern),
                PatternMatchArm::Default(_) => None,
            })
            .collect(),
        Node::Expression(Expression::Is(is)) => vec![is.pattern],
        _ => Vec::new(),
    };
    let field_tests: Vec<Span> = tested_patterns
        .into_iter()
        .filter(|pattern| matches!(pattern, Pattern::Type(TypePattern { fields: Some(_), .. })))
        .filter_map(|pattern| tests.iter().find(|(tested, _)| *tested == pattern.span()))
        // A field test that can never hold is the user's mistake, which its `&&` reports too.
        .filter(|(_, test)| artifacts.get_rc_expression_type(*test).is_none_or(|test| !test.is_false()))
        .map(|(_, test)| test.span())
        .collect();
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
    // The test states the type of the value it tests, so a read of a property whose type is unknown, spec section 14.3,
    // needs no warning there: the warning's own help asks for this test.
    let tested_read = tested_value(node).unparenthesized().span();
    let issues: Vec<Issue> = issues
        .into_iter()
        .filter(|issue| {
            let span = issue.primary_span();
            let called = span.and_then(pattern_call);
            let reads_tested_value = is_code(issue, IssueCode::NonDocumentedProperty)
                && span.is_some_and(|span| span.end == tested_read.end && span.start > tested_read.start);
            let redundant = is_code(issue, IssueCode::RedundantTypeComparison)
                || is_code(issue, IssueCode::RedundantLogicalOperation);
            let always_true = redundant
                || is_code(issue, IssueCode::RedundantComparison)
                || is_code(issue, IssueCode::RedundantCondition);

            // PHP's report on `match (true)` names `true`, so a PHP# `match` reports what it misses itself.
            !is_code(issue, IssueCode::MatchNotExhaustive)
                && !reads_tested_value
                && (!is_code(issue, IssueCode::RedundantLogicalOperation)
                    || span.is_none_or(|span| !field_tests.iter().any(|test| test.start == span.start)))
                && (!always_true || called.is_none_or(|call| !is_shape(call)))
                && (!always_true
                    || span.is_none_or(|span| !list_reads.iter().any(|(_, read)| read.span().start == span.start)))
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
        .map(|issue| reword_list_element_test(issue, &pattern_calls.list_elements, &list_reads, context, artifacts))
        .collect();

    // Spec section 21: a function value from plain PHP keeps no PHP# type while the code runs, so a function-type test
    // of a value whose source declares none, such as the `Closure` a plain PHP method returns, is an error, and never
    // always true.
    let tested = tested_value(node);
    let tests_unknown_function =
        tests_function_type && artifacts.get_expression_type(tested).is_some_and(has_undeclared_function);
    if let Some((value, hint, test)) = redundant_test
        && !tests_unknown_function
    {
        let value_text = quoted_source(context, value.span());
        let hint_text = quoted_source(context, hint);
        let article = article(&hint_text);
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

    if tests_unknown_function {
        let tested_text = quoted_source(context, tested.span());
        context.collector.report_with_code(
            IssueCode::UnknownFunctionType,
            Issue::error(format!(
                "The type of `{tested_text}`'s function is unknown: its source declares no parameter or result types."
            ))
            .with_annotation(Annotation::primary(tested.span()).with_message("Tested here."))
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
/// a value that is not an enum, a sealed interface or a `List`, and makes a `match` on one handle each of its cases or
/// lists. The value left is the value once every `handled` test is false, which Mago's reconciler narrows as it
/// narrows the arms of PHP's `match`.
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
            || list_element_type(atomic).is_some()
    });

    let (missing, enums) = if is_closed {
        missing_cases(pattern_match, &value_type, handled, context, block_context, artifacts)
    } else {
        (vec![], vec![])
    };
    let message = match missing.as_slice() {
        _ if !is_closed => "A `match` needs a `default` arm.".to_owned(),
        [] => return,
        [only] => format!("This `match` misses {only}."),
        [rest @ .., last] => format!("This `match` misses {} and {last}.", rest.join(", ")),
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
            .with_note("A `match` may leave out `default` only when its arms handle every case of an enum or a sealed interface, or every value of a `List`.")
            .with_help("Add an arm for each value it misses, or add `default => …` as the last arm."),
    );
}

/// The PHP variable the PHP of `pattern_match` tests, as `php_shape` writes it: the local the `match` names, or the
/// hidden `$match#N` that holds its value, numbered after the hidden variables of the forms around it.
fn tested_variable<A>(pattern_match: &PatternMatch<'_>, context: &Context<'_, '_, A>) -> Word
where
    A: Arena,
{
    match pattern_match.expression {
        Expression::ConstantAccess(access)
            if matches!(context.resolved_names.binding(&access.name), Some(Binding::Local(_))) =>
        {
            php_variable_name(access.name.value())
        }
        _ => word(format!("$match#{}", context.temporaries + 1)),
    }
}

/// The cases of the enum `value_type` that no `handled` test of `pattern_match` matches, as PHP# writes them in
/// backticks, in the order the enum declares them, then the lists of a `List` that no arm without `when` holds, as
/// "lists of 2 or more items", and null last, with the enums those cases name.
fn missing_cases<'ctx, 'arena, A>(
    pattern_match: &PatternMatch<'arena>,
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
        value_type.clone()
    } else {
        let mut unhandled_context = block_context.clone();
        reconcile_keyed_types(
            context,
            &assertions,
            IndexMap::new(),
            &mut unhandled_context,
            &mut WordSet::default(),
            &WordSet::default(),
            &pattern_match.span(),
            false,
            false,
        );

        // Only the tested value is left to handle: the PHP of a pattern narrows the hidden variables of its items too.
        unhandled_context
            .locals
            .get(&tested_variable(pattern_match, context))
            .map_or_else(|| value_type.clone(), |left| (**left).clone())
    };

    let atomics = || left.types.iter();
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
                missing.push(format!("`{}`", display_code_member(context, metadata.original_name, case.name)));
                if !named.contains(&metadata.original_name) {
                    named.push(metadata.original_name);
                }
            }
        }
    }
    // A closed set of classes leaves each class no arm tests, of all the cases of an enum with data or all the
    // implementers of a sealed interface while the value is still the set's type, and else each class left.
    let together = cases_handled_together(pattern_match, context, block_context, artifacts);
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
        .chain(together.iter().map(|case| case.as_bytes()))
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
        let name = display_enum_case(context, class)
            .unwrap_or_else(|| display_class_like_name(context, class.original_name).to_string());
        missing.push(format!("`{name}`"));
    }

    // The arms of a `match` on a `List` are joined on the declared `List`: the reconciler narrows a `List` that a list
    // pattern's count tests to a tuple, which is no longer the `List` the arms cover.
    if let Some(list) = value_type.types.iter().find(|atomic| list_element_type(atomic).is_some()) {
        let patterns: Vec<&Pattern> = pattern_match
            .arms
            .iter()
            .filter_map(|arm| match arm {
                PatternMatchArm::Pattern(arm) if arm.guard.is_none() => Some(arm.pattern),
                _ => None,
            })
            .collect();
        let mut coverage = ListCoverage { context, block_context, artifacts, shapes: Vec::new() };
        let uncovered = coverage.uncovered(Values::Every(TUnion::from_atomic(list.clone())), &patterns);
        missing.extend(joined(uncovered).iter().map(|lists| described_lists(lists, true, context)));
    }

    if atomics().any(|atomic| matches!(atomic, TAtomic::Null)) {
        missing.push("`null`".to_owned());
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

/// The cases of an enum with data that the arms of `pattern_match` without `when` handle together, spec section 20:
/// arms that unpack one case, whose patterns hold every value of each field's declared type but one field's, and
/// together hold every value of that field's type, as `Delivery.Noted(string note)` and `Delivery.Noted(null)` hold
/// every `string?`.
fn cases_handled_together<'ctx, A>(
    pattern_match: &PatternMatch<'_>,
    context: &Context<'ctx, '_, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Vec<Word>
where
    A: Arena,
{
    // Each arm's case, the types its fields declare, and the type each field's pattern holds every value of. A case
    // pattern's PHP exists only for a case of an enum with data, so its hint names one, and the case's class has a
    // constructor, which takes no field for a plain case.
    let unpacking = pattern_match.arms.iter().filter_map(|arm| match arm {
        PatternMatchArm::Pattern(PatternMatchPatternArm {
            pattern: Pattern::Type(TypePattern { hint, fields: Some(fields), properties: None, .. }),
            guard: None,
            ..
        }) => Some((hint, fields)),
        _ => None,
    });
    // A field of a case of a generic enum declares its type with the type arguments the tested value gives the enum.
    let tested = artifacts.get_expression_type(pattern_match.expression).cloned().unwrap_or_else(get_mixed);
    let mut arms: Vec<(Word, Vec<TUnion>, Vec<Option<TUnion>>)> = Vec::new();
    for (hint, fields) in unpacking {
        let case = context.resolved_names.resolve(hint).and_then(|name| context.codebase.get_class_like(name));
        let (Some(case), Some(declared)) = (case, case_field_types(context, hint, &tested)) else {
            unreachable!("a case pattern with PHP names a case's class, which has a constructor")
        };
        let held = fields
            .fields
            .iter()
            .zip(&declared)
            .map(|(field, declared)| held_type(field, declared, context, block_context, artifacts))
            .collect();
        arms.push((case.name, declared, held));
    }

    let holds = |held: &Option<TUnion>, declared: &TUnion| {
        held.as_ref().is_some_and(|held| holds_every_value(context.codebase, held, declared))
    };
    let mut together: Vec<Word> = Vec::new();
    for (case, declared, _) in &arms {
        let case_arms: Vec<&Vec<Option<TUnion>>> =
            arms.iter().filter(|(other, _, _)| other == case).map(|(_, _, held)| held).collect();
        let handles_case = (0..declared.len()).any(|varying| {
            let others_hold = case_arms.iter().all(|held| {
                held.iter()
                    .zip(declared)
                    .enumerate()
                    .all(|(field, (held, declared))| field == varying || holds(held, declared))
            });
            let varying_held = case_arms.iter().map(|held| held[varying].clone()).reduce(|left, right| {
                Some(combine_union_types(&left?, &right?, context.codebase, CombinerOptions::default()))
            });

            others_hold && varying_held.is_some_and(|held| holds(&held, &declared[varying]))
        });
        if handles_case && !together.contains(case) {
            together.push(*case);
        }
    }

    together
}

/// The type a field pattern that tests a value of `tested` holds every value of: a type pattern's type, without
/// properties, whose fields each hold every value of their own type, the type of `null`, `true`, `false` or a case,
/// which is the value itself, and both sides' of `or`. A case of a generic enum has the type arguments `tested` gives
/// it, so `Option.Some(int value)` holds every `Option<int>.Some` of an `Option<int>`. `None` for any other pattern,
/// which holds only some values of a type.
fn held_type<'ctx, A>(
    pattern: &Pattern<'_>,
    tested: &TUnion,
    context: &Context<'ctx, '_, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Option<TUnion>
where
    A: Arena,
{
    match pattern {
        Pattern::Type(TypePattern { hint, fields, properties: None, .. }) => {
            if let Some(fields) = fields {
                let declared = case_field_types(context, hint, tested)?;
                for (field, declared) in fields.fields.iter().zip(&declared) {
                    let held = held_type(field, declared, context, block_context, artifacts)?;
                    if !holds_every_value(context.codebase, &held, declared) {
                        return None;
                    }
                }
            }

            Some(match case_hint_type(context, hint, tested) {
                Some((case_type, _)) => case_type,
                None => get_type_from_hint(context, block_context, artifacts, hint),
            })
        }
        Pattern::Value(Expression::Literal(Literal::Null(_))) => Some(get_null()),
        Pattern::Value(Expression::Literal(Literal::True(_))) => Some(get_true()),
        Pattern::Value(Expression::Literal(Literal::False(_))) => Some(get_false()),
        Pattern::Value(Expression::Access(Access::Property(
            access @ PropertyAccess { property: ClassLikeMemberSelector::Identifier(case), .. },
        ))) => {
            // A plain case of an enum with data, `Option.None`, whose class the binder resolves at the case's name.
            if let Some(name) = context.resolved_names.resolve(&case.span)
                && let Some(class) =
                    context.codebase.get_class_like(name).filter(|class| class.flags.is_enum_case_class())
            {
                return Some(
                    case_type(context, name, tested)
                        .map_or_else(|| get_named_object(class.original_name, None), |(case_type, _)| case_type),
                );
            }

            // A case of an enum without data, `Status.Open`.
            let class = context.resolved_names.static_property_class(access)?;
            let r#enum = context.codebase.get_enum(context.resolved_names.get(&class.name))?;
            let case = r#enum.enum_cases.values().find(|declared| declared.name.as_bytes() == case.value)?;

            Some(TUnion::from_atomic(TAtomic::Object(TObject::Enum(TEnum::new_case(r#enum.name, case.name)))))
        }
        Pattern::Binary(binary) if !binary.is_and() => {
            let left = held_type(binary.left, tested, context, block_context, artifacts)?;
            let right = held_type(binary.right, tested, context, block_context, artifacts)?;

            Some(combine_union_types(&left, &right, context.codebase, CombinerOptions::default()))
        }
        Pattern::Parenthesized(parenthesized) => {
            held_type(parenthesized.pattern, tested, context, block_context, artifacts)
        }
        _ => None,
    }
}

/// Whether patterns that hold every value of `held` hold every value of `declared`. An enum with data is the set of its
/// cases, spec section 20, so `Option<int>.Some` and `Option<int>.None` together hold every `Option<int>`.
fn holds_every_value(codebase: &CodebaseMetadata, held: &TUnion, declared: &TUnion) -> bool {
    let cases: Vec<TAtomic> = declared
        .types
        .iter()
        .flat_map(|atomic| {
            let TAtomic::Object(TObject::Named(object)) = atomic else {
                return vec![atomic.clone()];
            };
            let cases = codebase.get_class_like(object.name.as_bytes()).map(|class| data_cases(codebase, class));
            let parameters = object.get_type_parameters().map(<[_]>::to_vec);
            match cases {
                Some(cases) if !cases.is_empty() => cases
                    .into_iter()
                    .map(|case| {
                        let case = TNamedObject::new_with_type_parameters(case.name, parameters.clone());

                        TAtomic::Object(TObject::Named(case))
                    })
                    .collect(),
                _ => vec![atomic.clone()],
            }
        })
        .collect();

    union_comparator::is_contained_by(
        codebase,
        &TUnion::from_vec(cases),
        held,
        false,
        false,
        false,
        &mut ComparisonResult::new(),
    )
}

/// Values of a `List` that a `match` tests, spec section 21: every value of a type, or the lists of a length whose first
/// and last items are such values. Coverage splits values where an arm tells them apart, as Rust splits a pattern's
/// constructors, so arms join per item over the cases an item can be: enum cases, sealed implementers, `null` and
/// lengths.
#[derive(Clone, PartialEq)]
enum Values {
    Every(TUnion),
    Lists { element: TUnion, length: Length, first: Vec<Values>, last: Vec<Values> },
}

/// The length of [`Values::Lists`]: exactly that many items, all of them in `first`, or that many or more, of which
/// `first` holds the first ones and `last` the last ones.
#[derive(Clone, Copy, PartialEq)]
enum Length {
    Exactly(usize),
    AtLeast(usize),
}

/// Whether a pattern holds all of some values, none of them, or some, with the place of the values whose split may
/// tell the pattern's apart: the indexes of the items down to them.
#[derive(Clone, PartialEq)]
enum Holds {
    All,
    None,
    Some(Option<Vec<usize>>),
}

impl Holds {
    fn and(self, other: Holds) -> Holds {
        match (self, other) {
            (Holds::None, _) | (_, Holds::None) => Holds::None,
            (Holds::All, other) | (other, Holds::All) => other,
            (Holds::Some(place), Holds::Some(other)) => Holds::Some(place.or(other)),
        }
    }

    fn or(self, other: Holds) -> Holds {
        match (self, other) {
            (Holds::All, _) | (_, Holds::All) => Holds::All,
            (Holds::None, other) | (other, Holds::None) => other,
            (Holds::Some(place), Holds::Some(other)) => Holds::Some(place.or(other)),
        }
    }
}

impl std::ops::Not for Holds {
    type Output = Holds;

    fn not(self) -> Holds {
        match self {
            Holds::All => Holds::None,
            Holds::None => Holds::All,
            some => some,
        }
    }
}

/// Finds the values of a `List` that no arm of a `match` holds.
struct ListCoverage<'coverage, 'ctx, 'arena, A>
where
    A: Arena,
{
    context: &'coverage Context<'ctx, 'arena, A>,
    block_context: &'coverage BlockContext<'ctx>,
    artifacts: &'coverage mut AnalysisArtifacts,
    /// Each list pattern that met every value of a `List` at a place: the place, its items before the rest, or all of
    /// them, and its items after the rest when it has one.
    shapes: Vec<(Vec<usize>, usize, Option<usize>)>,
}

impl<A> ListCoverage<'_, '_, '_, A>
where
    A: Arena,
{
    /// The parts of `values` that none of `patterns` holds.
    fn uncovered(&mut self, values: Values, patterns: &[&Pattern<'_>]) -> Vec<Values> {
        self.shapes.clear();
        let mut split_at = None;
        for pattern in patterns {
            match self.holds(pattern, &values, &mut Vec::new()) {
                Holds::All => return Vec::new(),
                Holds::Some(Some(place)) => {
                    split_at.get_or_insert(place);
                }
                Holds::Some(None) | Holds::None => {}
            }
        }
        let Some(place) = split_at else {
            return vec![values];
        };

        let shapes = std::mem::take(&mut self.shapes);
        split(&values, &place, &shapes, self.context.codebase)
            .into_iter()
            .flat_map(|part| self.uncovered(part, patterns))
            .collect()
    }

    /// Whether `pattern` holds `values`, which sit at `place`.
    fn holds(&mut self, pattern: &Pattern<'_>, values: &Values, place: &mut Vec<usize>) -> Holds {
        match pattern {
            Pattern::Parenthesized(parenthesized) => self.holds(parenthesized.pattern, values, place),
            Pattern::Not(not) => !self.holds(not.pattern, values, place),
            Pattern::Binary(binary) if binary.is_and() => {
                let left = self.holds(binary.left, values, place);

                left.and(self.holds(binary.right, values, place))
            }
            Pattern::Binary(binary) => {
                // Both sides together may hold every value no split tells apart, as `true or false` holds a `bool`.
                if self.holds_type(pattern, values, place) == Holds::All {
                    return Holds::All;
                }
                let left = self.holds(binary.left, values, place);

                left.or(self.holds(binary.right, values, place))
            }
            Pattern::List(list) => self.holds_list(list, values, place),
            Pattern::Type(_) | Pattern::Value(_) | Pattern::Comparison(_) | Pattern::Properties(_) => {
                self.holds_type(pattern, values, place)
            }
        }
    }

    /// Whether a pattern other than a list pattern holds `values`, by the type it holds every value of.
    fn holds_type(&mut self, pattern: &Pattern<'_>, values: &Values, place: &[usize]) -> Holds {
        let tested = values.r#type();
        let Some(held) = held_type(pattern, &tested, self.context, self.block_context, self.artifacts) else {
            return may_hold(values, place, self.context.codebase);
        };

        if holds_every_value(self.context.codebase, &held, &tested) {
            Holds::All
        } else if !union_comparator::can_expression_types_be_identical(
            self.context.codebase,
            &held,
            &tested,
            true,
            false,
        ) {
            Holds::None
        } else {
            may_hold(values, place, self.context.codebase)
        }
    }

    /// Whether a list pattern holds `values`: lists of a length it takes, whose items its patterns hold, and whose items
    /// its rest takes, which a typed rest does when it takes any `List` of the item type.
    fn holds_list(&mut self, list: &ListPattern<'_>, values: &Values, place: &mut Vec<usize>) -> Holds {
        let patterns: Vec<&Pattern> = list
            .elements
            .iter()
            .filter_map(|element| match element {
                ListPatternElement::Pattern(pattern) => Some(*pattern),
                ListPatternElement::Rest(_) => None,
            })
            .collect();
        let rest = list.elements.iter().find_map(|element| match element {
            ListPatternElement::Rest(rest) => Some(rest),
            ListPatternElement::Pattern(_) => None,
        });
        let before = list
            .elements
            .iter()
            .position(|element| matches!(element, ListPatternElement::Rest(_)))
            .unwrap_or(patterns.len());

        let Values::Lists { element, length, first, last } = values else {
            if !values.r#type().types.iter().any(|atomic| list_element_type(atomic).is_some()) {
                return if values.r#type().types.iter().any(may_be_list) { Holds::Some(None) } else { Holds::None };
            }
            self.shapes.push((place.clone(), before, rest.map(|_| patterns.len() - before)));

            return Holds::Some(Some(place.clone()));
        };

        let fits = match (*length, rest) {
            (Length::Exactly(count), None) => patterns.len() == count,
            (Length::Exactly(count), Some(_)) => patterns.len() <= count,
            (Length::AtLeast(_), rest) => rest.is_some(),
        };
        if !fits {
            return Holds::None;
        }

        let items: Vec<&Values> = first.iter().chain(last).collect();
        let mut holds = Holds::All;
        for (index, pattern) in patterns.iter().enumerate() {
            let item = if index < before { index } else { items.len() - patterns.len() + index };
            place.push(item);
            holds = holds.and(self.holds(pattern, items[item], place));
            place.pop();
        }
        if let Some(hint) = rest.and_then(|rest| rest.hint) {
            let held = get_type_from_hint(self.context, self.block_context, self.artifacts, hint);
            if !holds_every_value(self.context.codebase, &held, &get_list(element.clone())) {
                holds = holds.and(Holds::Some(None));
            }
        }

        holds
    }
}

impl Values {
    fn r#type(&self) -> TUnion {
        match self {
            Values::Every(r#type) => r#type.clone(),
            Values::Lists { element, .. } => get_list(element.clone()),
        }
    }
}

/// That a pattern holds some of `values`, at `place` when splitting them may tell the pattern's apart.
fn may_hold(values: &Values, place: &[usize], codebase: &CodebaseMetadata) -> Holds {
    Holds::Some(matches!(values, Values::Every(r#type) if splits(r#type, codebase)).then(|| place.to_vec()))
}

/// Whether every value of `r#type` splits into cases a pattern other than a list pattern tells apart: one type each of
/// a union, the cases of an enum, or the members of a closed set of classes. Only a list pattern tells the lengths of a
/// `List` apart.
fn splits(r#type: &TUnion, codebase: &CodebaseMetadata) -> bool {
    r#type.types.len() > 1
        || r#type
            .types
            .first()
            .is_some_and(|atomic| enum_cases(atomic, codebase).is_some() || closed_members(atomic, codebase).is_some())
}

/// `values` with the values at `place` split into the cases they can be. A `List` splits into the lengths the
/// `shapes` of its list patterns tell apart: each length below the longest pattern without a rest, or below the items
/// around the longest rest, and every longer length.
fn split(
    values: &Values,
    place: &[usize],
    shapes: &[(Vec<usize>, usize, Option<usize>)],
    codebase: &CodebaseMetadata,
) -> Vec<Values> {
    let Some((&item, inner)) = place.split_first() else {
        let Values::Every(r#type) = values else {
            unreachable!("only every value of a type splits");
        };
        return split_type(r#type, place, shapes, codebase);
    };
    let Values::Lists { element, length, first, last } = values else {
        unreachable!("a place inside values is an item of lists");
    };
    let items: Vec<&Values> = first.iter().chain(last).collect();

    split(items[item], inner, &shapes_inside(shapes, item), codebase)
        .into_iter()
        .map(|part| {
            let (mut first, mut last) = (first.clone(), last.clone());
            if item < first.len() {
                first[item] = part;
            } else {
                last[item - first.len()] = part;
            }

            Values::Lists { element: element.clone(), length: *length, first, last }
        })
        .collect()
}

/// The `shapes` met inside the item at `item`, with their places from that item.
fn shapes_inside(
    shapes: &[(Vec<usize>, usize, Option<usize>)],
    item: usize,
) -> Vec<(Vec<usize>, usize, Option<usize>)> {
    shapes
        .iter()
        .filter_map(|(place, before, after)| match place.split_first() {
            Some((&at, inner)) if at == item => Some((inner.to_vec(), *before, *after)),
            _ => None,
        })
        .collect()
}

/// Every value of `r#type` split into the cases it can be, as [`splits`] lists them.
fn split_type(
    r#type: &TUnion,
    place: &[usize],
    shapes: &[(Vec<usize>, usize, Option<usize>)],
    codebase: &CodebaseMetadata,
) -> Vec<Values> {
    if r#type.types.len() > 1 {
        return r#type.types.iter().map(|atomic| Values::Every(TUnion::from_atomic(atomic.clone()))).collect();
    }
    let atomic = r#type.get_single();
    if let Some(element) = list_element_type(atomic) {
        let here = shapes.iter().filter(|(at, ..)| at == place);
        let fixed = here.clone().filter(|(.., after)| after.is_none()).map(|(_, count, _)| count + 1).max();
        let before = here.clone().filter(|(.., after)| after.is_some()).map(|(_, before, _)| *before).max();
        let after = here.filter_map(|(.., after)| *after).max();
        let (before, after) = (before.unwrap_or(0), after.unwrap_or(0));
        let longer = fixed.unwrap_or(0).max(before + after);
        let every = Values::Every(element.clone());
        let lists = |length, first, last| Values::Lists { element: element.clone(), length, first, last };

        return (0..longer)
            .map(|count| lists(Length::Exactly(count), vec![every.clone(); count], Vec::new()))
            .chain([lists(Length::AtLeast(longer), vec![every.clone(); before], vec![every.clone(); after])])
            .collect();
    }

    enum_cases(atomic, codebase)
        .or_else(|| closed_members(atomic, codebase))
        .unwrap_or_else(|| unreachable!("a value that splits is a union, a `List`, an enum or a closed set"))
        .into_iter()
        .map(|atomic| Values::Every(TUnion::from_atomic(atomic)))
        .collect()
}

/// Each case of the enum `atomic` names every case of, in the order the enum declares them.
fn enum_cases(atomic: &TAtomic, codebase: &CodebaseMetadata) -> Option<Vec<TAtomic>> {
    let (name, None) = enum_value(atomic, codebase)? else {
        return None;
    };
    let mut cases: Vec<_> = codebase.get_enum(name.as_bytes())?.enum_cases.values().collect();
    cases.sort_by_key(|case| case.span.start.offset);

    (!cases.is_empty()).then(|| {
        cases.into_iter().map(|case| TAtomic::Object(TObject::Enum(TEnum::new_case(name, case.name)))).collect()
    })
}

/// Each member of the closed set of classes `atomic` stands for, with its type arguments: a case of an enum with data,
/// or an implementer of a sealed interface. `None` for a class that is a member itself.
fn closed_members(atomic: &TAtomic, codebase: &CodebaseMetadata) -> Option<Vec<TAtomic>> {
    let class = closed_class(atomic, codebase)?;
    let inheritors = class.permitted_inheritors.as_ref().filter(|inheritors| !inheritors.contains(&class.name))?;
    let TAtomic::Object(TObject::Named(object)) = atomic else {
        unreachable!("a closed class is a named object");
    };
    let parameters = object.get_type_parameters().map(<[_]>::to_vec);
    let mut members: Vec<&ClassLikeMetadata> =
        inheritors.iter().filter_map(|inheritor| codebase.get_class_like(inheritor.as_bytes())).collect();
    members.sort_by_key(|member| (member.span.file_id, member.span.start.offset));

    Some(
        members
            .into_iter()
            .map(|member| {
                TAtomic::Object(TObject::Named(TNamedObject::new_with_type_parameters(member.name, parameters.clone())))
            })
            .collect(),
    )
}

/// The lists of exactly `n` items joined to the lists of `n + 1` or more items whose first items they share, so `[null]`
/// and `[null, …]` read as the lists whose first item is `null`.
fn joined(mut lists: Vec<Values>) -> Vec<Values> {
    loop {
        let found = lists.iter().enumerate().find_map(|(longer_at, longer)| {
            let Values::Lists { element, length: Length::AtLeast(count), first, last } = longer else {
                return None;
            };
            if !last.is_empty() || *count <= first.len().max(1) {
                return None;
            }
            let every = Values::Every(element.clone());
            let shorter_at = lists.iter().position(|shorter| {
                matches!(shorter, Values::Lists { length: Length::Exactly(shorter_count), first: items, .. }
                    if *shorter_count + 1 == *count
                        && items[..first.len()] == first[..]
                        && items[first.len()..].iter().all(|item| *item == every))
            })?;

            let widened = Values::Lists {
                element: element.clone(),
                length: Length::AtLeast(*count - 1),
                first: first.clone(),
                last: Vec::new(),
            };

            Some((longer_at, shorter_at, widened))
        });
        let Some((longer_at, shorter_at, widened)) = found else {
            return lists;
        };
        lists[longer_at] = widened;
        lists.remove(shorter_at);
    }
}

/// `values`, lists of a `List`, in plain words: "every list", "lists of 2 or more items" or "lists whose first item
/// is a `Square`", or one such list when `plural` is false.
fn described_lists<A>(values: &Values, plural: bool, context: &Context<'_, '_, A>) -> String
where
    A: Arena,
{
    let Values::Lists { element, length, first, last } = values else {
        return if plural { "every list".to_owned() } else { "a list".to_owned() };
    };

    let every = Values::Every(element.clone());
    let from_start = first.iter().enumerate().map(|(index, item)| (index + 1, 0, item));
    let from_end = last.iter().enumerate().map(|(index, item)| (0, last.len() - index, item));
    let constrained: Vec<(usize, usize, &Values)> =
        from_start.chain(from_end).filter(|(.., item)| **item != every).collect();
    let implied = constrained.iter().map(|(from_start, ..)| *from_start).max().unwrap_or(0)
        + constrained.iter().map(|(_, from_end, _)| *from_end).max().unwrap_or(0);

    let lists = if plural { "lists" } else { "a list" };
    let length = match *length {
        Length::Exactly(0) if plural => "empty lists".to_owned(),
        Length::Exactly(0) => "an empty list".to_owned(),
        Length::Exactly(1) => format!("{lists} of exactly 1 item"),
        Length::Exactly(count) => format!("{lists} of exactly {count} items"),
        Length::AtLeast(count) if !constrained.is_empty() && count <= implied => lists.to_owned(),
        Length::AtLeast(count) => format!("{lists} of {count} or more items"),
    };
    let items: Vec<String> = constrained
        .iter()
        .map(|(from_start, from_end, item)| {
            format!("whose {} is {}", item_place(*from_start, *from_end), described_item(item, context))
        })
        .collect();

    if items.is_empty() { length } else { format!("{length} {}", items.join(" and ")) }
}

/// An item of a `List` in plain words: a value as itself, "`null`", "`Status.Open`" or "`PaymentResult.Paid`", and
/// any other type with its article, "a `Square`", "an `int`" or "an empty list".
fn described_item<A>(item: &Values, context: &Context<'_, '_, A>) -> String
where
    A: Arena,
{
    let Values::Every(r#type) = item else {
        return described_lists(item, false, context);
    };

    let class = match r#type.get_single() {
        TAtomic::Null => return "`null`".to_owned(),
        TAtomic::Object(TObject::Enum(TEnum { name, case: Some(case) })) => {
            let r#enum = context.codebase.get_enum(name.as_bytes()).map_or(*name, |r#enum| r#enum.original_name);

            return format!("`{}`", display_code_member(context, r#enum, case));
        }
        TAtomic::Object(TObject::Named(object)) => context.codebase.get_class_like(object.name.as_bytes()),
        _ => None,
    };
    if let Some(case) = class.and_then(|class| display_enum_case(context, class)) {
        return format!("`{case}`");
    }
    let name = class.map_or_else(
        || display_type(context, r#type),
        |class| display_class_like_name(context, class.original_name).to_string(),
    );

    format!("{} `{name}`", article(&name))
}

/// The place of an item `from_start` items into a list, or `from_end` items from its end when `from_start` is 0, in
/// plain words: "first item", "last item", "second-to-last item", and "item 11" or "item 11 from the end" past the
/// tenth.
fn item_place(from_start: usize, from_end: usize) -> String {
    const ORDINALS: [&str; 10] =
        ["first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth", "tenth"];

    match (from_start, ORDINALS.get(from_start.max(from_end) - 1)) {
        (0, _) if from_end == 1 => "last item".to_owned(),
        (0, Some(ordinal)) => format!("{ordinal}-to-last item"),
        (0, None) => format!("item {from_end} from the end"),
        (_, Some(ordinal)) => format!("{ordinal} item"),
        (_, None) => format!("item {from_start}"),
    }
}

/// The type of the elements of a `List`, whose arms handle it by its lengths. A tuple, which has one length, has none.
fn list_element_type(atomic: &TAtomic) -> Option<&TUnion> {
    match atomic {
        TAtomic::Array(TArray::List(TList { element_type, known_elements: None, .. })) => Some(element_type.as_ref()),
        _ => None,
    }
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

/// `issue`, in PHP#'s words when it is the PHP's report that a list pattern's element, one of `elements`, can never be
/// of its type: "`tail` is an `int` in a `List<int>`, so it is never a `string`." `list_reads` gives the value each
/// list pattern tests. The element of a tuple is the tuple's element at its index.
fn reword_list_element_test<A>(
    issue: Issue,
    elements: &[(Span, Span, Option<Span>, Option<usize>)],
    list_reads: &[(Span, &Expression<'_>)],
    context: &Context<'_, '_, A>,
    artifacts: &AnalysisArtifacts,
) -> Issue
where
    A: Arena,
{
    let Some((hint, variable, list_type, element_type)) = issue
        .primary_span()
        .filter(|_| issue.code.as_deref() == Some(IssueCode::ImpossibleTypeComparison.as_str()))
        .and_then(|span| elements.iter().find(|(hint, ..)| hint.start == span.start))
        .and_then(|(hint, list, variable, index)| {
            let (_, value) = list_reads.iter().find(|(read, _)| read == list)?;
            let list_type = artifacts.get_expression_type(*value)?;
            let element_type = list_type.types.iter().find_map(|atomic| match atomic {
                TAtomic::Array(TArray::List(TList { known_elements: Some(known), .. })) if let Some(index) = index => {
                    known.get(index).map(|(_, element)| element)
                }
                _ => atomic.get_list_element_type(),
            })?;
            Some((hint, variable, list_type, element_type))
        })
        // A tuple's element after a rest has no one type to name.
        .filter(|(.., element_type)| !element_type.is_never())
    else {
        return issue;
    };

    let element = display_type(context, element_type);
    let list = display_type(context, list_type);
    let tested = quoted_source(context, *hint);
    let name = variable
        .map_or_else(|| "This element".to_owned(), |variable| format!("`{}`", quoted_source(context, variable)));

    Issue::error(format!(
        "{name} is {} `{element}` in {} `{list}`, so it is never {} `{tested}`.",
        article(&element),
        article(&list),
        article(&tested)
    ))
    .with_code(IssueCode::ImpossibleTypeComparison.as_str())
    .with_annotation(Annotation::primary(*hint).with_message("Tested here."))
    .with_help(format!("Test the element against a type {} `{element}` can be.", article(&element)))
}

/// Each list pattern of `lists`, by its span and its `[`, with the value its PHP's first test, the `is List<Any?>` at
/// the `[`, reads.
fn list_reads<'ast, 'arena>(php: Node<'ast, 'arena>, lists: &[(Span, Span)]) -> Vec<(Span, &'ast Expression<'arena>)> {
    let mut reads = ListReads::default();
    match php {
        Node::Expression(expression) => ListReadWalker.walk_expression(expression, &mut reads),
        Node::Statement(statement) => ListReadWalker.walk_statement(statement, &mut reads),
        _ => unreachable!("the PHP of a pattern is an expression or a statement"),
    }

    lists
        .iter()
        .filter_map(|&(list, left_bracket)| {
            reads
                .0
                .iter()
                .find(|(class, _)| class.start.offset == left_bracket.start.offset)
                .map(|&(_, read)| (list, read))
        })
        .collect()
}

/// Checks the value each list pattern tests, its PHP's first read, may be a `List`. A value that can hold no `List`, as
/// a `Map`, a `string` or another array, never matches, and the refusal replaces the issues inside the list pattern
/// and inside its value's read, which count and index the value as a list.
fn refuse_list_patterns_on_other_values(
    list_reads: &[(Span, &Expression<'_>)],
    artifacts: &AnalysisArtifacts,
    issues: impl IntoIterator<Item = Issue>,
) -> Vec<Issue> {
    let mut refused = Vec::new();
    let mut refusals = Vec::new();
    for &(list, read) in list_reads {
        // A read the analysis gave no type has no type to refuse.
        if artifacts.get_expression_type(read).is_none_or(|read_type| read_type.types.iter().any(may_be_list)) {
            continue;
        }

        refused.extend([list, read.span()]);
        refusals.push(never_matches(list));
    }

    issues
        .into_iter()
        .filter(|issue| issue.primary_span().is_none_or(|span| !refused.iter().any(|at| at.contains(&span.start))))
        .chain(refusals)
        .collect()
}

/// Whether a value of `atomic` may be a `List`: a list, a type that may be any value, as `Any` or a type parameter,
/// or the type of an expression a parse error refused. Another array, a scalar, an object and null are none.
fn may_be_list(atomic: &TAtomic) -> bool {
    match atomic {
        TAtomic::Array(array) => matches!(array, TArray::List(_)),
        TAtomic::Scalar(_)
        | TAtomic::Object(_)
        | TAtomic::Callable(_)
        | TAtomic::Resource(_)
        | TAtomic::Null
        | TAtomic::Void
        | TAtomic::Never => false,
        _ => true,
    }
}

/// The value each `instanceof` of a pattern's PHP tests, by the span of its class name.
#[derive(Default)]
struct ListReads<'ast, 'arena>(Vec<(Span, &'ast Expression<'arena>)>);

struct ListReadWalker;

impl<'ast, 'arena> Walker<'ast, 'arena, ListReads<'ast, 'arena>> for ListReadWalker {
    fn walk_in_binary(&self, binary: &'ast Binary<'arena>, reads: &mut ListReads<'ast, 'arena>) {
        if let (BinaryOperator::Instanceof(_), Expression::Identifier(class)) = (binary.operator, binary.rhs) {
            reads.0.push((class.span(), binary.lhs));
        }
    }
}

/// Each hidden variable a pattern's PHP assigns, `$match#N` or `$as#N`, with the value it holds.
#[derive(Default)]
struct HiddenVariables<'ast, 'arena>(Vec<(&'arena [u8], &'ast Expression<'arena>)>);

struct HiddenVariableWalker;

impl<'ast, 'arena> Walker<'ast, 'arena, HiddenVariables<'ast, 'arena>> for HiddenVariableWalker {
    fn walk_in_assignment(&self, assignment: &'ast Assignment<'arena>, assigned: &mut HiddenVariables<'ast, 'arena>) {
        if let Expression::Variable(Variable::Direct(variable)) = assignment.lhs
            && variable.name.contains(&b'#')
        {
            assigned.0.push((variable.name, assignment.rhs));
        }
    }
}

/// Reports each local, parameter and `this` that a pattern of a PHP# `is` or `match` reads, and returns whether it
/// reported one. Spec section 21 takes types and constants only in a pattern and compares a value read from code in
/// `when`, so the report writes that `when` with the type of the value the pattern compares and the pattern's
/// operator, as `int n when n == top`. A bare member is the semantic check's error.
fn report_pattern_reads<'ctx, 'arena, A>(
    node: Node<'_, 'arena>,
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> bool
where
    A: Arena,
{
    // Each value a pattern compares with: a bare name, which the binder binds to a local of that name, or a value.
    let compared = node.filter_map(|node| match node {
        Node::Pattern(Pattern::Type(TypePattern {
            hint: Hint::Identifier(name),
            fields: None,
            properties: None,
            variable: None,
        })) => Some((Node::Identifier(name), "==".to_owned())),
        Node::Pattern(Pattern::Value(value)) => Some((Node::Expression(value), "==".to_owned())),
        Node::ComparisonPattern(comparison) => Some((
            Node::Expression(comparison.value),
            String::from_utf8_lossy(comparison.operator.as_bytes()).into_owned(),
        )),
        _ => None,
    });

    let mut reported = false;
    for (value, operator) in compared {
        // A lambda's own parameter, declared inside the value, is read where the lambda runs.
        let value_span = value.span();
        let reads = value.filter_map(|node| match node {
            Node::Identifier(name) => match context.resolved_names.binding(*name) {
                Some(Binding::Local(Local { declaration, .. })) if value_span.contains(&declaration.start) => None,
                Some(Binding::Local(Local { kind: LocalKind::Parameter, .. })) => Some((*name, "a parameter")),
                Some(Binding::Local(_)) => Some((*name, "a local")),
                Some(Binding::This) => Some((*name, "the object the method runs on")),
                _ => None,
            },
            _ => None,
        });
        if reads.is_empty() {
            continue;
        }

        // The value's own type, as the pattern's binding of it would be typed. The value is read where `when` would
        // read it, and its issues wait until the pattern is written so.
        let compared_type = match value {
            Node::Expression(expression) => {
                let mut value_context = block_context.clone();
                let _ = context.record(|context| expression.analyze(context, &mut value_context, artifacts));

                artifacts.get_expression_type(expression).cloned()
            }
            Node::Identifier(name) => get_bare_name_variable_id(name, context.resolved_names)
                .and_then(|id| block_context.locals.get(&id))
                .map(|local| (**local).clone()),
            _ => unreachable!("a pattern compares a bare name or a value"),
        };
        let mut compared_type = compared_type.unwrap_or_else(get_mixed).to_non_nullable();
        compared_type.widen_literals();
        let type_text = display_sharp_type(context, &compared_type);
        let value_text = quoted_source(context, value.span());
        for (name, what) in reads {
            let name_text = quoted_source(context, name.span());
            context.collector.report_with_code(
                IssueCode::ReplacedBySyntax,
                Issue::error(format!(
                    "`{name_text}` is {what}: compare its value in `when`, as in `{type_text} n when n {operator} {value_text}`."
                ))
                .with_annotation(Annotation::primary(name.span()).with_message("Read here."))
                .with_note("A pattern takes types and constants only, such as `Circle`, `200` or `Status.Paid`."),
            );
            reported = true;
        }
    }

    reported
}

/// Reports each case pattern of a PHP# `is` or `match` that `php_shape` builds no PHP for, spec section 21: one whose
/// type is no case of an enum with data, or whose fields are not the case's in number. Returns whether it reported one.
fn report_case_patterns_without_php<A>(
    node: Node<'_, '_>,
    context: &mut Context<'_, '_, A>,
    artifacts: &AnalysisArtifacts,
) -> bool
where
    A: Arena,
{
    let names = CodebasePatternNames { names: context.resolved_names, codebase: context.codebase, artifacts };
    let case_patterns = node.filter_map(|node| match node {
        Node::TypePattern(TypePattern { hint, fields: Some(fields), .. }) => Some((hint.span(), *fields)),
        _ => None,
    });
    let mut reported = false;
    for (hint, fields) in case_patterns {
        let written = BytesDisplay(&context.source_file.contents[hint.start.offset as usize..hint.end.offset as usize]);
        let message = match names.case_fields(hint) {
            // A dotted name of a class that is no case has the error `report_dotted_types` gives where it is read.
            None if context
                .resolved_names
                .resolve(&hint)
                .and_then(|class| context.codebase.get_class_like(class))
                .is_some() =>
            {
                reported = true;
                continue;
            }
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
    /// Each list pattern element with a type: its type, its list pattern, the name it binds, and its index when no
    /// rest comes before it.
    list_elements: Vec<(Span, Span, Option<Span>, Option<usize>)>,
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
        // An element after a rest has no index from the start.
        let before_rest = list.elements.iter().position(|element| matches!(element, ListPatternElement::Rest(_)));
        calls.list_elements.extend(list.elements.iter().enumerate().filter_map(|(index, element)| match element {
            ListPatternElement::Pattern(Pattern::Type(TypePattern { hint, variable, .. })) => Some((
                hint.span(),
                list.span(),
                variable.as_ref().map(HasSpan::span),
                before_rest.is_none_or(|rest| index < rest).then_some(index),
            )),
            _ => None,
        }));
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

/// The type each case pattern of a generic enum in `node` tests, at its hint's span, spec section 31. A case pattern
/// takes the type arguments of the value it tests, so `Result.Ok(Order order)` tests a `Result<Order, PaymentError>.Ok`
/// of a `Result<Order, PaymentError>`, and a case pattern a field holds takes those of the field's type, so
/// `Option.Some` in `Result.Ok(Option.Some(int value))` tests an `Option<int>.Some` of a `Result<Option<int>, …>`. The
/// value is typed in a copy of the block, as the analysis of the PHP types it again, when `hints` name a case of a
/// generic enum.
fn generic_case_types<'ctx, 'arena, A>(
    node: Node<'_, 'arena>,
    hints: &[&Hint<'_>],
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<HashMap<Span, TUnion>, AnalysisError>
where
    A: Arena,
{
    let mut case_types = HashMap::default();
    let names_generic_case = hints.iter().any(|hint| {
        matches!(hint, Hint::Identifier(name) if context
            .codebase
            .get_class_like(context.resolved_names.get(name))
            .is_some_and(|case| case.flags.is_enum_case_class() && !case.template_types.is_empty()))
    });
    if !names_generic_case {
        return Ok(case_types);
    }

    let subject = tested_value(node);
    let mut typing = block_context.clone();
    let (typed, _) = context.record(|context| subject.analyze(context, &mut typing, artifacts));
    typed?;
    let subject_type = artifacts.get_expression_type(subject).cloned().unwrap_or_else(get_mixed);

    match node {
        Node::Expression(Expression::Is(is)) => case_pattern_types(context, is.pattern, &subject_type, &mut case_types),
        Node::Expression(Expression::As(r#as)) => case_types.extend(
            case_hint_type(context, r#as.hint, &subject_type).map(|(case_type, _)| (r#as.hint.span(), case_type)),
        ),
        Node::Expression(Expression::PatternMatch(pattern_match))
        | Node::Statement(Statement::PatternMatch(pattern_match)) => {
            for arm in &pattern_match.arms {
                if let PatternMatchArm::Pattern(arm) = arm {
                    case_pattern_types(context, arm.pattern, &subject_type, &mut case_types);
                }
            }
        }
        _ => unreachable!("only an `is`, an `as` and a `match` have the PHP of a pattern"),
    }

    Ok(case_types)
}

/// Records in `types` the type each case pattern of a generic enum in `pattern`, which tests a value of `tested`,
/// tests, and goes on into the fields of each case pattern with the field's type.
fn case_pattern_types<A>(
    context: &Context<'_, '_, A>,
    pattern: &Pattern<'_>,
    tested: &TUnion,
    types: &mut HashMap<Span, TUnion>,
) where
    A: Arena,
{
    match pattern {
        Pattern::Type(TypePattern { hint, fields, .. }) => {
            if let Some(fields) = fields
                && let Some(field_types) = case_field_types(context, hint, tested)
            {
                for (field, field_type) in fields.fields.iter().zip(&field_types) {
                    case_pattern_types(context, field, field_type, types);
                }
            }
            if let Some((case_type, _)) = case_hint_type(context, hint, tested) {
                types.insert(hint.span(), case_type);
            }
        }
        Pattern::Not(not) => case_pattern_types(context, not.pattern, tested, types),
        Pattern::Parenthesized(parenthesized) => case_pattern_types(context, parenthesized.pattern, tested, types),
        Pattern::Binary(binary) => {
            case_pattern_types(context, binary.left, tested, types);
            case_pattern_types(context, binary.right, tested, types);
        }
        _ => {}
    }
}

/// The type a case `hint` of a generic enum tests in a value of `tested`, with the type arguments `tested` gives the
/// enum, as `Result.Err` tests a `Result<Order, PaymentError>.Err` of a `Result<Order, PaymentError>`. None for any
/// other hint, and when `tested` gives the enum no type arguments.
fn case_hint_type<A>(context: &Context<'_, '_, A>, hint: &Hint<'_>, tested: &TUnion) -> Option<(TUnion, Vec<TUnion>)>
where
    A: Arena,
{
    case_type(context, context.resolved_names.resolve(hint)?, tested)
}

/// The type the case `name` of a generic enum is in a value of `tested`, with the type arguments `tested` gives the
/// enum, and those type arguments. None for any other class, and when `tested` gives the enum no type arguments.
fn case_type<A>(context: &Context<'_, '_, A>, name: &[u8], tested: &TUnion) -> Option<(TUnion, Vec<TUnion>)>
where
    A: Arena,
{
    let codebase = context.codebase;
    let case = codebase.get_class_like(name)?;
    let r#enum =
        codebase.get_class_like(case.direct_parent_class.filter(|_| case.flags.is_enum_case_class())?.as_bytes())?;
    let arguments = tested.types.iter().find_map(|atomic| ancestor_type_arguments(codebase, atomic, r#enum))?;
    let case_type = TNamedObject::new_with_type_parameters(case.original_name, Some(arguments.clone()));

    Some((TUnion::from_atomic(TAtomic::Object(TObject::Named(case_type))), arguments))
}

/// The declared type of each field of the case `hint` names, in the order the case declares them, with the type
/// arguments a value of `tested` gives its enum, as `Result.Ok` in a `Result<Option<int>, PaymentError>` has the field
/// `Option<int> value`. None when `hint` names no case.
fn case_field_types<A>(context: &Context<'_, '_, A>, hint: &Hint<'_>, tested: &TUnion) -> Option<Vec<TUnion>>
where
    A: Arena,
{
    let case = context.codebase.get_class_like(context.resolved_names.resolve(hint)?)?;
    let constructor = context.codebase.get_method(case.name.as_bytes(), b"__construct")?;
    let arguments = case_hint_type(context, hint, tested).map(|(_, arguments)| arguments);

    Some(
        constructor
            .parameters
            .iter()
            .map(|parameter| {
                let declared = parameter.get_type_metadata().map_or_else(get_mixed, |r#type| r#type.type_union.clone());

                with_type_arguments(&declared, case.name, arguments.as_deref(), context.codebase)
            })
            .collect(),
    )
}

/// Analyzes `try result`, spec section 31. `result` is a `Result`, and the expression gives its `Ok` value. On `Err`
/// the function it is in returns an `Err` holding the same error, so that function returns a `Result` whose error type
/// holds `result`'s. The type arguments of the function's `Result`, which the returned `Err` takes, are recorded at the
/// expression, as those of a generic `new` are, and the `Err` joins the types the function returns.
fn analyze_propagate<'ctx, 'arena, A>(
    expression: &Expression<'arena>,
    propagate: &Propagate<'arena>,
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    propagate.value.analyze(context, block_context, artifacts)?;
    let refused = TUnion::from_atomic(TAtomic::Error);
    let operand = artifacts.get_expression_type(propagate.value).cloned().unwrap_or_else(get_mixed);
    if is_refused_type(&operand) {
        artifacts.set_expression_type(expression, refused);

        return Ok(());
    }
    let Some([value, error]) = result_type_arguments(context.codebase, &operand) else {
        context.collector.report_with_code(
            IssueCode::InvalidOperand,
            Issue::error(format!("`try` takes a `Result`, and this is `{}`.", display_type(context, &operand)))
                .with_annotation(Annotation::primary(propagate.value.span()).with_message("Not a `Result`."))
                .with_help("Call `try` on a value of type `Result<T, TError>`, as in `try gateway.charge(cart)`."),
        );
        artifacts.set_expression_type(expression, refused);

        return Ok(());
    };
    // A `try` that can't return its failure is refused, so its value takes no other check.
    artifacts.set_expression_type(expression, refused);

    // A lambda's result type is the one the function type it is written for gives, spec section 14.2. A property's
    // `get` accessor is the method a `try` in it returns from, and returns the property's type. A constructor and a
    // `set` accessor return nothing, as a `void` method does.
    let function = block_context.scope.get_function_like();
    let in_lambda = function
        .is_some_and(|function| matches!(function.kind, FunctionLikeKind::Closure | FunctionLikeKind::ArrowFunction));
    let declared = if in_lambda {
        artifacts.lambda_result.clone().map(|result| (result, None))
    } else if let Some((_, accessor)) = block_context.scope.get_property_hook() {
        Some(match accessor_property_type(block_context).filter(|_| accessor.name.as_bytes() == b"get") {
            Some(property_type) => (property_type.type_union.clone(), Some(property_type.span)),
            None => (get_void(), None),
        })
    } else {
        function.map(|function| match body_return_type(function) {
            Some(result) => (result.type_union.clone(), Some(result.span)),
            None => (get_void(), None),
        })
    };
    let Some((returned, declared_at)) = declared else {
        let (message, help) = if in_lambda {
            (
                "`try` returns its failure from the lambda, and no function type gives the lambda's result type.",
                "Give the lambda a declared type, as in `Function<Result<Order, PaymentError>(Cart)> charge = (cart) => …`.",
            )
        } else {
            (
                "`try` returns its failure from the method it is in, and it stands in no method here.",
                "Read the `Result` with `match`, as in `match (result) { Result.Ok(Order order) => …, … }`.",
            )
        };
        context.collector.report_with_code(
            IssueCode::InvalidReturnStatement,
            Issue::error(message)
                .with_annotation(Annotation::primary(propagate.r#try.span).with_message("Returns its failure here."))
                .with_help(help),
        );

        return Ok(());
    };
    let returned = known_type_or_error(returned);
    if is_refused_type(&returned) {
        return Ok(());
    }
    let declared_annotation =
        declared_at.map(|span| Annotation::secondary(span).with_message("The result type it returns into."));
    let Some([returned_value, returned_error]) = result_type_arguments(context.codebase, &returned) else {
        context.collector.report_with_code(
            IssueCode::InvalidReturnStatement,
            Issue::error(format!(
                "`try` returns its failure, so its function returns a `Result`, and this one returns `{}`.",
                display_type(context, &returned)
            ))
            .with_annotation(Annotation::primary(propagate.r#try.span).with_message("Returns its failure here."))
            .with_annotations(declared_annotation),
        );

        return Ok(());
    };
    artifacts.set_expression_type(expression, value);
    if !union_comparator::is_contained_by(
        context.codebase,
        &error,
        &returned_error,
        false,
        false,
        false,
        &mut ComparisonResult::for_dialect(context.dialect),
    ) {
        context.collector.report_with_code(
            IssueCode::InvalidReturnStatement,
            Issue::error(format!(
                "`try` returns a `{}` failure, and its function's `{}` holds a `{}`.",
                display_type(context, &error),
                display_type(context, &returned),
                display_type(context, &returned_error),
            ))
            .with_annotation(Annotation::primary(propagate.value.span()).with_message("Fails with this error type."))
            .with_annotations(declared_annotation),
        );

        return Ok(());
    }

    let span = expression.span();
    let type_arguments = vec![returned_value, returned_error];
    let err = TNamedObject::new_with_type_parameters(word(RESULT_ERR), Some(type_arguments.clone()));
    artifacts.inferred_return_types.push(Rc::new(TUnion::from_atomic(TAtomic::Object(TObject::Named(err)))));
    artifacts.inferred_type_arguments.insert((span.start.offset, span.end.offset), type_arguments);

    Ok(())
}

/// The standard library's `Result`, spec section 31, and its failure case.
const RESULT: &str = "Sharp\\Result";
const RESULT_ERR: &str = "Sharp\\Result\\Err";

/// The type arguments `union` gives `Result<T, TError>`, spec section 31, when each of its types is a `Result` or one
/// of its cases, joined across them.
pub(crate) fn result_type_arguments(codebase: &CodebaseMetadata, union: &TUnion) -> Option<[TUnion; 2]> {
    let result = codebase.get_class_like(RESULT.as_bytes())?;
    let mut joined: Option<[TUnion; 2]> = None;
    for atomic in union.types.iter() {
        let found: [TUnion; 2] = ancestor_type_arguments(codebase, atomic, result)?.try_into().ok()?;
        joined = Some(match joined {
            None => found,
            Some([value, error]) => [
                combine_union_types(&value, &found[0], codebase, CombinerOptions::default()),
                combine_union_types(&error, &found[1], codebase, CombinerOptions::default()),
            ],
        });
    }

    joined
}

/// The type arguments the value of type `atomic` gives the generic class `ancestor` when it is one of its objects, as
/// a `Result<int, PaymentError>.Ok` gives `Result` `int` and `PaymentError`, spec section 31. None for any other value,
/// and for an `ancestor` without type parameters.
pub(crate) fn ancestor_type_arguments(
    codebase: &CodebaseMetadata,
    atomic: &TAtomic,
    ancestor: &ClassLikeMetadata,
) -> Option<Vec<TUnion>> {
    let TAtomic::Object(TObject::Named(object)) = atomic else {
        return None;
    };
    let class = codebase.get_class_like(object.name.as_bytes())?;
    let is_generic_object =
        !ancestor.template_types.is_empty() && codebase.is_instance_of(class.name.as_bytes(), ancestor.name.as_bytes());

    is_generic_object
        .then(|| {
            ancestor.template_types.keys().map(|template| {
                get_specialized_template_type(codebase, *template, ancestor.name, class, object.get_type_parameters())
            })
        })?
        .collect()
}

/// `declared`, the type of a field of a case of `name`, a generic enum or one of its cases, with the enum's
/// `type_arguments` for its type parameters, which every case declares in the enum's order, as `Ok`'s `value` is an
/// `int` in a `Result<int, PaymentError>`. `declared` itself without them.
pub(crate) fn with_type_arguments(
    declared: &TUnion,
    name: Word,
    type_arguments: Option<&[TUnion]>,
    codebase: &CodebaseMetadata,
) -> TUnion {
    let Some((type_arguments, class)) = type_arguments.zip(codebase.get_class_like(name.as_bytes())) else {
        return declared.clone();
    };
    let r#enum = data_enum(codebase, class).and_then(|r#enum| codebase.get_class_like(r#enum.as_bytes()));
    let entities = r#enum
        .into_iter()
        .flat_map(|r#enum| std::iter::once(r#enum.name).chain(r#enum.permitted_inheritors.iter().flatten().copied()));
    let mut template_result = TemplateResult::default();
    for entity in entities {
        for (template, argument) in class.template_types.keys().zip(type_arguments) {
            template_result.add_lower_bound(*template, GenericParent::ClassLike(entity), argument.clone());
        }
    }

    inferred_type_replacer::replace(declared, &template_result, codebase)
}

/// Whether an error already refuses `expression`. `check_slice` refuses it when it failed to parse, it is `!x is T`, or
/// it tests or converts a value to a type with no [type test](mago_syntax::utils::pattern::has_type_test) in `is`, `as`
/// or a `match` arm. Its type is the error type in PHP# and `never` in PHP, a variable its `is` pattern names holds the
/// type written beside it, and it adds no other issue.
pub(crate) fn is_refused(expression: &Expression<'_>) -> bool {
    match expression {
        Expression::Error(_) => true,
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
fn tested_value<'ast, 'arena>(node: Node<'ast, 'arena>) -> &'ast Expression<'arena> {
    match node {
        Node::Expression(Expression::Is(is)) => is.value,
        Node::Expression(Expression::As(r#as)) => r#as.value,
        Node::Expression(Expression::PatternMatch(pattern_match))
        | Node::Statement(Statement::PatternMatch(pattern_match)) => pattern_match.expression,
        _ => unreachable!("only an `is`, an `as` and a `match` have the PHP of a pattern"),
    }
}

/// Whether `value_type` has a function whose source declares no PHP# type: a `Closure`, or the signature of a function
/// plain PHP wrote, as a plain PHP method named as a value. The engine finds no type on such a function.
fn has_undeclared_function(value_type: &TUnion) -> bool {
    value_type.types.iter().any(|atomic| match atomic {
        TAtomic::Callable(TCallable::Signature(signature)) => signature.effects.is_none(),
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
