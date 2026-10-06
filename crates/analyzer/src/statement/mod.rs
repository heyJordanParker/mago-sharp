use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;

use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::scanner::get_union_from_hint;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::array::keyed::TKeyedArray;
use mago_codex::ttype::atomic::array::list::TList;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::cast::cast_atomic_to_callable;
use mago_codex::ttype::combiner;
use mago_codex::ttype::combiner::CombinerOptions;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::expander;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::get_backing_key_type;
use mago_codex::ttype::union::TUnion;
use mago_codex::ttype::union::populate_union_type;
use mago_names::binding::php_variable_name;
use mago_names::kind::NameKind;
use mago_names::scope::NamespaceScope;
use mago_names::scope::php_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Call;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::ExpressionStatement;
use mago_syntax::cst::ForOf;
use mago_syntax::cst::ForOfKeyValueTarget;
use mago_syntax::cst::ForOfTarget;
use mago_syntax::cst::ForOfVariable;
use mago_syntax::cst::Foreach;
use mago_syntax::cst::ForeachBody;
use mago_syntax::cst::ForeachKeyValueTarget;
use mago_syntax::cst::ForeachTarget;
use mago_syntax::cst::ForeachValueTarget;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::Node;
use mago_syntax::cst::Statement;
use mago_syntax_core::stack::ensure_sufficient_stack;
use mago_word::Word;

use crate::Context;
use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::analyze_php_shape;
use crate::expression::assignment::analyze_assignment;
use crate::plugin::HookAction;
use crate::plugin::context::HookContext;
use crate::statement::function_like::report_map_keys_without_backing_value;
use crate::utils::docblock::populate_docblock_variables;
use crate::utils::docblock::populate_docblock_variables_excluding;
use crate::utils::expression::expression_has_observable_side_effect;
use crate::utils::expression::get_block_expression_id;
use crate::utils::expression::get_function_like_id_from_call;
use crate::utils::expression::is_variable;
use crate::utils::misc::unwrap_expression;

pub mod attributes;
pub mod class_like;
pub mod constant;
pub mod echo;
pub mod function_like;
pub mod global;
pub mod r#if;
pub mod r#loop;
pub mod r#return;
pub mod r#static;
pub mod switch;
pub mod r#try;
pub mod unset;
pub mod use_statement;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Statement<'arena> {
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
            let last_statement_span = context.statement_span;
            context.statement_span = self.span();
            artifacts.record_variable_definedness(Node::Statement(self), block_context);

            // Call plugin before_statement hooks
            if context.plugin_registry.has_statement_hooks() {
                let mut hook_context = HookContext::new(context, block_context, artifacts);
                if context.plugin_registry.before_statement(self, &mut hook_context)? == HookAction::Skip {
                    for reported in hook_context.take_issues() {
                        context.collector.report_with_code(reported.code, reported.issue);
                    }

                    context.statement_span = last_statement_span;
                    return Ok(());
                }

                for reported in hook_context.take_issues() {
                    context.collector.report_with_code(reported.code, reported.issue);
                }
            }

            // For assignment statements, we populate all @var annotations except the one
            // for the assignment target variable. The assignment analyzer handles that one
            // to support the pattern: /** @var Type */ $var = something();
            if let Statement::Expression(ExpressionStatement { expression, .. }) = self
                && let Some(target_var) = get_block_expression_id(expression, context, block_context)
            {
                populate_docblock_variables_excluding(
                    context,
                    block_context,
                    artifacts,
                    true, // override existing for non-target variables
                    Some(target_var),
                );
            } else {
                let override_existing = !matches!(self, Statement::Foreach(_));

                populate_docblock_variables(context, block_context, artifacts, override_existing);
            }

            let result = match self {
                Statement::Inline(_)
                | Statement::OpeningTag(_)
                | Statement::Declare(_)
                | Statement::Noop(_)
                | Statement::ClosingTag(_)
                | Statement::HaltCompiler(_) => {
                    // ignore
                    Ok(())
                }
                Statement::Goto(_) | Statement::Label(_) => {
                    // not supported, unlikely to be supported
                    Ok(())
                }
                Statement::Use(r#use) => {
                    context.scope.populate_from_use(r#use);
                    if context.settings.check_use_statements {
                        r#use.analyze(context, block_context, artifacts)?;
                    }
                    if context.settings.check_name_casing {
                        crate::utils::casing::check_use_statement_casing(context, r#use);
                    }

                    Ok(())
                }
                Statement::Namespace(namespace) => {
                    match &namespace.name {
                        Some(name) => {
                            context.scope = NamespaceScope::for_namespace(php_name(name));
                        }
                        None => {
                            context.scope = NamespaceScope::global();
                        }
                    }

                    analyze_statements(namespace.statements().as_slice(), context, block_context, artifacts)
                }
                Statement::Class(class) => {
                    let class_name = context.resolved_names.get(&class.name);

                    context.scope.add(NameKind::Default, class_name, &None::<&str>);

                    class.analyze(context, block_context, artifacts)
                }
                Statement::Interface(interface) => {
                    let interface_name = context.resolved_names.get(&interface.name);

                    context.scope.add(NameKind::Default, interface_name, &None::<&str>);

                    interface.analyze(context, block_context, artifacts)
                }
                Statement::Trait(r#trait) => {
                    let trait_name = context.resolved_names.get(&r#trait.name);

                    context.scope.add(NameKind::Default, trait_name, &None::<&str>);

                    r#trait.analyze(context, block_context, artifacts)
                }
                Statement::Enum(r#enum) => {
                    let enum_name = context.resolved_names.get(&r#enum.name);

                    context.scope.add(NameKind::Default, enum_name, &None::<&str>);

                    r#enum.analyze(context, block_context, artifacts)
                }
                Statement::Constant(constant) => {
                    for item in &constant.items {
                        let constant_item_name = context.resolved_names.get(&item.name);

                        context.scope.add(NameKind::Constant, constant_item_name, &None::<&str>);
                    }

                    constant.analyze(context, block_context, artifacts)
                }
                Statement::Function(function) => {
                    let function_name = context.resolved_names.get(&function.name);

                    context.scope.add(NameKind::Function, function_name, &None::<&str>);

                    function.analyze(context, block_context, artifacts)
                }
                Statement::Block(block) => {
                    analyze_statements(block.statements.as_slice(), context, block_context, artifacts)
                }
                Statement::Expression(expression) => expression.expression.analyze(context, block_context, artifacts),
                Statement::LocalDeclaration(local_declaration) => {
                    local_declaration.analyze(context, block_context, artifacts)
                }
                Statement::Try(r#try) => r#try.analyze(context, block_context, artifacts),
                Statement::Foreach(foreach) => foreach.analyze(context, block_context, artifacts),
                Statement::For(r#for) => r#for.analyze(context, block_context, artifacts),
                Statement::ForOf(for_of) => analyze_for_of(for_of, context, block_context, artifacts),
                Statement::PatternMatch(_) => {
                    analyze_php_shape(Node::Statement(self), context, block_context, artifacts)
                }
                Statement::While(r#while) => r#while.analyze(context, block_context, artifacts),
                Statement::DoWhile(do_while) => do_while.analyze(context, block_context, artifacts),
                Statement::Continue(r#continue) => r#continue.analyze(context, block_context, artifacts),
                Statement::Break(r#break) => r#break.analyze(context, block_context, artifacts),
                Statement::If(r#if) => r#if.analyze(context, block_context, artifacts),
                Statement::Return(r#return) => r#return.analyze(context, block_context, artifacts),
                Statement::Echo(echo) => echo.analyze(context, block_context, artifacts),
                Statement::EchoTag(echo) => echo.analyze(context, block_context, artifacts),
                Statement::Global(global) => global.analyze(context, block_context, artifacts),
                Statement::Static(r#static) => r#static.analyze(context, block_context, artifacts),
                Statement::Unset(unset) => unset.analyze(context, block_context, artifacts),
                Statement::Switch(r#switch) => r#switch.analyze(context, block_context, artifacts),
                #[allow(clippy::unreachable)]
                _ => unreachable!("A statement variant was not handled in analyzer: {self:?}"),
            };

            result?;

            if let Statement::Expression(expression) = self
                && context.settings.find_unused_expressions
            {
                detect_unused_statement_expressions(expression.expression, self, context, artifacts);
            }

            // Call plugin after_statement hooks
            if context.plugin_registry.has_statement_hooks() {
                let mut hook_context = HookContext::new(context, block_context, artifacts);
                context.plugin_registry.after_statement(self, &mut hook_context)?;
                for reported in hook_context.take_issues() {
                    context.collector.report_with_code(reported.code, reported.issue);
                }
            }

            context.statement_span = last_statement_span;
            block_context.conditionally_referenced_variable_ids.clear();

            artifacts.record_static_local_types(block_context, context.codebase, context.settings.combiner_options());

            Ok(())
        })
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for LocalDeclaration<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        // A written type binds every value the local takes, and is its type after the declaration, as a `@var` tag
        // on the PHP assignment would make it.
        let variable_id = php_variable_name(self.name.value);
        let local_type = declare_local_type(context, block_context, artifacts, variable_id, self.hint);

        // A PHP# local declaration runs as the PHP assignment of its value to the variable it declares.
        let name =
            context.arena.alloc(Expression::ConstantAccess(ConstantAccess { name: Identifier::Local(self.name) }));

        analyze_assignment(
            context,
            block_context,
            artifacts,
            Some(self.name.span.join(self.value.span())),
            name,
            None,
            Some(self.value),
            None,
        )?;

        match local_type {
            Some((local_type, _)) => {
                block_context.locals.insert(variable_id, local_type);
            }
            // A local without a written type takes its first value's general type, as C#'s `var` and TypeScript's
            // `let` do. `null` and an empty literal have none, and the semantic checks refuse them.
            None => {
                if let Some(first_value) = block_context.locals.get(&variable_id)
                    && !first_value.is_null()
                    && !first_value
                        .types
                        .iter()
                        .any(|atomic| matches!(atomic, TAtomic::Array(array) if array.is_empty()))
                {
                    let general_type = get_general_type(first_value, context.codebase);
                    block_context.local_types.insert(variable_id, (Rc::new(general_type), self.value.span()));
                }
            }
        }

        Ok(())
    }
}

/// Binds the local `variable_id` to its written type, or unbinds it when none is written, and returns the written type
/// with its span.
fn declare_local_type<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &mut BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    variable_id: Word,
    hint: Option<&Hint<'_>>,
) -> Option<(Rc<TUnion>, Span)>
where
    A: Arena,
{
    let Some(hint) = hint else {
        block_context.local_types.remove(&variable_id);

        return None;
    };

    let mut local_type = get_union_from_hint(hint, block_context.scope.get_class_like_name(), context.resolved_names);
    populate_union_type(
        &mut local_type,
        &context.codebase.symbols,
        block_context.scope.get_reference_source().as_ref(),
        &mut artifacts.symbol_references,
        true,
    );
    expander::expand_union(
        context.codebase,
        &mut local_type,
        &TypeExpansionOptions { self_class: block_context.scope.get_class_like_name(), ..Default::default() },
    );

    let local_type = (Rc::new(local_type), hint.span());
    report_map_keys_without_backing_value(context, &local_type.0, local_type.1);
    block_context.local_types.insert(variable_id, local_type.clone());

    Some(local_type)
}

/// Analyzes a PHP# `for … of` as the PHP `foreach` over its collection, into the variables it declares. A written type
/// binds its loop variable as a typed local's does.
fn analyze_for_of<'ctx, 'arena, A>(
    for_of: &ForOf<'arena>,
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    for variable in for_of.target.variables() {
        declare_local_type(context, block_context, artifacts, php_variable_name(variable.name.value), variable.hint);
    }

    let variable = |variable: &ForOfVariable<'arena>| -> &'arena Expression<'arena> {
        context.arena.alloc(Expression::ConstantAccess(ConstantAccess { name: Identifier::Local(variable.name) }))
    };
    let target = match &for_of.target {
        ForOfTarget::Value(value) => ForeachTarget::Value(ForeachValueTarget { value: variable(value) }),
        ForOfTarget::KeyValue(pair) => ForeachTarget::KeyValue(ForeachKeyValueTarget {
            key: variable(&pair.key),
            double_arrow: pair.comma,
            value: variable(&pair.value),
        }),
    };
    let foreach = context.arena.alloc(Foreach {
        foreach: for_of.r#for,
        left_parenthesis: for_of.left_parenthesis,
        expression: for_of.expression,
        r#as: for_of.of,
        target,
        right_parenthesis: for_of.right_parenthesis,
        body: ForeachBody::Statement(for_of.body),
    });

    foreach.analyze(context, block_context, artifacts)?;

    if let ForOfTarget::KeyValue(pair) = &for_of.target {
        report_backed_key_without_its_enum(context, block_context, artifacts, for_of, pair);
    }

    Ok(())
}

/// Reports a loop over a `Map` keyed by a backed enum whose key's written type is not the enum's name. The engine
/// holds each key as its backing value, and reads it back as the case only through the class the loop names. A written
/// type that cannot hold the case is already refused where the loop assigns the key.
fn report_backed_key_without_its_enum<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &AnalysisArtifacts,
    for_of: &ForOf<'_>,
    pair: &ForOfKeyValueTarget<'_>,
) where
    A: Arena,
{
    let Some(collection_type) = artifacts.get_expression_type(for_of.expression) else {
        return;
    };
    let Some((key_type, value_type)) = collection_type.types.iter().find_map(|atomic| match atomic {
        TAtomic::Array(TArray::Keyed(keyed_array)) => keyed_array
            .get_generic_parameters()
            .filter(|(key_type, _)| matches!(get_backing_key_type(key_type, context.codebase), Cow::Owned(_))),
        _ => None,
    }) else {
        return;
    };

    let enum_name = match key_type.types.as_ref() {
        [TAtomic::Object(TObject::Enum(enum_object))] => Some(enum_object.name),
        _ => None,
    };
    if let (Some(Hint::Identifier(identifier)), Some(enum_name)) = (pair.key.hint, enum_name)
        && context.resolved_names.get(identifier).eq_ignore_ascii_case(enum_name.as_bytes())
    {
        return;
    }

    if let Some((written_type, _)) = block_context.local_types.get(&php_variable_name(pair.key.name.value))
        && !union_comparator::is_contained_by(
            context.codebase,
            key_type,
            written_type,
            false,
            false,
            false,
            &mut ComparisonResult::default(),
        )
    {
        return;
    }

    let source = |span: Span| {
        context
            .source_file
            .contents
            .get(span.start.offset as usize..span.end.offset as usize)
            .map(|text| String::from_utf8_lossy(text).into_owned())
            .unwrap_or_default()
    };
    let key_type_name = match enum_name {
        Some(enum_name) => {
            String::from_utf8_lossy(enum_name.as_bytes()).rsplit('\\').next().unwrap_or_default().to_owned()
        }
        None => key_type.get_id().to_string(),
    };
    let value_type_name = pair.value.hint.map_or_else(|| value_type.get_id().to_string(), |hint| source(hint.span()));
    let rewritten = format!(
        "for (const [{key_type_name} {}, {value_type_name} {}] of {})",
        BytesDisplay(pair.key.name.value),
        BytesDisplay(pair.value.name.value),
        source(for_of.expression.span()),
    );

    context.collector.report_with_code(
        IssueCode::InvalidForeachKey,
        Issue::error(format!("A `{key_type_name}` key needs its type written: `{rewritten}`."))
            .with_annotation(
                Annotation::primary(pair.key.span()).with_message(format!("Written without `{key_type_name}`.")),
            )
            .with_note("The engine holds each key as its backing value, and the loop reads it back as the case only through the class it names.")
            .with_help(format!("Write `{rewritten}`.")),
    );
}

/// The type a written type would give a value: every literal and narrowed scalar widened, and every list or map shape
/// made a `List<T>` or `Map<TKey, TValue>` of any length.
fn get_general_type(value: &TUnion, codebase: &CodebaseMetadata) -> TUnion {
    let mut widened = value.clone();
    widened.widen_scalars();

    let general = widened
        .types
        .iter()
        .map(|atomic| match atomic {
            TAtomic::Array(array) => {
                let (key, value) = get_array_parameters(array, codebase);
                let value = Arc::new(get_general_type(&value, codebase));

                TAtomic::Array(match array {
                    TArray::List(_) => TArray::List(TList::new(value)),
                    TArray::Keyed(_) => TArray::Keyed(TKeyedArray::new_with_parameters(
                        Arc::new(get_general_type(&key, codebase)),
                        value,
                    )),
                })
            }
            atomic => atomic.clone(),
        })
        .collect();

    TUnion::from_vec(combiner::combine(general, codebase, CombinerOptions::default()))
}

#[inline]
pub fn analyze_statements<'ctx, 'arena, A>(
    statements: &[Statement<'arena>],
    context: &mut Context<'ctx, 'arena, A>,
    block: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    for statement in statements {
        let is_declaration = statement.is_declaration();

        if block.flags.has_returned() {
            if context.settings.find_unused_expressions {
                let is_harmless = match &statement {
                    Statement::Break(_) => true,
                    Statement::Continue(_) => true,
                    Statement::Return(return_statement) => return_statement.value.is_none(),
                    _ => false,
                };

                if is_harmless {
                    context.collector.report_with_code(
                        IssueCode::UselessControlFlow,
                        Issue::help("This control flow is unnecessary")
                            .with_annotation(
                                Annotation::primary(statement.span()).with_message("This statement has no effect."),
                            )
                            .with_note("This statement is unreachable because the block has already returned.")
                            .with_help("Consider removing this statement as it does not do anything in this context."),
                    );
                } else if !is_declaration {
                    context.collector.report_with_code(
                        IssueCode::UnevaluatedCode,
                        Issue::help("Unreachable code detected.")
                            .with_annotation(Annotation::primary(statement.span()).with_message("This code will never be executed."))
                            .with_note("Execution cannot reach this point due to preceding code (e.g., return, throw, break, continue, exit, or an infinite loop).")
                            .with_help("Consider removing this unreachable code."),
                    );
                }
            }

            if !is_declaration && !context.settings.analyze_dead_code {
                continue;
            }
        }

        statement.analyze(context, block, artifacts)?;
    }

    Ok(())
}

/// Checks statement expressions for unused results or lack of side effects.
fn detect_unused_statement_expressions<'ast, 'arena, A>(
    expression: &'ast Expression<'arena>,
    statement: &'ast Statement<'arena>,
    context: &mut Context<'_, 'arena, A>,
    artifacts: &AnalysisArtifacts,
) where
    A: Arena,
{
    if let Some((issue_kind, name)) = has_unused_must_use(expression, context, artifacts) {
        context.collector.report_with_code(
            issue_kind,
            Issue::error(format!("The return value of '{name}' must be used."))
                .with_annotation(Annotation::primary(statement.span()).with_message("The result of this call is ignored"))
                .with_note(format!("The function or method '{name}' is marked with @must-use or #[NoDiscard], indicating its return value is important and should not be discarded."))
                .with_help("Assign the result to a variable, pass it to another function, or use it in an expression.")
        );

        return;
    }

    let useless_expression_message: &str = match expression {
        Expression::Literal(_) => "Evaluating a literal as a statement has no effect.",
        Expression::CompositeString(_) => "Evaluating a string as a statement has no effect.",
        Expression::Array(_) | Expression::LegacyArray(_) | Expression::List(_) => {
            "Creating an array or list as a statement has no effect."
        }
        _ if is_variable(expression, context.resolved_names) => "Accessing a variable as a statement has no effect.",
        Expression::ConstantAccess(_) => "Accessing a constant as a statement has no effect.",
        Expression::Identifier(_) => {
            "Using an identifier directly as a statement likely has no effect (perhaps a typo?)."
        }
        Expression::Access(_) => {
            "Accessing a property or constant as a statement might have no effect (unless it's meant to trigger a magic method call)."
        }
        Expression::AnonymousClass(_) => "Defining an anonymous class without assigning it has no effect.",
        Expression::Closure(_) | Expression::ArrowFunction(_) | Expression::PartialApplication(_) => {
            "Defining a closure or arrow function without assigning or calling it has no effect."
        }
        Expression::Parent(_) | Expression::Static(_) | Expression::Self_(_) => {
            "Using 'parent', 'static', or 'self' directly as a statement has no effect."
        }
        Expression::MagicConstant(_) => "Evaluating a magic constant as a statement has no effect.",
        Expression::Binary(binary) => {
            if (binary.operator.is_null_coalesce() || binary.operator.is_logical())
                && (expression_has_observable_side_effect(binary.rhs)
                    || call_may_have_observable_side_effect(binary.rhs, context, artifacts))
            {
                return;
            }

            "A binary operation used as a statement likely has no effect."
        }
        Expression::Call(Call::Function(FunctionCall { function, .. })) => {
            let Expression::Identifier(function_name) = function else {
                return;
            };

            let unqualified_name = function_name.value();
            let name = context.resolved_names.get(function_name);

            let Some(function) = context.codebase.get_function(name).or_else(|| {
                if function_name.is_local() { context.codebase.get_function(unqualified_name) } else { None }
            }) else {
                return;
            };

            // If the function has side effects, we don't report it as useless.
            if !function.flags.is_pure() {
                return;
            }

            // If the function does throw or has thrown types, we don't report it as useless.
            if !function.thrown_types.is_empty() || function.flags.has_throw() {
                return;
            }

            // If the function has parameters that are by reference, we don't report it as useless.
            if function.parameters.iter().any(|param| param.flags.is_by_reference()) {
                return;
            }

            "Calling a pure function without using its result has no effect (consider using the result or removing the call)."
        }
        _ => return,
    };

    context.collector.report_with_code(
        IssueCode::UnusedStatement,
        Issue::note("Expression has no effect as a statement")
            .with_annotation(Annotation::primary(expression.span()).with_message(useless_expression_message))
            .with_note("This expression does not produce a side effect or return value that is used.")
            .with_help(
                "To fix this, assign the value to a variable, return it, or remove the statement if it is truly unnecessary.",
            ),
    );
}

fn call_may_have_observable_side_effect<'arena, A>(
    expression: &Expression<'arena>,
    context: &Context<'_, 'arena, A>,
    artifacts: &AnalysisArtifacts,
) -> bool
where
    A: Arena,
{
    if let Expression::Parenthesized(parenthesized) = expression {
        return call_may_have_observable_side_effect(parenthesized.expression, context, artifacts);
    }

    let Expression::Call(call) = expression else {
        return false;
    };

    let Some(identifier) = get_function_like_id_from_call(call, context.resolved_names, &artifacts.expression_types)
    else {
        return true;
    };

    let Some(metadata) = context.codebase.get_function_like(&identifier) else {
        return true;
    };

    !metadata.flags.is_pure()
        || metadata.flags.has_throw()
        || !metadata.thrown_types.is_empty()
        || metadata.parameters.iter().any(|parameter| parameter.flags.is_by_reference())
}

/// Checks if an expression is a call to a `@must-use` function/method
/// and returns the appropriate issue kind and the name identifier if the result is unused.
fn has_unused_must_use<'arena, A>(
    expression: &Expression<'arena>,
    context: &Context<'_, 'arena, A>,
    artifacts: &AnalysisArtifacts,
) -> Option<(IssueCode, Word)>
where
    A: Arena,
{
    let Expression::Call(call_expression) = unwrap_expression(expression) else {
        return None;
    };

    let check_target = |identifier| {
        let (code, name) = match identifier {
            FunctionLikeIdentifier::Function(name) | FunctionLikeIdentifier::Closure(name) => {
                (IssueCode::UnusedFunctionCall, name)
            }
            FunctionLikeIdentifier::Method(_, name) => (IssueCode::UnusedMethodCall, name),
        };

        let metadata = context.codebase.get_function_like(&identifier)?;
        let must_use = metadata.flags.must_use()
            || metadata.attributes.iter().any(|attr| attr.name.as_bytes().eq_ignore_ascii_case(b"NoDiscard"));

        must_use.then_some((code, name))
    };

    if let Some(identifier) =
        get_function_like_id_from_call(call_expression, context.resolved_names, &artifacts.expression_types)
    {
        return check_target(identifier);
    }

    let Call::Function(FunctionCall { function, .. }) = call_expression else {
        return None;
    };

    artifacts.get_expression_type(function)?.types.iter().find_map(|atomic| {
        let callable = cast_atomic_to_callable(atomic, context.codebase, None)?;
        let identifier = match callable.as_ref() {
            TCallable::Alias(identifier) => *identifier,
            TCallable::Signature(signature) => signature.get_source()?,
        };

        check_target(identifier)
    })
}
