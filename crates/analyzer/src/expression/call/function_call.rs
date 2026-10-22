use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::callable::TCallableSignature;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::cast::cast_atomic_to_callable;
use mago_codex::ttype::expander::contains_parameter_variable;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::template::TemplateResult;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::NullSafeFunctionCall;
use mago_syntax::cst::NullSafePropertyAccess;
use mago_syntax::cst::PropertyAccess;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::access::analyze_through_value;
use crate::expression::call::analyze_invocation_targets;
use crate::expression::call::get_function_like_target;
use crate::expression::call::get_function_like_target_with_skip;
use crate::expression::is_refused_type;
use crate::expression::variable::read_variable;
use crate::invocation::InvocationArgumentsSource;
use crate::invocation::InvocationTarget;
use crate::plugin::ExpressionHookResult;
use crate::plugin::context::HookContext;
use crate::plugin::hook::NamedCall;
use crate::resolver::class_name::report_keyword_in_another_case;
use crate::resolver::extension::is_imported_by_default;
use crate::resolver::property::check_redundant_nullsafe;
use crate::statement::class_like::law::uncallable_law;
use crate::utils::expression::get_bare_name_variable_id;
use crate::utils::names::display_atomic;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_code_member;
use crate::utils::names::display_missing_imports;
use crate::utils::names::namespace_function;
use crate::utils::names::root_member_form;
use crate::utils::names::sharp_code_class_name;
use crate::utils::quoted_source;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for FunctionCall<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        // PHP# calls no function by its bare name, so a bare call whose name spells a keyword in another case, as
        // `Typeof(Order)`, is the keyword, spec section 2. Its arguments are the keyword's, so nothing else is checked.
        if let Expression::Identifier(name) = self.function
            && name.is_local()
            && get_bare_name_variable_id(name, context.resolved_names).is_none()
            && report_keyword_in_another_case(context, name.value(), name.span())
        {
            artifacts.set_expression_type(self, get_mixed());

            return Ok(());
        }

        let mut template_result = TemplateResult::default();
        let (invocation_targets, encountered_invalid_targets) =
            resolve_targets(context, block_context, artifacts, self.function, &mut template_result)?;
        // Spec section 23 refuses a bare call of a plain PHP function once its arguments are analyzed, so the form it
        // names fits the kind of collection a collection function is given.
        let bare_function = match self.function {
            Expression::Identifier(name)
                if context.dialect.is_sharp() && get_bare_name_variable_id(name, context.resolved_names).is_none() =>
            {
                invocation_targets
                    .first()
                    .and_then(InvocationTarget::get_function_like_metadata)
                    .map(|metadata| (metadata, name.span()))
            }
            _ => None,
        };

        analyze_invocation_targets(
            context,
            block_context,
            artifacts,
            template_result,
            invocation_targets,
            InvocationArgumentsSource::ArgumentList(&self.argument_list),
            self.span(),
            None,
            encountered_invalid_targets,
            false,
            false,
            false, // object_has_nullsafe_null - not applicable for function calls
        )?;
        if let Some((function, span)) = bare_function {
            report_bare_function_call(context, block_context, artifacts, function, Some(&self.argument_list), span);
        }

        if let Some(callee_type) = artifacts.get_rc_expression_type(self.function).cloned()
            && is_refused_type(&callee_type)
        {
            artifacts.set_rc_expression_type(self, callee_type);
        }

        Ok(())
    }
}

/// Analyzes PHP#'s `f?.(x)`, spec section 14.4, as a call of the function `f` holds when it is not null: the arguments
/// are checked against its signature, the result can also be `null`, and a function that is never null makes `?.`
/// redundant.
pub(super) fn analyze_null_safe_function_call<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    call: &NullSafeFunctionCall<'arena>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let was_inside_call = block_context.flags.inside_call();
    block_context.flags.set_inside_call(true);
    call.function.analyze(context, block_context, artifacts)?;
    block_context.flags.set_inside_call(was_inside_call);

    let function_type = artifacts.get_expression_type(call.function).cloned().unwrap_or_else(get_mixed);
    check_redundant_nullsafe(context, call.question_mark_dot, call.function, &function_type, true);

    analyze_through_value(
        context,
        block_context,
        artifacts,
        call.function,
        true,
        call.span(),
        |context, block_context, artifacts| {
            let mut template_result = TemplateResult::default();
            let (targets, encountered_invalid_targets) = resolve_callable_targets(
                context,
                &function_type.to_non_nullable(),
                call.function.span(),
                &mut template_result,
            );

            analyze_invocation_targets(
                context,
                block_context,
                artifacts,
                template_result,
                targets,
                InvocationArgumentsSource::ArgumentList(&call.argument_list),
                call.span(),
                None,
                encountered_invalid_targets,
                false,
                false,
                false,
            )
        },
    )
}

/// Analyzes `call` with `analyze`, between the function call hooks. A hook before it may skip the analysis and give
/// the call its type, and the hooks after it see the analyzed call.
pub(super) fn analyze_with_function_call_hooks<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    call: &NamedCall<'_, 'arena>,
    analyze: impl FnOnce(
        &mut Context<'ctx, 'arena, A>,
        &mut BlockContext<'ctx>,
        &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    if !context.plugin_registry.has_function_call_hooks() {
        return analyze(context, block_context, artifacts);
    }

    let mut hook_context = HookContext::new(context, block_context, artifacts);
    let result = context.plugin_registry.before_function_call(call, &mut hook_context)?;
    for reported in hook_context.take_issues() {
        context.collector.report_with_code(reported.code, reported.issue);
    }

    match result {
        ExpressionHookResult::Continue => {}
        ExpressionHookResult::Skip => {
            return Ok(());
        }
        ExpressionHookResult::SkipWithType(ty) => {
            artifacts.set_expression_type(&call.span, ty);
            return Ok(());
        }
    }

    analyze(context, block_context, artifacts)?;

    let mut hook_context = HookContext::new(context, block_context, artifacts);
    context.plugin_registry.after_function_call(call, &mut hook_context)?;
    for reported in hook_context.take_issues() {
        context.collector.report_with_code(reported.code, reported.issue);
    }

    Ok(())
}

/// Analyzes `Name.member(…)` as a call of the plain PHP function `function`, which
/// [`crate::utils::names::namespace_function_name`] found. Only a `.sharp` file writes the call.
pub(super) fn analyze_namespace_function_call<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    call: &MethodCall<'arena>,
    function: Word,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let target = get_function_like_target_with_skip(
        context,
        FunctionLikeIdentifier::Function(function),
        None,
        call.method.span(),
        None,
        block_context.known_functions.contains(&ascii_lowercase_word(function.as_bytes())),
    );
    let metadata = target.as_ref().and_then(InvocationTarget::get_function_like_metadata);
    if let Some(metadata) = metadata {
        check_function_use(context, block_context, metadata, function, call.method.span());
    }

    analyze_invocation_targets(
        context,
        block_context,
        artifacts,
        TemplateResult::default(),
        target.into_iter().collect(),
        InvocationArgumentsSource::ArgumentList(&call.argument_list),
        call.span(),
        None,
        false,
        false,
        false,
        false,
    )?;
    if let Some(metadata) = metadata {
        report_argument_by_reference(context, call, metadata);
    }

    Ok(())
}

/// Refuses a PHP# call that gives a plain PHP function an argument it takes by reference, as `Pdf.fill(pages)` for a
/// `fill(array &$pages)`, which would change `pages`: PHP# passes every argument by value. The standard library alone
/// passes one, to give its methods, as `Regex.match`, the results PHP gives only that way.
fn report_argument_by_reference<A>(
    context: &mut Context<'_, '_, A>,
    call: &MethodCall<'_>,
    metadata: &FunctionLikeMetadata,
) where
    A: Arena,
{
    if context.source_file.is_standard_library {
        return;
    }

    let parameters = &metadata.parameters;
    let Some(argument) = call.argument_list.arguments.iter().enumerate().find_map(|(index, argument)| {
        let parameter = match argument {
            Argument::Named(named) => parameters
                .iter()
                .find(|parameter| parameter.name.0.as_bytes().strip_prefix(b"$") == Some(named.name.value)),
            Argument::Positional(_) => {
                parameters.get(index).or_else(|| parameters.last().filter(|last| last.flags.is_variadic()))
            }
        }?;

        parameter.flags.is_by_reference().then(|| argument.value())
    }) else {
        return;
    };

    let written = quoted_source(context, call.object.span().join(call.method.span()));
    let given = quoted_source(context, argument.span());
    let message = format!(
        "`{written}` changes the variable it is given, and PHP# passes every argument by value, so `{given}` can't go there."
    );
    context.collector.report_with_code(
        IssueCode::InvalidPassByReference,
        Issue::error(message).with_annotation(Annotation::primary(argument.span()).with_message("Passed by value.")),
    );
}

pub(super) fn resolve_targets<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    expression: &Expression<'arena>,
    template_result: &mut TemplateResult,
) -> Result<(Vec<InvocationTarget<'ctx>>, bool), AnalysisError>
where
    A: Arena,
{
    if let Expression::Identifier(function_name) = expression
        && let Some(variable_id) = get_bare_name_variable_id(function_name, context.resolved_names)
    {
        // A PHP# call of a local, `f(x)`, calls the closure the local holds, as PHP's `$f($x)` does.
        let local_type = read_variable(context, block_context, artifacts, variable_id.as_bytes(), function_name.span());
        artifacts.set_rc_expression_type(expression, local_type);
    } else if let Expression::Identifier(function_name) = expression {
        let name = word(context.resolved_names.get(function_name));
        let unqualified_name = word(function_name.value());

        let identifier = FunctionLikeIdentifier::Function(name);
        let alternative = if function_name.is_local() && unqualified_name != name {
            Some(FunctionLikeIdentifier::Function(unqualified_name))
        } else {
            None
        };

        let lowercased_name = ascii_lowercase_word(name.as_bytes());
        let skip_error = block_context.known_functions.contains(&lowercased_name);
        let member = if context.dialect.is_sharp() {
            bare_member_call(context, block_context, function_name.value())
        } else {
            None
        };
        let imported = if context.dialect.is_sharp() && member.is_none() {
            imported_namespace_function(context, function_name.value())
        } else {
            None
        };

        let target = get_function_like_target_with_skip(
            context,
            identifier,
            alternative,
            expression.span(),
            None,
            skip_error || member.is_some() || imported.is_some(),
        )
        .or_else(|| {
            let imported = FunctionLikeIdentifier::Function(imported?);

            get_function_like_target_with_skip(context, imported, None, expression.span(), None, true)
        });

        if target.is_none()
            && !skip_error
            && let Some(member) = member
        {
            context.collector.report_with_code(
                IssueCode::NonExistentFunction,
                Issue::error(member)
                    .with_annotation(Annotation::primary(function_name.span()).with_message("Used here.")),
            );
        }

        if let Some(metadata) = target.as_ref().and_then(InvocationTarget::get_function_like_metadata) {
            check_function_use(context, block_context, metadata, name, function_name.span());
        }

        return Ok(if let Some(t) = target { (vec![t], false) } else { (vec![], false) });
    } else {
        let was_inside_call = block_context.flags.inside_call();
        block_context.flags.set_inside_call(true);
        expression.analyze(context, block_context, artifacts)?;
        block_context.flags.set_inside_call(was_inside_call);
    }

    let Some(expression_type) = artifacts.get_expression_type(expression) else {
        return Ok((vec![], false));
    };

    Ok(resolve_callable_targets(context, &expression_type.clone(), expression.span(), template_result))
}

/// Reports a call of `function` by `used_name` in a casing other than its declared one, and a call of a function marked
/// `@experimental`: the checks PHP's `name(…)` and PHP#'s `Pdf.render(…)` both get at `span`.
fn check_function_use<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    function: &FunctionLikeMetadata,
    used_name: Word,
    span: Span,
) where
    A: Arena,
{
    crate::utils::casing::check_function_casing_with_metadata(context, function, used_name, span);
    crate::utils::experimental::check_experimental_function_with_metadata(context, block_context, function, span);
}

/// The targets a call of a value of `expression_type` runs: the function a closure or a callable names, or the
/// callable's signature. Reports a value that cannot be called, and returns whether one was found.
pub(super) fn resolve_callable_targets<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    expression_type: &TUnion,
    span: Span,
    template_result: &mut TemplateResult,
) -> (Vec<InvocationTarget<'ctx>>, bool)
where
    A: Arena,
{
    let mut encountered_invalid_targets = false;
    let mut targets = vec![];
    for atomic in expression_type.types.as_ref() {
        if (atomic.is_null() && expression_type.ignore_nullable_issues())
            || (atomic.is_false() && expression_type.ignore_falsable_issues())
            || matches!(atomic, TAtomic::Error)
        {
            continue;
        }

        if let Some(callable) = cast_atomic_to_callable(atomic, context.codebase, Some(template_result)) {
            match callable.as_ref() {
                TCallable::Signature(callable_signature) => {
                    // An extension method's value takes the method's arguments after its receiver, spec section 14.3,
                    // so its own signature checks the call.
                    if let Some(id) = callable_signature.get_source()
                        && !context
                            .codebase
                            .get_function_like(&id)
                            .is_some_and(|metadata| metadata.flags.is_extension_method())
                        && let Some(target) = get_function_like_target(
                            context,
                            id,
                            None,
                            span,
                            if context
                                .codebase
                                .get_function_like(&context.codebase.get_declaring_function_like_identifier(&id))
                                .and_then(|metadata| metadata.return_type_metadata.as_ref())
                                .is_some_and(|metadata| contains_parameter_variable(&metadata.type_union))
                            {
                                None
                            } else {
                                callable_signature.return_type.clone()
                            },
                        )
                    {
                        targets.push(target);
                        continue;
                    }

                    targets.push(InvocationTarget::Callable {
                        signature: callable_signature.clone(),
                        effective_signature: None,
                        span,
                        source: callable_signature.get_source(),
                    });
                }
                TCallable::Alias(id) => {
                    if let Some(t) = get_function_like_target(context, *id, None, span, None) {
                        targets.push(t);
                    } else {
                        encountered_invalid_targets = true;
                    }
                }
            }
        } else if atomic.can_be_callable() {
            targets.push(InvocationTarget::Callable {
                signature: TCallableSignature::mixed(false),
                effective_signature: None,
                span,
                source: None,
            });
        } else if context.dialect.is_sharp() {
            // Spec sections 14.1 and 14.4: any expression that gives a function is called, and one that may be null is
            // called with `?.(…)`.
            let written = quoted_source(context, span);
            let message = if atomic.is_null() && expression_type.types.iter().any(|other| !other.is_null()) {
                format!("`{written}` may be null: call it as `{written}?.(…)`.")
            } else {
                format!(
                    "Only a function value is called with `(…)`, and `{written}` is `{}`.",
                    display_atomic(context, atomic)
                )
            };
            context.collector.report_with_code(
                IssueCode::InvalidCallable,
                Issue::error(message).with_annotation(Annotation::primary(span).with_message("Called here.")),
            );

            encountered_invalid_targets = true;
        } else {
            let type_name = display_atomic(context, atomic);

            context.collector.report_with_code(
                IssueCode::InvalidCallable,
                Issue::error(format!(
                    "Expression of type `{type_name}` cannot be called as a function or method.",
                ))
                .with_annotation(
                    Annotation::primary(span)
                        .with_message(format!("This expression (type `{type_name}` ) is not a valid callable"))
                )
                .with_note("To be callable, an expression must resolve to a function name (string), a Closure, an invocable object (object with `__invoke` method), or an array representing a static/instance method.")
                .with_help("Ensure the expression evaluates to a callable type. If it's a variable, check its assigned type. If it's a string, ensure it's a defined function name or valid callable array syntax.".to_string()),
            );

            encountered_invalid_targets = true;
        }
    }

    (targets, encountered_invalid_targets)
}

/// What a `.sharp` file writes in place of a plain PHP function or constant that PHP# replaces, decision 076.
enum Replacement {
    /// The standard library's members that do the function's work, or give the constant's value, each by its class and
    /// name: a static method, an extension member, a member of an effect class, or a class constant.
    Members(&'static [(&'static str, &'static str)]),
    /// As [`Replacement::Members`], but an extension member is written on the call's argument at this index instead of
    /// its first: `explode`'s second argument is the text `split` runs on.
    MembersOn(usize, &'static [(&'static str, &'static str)]),
    /// The PHP# syntax for it: `is` (spec section 21), `(string)` and `/` on two ints (section 24), a spread literal
    /// (section 12) or `for … of` (section 17). `{x}` stands for the call's first argument, `{a}` and `{b}` for its
    /// first two, and `{...}` for every argument, each spread.
    Syntax(&'static str),
    /// A collection method of spec section 12, by the kinds of collection, `List`, `Map` and `Set`, that have it: each
    /// group of kinds with the method written on the call's collection, with the placeholders of
    /// [`Replacement::Syntax`]. The placeholder the method is written on, `{a}` or `{b}`, is the collection.
    CollectionMethod(&'static [(&'static [&'static str], &'static str)]),
    /// Nothing, as PHP# has no counterpart of the function: what PHP# code does instead.
    Removed(&'static str),
}

/// Every kind of PHP# collection, spec section 12, for a method each of them has.
const EVERY_COLLECTION: &[&str] = &["List", "Map", "Set"];

/// Each plain PHP function and constant PHP# replaces or removes, with its replacement. A row that names a standard
/// library member lands in the change that ships the member, and a test holds every row to the library. A name that
/// ends in `*` is a family, every function whose name it begins, as `curl_*`, and a row of the function's whole name
/// wins over it. A member named `__construct` is its class's constructor, and a member named with `$` is its class's
/// property, as `$arguments`. A constant's row is its uppercase name, and its member the class constant that gives its
/// value.
const REPLACEMENTS: &[(&str, Replacement)] = &[
    // Spec section 8: `die("…")` prints its message and exits with status 0, which reports success.
    ("die", Replacement::Removed("write the message with `console.writeError(text)`, then `process.exit(1)`")),
    ("compact", Replacement::Removed("PHP# code reads and writes a local only by its name")),
    ("extract", Replacement::Removed("PHP# code reads and writes a local only by its name")),
    // `clone` copies an object without running its constructor, and spec section 16 copies a value with `with`.
    ("clone", Replacement::Removed("copy a value with `with`, as in `price with { amount: 400 }`")),
    ("is_string", Replacement::Syntax("`{x} is string`")),
    ("is_int", Replacement::Syntax("`{x} is int`")),
    ("is_float", Replacement::Syntax("`{x} is float`")),
    ("is_bool", Replacement::Syntax("`{x} is bool`")),
    ("strval", Replacement::Syntax("`(string){x}`")),
    ("intdiv", Replacement::Syntax("`{a} / {b}`")),
    ("array_merge", Replacement::Syntax("`[{...}]`")),
    ("array_replace", Replacement::Syntax("`[{...}]`")),
    ("array_walk_recursive", Replacement::Syntax("a `for … of` loop")),
    ("count", Replacement::CollectionMethod(&[(EVERY_COLLECTION, "{a}.count()")])),
    ("sizeof", Replacement::CollectionMethod(&[(EVERY_COLLECTION, "{a}.count()")])),
    ("array_filter", Replacement::CollectionMethod(&[(EVERY_COLLECTION, "{a}.filter({b})")])),
    ("array_map", Replacement::CollectionMethod(&[(EVERY_COLLECTION, "{b}.map({a})")])),
    ("array_any", Replacement::CollectionMethod(&[(EVERY_COLLECTION, "{a}.any({b})")])),
    ("array_find", Replacement::CollectionMethod(&[(EVERY_COLLECTION, "{a}.first({b})")])),
    (
        "in_array",
        Replacement::CollectionMethod(&[
            (&["List", "Set"], "{b}.contains({a})"),
            (&["Map"], "{b}.values().contains({a})"),
        ]),
    ),
    ("array_search", Replacement::CollectionMethod(&[(&["List"], "{b}.indexOf({a})")])),
    ("end", Replacement::CollectionMethod(&[(&["List"], "{a}.last()"), (&["Map"], "{a}.values().last()")])),
    ("array_push", Replacement::CollectionMethod(&[(&["List", "Set"], "{a}.add({b})")])),
    ("array_key_exists", Replacement::CollectionMethod(&[(&["Map"], "{b}.has({a})")])),
    ("key_exists", Replacement::CollectionMethod(&[(&["Map"], "{b}.has({a})")])),
    ("array_keys", Replacement::CollectionMethod(&[(&["Map"], "{a}.keys()")])),
    ("array_values", Replacement::CollectionMethod(&[(&["Map"], "{a}.values()")])),
    ("intval", Replacement::Members(&[("Sharp\\Int", "parse")])),
    ("floatval", Replacement::Members(&[("Sharp\\Float", "parse")])),
    (
        "filter_var",
        Replacement::Members(&[
            ("Sharp\\Bool", "tryParse"),
            ("Sharp\\Net\\Email", "isValid"),
            ("Sharp\\Net\\Ip", "isValid"),
            ("Sharp\\Net\\Url", "isValid"),
        ]),
    ),
    ("base64_encode", Replacement::Members(&[("Sharp\\Data\\Base64", "encode")])),
    ("base64_decode", Replacement::Members(&[("Sharp\\Data\\Base64", "decode")])),
    ("pack", Replacement::Members(&[("Sharp\\Data\\Binary", "pack")])),
    ("unpack", Replacement::Members(&[("Sharp\\Data\\Binary", "unpack")])),
    ("gzcompress", Replacement::Members(&[("Sharp\\Data\\Compression", "compress")])),
    ("gzuncompress", Replacement::Members(&[("Sharp\\Data\\Compression", "uncompress")])),
    ("hash", Replacement::Members(&[("Sharp\\Data\\Hash", "of")])),
    ("hash_hmac", Replacement::Members(&[("Sharp\\Data\\Hash", "hmac")])),
    ("md5", Replacement::Members(&[("Sharp\\Data\\Hash", "md5")])),
    ("sha1", Replacement::Members(&[("Sharp\\Data\\Hash", "sha1")])),
    ("crc32", Replacement::Members(&[("Sharp\\Data\\Hash", "crc32")])),
    ("hash_equals", Replacement::Members(&[("Sharp\\Data\\Hash", "equals")])),
    ("bin2hex", Replacement::Members(&[("Sharp\\Data\\Hex", "encode")])),
    ("hex2bin", Replacement::Members(&[("Sharp\\Data\\Hex", "decode")])),
    ("password_hash", Replacement::Members(&[("Sharp\\Data\\Password", "hash")])),
    ("password_verify", Replacement::Members(&[("Sharp\\Data\\Password", "verify")])),
    ("serialize", Replacement::Members(&[("Sharp\\Data\\PhpSerializer", "serialize")])),
    ("unserialize", Replacement::Members(&[("Sharp\\Data\\PhpSerializer", "unserialize")])),
    ("var_export", Replacement::Members(&[("Sharp\\Data\\PhpSerializer", "export")])),
    ("basename", Replacement::Members(&[("Sharp\\IO\\Path", "fileName")])),
    ("dirname", Replacement::Members(&[("Sharp\\IO\\Path", "directory")])),
    (
        "pathinfo",
        Replacement::Members(&[("Sharp\\IO\\Path", "extension"), ("Sharp\\IO\\Path", "fileNameWithoutExtension")]),
    ),
    ("json_decode", Replacement::Members(&[("Sharp\\Json\\Json", "decode")])),
    ("json_encode", Replacement::Members(&[("Sharp\\Json\\Json", "encode")])),
    ("ceil", Replacement::Members(&[("Sharp\\Math\\Math", "ceil")])),
    ("floor", Replacement::Members(&[("Sharp\\Math\\Math", "floor")])),
    ("sqrt", Replacement::Members(&[("Sharp\\Math\\Math", "sqrt")])),
    ("log", Replacement::Members(&[("Sharp\\Math\\Math", "log")])),
    ("round", Replacement::Members(&[("Sharp\\Math\\Math", "round")])),
    ("pow", Replacement::Members(&[("Sharp\\Math\\Math", "pow")])),
    ("max", Replacement::Members(&[("Sharp\\Math\\Math", "max")])),
    ("min", Replacement::Members(&[("Sharp\\Math\\Math", "min")])),
    ("inet_pton", Replacement::Members(&[("Sharp\\Net\\Ip", "toBinary")])),
    ("rawurlencode", Replacement::Members(&[("Sharp\\Net\\Url", "encode")])),
    ("urlencode", Replacement::Members(&[("Sharp\\Net\\Url", "encodeForm")])),
    ("urldecode", Replacement::Members(&[("Sharp\\Net\\Url", "decode")])),
    ("rawurldecode", Replacement::Members(&[("Sharp\\Net\\Url", "decode")])),
    ("http_build_query", Replacement::Members(&[("Sharp\\Net\\Url", "buildQuery")])),
    ("parse_str", Replacement::Members(&[("Sharp\\Net\\Url", "parseQuery")])),
    ("htmlspecialchars", Replacement::Members(&[("Sharp\\Text\\Html", "escape")])),
    ("htmlspecialchars_decode", Replacement::Members(&[("Sharp\\Text\\Html", "unescape")])),
    ("html_entity_decode", Replacement::Members(&[("Sharp\\Text\\Html", "decodeEntities")])),
    ("strip_tags", Replacement::Members(&[("Sharp\\Text\\Html", "stripTags")])),
    ("preg_match", Replacement::Members(&[("Sharp\\Text\\Regex", "matches"), ("Sharp\\Text\\Regex", "match")])),
    ("preg_match_all", Replacement::Members(&[("Sharp\\Text\\Regex", "matchAll")])),
    ("preg_replace", Replacement::Members(&[("Sharp\\Text\\Regex", "replace")])),
    ("preg_split", Replacement::Members(&[("Sharp\\Text\\Regex", "split")])),
    ("preg_grep", Replacement::Members(&[("Sharp\\Text\\Regex", "filter")])),
    ("preg_quote", Replacement::Members(&[("Sharp\\Text\\Regex", "escape")])),
    ("chr", Replacement::Members(&[("Sharp\\Text\\Text", "fromByte")])),
    ("trim", Replacement::Members(&[("Sharp\\Text\\Text", "trim")])),
    ("str_contains", Replacement::Members(&[("Sharp\\Text\\Text", "contains")])),
    ("strtolower", Replacement::Members(&[("Sharp\\Text\\Text", "lowercase")])),
    ("mb_strtolower", Replacement::Members(&[("Sharp\\Text\\Text", "lowercase")])),
    ("strtoupper", Replacement::Members(&[("Sharp\\Text\\Text", "uppercase")])),
    ("mb_strtoupper", Replacement::Members(&[("Sharp\\Text\\Text", "uppercase")])),
    ("str_starts_with", Replacement::Members(&[("Sharp\\Text\\Text", "startsWith")])),
    ("str_ends_with", Replacement::Members(&[("Sharp\\Text\\Text", "endsWith")])),
    ("str_replace", Replacement::MembersOn(2, &[("Sharp\\Text\\Text", "replace")])),
    ("explode", Replacement::MembersOn(1, &[("Sharp\\Text\\Text", "split")])),
    ("str_getcsv", Replacement::Members(&[("Sharp\\Text\\Text", "parseCsv")])),
    ("date", Replacement::Members(&[("Sharp\\Time\\Date", "format")])),
    ("gmdate", Replacement::Members(&[("Sharp\\Time\\Date", "format")])),
    ("strtotime", Replacement::Members(&[("Sharp\\Time\\Date", "parse")])),
    ("timezone_identifiers_list", Replacement::Members(&[("Sharp\\Time\\TimeZone", "names")])),
    ("getenv", Replacement::Members(&[("Sharp\\Environment", "variable"), ("Sharp\\Environment", "variables")])),
    ("apache_getenv", Replacement::Members(&[("Sharp\\Environment", "variable")])),
    ("apache_setenv", Replacement::Members(&[("Sharp\\Environment", "setVariable")])),
    ("getopt", Replacement::Members(&[("Sharp\\Environment", "$arguments")])),
    ("getcwd", Replacement::Members(&[("Sharp\\Environment", "currentDirectory")])),
    ("chdir", Replacement::Members(&[("Sharp\\Environment", "changeDirectory")])),
    ("time", Replacement::Members(&[("Sharp\\Clock", "now")])),
    ("gettimeofday", Replacement::Members(&[("Sharp\\Clock", "now")])),
    ("hrtime", Replacement::Members(&[("Sharp\\Clock", "timestamp")])),
    ("microtime", Replacement::Members(&[("Sharp\\Clock", "timestamp")])),
    ("sleep", Replacement::Members(&[("Sharp\\Clock", "sleep")])),
    ("usleep", Replacement::Members(&[("Sharp\\Clock", "sleep")])),
    ("time_nanosleep", Replacement::Members(&[("Sharp\\Clock", "sleep")])),
    ("time_sleep_until", Replacement::Members(&[("Sharp\\Clock", "sleep")])),
    ("random_int", Replacement::Members(&[("Sharp\\Random", "nextInt")])),
    ("rand", Replacement::Members(&[("Sharp\\Random", "nextInt")])),
    ("mt_rand", Replacement::Members(&[("Sharp\\Random", "nextInt")])),
    ("lcg_value", Replacement::Members(&[("Sharp\\Random", "nextFloat")])),
    ("random_bytes", Replacement::Members(&[("Sharp\\Random", "nextBytes")])),
    ("shuffle", Replacement::Members(&[("Sharp\\Random", "shuffle")])),
    ("str_shuffle", Replacement::Members(&[("Sharp\\Random", "shuffle")])),
    ("array_rand", Replacement::Members(&[("Sharp\\Random", "pick")])),
    ("printf", Replacement::Members(&[("Sharp\\Console", "write")])),
    ("vprintf", Replacement::Members(&[("Sharp\\Console", "write")])),
    ("print_r", Replacement::Members(&[("Sharp\\Console", "write")])),
    ("var_dump", Replacement::Members(&[("Sharp\\Console", "write")])),
    ("fwrite", Replacement::Members(&[("Sharp\\Console", "write"), ("Sharp\\Console", "writeError")])),
    ("fputs", Replacement::Members(&[("Sharp\\Console", "write"), ("Sharp\\Console", "writeError")])),
    ("fprintf", Replacement::Members(&[("Sharp\\Console", "write"), ("Sharp\\Console", "writeError")])),
    ("readline", Replacement::Members(&[("Sharp\\Console", "readLine")])),
    ("fgets", Replacement::Members(&[("Sharp\\Console", "readLine")])),
    ("exit", Replacement::Members(&[("Sharp\\Process", "exit")])),
    ("getmypid", Replacement::Members(&[("Sharp\\Process", "id")])),
    ("exec", Replacement::Members(&[("Sharp\\Process", "run")])),
    ("shell_exec", Replacement::Members(&[("Sharp\\Process", "run")])),
    ("system", Replacement::Members(&[("Sharp\\Process", "run")])),
    ("passthru", Replacement::Members(&[("Sharp\\Process", "run")])),
    ("proc_open", Replacement::Members(&[("Sharp\\Process", "run")])),
    ("popen", Replacement::Members(&[("Sharp\\Process", "run")])),
    ("file_get_contents", Replacement::Members(&[("Sharp\\Files", "read")])),
    ("readfile", Replacement::Members(&[("Sharp\\Files", "read")])),
    ("file", Replacement::Members(&[("Sharp\\Files", "readLines")])),
    ("file_put_contents", Replacement::Members(&[("Sharp\\Files", "write"), ("Sharp\\Files", "append")])),
    ("file_exists", Replacement::Members(&[("Sharp\\Files", "exists")])),
    ("is_file", Replacement::Members(&[("Sharp\\Files", "exists")])),
    ("is_readable", Replacement::Members(&[("Sharp\\Files", "exists")])),
    ("is_dir", Replacement::Members(&[("Sharp\\Files", "isDirectory")])),
    ("unlink", Replacement::Members(&[("Sharp\\Files", "delete")])),
    ("copy", Replacement::Members(&[("Sharp\\Files", "copy")])),
    ("rename", Replacement::Members(&[("Sharp\\Files", "move")])),
    ("scandir", Replacement::Members(&[("Sharp\\Files", "list")])),
    ("glob", Replacement::Members(&[("Sharp\\Files", "list")])),
    ("opendir", Replacement::Members(&[("Sharp\\Files", "list")])),
    ("readdir", Replacement::Members(&[("Sharp\\Files", "list")])),
    ("mkdir", Replacement::Members(&[("Sharp\\Files", "createDirectory")])),
    ("rmdir", Replacement::Members(&[("Sharp\\Files", "deleteDirectory")])),
    ("filesize", Replacement::Members(&[("Sharp\\Files", "size")])),
    ("filemtime", Replacement::Members(&[("Sharp\\Files", "lastModified")])),
    ("filectime", Replacement::Members(&[("Sharp\\Files", "lastModified")])),
    ("fileatime", Replacement::Members(&[("Sharp\\Files", "lastModified")])),
    ("stat", Replacement::Members(&[("Sharp\\Files", "lastModified")])),
    ("tempnam", Replacement::Members(&[("Sharp\\Files", "temporaryFile")])),
    ("sys_get_temp_dir", Replacement::Members(&[("Sharp\\Files", "temporaryFile")])),
    ("fopen", Replacement::Members(&[("Sharp\\Files", "readLinesLazily"), ("Sharp\\Files", "append")])),
    ("fread", Replacement::Members(&[("Sharp\\Files", "read"), ("Sharp\\Files", "readLinesLazily")])),
    ("fclose", Replacement::Members(&[("Sharp\\Files", "readLinesLazily")])),
    ("feof", Replacement::Members(&[("Sharp\\Files", "readLinesLazily")])),
    ("fseek", Replacement::Members(&[("Sharp\\Files", "readLinesLazily")])),
    ("ftell", Replacement::Members(&[("Sharp\\Files", "readLinesLazily")])),
    ("rewind", Replacement::Members(&[("Sharp\\Files", "readLinesLazily")])),
    ("flock", Replacement::Members(&[("Sharp\\Files", "write"), ("Sharp\\Files", "append")])),
    ("ftruncate", Replacement::Members(&[("Sharp\\Files", "write"), ("Sharp\\Files", "append")])),
    ("fflush", Replacement::Members(&[("Sharp\\Files", "write"), ("Sharp\\Files", "append")])),
    ("tmpfile", Replacement::Members(&[("Sharp\\Files", "temporaryFile")])),
    ("curl_*", Replacement::Members(&[("Sharp\\Http", "send")])),
    ("fsockopen", Replacement::Members(&[("Sharp\\Http", "send")])),
    ("stream_socket_client", Replacement::Members(&[("Sharp\\Http", "send")])),
    ("mail", Replacement::Members(&[("Sharp\\Mail", "send")])),
    ("mb_send_mail", Replacement::Members(&[("Sharp\\Mail", "send")])),
    ("mysqli_query", Replacement::Members(&[("Sharp\\Database", "query"), ("Sharp\\Database", "execute")])),
    ("mysqli_real_query", Replacement::Members(&[("Sharp\\Database", "query"), ("Sharp\\Database", "execute")])),
    ("mysqli_multi_query", Replacement::Members(&[("Sharp\\Database", "execute")])),
    ("mysqli_execute_query", Replacement::Members(&[("Sharp\\Database", "query"), ("Sharp\\Database", "execute")])),
    ("mysqli_prepare", Replacement::Members(&[("Sharp\\Database", "query"), ("Sharp\\Database", "execute")])),
    ("mysqli_stmt_execute", Replacement::Members(&[("Sharp\\Database", "query"), ("Sharp\\Database", "execute")])),
    ("mysqli_fetch_all", Replacement::Members(&[("Sharp\\Database", "query")])),
    ("mysqli_fetch_assoc", Replacement::Members(&[("Sharp\\Database", "queryFirst")])),
    ("mysqli_fetch_array", Replacement::Members(&[("Sharp\\Database", "queryFirst")])),
    ("mysqli_fetch_row", Replacement::Members(&[("Sharp\\Database", "queryFirst")])),
    ("mysqli_fetch_object", Replacement::Members(&[("Sharp\\Database", "queryFirst")])),
    ("mysqli_fetch_column", Replacement::Members(&[("Sharp\\Database", "scalar")])),
    ("mysqli_affected_rows", Replacement::Members(&[("Sharp\\Database", "execute")])),
    ("mysqli_begin_transaction", Replacement::Members(&[("Sharp\\Database", "beginTransaction")])),
    ("mysqli_autocommit", Replacement::Members(&[("Sharp\\Database", "beginTransaction")])),
    ("mysqli_commit", Replacement::Members(&[("Sharp\\Database", "commit")])),
    ("mysqli_rollback", Replacement::Members(&[("Sharp\\Database", "rollback")])),
    ("mysqli_connect", Replacement::Members(&[("Sharp\\Database", "__construct")])),
    ("mysqli_real_connect", Replacement::Members(&[("Sharp\\Database", "__construct")])),
    ("mysqli_init", Replacement::Members(&[("Sharp\\Database", "__construct")])),
    ("mysqli_*", Replacement::Members(&[("Sharp\\Database", "query"), ("Sharp\\Database", "execute")])),
    ("pg_connect", Replacement::Members(&[("Sharp\\Database", "__construct")])),
    ("pg_pconnect", Replacement::Members(&[("Sharp\\Database", "__construct")])),
    ("pg_fetch_all", Replacement::Members(&[("Sharp\\Database", "query")])),
    ("pg_fetch_all_columns", Replacement::Members(&[("Sharp\\Database", "query")])),
    ("pg_select", Replacement::Members(&[("Sharp\\Database", "query")])),
    ("pg_fetch_assoc", Replacement::Members(&[("Sharp\\Database", "queryFirst")])),
    ("pg_fetch_array", Replacement::Members(&[("Sharp\\Database", "queryFirst")])),
    ("pg_fetch_row", Replacement::Members(&[("Sharp\\Database", "queryFirst")])),
    ("pg_fetch_object", Replacement::Members(&[("Sharp\\Database", "queryFirst")])),
    ("pg_fetch_result", Replacement::Members(&[("Sharp\\Database", "scalar")])),
    ("pg_affected_rows", Replacement::Members(&[("Sharp\\Database", "execute")])),
    ("pg_cmdtuples", Replacement::Members(&[("Sharp\\Database", "execute")])),
    ("pg_insert", Replacement::Members(&[("Sharp\\Database", "execute")])),
    ("pg_update", Replacement::Members(&[("Sharp\\Database", "execute")])),
    ("pg_delete", Replacement::Members(&[("Sharp\\Database", "execute")])),
    ("pg_*", Replacement::Members(&[("Sharp\\Database", "query"), ("Sharp\\Database", "execute")])),
    ("apcu_fetch", Replacement::Members(&[("Sharp\\Cache", "get")])),
    ("apcu_exists", Replacement::Members(&[("Sharp\\Cache", "get")])),
    ("apcu_key_info", Replacement::Members(&[("Sharp\\Cache", "get")])),
    ("apcu_store", Replacement::Members(&[("Sharp\\Cache", "set")])),
    ("apcu_add", Replacement::Members(&[("Sharp\\Cache", "set")])),
    ("apcu_delete", Replacement::Members(&[("Sharp\\Cache", "remove")])),
    ("apcu_entry", Replacement::Members(&[("Sharp\\Cache", "getOrCreate")])),
    ("apcu_*", Replacement::Members(&[("Sharp\\Cache", "get"), ("Sharp\\Cache", "set"), ("Sharp\\Cache", "remove")])),
    ("putenv", Replacement::Members(&[("Sharp\\Environment", "setVariable")])),
    ("mt_srand", Replacement::Members(&[("Sharp\\Random", "__construct")])),
    ("srand", Replacement::Members(&[("Sharp\\Random", "__construct")])),
    ("PHP_INT_MAX", Replacement::Members(&[("Sharp\\Int", "MAX")])),
    ("PHP_INT_MIN", Replacement::Members(&[("Sharp\\Int", "MIN")])),
];

/// Each PHP built-in class the standard library replaces, spec section 29, with the standard library members to write
/// in place of creating one. A static member comes before an effect class's, which names the object it is called on.
const WRAPPED_CLASSES: &[(&str, &[(&str, &str)])] = &[
    ("PDO", &[("Sharp\\Database", "query"), ("Sharp\\Database", "execute")]),
    ("PDOStatement", &[("Sharp\\Database", "query"), ("Sharp\\Database", "execute")]),
    ("mysqli", &[("Sharp\\Database", "query"), ("Sharp\\Database", "execute")]),
    ("SQLite3", &[("Sharp\\Database", "query"), ("Sharp\\Database", "execute")]),
    ("DateTime", &[("Sharp\\Time\\Date", "parse"), ("Sharp\\Clock", "now")]),
    ("DateTimeImmutable", &[("Sharp\\Time\\Date", "parse"), ("Sharp\\Clock", "now")]),
    ("DateTimeZone", &[("Sharp\\Time\\Date", "format")]),
    ("DateInterval", &[("Sharp\\Time\\Duration", "ofSeconds"), ("Sharp\\Time\\Duration", "ofMilliseconds")]),
    ("CurlHandle", &[("Sharp\\Http", "send")]),
    ("CurlMultiHandle", &[("Sharp\\Http", "send")]),
    ("SplFileObject", &[("Sharp\\Files", "read"), ("Sharp\\Files", "readLines")]),
    ("SplFileInfo", &[("Sharp\\Files", "exists"), ("Sharp\\Files", "size"), ("Sharp\\Files", "lastModified")]),
    ("DirectoryIterator", &[("Sharp\\Files", "list")]),
    ("FilesystemIterator", &[("Sharp\\Files", "list")]),
    ("RecursiveDirectoryIterator", &[("Sharp\\Files", "list")]),
    ("Random\\Randomizer", &[("Sharp\\Random", "nextInt")]),
];

/// Spec section 29: a `.sharp` file outside the standard library may name a class [`WRAPPED_CLASSES`] lists, but not
/// create one, with `new` or a static factory, which is a static method that returns the class. The refusal names the
/// members to write instead, and the code references each of them, so a change to one reaches it.
pub(crate) fn report_wrapped_class<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    class: &ClassLikeMetadata,
    factory: Option<&FunctionLikeMetadata>,
    span: Span,
) where
    A: Arena,
{
    if !context.dialect.is_sharp() || context.source_file.is_standard_library {
        return;
    }
    let Some((_, members)) =
        WRAPPED_CLASSES.iter().find(|(wrapped, _)| wrapped.as_bytes().eq_ignore_ascii_case(class.name.as_bytes()))
    else {
        return;
    };
    if let Some(method) = factory
        && !method.return_type_metadata.as_ref().is_some_and(|returned| {
            returned.type_union.types.iter().any(|atomic| {
                matches!(atomic, TAtomic::Object(TObject::Named(named))
                    if named.name.as_bytes().eq_ignore_ascii_case(class.name.as_bytes()))
            })
        })
    {
        return;
    }
    for (member_class, method) in *members {
        artifacts.symbol_references.add_reference_to_class_member(
            &block_context.scope,
            (ascii_lowercase_word(member_class.as_bytes()), ascii_lowercase_word(method.as_bytes())),
            false,
        );
    }
    // A project without the standard library has no member to name, and creating one is refused all the same.
    let form = member_form(context, members, 0, None).map_or_else(|| ".".to_owned(), |form| format!(": write {form}"));
    let name = display_class_like_name(context, class.original_name);

    context.collector.report_with_code(
        IssueCode::WrappedClass,
        Issue::error(format!("Create no `{name}`{form}"))
            .with_annotation(Annotation::primary(span).with_message("Created here.")),
    );
}

/// Spec section 23: PHP# calls a plain PHP function through its namespace, so a bare call of one is refused with the
/// form to write: the replacement [`REPLACEMENTS`] gives it, or the call through its namespace, as [`namespace_form`]
/// writes it.
pub(crate) fn report_bare_function_call<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    function: &FunctionLikeMetadata,
    arguments: Option<&ArgumentList<'_>>,
    span: Span,
) where
    A: Arena,
{
    let name = function.original_name;
    let (code, message) = replacement_of(name)
        .and_then(|replacement| replacement_refusal(context, block_context, artifacts, name, replacement, arguments))
        .unwrap_or_else(|| (IssueCode::BareFunctionCall, namespace_form(context, function)));

    context.collector.report_with_code(
        code,
        Issue::error(message).with_annotation(Annotation::primary(span).with_message("Called here.")),
    );
}

/// Spec sections 8 and 23: a bare read of a plain PHP constant the standard library replaces, `name`, is refused at
/// `span` with the member to write, as a bare call of a replaced function is. The standard library, whose members are
/// made of those reads, keeps them, and a row matches the constant's whole name only, so a family of functions such
/// as `curl_*` never takes PHP's `CURL_…` constants. Returns whether it reported one.
pub(crate) fn report_replaced_constant<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    name: &[u8],
    span: Span,
) -> bool
where
    A: Arena,
{
    if context.source_file.is_standard_library {
        return false;
    }
    let Some((_, replacement)) = REPLACEMENTS.iter().find(|(replaced, _)| replaced.as_bytes() == name) else {
        return false;
    };
    let Some((code, message)) = replacement_refusal(context, block_context, artifacts, word(name), replacement, None)
    else {
        return false;
    };
    context.collector.report_with_code(
        code,
        Issue::error(message).with_annotation(Annotation::primary(span).with_message("Read here.")),
    );

    true
}

/// The row of [`REPLACEMENTS`] that replaces the plain PHP function `name`: the row of its whole name, or else the row
/// of the family its name begins.
fn replacement_of(name: Word) -> Option<&'static Replacement> {
    let in_family = |replaced: &str| {
        replaced.strip_suffix('*').is_some_and(|family| {
            name.as_bytes().get(..family.len()).is_some_and(|start| start.eq_ignore_ascii_case(family.as_bytes()))
        })
    };

    REPLACEMENTS
        .iter()
        .find(|(replaced, _)| replaced.as_bytes().eq_ignore_ascii_case(name.as_bytes()))
        .or_else(|| REPLACEMENTS.iter().find(|(replaced, _)| in_family(replaced)))
        .map(|(_, replacement)| replacement)
}

/// The refusal of a call of `name`, which `replacement` replaces, as its code and message. A member the codebase lacks
/// is never named, so a row whose members it lacks gives none, and the call references each member the row names, so
/// a change to one reaches the call.
fn replacement_refusal<A>(
    context: &Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    name: Word,
    replacement: &Replacement,
    arguments: Option<&ArgumentList<'_>>,
) -> Option<(IssueCode, String)>
where
    A: Arena,
{
    let wrapped = |form: String| {
        (IssueCode::WrappedFunction, format!("`{name}` is wrapped by the standard library: write {form}"))
    };

    match replacement {
        Replacement::Syntax(form) => Some((
            IssueCode::ReplacedBySyntax,
            format!("`{name}` is replaced by PHP# syntax: write {}.", syntax_form(context, form, arguments)),
        )),
        Replacement::CollectionMethod(groups) => {
            Some(wrapped(format!("{}.", collection_form(context, artifacts, groups, arguments))))
        }
        Replacement::Removed(instead) => {
            Some((IssueCode::WrappedFunction, format!("PHP# has no `{name}`: {instead}.")))
        }
        Replacement::Members(members) | Replacement::MembersOn(_, members) => {
            for (class, member) in *members {
                let class = ascii_lowercase_word(class.as_bytes());
                if member.starts_with('$') {
                    artifacts.symbol_references.add_reference_for_property_read(
                        &block_context.scope,
                        class,
                        word(member.as_bytes()),
                    );
                } else if context.codebase.class_constant_exists(class.as_bytes(), member.as_bytes()) {
                    artifacts.symbol_references.add_reference_to_class_member(
                        &block_context.scope,
                        (class, word(member.as_bytes())),
                        false,
                    );
                } else {
                    artifacts.symbol_references.add_reference_to_class_member(
                        &block_context.scope,
                        (class, ascii_lowercase_word(member.as_bytes())),
                        false,
                    );
                }
            }

            let receiver = if let Replacement::MembersOn(receiver, _) = replacement { *receiver } else { 0 };

            member_form(context, members, receiver, arguments).map(wrapped)
        }
    }
}

/// The standard library members `members` names, as a `.sharp` file writes them, with the imports the file lacks. An
/// extension's member, spec section 26, is written on the call's argument at the index `receiver` from `arguments`, and
/// needs an import only where the file lacks its extensions by default. An effect class's member, spec section 29, is
/// written on the object of that class a constructor takes, as in `clock.now()`, and a member named with `$` is its
/// property, as in `environment.arguments`. None when the codebase declares none of them.
fn member_form<A>(
    context: &Context<'_, '_, A>,
    members: &[(&str, &str)],
    receiver: usize,
    arguments: Option<&ArgumentList<'_>>,
) -> Option<String>
where
    A: Arena,
{
    let codebase = context.codebase;
    let receiver = arguments
        .and_then(|arguments| arguments.arguments.get(receiver))
        .and_then(|argument| argument_text(context, argument))
        .unwrap_or_else(|| "x".to_owned());
    let mut taken: Vec<String> = Vec::new();
    let (classes, methods): (Vec<Option<Word>>, Vec<String>) = members
        .iter()
        .filter_map(|(class, member)| {
            let class = codebase.get_class_like(class.as_bytes())?;
            let mut on_taken_object = |member: String| {
                let class_name = sharp_code_class_name(codebase, &context.imported_names, &context.scope, class.name);
                let object = lower_first(class_name.rsplit('.').next().unwrap_or_default());
                let held = format!("`{class_name}`");
                if !taken.contains(&held) {
                    taken.push(held);
                }

                (Some(class.original_name), format!("`{object}.{member}`"))
            };
            if let Some(property) = member.strip_prefix('$') {
                codebase.get_property(class.name.as_bytes(), member.as_bytes())?;

                return Some(on_taken_object(property.to_owned()));
            }
            if codebase.class_constant_exists(class.name.as_bytes(), member.as_bytes()) {
                return Some((
                    Some(class.original_name),
                    format!("`{}`", display_code_member(context, class.original_name, member)),
                ));
            }
            let method = codebase.get_method(class.name.as_bytes(), member.as_bytes())?;
            let name = method.original_name;
            let import = (!is_imported_by_default(context, class.name, method)).then_some(class.original_name);
            let arguments = |taken_first: usize| if method.parameters.len() > taken_first { "…" } else { "" };

            Some(if method.flags.is_extension_property() {
                (import, format!("`{receiver}.{name}`"))
            } else if method.flags.is_extension_method() {
                (import, format!("`{receiver}.{name}({})`", arguments(1)))
            } else if method.method_metadata.as_ref().is_some_and(|metadata| metadata.is_constructor) {
                // A `foreign` object is created only in a module binding, spec section 32.
                let class_name = sharp_code_class_name(codebase, &context.imported_names, &context.scope, class.name);
                let place = if class.flags.is_foreign() { " in a module binding" } else { "" };

                (Some(class.original_name), format!("`new {class_name}({})`{place}", arguments(0)))
            } else if method.method_metadata.as_ref().is_none_or(|metadata| metadata.is_static) {
                (Some(class.original_name), format!("`{}(…)`", display_code_member(context, class.original_name, name)))
            } else {
                on_taken_object(format!("{name}({})", arguments(0)))
            })
        })
        .unzip();
    let classes: Vec<Word> = classes.into_iter().flatten().collect();
    let (last, others) = methods.split_last()?;
    let methods = if others.is_empty() { last.clone() } else { format!("{} or {last}", others.join(", ")) };
    let taken =
        if taken.is_empty() { String::new() } else { format!(" on the {} your constructor takes", taken.join(" or ")) };
    let imports = display_missing_imports(context, classes).map(|imports| format!(" {imports}")).unwrap_or_default();

    Some(format!("{methods}{taken}.{imports}"))
}

/// `name` with its first letter in lowercase, the name PHP# code gives a local or a parameter of the class `name`.
fn lower_first(name: &str) -> String {
    let mut letters = name.chars();

    letters.next().map(|first| first.to_lowercase().chain(letters).collect()).unwrap_or_default()
}

/// The collection method `groups` gives a call, spec section 12: the method of the group that holds every kind, or the
/// kind of the call's collection, which the analyzed type of its argument gives, written on that collection. Otherwise
/// each group's method on a collection `x`, with the kinds that have it, so the call's collection is never written into
/// a method its kind lacks.
fn collection_form<A>(
    context: &Context<'_, '_, A>,
    artifacts: &AnalysisArtifacts,
    groups: &[(&[&str], &str)],
    arguments: Option<&ArgumentList<'_>>,
) -> String
where
    A: Arena,
{
    let receiver = groups.first().map_or(0, |(_, form)| usize::from(form.starts_with("{b}")));
    let kind = arguments
        .and_then(|arguments| arguments.arguments.get(receiver))
        .and_then(|argument| artifacts.get_expression_type(argument.value()))
        .and_then(collection_kind);
    let chosen =
        groups.iter().find(|(kinds, _)| *kinds == EVERY_COLLECTION || kind.is_some_and(|kind| kinds.contains(&kind)));

    match chosen {
        Some((_, form)) => format!("`{}`", collection_call(context, form, receiver, arguments)),
        None => groups
            .iter()
            .map(|(kinds, form)| {
                let kinds: Vec<String> = kinds.iter().map(|kind| format!("a `{kind}`")).collect();

                format!("`{}` on {}", collection_call(context, form, receiver, None), kinds.join(" or "))
            })
            .collect::<Vec<_>>()
            .join(", or "),
    }
}

/// The collection method `form` written with the call's arguments: each that is a name or a member read as written,
/// and each other one as `…`, the collection as `x`. A call whose arguments the method does not all take is written
/// with none of them, so a dropped argument never reads as kept.
fn collection_call<A>(
    context: &Context<'_, '_, A>,
    form: &str,
    receiver: usize,
    arguments: Option<&ArgumentList<'_>>,
) -> String
where
    A: Arena,
{
    let taken = if form.contains("{b}") { 2 } else { 1 };
    let arguments = arguments.filter(|arguments| arguments.arguments.len() <= taken);
    let written = |index: usize| {
        arguments
            .and_then(|arguments| arguments.arguments.get(index))
            .and_then(|argument| argument_text(context, argument))
            .unwrap_or_else(|| if index == receiver { "x" } else { "…" }.to_owned())
    };

    form.replace("{a}", &written(0)).replace("{b}", &written(1))
}

/// The kind of PHP# collection, `List`, `Map` or `Set`, every type of `union` is, if one is.
fn collection_kind(union: &TUnion) -> Option<&'static str> {
    let kind = |atomic: &TAtomic| match atomic {
        TAtomic::Array(TArray::List(_)) => Some("List"),
        TAtomic::Array(TArray::Keyed(_)) => Some("Map"),
        TAtomic::Array(TArray::Set(_)) => Some("Set"),
        _ => None,
    };
    let first = kind(union.types.first()?)?;

    union.types.iter().all(|atomic| kind(atomic) == Some(first)).then_some(first)
}

/// The PHP# syntax `form` with each argument that is a name or a member read written into it.
fn syntax_form<A>(context: &Context<'_, '_, A>, form: &str, arguments: Option<&ArgumentList<'_>>) -> String
where
    A: Arena,
{
    let written: Vec<Option<String>> = arguments
        .map(|arguments| arguments.arguments.iter().map(|argument| argument_text(context, argument)).collect())
        .unwrap_or_default();
    let argument = |index: usize| written.get(index).cloned().flatten().unwrap_or_else(|| placeholder(index));
    let spread: Vec<String> = (0..written.len()).map(|index| format!("...{}", argument(index))).collect();

    form.replace("{x}", &written.first().cloned().flatten().unwrap_or_else(|| "x".to_owned()))
        .replace("{a}", &argument(0))
        .replace("{b}", &argument(1))
        .replace("{...}", &spread.join(", "))
}

/// The call of the plain PHP function `function` through its namespace: `Pdf.render(…)` for one inside a namespace,
/// and `Foundation.abort(…)` for one a library declares in PHP's root namespace, as [`root_member_form`] names it, by
/// the name the file imports the namespace under, with the import the file lacks. One of PHP's built-ins belongs to no
/// namespace PHP# code imports, so the refusal says to call it from a plain PHP class.
fn namespace_form<A>(context: &Context<'_, '_, A>, function: &FunctionLikeMetadata) -> String
where
    A: Arena,
{
    let name = function.original_name.as_bytes();
    let (call, import) = match name.iter().rposition(|byte| *byte == b'\\') {
        Some(separator) => {
            let namespace = word(&name[..separator]);
            let written = sharp_code_class_name(context.codebase, &context.imported_names, &context.scope, namespace);

            (
                format!("{written}.{}", String::from_utf8_lossy(&name[separator + 1..])),
                display_missing_imports(context, [namespace]),
            )
        }
        None => match root_member_form(context, name, function.span) {
            Some(form) => form,
            None => {
                return format!(
                    "`{}` belongs to no namespace PHP# code imports: call it from a plain PHP class.",
                    function.original_name
                );
            }
        },
    };
    let import = import.map(|import| format!(" {import}")).unwrap_or_default();

    format!("A plain PHP function is called through its namespace: write `{call}(…)`.{import}")
}

/// The name a form gives the argument at `index` when it cannot write the argument itself: `a`, `b`, `c` and on
/// through the alphabet, then `argument27` and on.
fn placeholder(index: usize) -> String {
    u8::try_from(index)
        .ok()
        .filter(|index| *index < 26)
        .map_or_else(|| format!("argument{}", index + 1), |index| char::from(b'a' + index).to_string())
}

/// The source text of a positional argument that is one name or a member read, such as `total` or `order.total`.
fn argument_text<A>(context: &Context<'_, '_, A>, argument: &Argument<'_>) -> Option<String>
where
    A: Arena,
{
    if !argument.is_positional() || argument.is_unpacked() {
        return None;
    }

    let mut receiver = argument.value();
    loop {
        receiver = match receiver {
            Expression::ConstantAccess(_) => break,
            Expression::Access(
                Access::Property(PropertyAccess { object, property: ClassLikeMemberSelector::Identifier(_), .. })
                | Access::NullSafeProperty(NullSafePropertyAccess {
                    object,
                    property: ClassLikeMemberSelector::Identifier(_),
                    ..
                }),
            ) => object,
            _ => return None,
        };
    }

    Some(String::from_utf8_lossy(&context.source_file.contents[argument.value().span().to_range_usize()]).into_owned())
}

/// Spec section 4 writes every member as `this.m()` or `Class.m()`, so a bare call is the global function's. When the
/// enclosing class declares a method of that name, returns the message that names the member call to write. When it
/// states a law of that name, spec section 28, a law's body writes the call through the class name, as a static
/// method's, and any other code is refused the call, for the call that finds no function.
fn bare_member_call<A>(context: &Context<'_, '_, A>, block_context: &BlockContext<'_>, name: &[u8]) -> Option<String>
where
    A: Arena,
{
    let class = block_context.scope.get_class_like()?;
    if let Some(law) = class.laws.get(&ascii_lowercase_word(name)) {
        return Some(if block_context.flags.inside_law_body() {
            format!(
                "Write `{}()`: a static method reaches the members of its class through the class name.",
                display_code_member(context, class.original_name, law.original_name)
            )
        } else {
            uncallable_law(context, class, law, law.span).message
        });
    }
    let method = context.codebase.get_method(class.name.as_bytes(), name)?;

    Some(if block_context.scope.is_static() {
        format!(
            "Write `{}()`: a static method reaches the members of its class through the class name.",
            display_code_member(context, class.original_name, method.original_name)
        )
    } else {
        format!("Write `this.{}()`: members of the same object are always written with `this.`.", method.original_name)
    })
}

/// Spec section 23: after `import Vendor.Pdf;`, a bare `render(html)` that finds no function means the namespace's
/// `render`, which the call is refused as and checked against. Returns its name when an imported namespace declares
/// `name`.
fn imported_namespace_function<A>(context: &Context<'_, '_, A>, name: &[u8]) -> Option<Word>
where
    A: Arena,
{
    context
        .imported_names
        .keys()
        .find_map(|imported| Some(namespace_function(context.codebase, *imported, name)?.original_name))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::borrow::Cow;
    use std::path::Path;

    use foldhash::HashSet;
    use indoc::indoc;

    use mago_allocator::LocalArena;
    use mago_codex::metadata::CodebaseMetadata;
    use mago_codex::populator::populate_codebase;
    use mago_codex::reference::SymbolReferences;
    use mago_codex::scanner::scan_program;
    use mago_database::file::File;
    use mago_names::resolver::NameResolver;
    use mago_php_version::PHPVersion;
    use mago_syntax::parser::parse_file;
    use mago_word::WordSet;

    use super::REPLACEMENTS;
    use super::Replacement;
    use super::WRAPPED_CLASSES;
    use crate::code::IssueCode;
    use crate::test_analysis;

    /// A bare call names the standard library members its row of [`REPLACEMENTS`] lists, creating a class names those
    /// of its row of [`WRAPPED_CLASSES`], and `member_form` drops a member the codebase lacks. So every listed member
    /// must be one the library declares, or the checker would name
    /// less than its table says. The library is `sharp/composer/library`, beside this tree's `sharp/mago`. The test's
    /// walk of the library and its failure message run only when the library changes shape, so coverage leaves it out.
    #[test]
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn every_member_a_replacement_names_is_declared_by_the_standard_library() {
        let arena = LocalArena::new();
        let mut codebase = CodebaseMetadata::new();
        let mut folders = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../composer/library")];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).expect("the standard library sits beside sharp/mago") {
                let path = entry.expect("a readable library entry").path();
                if path.is_dir() {
                    folders.push(path);
                } else if path.extension().is_some_and(|extension| extension == "sharp") {
                    let name = path.to_string_lossy().into_owned().into_bytes();
                    let file =
                        File::ephemeral(Cow::Owned(name), Cow::Owned(std::fs::read(&path).expect("a library file")));
                    let program = parse_file(&arena, &file);
                    let names = NameResolver::new(&arena).resolve(program);
                    codebase.extend(scan_program(&arena, &file, program, &names, PHPVersion::LATEST));
                }
            }
        }
        populate_codebase(&mut codebase, &mut SymbolReferences::new(), WordSet::default(), HashSet::default());

        let missing: Vec<String> = REPLACEMENTS
            .iter()
            .flat_map(|(function, replacement)| match replacement {
                Replacement::Members(members) | Replacement::MembersOn(_, members) => {
                    members.iter().map(|member| (*function, *member)).collect()
                }
                Replacement::Syntax(_) | Replacement::CollectionMethod(_) | Replacement::Removed(_) => Vec::new(),
            })
            .chain(WRAPPED_CLASSES.iter().flat_map(|(class, members)| members.iter().map(|member| (*class, *member))))
            .filter(|(_, (class, member))| {
                codebase.get_class_like(class.as_bytes()).is_none_or(|metadata| {
                    let class = metadata.name.as_bytes();
                    if member.starts_with('$') {
                        codebase.get_property(class, member.as_bytes()).is_none()
                    } else {
                        codebase.get_method(class, member.as_bytes()).is_none()
                            && !codebase.class_constant_exists(class, member.as_bytes())
                    }
                })
            })
            .map(|(function, (class, method))| format!("{function}: {class}.{method}"))
            .collect();

        assert_eq!(missing, Vec::<String>::new());
    }

    test_analysis! {
        name = call_simple_addition,
        code = indoc! {"
            <?php

            function add(int $a, int $b): int {
                return $a + $b;
            }

            $result = add(5, 10);
        "}
    }

    test_analysis! {
        name = call_string_concat,
        code = indoc! {r#"
            <?php

            function concatenate(string $s1, string $s2): string {
                return $s1 . $s2;
            }

            $greeting = concatenate("Hello, ", "World!");
        "#}
    }

    test_analysis! {
        name = call_function_returning_void,
        code = indoc! {r#"
            <?php

            function print_message(string $message): void {
                echo $message;
            }

            print_message("Test message");
        "#}
    }

    test_analysis! {
        name = call_with_optional_parameter_provided,
        code = indoc! {r#"
            <?php

            function greet(string $name, string $greeting = "Hello"): string {
                return $greeting . ", " . $name . "!";
            }

            $message = greet("Alice", "Hi");
        "#}
    }

    test_analysis! {
        name = call_with_optional_parameter_omitted,
        code = indoc! {r#"
            <?php

            function greet(string $name, string $greeting = "Hello"): string {
                return $greeting . ", " . $name . "!";
            }

            $message = greet("Bob");
        "#}
    }

    test_analysis! {
        name = call_with_function_result_as_argument,
        code = indoc! {r#"
            <?php

            function get_number(): int {
                return 42;
            }

            function display_number(int $num): void {
                echo "Number is: " . $num;
            }

            display_number(get_number());
        "#}
    }

    test_analysis! {
        name = call_wrong_argument_type_int_for_string,
        code = indoc! {"
            <?php

            function needs_string(string $s): void {
                echo $s;
            }

            needs_string(123);
        "},
        issues = [IssueCode::InvalidArgument]
    }

    test_analysis! {
        name = call_wrong_argument_type_string_for_int,
        code = indoc! {r#"
            <?php

            function needs_int(int $i): void {}

            needs_int("hello");
        "#},
        issues = [
            IssueCode::InvalidArgument,
        ]
    }

    test_analysis! {
        name = call_too_few_arguments,
        code = indoc! {"
            <?php

            function requires_two(int $a, int $b): void {}

            requires_two(1);
        "},
        issues = [
            IssueCode::TooFewArguments,
        ]
    }

    test_analysis! {
        name = call_too_many_arguments,
        code = indoc! {"
            <?php

            function accepts_one(int $a): void {}

            accepts_one(1, 2);
        "},
        issues = [
            IssueCode::TooManyArguments,
        ]
    }

    test_analysis! {
        name = call_null_for_non_nullable_string,
        code = indoc! {"
            <?php

            function needs_string(string $s): void {}

            needs_string(null);
        "},
        issues = [
            IssueCode::NullArgument,
        ]
    }

    test_analysis! {
        name = call_nullable_for_non_nullable_string,
        code = indoc! {"
            <?php

            function needs_string(string $s): void {}
            function get_string_or_null(): string|null {
                return get_string_or_null();
            }

            needs_string(get_string_or_null());
        "},
        issues = [
            IssueCode::PossiblyNullArgument,
        ]
    }

    test_analysis! {
        name = call_false_for_non_falsable_string,
        code = indoc! {"
            <?php

            function needs_string(string $s): void {}

            needs_string(false);
        "},
        issues = [
            IssueCode::FalseArgument,
        ]
    }

    test_analysis! {
        name = call_falsable_for_non_falsable_string,
        code = indoc! {"
            <?php

            function needs_string(string $s): void {}
            function get_string_or_false(): string|false {
                return get_string_or_false();
            }

            needs_string(get_string_or_false());
        "},
        issues = [
            IssueCode::PossiblyFalseArgument,
        ]
    }

    test_analysis! {
        name = call_unknown_named_argument,

        code = indoc!{r#"
            <?php

            function known_params(int $a, string $b): void {}

            known_params(a: 1, c: "test");
        "#},
        issues = [
            IssueCode::InvalidNamedArgument,
        ]
    }

    test_analysis! {
        name = call_callable_param_type_mismatch,
        code = indoc! {r#"
            <?php

            /**
             * @param (callable(string): void) $cb
             */
            function needs_callable_string_param(callable $cb): void {
                $cb("hello");
            }

            /**
             * @param (callable(int): void) $arg_cb
             */
            function needs_callable_int_param(callable $arg_cb): void {
                needs_callable_string_param($arg_cb); // invalid callable type
            }
        "#},
        issues = [
            IssueCode::InvalidArgument
        ]
    }

    test_analysis! {
        name = call_callable_return_type_mismatch,
        code = indoc! {r#"
            <?php

            /**
             * @param (callable(string): int) $cb
             */
            function needs_callable_return_int(callable $cb): void {
                $val = $cb("test");

                echo $val;
            }

            /**
             * @param (callable(string): string) $arg_cb
             */
            function main_callable_return_string(callable $arg_cb): void {
                needs_callable_return_int($arg_cb);
            }
        "#},
        issues = [IssueCode::InvalidArgument]
    }

    test_analysis! {
        name = call_callable_too_few_params_in_arg,
        code = indoc! {r#"
            <?php

            /**
             * @param (callable(string, int): void) $cb
             */
            function needs_callable_two_params(callable $cb): void {
                $cb("hello", 1);
            }

            /**
             * @param (callable(string): void) $arg_cb
             */
            function main_callable_one_param(callable $arg_cb): void {
                needs_callable_two_params($arg_cb);
            }
        "#}
    }

    test_analysis! {
        name = call_callable_too_many_params_in_arg,
        code = indoc! {r#"
            <?php

            /**
             * @param (callable(string): void) $cb
             */
            function needs_callable_one_param(callable $cb): void {
                $cb("hello");
            }

            /**
             * @param (callable(string, int): void) $arg_cb
             */
            function main_callable_two_params(callable $arg_cb): void {
                needs_callable_one_param($arg_cb);
            }
        "#},
        issues = [IssueCode::PossiblyInvalidArgument]
    }

    test_analysis! {
        name = call_array_element_type_mismatch,
        code = indoc! {"
            <?php

            /**
             * @param list<string> $_list_of_strings
             */
            function needs_list_of_strings(array $_list_of_strings): void {}

            /**
             * @param list<int> $arg_list_of_ints
             */
            function main_list_of_ints(array $arg_list_of_ints): void {
                needs_list_of_strings($arg_list_of_ints);
            }

            main_list_of_ints([1, 2, 3]);
        "},
        issues = [IssueCode::InvalidArgument]
    }

    test_analysis! {
        name = call_keyed_array_value_mismatch,
        code = indoc! {r#"
            <?php

            /**
             * @param array<string, int> $_map_to_int
             */
            function needs_map_to_int(array $_map_to_int): void {}

            /**
             * @param array<string, string> $arg_map_to_string
             */
            function main_map_to_string(array $arg_map_to_string): void {
                needs_map_to_int($arg_map_to_string);
            }

            main_map_to_string(["a" => "apple", "b" => "banana"]);
        "#},
        issues = [IssueCode::InvalidArgument]
    }

    test_analysis! {
        name = call_non_empty_list_constraint_violation,
        code = indoc! {"
            <?php

            /**
             * @param non-empty-list<int> $_nel
             */
            function needs_non_empty_list(array $_nel): void {}

            /**
             * @param list<int> $arg_list Can be empty
             */
            function main_list_can_be_empty(array $arg_list): void {
                needs_non_empty_list($arg_list);
            }

            main_list_can_be_empty([]); // Definitely empty
        "},
        issues = [IssueCode::PossiblyInvalidArgument]
    }

    test_analysis! {
        name = call_non_empty_array_key_type_mismatch,
        code = indoc! {"
            <?php

            /**
             * @param non-empty-array<string, int> $_nea
             */
            function needs_non_empty_array_string_keys(array $_nea): void {}

            /**
             * @param non-empty-array<int, int> $arg_array_int_keys
             */
            function main_array_int_keys(array $arg_array_int_keys): void {
                needs_non_empty_array_string_keys($arg_array_int_keys);
            }

            main_array_int_keys([0 => 1, 1 => 2]);
        "},
        issues = [IssueCode::InvalidArgument]
    }

    test_analysis! {
        name = call_union_param_invalid_type,
        code = indoc! {"
            <?php

            /**
             * @param int|string $_value
             */
            function needs_int_or_string(mixed $_value): void {}

            /**
             * @param bool $arg_bool
             */
            function main_bool_arg(bool $arg_bool): void {
                needs_int_or_string($arg_bool);
            }

            main_bool_arg(true);
        "},
        issues = [IssueCode::InvalidArgument]
    }

    test_analysis! {
        name = call_template_callable_param_mismatch,
        code = indoc!{"
            <?php

            /**
             * @template T
             * @param (callable(T): int) $cb
             * @param T $val
             */
            function process_with_template(callable $cb, mixed $val): int {
                return $cb($val);
            }

            /**
             * @param (callable(string): int) $string_cb
             */
            function main_template_callable_mismatch(callable $string_cb): void {
                process_with_template($string_cb, 123); // T is int from 123, but $string_cb expects string
            }

            function string_to_int(string $s): int {
                return (int) $s;
            }

            main_template_callable_mismatch(
                string_to_int(...)
            );
        "},
        issues = [IssueCode::PossiblyInvalidArgument]
    }

    test_analysis! {
        name = map_twice,
        code = indoc!{"
            <?php

            /**
             * @template T1
             * @template T2
             * @template T3
             * @param (Closure(T2):T3) $c2
             * @param (Closure(T1):T2) $c1
             * @param list<T1> $a
             * @return list<T3>
             */
            function maptwice(Closure $c2, Closure $c1, array $a): array
            {
                $res = [];
                foreach ($a as $v) {
                    $res[] = $c2($c1($v));
                }
                return $res;
            }

            /**
             * @param list<array{'a': string, 'b': int}> $input
             * @return list<int>
             */
            function foo(array $input): array
            {
                return maptwice(
                    static function (int $b): int {
                        return $b + 1;
                    },
                    /**
                     * @param array{'b': int, ...} $in
                     */
                    static function (array $in): int {
                        return $in['b'];
                    },
                    $input,
                );
            }
        "}
    }

    test_analysis! {
        name = call_with_generic_constraints,
        code = indoc!{"
            <?php

            /**
             * @template TKey as array-key
             * @template TValue
             * @param array<TKey, TValue> $_array
             * @param array ...$_arrays
             * @return array<TKey, TValue>
             */
            function array_intersect(array $_array, array ...$_arrays): array
            {
                exit();
            }

            /**
             * @template K of array-key
             * @template V
             * @param iterable<K, V> $iterable
             * @return array<K, V>
             */
            function keyed_array_from_iterable(iterable $iterable): array
            {
                $dict = [];
                foreach ($iterable as $key => $value) {
                    $dict[$key] = $value;
                }
                return $dict;
            }

            /**
             * @template T
             * @template R
             * @param iterable<T> $iterable
             * @param (callable(T): R) $callback
             * @return list<R>
             */
            function map_list(iterable $iterable, callable $callback): array
            {
                $result = [];
                foreach ($iterable as $value) {
                    $result[] = $callback($value);
                }

                return $result;
            }

            /**
             * @template Tk of array-key
             * @template Tv
             * @param iterable<Tk, Tv> $first
             * @param iterable<Tk, mixed> $second
             * @param list<iterable<Tk, mixed>> $rest
             * @return array<Tk, Tv>
             */
            function intersect(iterable $first, iterable $second, iterable ...$rest): array
            {
                return array_intersect(
                    keyed_array_from_iterable($first),
                    keyed_array_from_iterable($second),
                    ...map_list(
                        $rest,
                        /**
                         * @param iterable<Tk, Tv> $iterable
                         * @return array<Tk, Tv>
                         */
                        static fn(iterable $iterable): array => keyed_array_from_iterable($iterable),
                    ),
                );
            }
        "},
    }

    test_analysis! {
        name = conditional_returns,
        code = indoc!{"
            <?php

            /**
             * @template Input as int
             *
             * @param Input $a
             *
             * @return (Input is 1 ? 2 : (
             *    Input is 2 ? 3 : (
             *      Input is 3 ? 4 : (
             *        Input is not 4 ? (
             *            Input is 5 ? 6 : int
             *        ) : 5
             *      )
             *    )
             * ))
             */
            function add_one(int $a): int {
                return $a + 1;
            }

            /**
             * @param 20 $_
             */
            function i_take_20(int $_): void {}

            $a = add_one(1); // 2
            $b = add_one(2); // 3
            $c = add_one(3); // 4
            $d = add_one(4); // 5
            $e = add_one(5); // 6

            $f = $a + $b + $c + $d + $e; // 5 + 6 + 7 + 8 + 9 = 20

            i_take_20($f); // no error, $f is 20
        "},
    }

    test_analysis! {
        name = call_capture_groups,
        code = indoc! {"
            <?php

            /**
             * @template-covariant T
             */
            interface TypeInterface
            {
            }

            /**
             * @template Tk of array-key
             * @template Tv
             * @param iterable<Tk, Tv> $iterable
             * @return array<Tk, Tv>
             */
            function dict_unique(iterable $iterable): array
            {
                return dict_unique($iterable);
            }

            /**
             * @template Tk as array-key
             * @template Tv
             * @param iterable<Tk> $keys
             * @param (Closure(Tk): Tv) $value_func
             * @return array<Tk, Tv>
             */
            function dict_from_keys(iterable $keys, Closure $value_func): array
            {
                return dict_from_keys($keys, $value_func);
            }

            /**
             * @template Tk of array-key
             * @template Tv
             * @param array<Tk, TypeInterface<Tv>> $elements
             * @return TypeInterface<array<Tk, Tv>>
             */
            function shape_type(array $elements, bool $allow_unknown_fields = false): TypeInterface
            {
                return shape_type($elements, $allow_unknown_fields);
            }

            /**
             * @return TypeInterface<string>
             */
            function string_type(): TypeInterface
            {
                return string_type();
            }

            /**
             * @param list<array-key> $groups
             * @return TypeInterface<array<array-key, string>>
             */
            function capture_groups(array $groups): TypeInterface
            {
                return shape_type(dict_from_keys(
                    dict_unique([0, ...$groups]),
                    /**
                     * @return TypeInterface<string>
                     */
                    static fn($_): TypeInterface => string_type(),
                ));
            }

            /**
             * @param list<array-key> $groups
             * @return TypeInterface<array<array-key, string>>
             */
            function capture_groups_2(array $groups): TypeInterface
            {
                return shape_type(dict_from_keys(
                    dict_unique([0, ...$groups]),
                    /**
                     * @return TypeInterface<string>
                     */
                    static fn(): TypeInterface => string_type(),
                ));
            }

            /**
             * @param list<array-key> $groups
             * @return TypeInterface<array<array-key, string>>
             */
            function capture_groups_3(array $groups): TypeInterface
            {
                return shape_type(dict_from_keys(
                    dict_unique([0, ...$groups]),
                    string_type(...),
                ));
            }
        "},
        issues = [
            // TypeInterface: T is not used in interface body
            IssueCode::UnusedTemplateParameter,
        ]
    }

    test_analysis! {
        name = member_reference_argument,
        code = indoc! {"
            <?php

            class ChangeKind {
                public const ADD = 'add';
                public const REMOVE = 'remove';
                public const UPDATE = 'update';
                public const RENAME = 'rename';
                public const MOVE = 'move';
            }

            /**
             * @param ChangeKind::ADD|ChangeKind::*ME|ChangeKind::U* $kind
             */
            function foo(string $kind): string {
                return $kind;
            }

            foo(ChangeKind::ADD);    // OK (literal matches)
            foo('add');              // OK (literal matches)
            foo(ChangeKind::UPDATE); // OK (starts with 'U')
            foo('update');           // OK (starts with 'U')
            foo(ChangeKind::RENAME); // OK (ends with 'ME')
            foo('rename');           // OK (ends with 'ME')
        "},
    }

    test_analysis! {
        name = iterable_for_traversable_or_array,
        code = indoc! {"
            <?php

            /**
             * @template K
             * @template-covariant V
             */
            interface Traversable {}

            /**
             * @template K as array-key
             * @template V
             *
             * @param array<K, V>|Traversable<K, V> $_input
             *
             * @return array<K, V>
             */
            function as_array(array|Traversable $_input): array {
                return [];
            }

            /**
             * @template K as array-key
             * @template V
             *
             * @param iterable<K, V> $input
             *
             * @return array<K, V>
             */
            function iter_as_array(iterable $input): array {
                return as_array($input);
            }
        "},
        issues = [
            // Traversable: K and V not used in interface body
            IssueCode::UnusedTemplateParameter,
            IssueCode::UnusedTemplateParameter,
        ]
    }

    test_analysis! {
        name = invalid_member_reference_argument,
        code = indoc! {"
            <?php

            class ChangeKind {
                public const ADD = 'add';
                public const REMOVE = 'remove';
                public const UPDATE = 'update';
                public const RENAME = 'rename';
                public const MOVE = 'move';
            }

            /**
             * @param ChangeKind::ADD|ChangeKind::*ME|ChangeKind::U* $kind
             */
            function foo(string $kind): string {
                return $kind;
            }

            foo(ChangeKind::MOVE);   // Error: 'move' does not match any pattern
            foo('move');             // Error: 'move' does not match any pattern
            foo(ChangeKind::REMOVE); // Error: 'remove' does not match any pattern
            foo('remove');           // Error: 'remove' does not match any pattern
            foo('unknown');          // Error: 'unknown' does not match any pattern
        "},
        issues = [
            IssueCode::InvalidArgument,
            IssueCode::InvalidArgument,
            IssueCode::InvalidArgument,
            IssueCode::InvalidArgument,
            IssueCode::InvalidArgument,
        ],
    }

    test_analysis! {
        name = enum_member_reference_argument,
        code = indoc! {"
            <?php

            enum ChangeKind {
                case ADD;
                case REMOVE;
                case UPDATE;
                case RENAME;
                case MOVE;
            }

            /**
             * @param ChangeKind::ADD|ChangeKind::*ME|ChangeKind::U* $kind
             */
            function foo(ChangeKind $kind): ChangeKind {
                return $kind;
            }

            foo(ChangeKind::ADD);    // OK (literal matches)
            foo(ChangeKind::UPDATE); // OK (starts with 'U')
            foo(ChangeKind::RENAME); // OK (ends with 'ME')
        "},
    }

    test_analysis! {
        name = invalid_enum_member_reference_argument,
        code = indoc! {"
            <?php

            enum ChangeKind {
                case ADD;
                case REMOVE;
                case UPDATE;
                case RENAME;
                case MOVE;
            }

            /**
             * @param ChangeKind::ADD|ChangeKind::*ME|ChangeKind::U* $kind
             */
            function foo(ChangeKind $kind): ChangeKind {
                return $kind;
            }

            foo(ChangeKind::MOVE);   // Error: 'move' does not match any pattern
            foo(ChangeKind::REMOVE); // Error: 'remove' does not match any pattern
        "},
        issues = [
            IssueCode::InvalidArgument,
            IssueCode::InvalidArgument,
        ],
    }

    test_analysis! {
        name = type_logic_sanity_test,
        code = indoc! {"
            <?php

            interface A {}
            interface B {}

            function i_take_a(A $a): A {
                i_take_a_or_b($a);
                i_take_b_or_a($a);

                return $a;
            }

            function i_take_b(B $b): B {
                i_take_b_or_a($b);
                i_take_a_or_b($b);

                return $b;
            }

            function i_take_a_or_b(A|B $aOrB): B|A {
                if ($aOrB instanceof A) {
                    i_take_a($aOrB);
                } else {
                    i_take_b($aOrB);
                }

                return $aOrB;
            }

            function i_take_b_or_a(B|A $bOrA): A|B {
                if ($bOrA instanceof B) {
                    i_take_b($bOrA);
                } else {
                    i_take_a($bOrA);
                }

                return $bOrA;
            }

            function i_take_a_and_b(A&B $aAndB): B&A {
                i_take_a($aAndB);
                i_take_b($aAndB);
                i_take_a_or_b($aAndB);
                i_take_b_or_a($aAndB);

                return $aAndB;
            }

            function i_take_b_and_a(B&A $bAndA): A&B {
                i_take_b($bAndA);
                i_take_a($bAndA);
                i_take_a_or_b($bAndA);
                i_take_b_or_a($bAndA);

                return $bAndA;
            }
        "},
    }
}
