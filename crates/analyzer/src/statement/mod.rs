use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;

use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::scanner::get_union_from_hint;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::array::keyed::TKeyedArray;
use mago_codex::ttype::atomic::array::list::TList;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::cast::cast_atomic_to_callable;
use mago_codex::ttype::combiner;
use mago_codex::ttype::combiner::CombinerOptions;
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
use mago_syntax::cst::UseItems;
use mago_syntax_core::stack::ensure_sufficient_stack;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::word;

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
use crate::statement::function_like::known_type_or_error;
use crate::statement::function_like::report_map_keys_without_backing_value;
use crate::statement::function_like::report_undefined_types;
use crate::statement::function_like::report_unsupported_set_elements;
use crate::utils::docblock::populate_docblock_variables;
use crate::utils::docblock::populate_docblock_variables_excluding;
use crate::utils::expression::expression_has_observable_side_effect;
use crate::utils::expression::get_block_expression_id;
use crate::utils::expression::get_function_like_id_from_call;
use crate::utils::expression::is_variable;
use crate::utils::misc::unwrap_expression;
use crate::utils::quoted_source;

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
                    Arc::make_mut(&mut context.scope).populate_from_use(r#use);
                    if context.dialect.is_sharp()
                        && let UseItems::Sequence(sequence) = &r#use.items
                    {
                        let imported_names = Arc::make_mut(&mut context.imported_names);
                        for item in &sequence.items {
                            let imported_name = item
                                .alias
                                .as_ref()
                                .map_or_else(|| item.name.last_segment(), |alias| alias.identifier.value);
                            imported_names.insert(ascii_lowercase_word(&php_name(&item.name)), word(imported_name));
                        }
                    }
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
                            context.scope = Arc::new(NamespaceScope::for_namespace(php_name(name)));
                        }
                        None => {
                            context.scope = Arc::new(NamespaceScope::global());
                        }
                    }

                    analyze_statements(namespace.statements().as_slice(), context, block_context, artifacts)
                }
                Statement::Class(class) => {
                    let class_name = context.resolved_names.get(&class.name);

                    Arc::make_mut(&mut context.scope).add(NameKind::Default, class_name, &None::<&str>);

                    class.analyze(context, block_context, artifacts)
                }
                Statement::Interface(interface) => {
                    let interface_name = context.resolved_names.get(&interface.name);

                    Arc::make_mut(&mut context.scope).add(NameKind::Default, interface_name, &None::<&str>);

                    interface.analyze(context, block_context, artifacts)
                }
                Statement::Trait(r#trait) => {
                    let trait_name = context.resolved_names.get(&r#trait.name);

                    Arc::make_mut(&mut context.scope).add(NameKind::Default, trait_name, &None::<&str>);

                    r#trait.analyze(context, block_context, artifacts)
                }
                Statement::Enum(r#enum) => {
                    let enum_name = context.resolved_names.get(&r#enum.name);

                    Arc::make_mut(&mut context.scope).add(NameKind::Default, enum_name, &None::<&str>);

                    r#enum.analyze(context, block_context, artifacts)
                }
                Statement::Event(event) => {
                    let event_name = context.resolved_names.get(&event.name);

                    Arc::make_mut(&mut context.scope).add(NameKind::Default, event_name, &None::<&str>);

                    event.analyze(context, block_context, artifacts)
                }
                Statement::Constant(constant) => {
                    for item in &constant.items {
                        let constant_item_name = context.resolved_names.get(&item.name);

                        Arc::make_mut(&mut context.scope).add(NameKind::Constant, constant_item_name, &None::<&str>);
                    }

                    constant.analyze(context, block_context, artifacts)
                }
                Statement::Function(function) => {
                    let function_name = context.resolved_names.get(&function.name);

                    Arc::make_mut(&mut context.scope).add(NameKind::Function, function_name, &None::<&str>);

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
                Statement::Extern(r#extern) => {
                    crate::effects::check::check_extern(context, r#extern);

                    Ok(())
                }
                Statement::Module(module) => module.analyze(context, block_context, artifacts),
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

    report_undefined_types(context, &local_type, hint.span());
    report_map_keys_without_backing_value(context, &local_type, hint.span());
    report_unsupported_set_elements(context, &local_type, hint.span());
    let local_type = (Rc::new(known_type_or_error(local_type)), hint.span());
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

    if let ForOfTarget::KeyValue(pair) = &for_of.target
        && artifacts
            .get_expression_type(for_of.expression)
            .is_some_and(|map_type| keys_mix_a_backed_enum(context, map_type))
    {
        report_keys_read_back(context, IssueCode::InvalidForeachKey, for_of.expression, "the loop", pair.key.span());
    }

    Ok(())
}

/// Whether the keys of the `Map`s that `map_type` may be, taken together, mix a backed enum with other types, as
/// `Status|int` or `Status|Rank` do. The engine holds each key of a `Map` as its backing value, and reads it back as a
/// case only through one enum.
pub(crate) fn keys_mix_a_backed_enum<A>(context: &Context<'_, '_, A>, map_type: &TUnion) -> bool
where
    A: Arena,
{
    let key_type = map_type.types.iter().fold(None, |key_type, atomic| match atomic {
        TAtomic::Array(map @ TArray::Keyed(_)) => {
            let key = get_array_parameters(map, context.codebase).0;
            Some(add_optional_union_type(key, key_type.as_ref(), context.codebase))
        }
        _ => key_type,
    });

    key_type.is_some_and(|key_type| key_type_mixes_a_backed_enum(context, &key_type))
}

/// Whether `key_type` mixes a backed enum with other types, as `Status|int` or `Status|Rank` do.
pub(crate) fn key_type_mixes_a_backed_enum<A>(context: &Context<'_, '_, A>, key_type: &TUnion) -> bool
where
    A: Arena,
{
    let mut keys = key_type.types.iter().filter(|atomic| !atomic.is_null());
    let is_one_enum = match keys.next() {
        Some(TAtomic::Object(TObject::Enum(first))) => {
            keys.all(|atomic| matches!(atomic, TAtomic::Object(TObject::Enum(other)) if other.name == first.name))
        }
        _ => false,
    };

    matches!(get_backing_key_type(key_type, context.codebase), Cow::Owned(_)) && !is_one_enum
}

/// Reports the `Map` `collection`, whose keys mix a backed enum with other types, where `reader`, the loop or `keys()`,
/// reads them back at `span`.
pub(crate) fn report_keys_read_back<A>(
    context: &mut Context<'_, '_, A>,
    code: IssueCode,
    collection: &Expression<'_>,
    reader: &str,
    span: Span,
) where
    A: Arena,
{
    let collection = quoted_source(context, collection.span());

    context.collector.report_with_code(
        code,
        Issue::error(format!(
            "The keys of `{collection}` mix a backed enum with other types, so {reader} cannot read them back."
        ))
        .with_annotation(Annotation::primary(span).with_message("Its key is read here."))
        .with_note(
            "The engine holds each key as its backing value, and reads it back as a case through one enum's `from`.",
        )
        .with_help("Key the `Map` by one backed enum alone."),
    );
}

/// Reports the `Map` at `collection`, whose keys mix a backed enum with other types, where `finder`, such as `has`,
/// an index or a literal, looks a key up or puts one in, as `annotation` marks it.
pub(crate) fn report_keys_sharing_entries<A>(
    context: &mut Context<'_, '_, A>,
    code: IssueCode,
    collection: Span,
    finder: &str,
    annotation: Annotation,
) where
    A: Arena,
{
    let collection = quoted_source(context, collection);

    context.collector.report_with_code(
        code,
        Issue::error(format!(
            "The keys of `{collection}` mix a backed enum with other types, so {finder} cannot tell them apart."
        ))
        .with_annotation(annotation)
        .with_note(
            "The engine holds each key as its backing value, so a case and the `int` or `string` key of its backing value share one entry.",
        )
        .with_help("Key the `Map` by one backed enum alone."),
    );
}

/// The type a written type would give a value: every literal and narrowed scalar widened, and every list or map shape
/// made a `List<T>` or `Map<TKey, TValue>` of any length. A `Set<T>` keeps its kind.
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
                    TArray::Set(_) => TArray::Set(value),
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
            TCallable::Signature(signature) => {
                context.codebase.get_declaring_function_like_identifier(&signature.get_source()?)
            }
        };

        check_target(identifier)
    })
}
