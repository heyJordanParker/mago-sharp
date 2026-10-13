use mago_allocator::Arena;
use mago_codex::context::ScopeContext;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::concat_word;
use mago_word::word;

use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::reference::ReferenceOrigin;
use mago_codex::ttype::TType;
use mago_codex::ttype::TypeRef;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::union::TUnion;

use mago_names::binding::MethodParts;
use mago_names::binding::php_method_name;
use mago_names::display_sharp_member;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstructorInitializer;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Law;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::ModifierSequenceExt;
use mago_syntax::cst::NamedConstructor;
use mago_syntax::cst::Operator;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::effects;
use crate::effects::Body;
use crate::error::AnalysisError;
use crate::expression::call::static_method_call::analyze_static_method_call;
use crate::plugin::hook::StaticCall;
use crate::statement::attributes::AttributeTarget;
use crate::statement::attributes::analyze_attributes;
use crate::statement::function_like::FunctionLikeBody;
use crate::statement::function_like::analyze_function_like;
use crate::statement::function_like::check_unused_function_template_parameters;
use crate::statement::function_like::rejected_nullable_parameter;
use crate::statement::function_like::unused_parameter;
use crate::statement::function_like::unused_parameter::utils::is_variable_used_in_expression;
use crate::utils::missing_type_hints;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_missing_imports;
use crate::utils::names::display_type;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Method<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let method = MethodParts::of_method(self);

        analyze_method(&method, self.initializer.as_ref(), word(self.name.value), context, block_context, artifacts)
            .map(|_| ())
    }
}

/// A PHP# operator analyzes as the static method it runs as, such as `op_Equality`. A class that declares `==` needs a
/// `public int hash()`, its own or a parent's, so equal values hash alike. An operator a class cannot declare, which
/// semantics refuses, is no method.
impl<'ast, 'arena> Analyzable<'ast, 'arena> for Operator<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let Some(method) = MethodParts::of_operator(self) else {
            return Ok(());
        };

        let member = concat_word!(b"operator ", self.symbol.as_bytes());
        if let Some(class) = analyze_method(&method, None, member, context, block_context, artifacts)?
            && matches!(self.symbol, BinaryOperator::Equal(_))
        {
            check_hash(context, class, self.operator.span);
        }

        Ok(())
    }
}

/// A PHP# named constructor analyzes as the static method it runs as, spec section 9.1. Its `: this(…)` creates the
/// object with the main constructor, and its body runs on that object.
impl<'ast, 'arena> Analyzable<'ast, 'arena> for NamedConstructor<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let method = MethodParts::of_named_constructor(self);

        analyze_method(&method, self.initializer.as_ref(), word(self.name.value), context, block_context, artifacts)
            .map(|_| ())
    }
}

/// Analyzes a method, which messages name as `member` of its class, and returns its class when the method was
/// analyzed. A PHP# constructor's `initializer` runs before its body.
fn analyze_method<'ctx, 'ast, 'arena, A>(
    method: &MethodParts<'ast, 'arena>,
    initializer: Option<&'ast ConstructorInitializer<'arena>>,
    member: Word,
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<Option<&'ctx ClassLikeMetadata>, AnalysisError>
where
    A: Arena,
{
    let name = method.name;

    analyze_attributes(context, block_context, artifacts, method.attribute_lists.as_slice(), AttributeTarget::Method)?;

    let Some(class_like_metadata) = block_context.scope.get_class_like() else {
        tracing::error!("Attempted to analyze method `{}` without class-like context.", mago_bytes::BytesDisplay(name));

        return Ok(None);
    };

    let method_name = word(name);
    let lowercase_method_name = ascii_lowercase_word(name);
    if context.settings.diff
        && context.codebase.safe_symbol_members.contains(&(class_like_metadata.name, lowercase_method_name))
    {
        return Ok(None);
    }

    let Some(method_metadata) =
        context.codebase.get_method_by_id(&MethodIdentifier::new(class_like_metadata.name, lowercase_method_name))
    else {
        tracing::error!(
            "Failed to find method metadata for `{}` in class `{}`.",
            mago_bytes::BytesDisplay(name),
            class_like_metadata.original_name
        );

        return Ok(None);
    };

    // Skip duplicate methods; semantics reports the error
    if method_metadata.span != method.span {
        return Ok(None);
    }

    // Spec section 29 keeps native bodies to the standard library's classes under `Sharp\`. The engine compiles the
    // library from `vendor/` as any other code, so only the analyzer knows the file's package.
    if method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Extern(_)))
        && !(context.source_file.is_standard_library && class_like_metadata.name.as_bytes().starts_with(b"sharp\\"))
    {
        context.collector.report_with_code(
            IssueCode::NativeBodyOutsideLibrary,
            Issue::error(format!(
                "Only the standard library declares native bodies: give `{}` a body.",
                mago_bytes::BytesDisplay(name)
            ))
            .with_annotation(Annotation::primary(method.name_span).with_message("Declared `extern` here."))
            .with_note("A native body is compiled into the PHP# engine, which ships only the standard library's."),
        );
    }

    check_replaced_functions(context, class_like_metadata, method_metadata);

    let body = match method.body {
        MethodBody::Abstract(_) => None,
        MethodBody::Concrete(block) => Some(FunctionLikeBody::Statements(block.statements.as_slice(), block.span())),
        MethodBody::Expression(body) if method.returns_value => Some(FunctionLikeBody::Expression(body.expression)),
        MethodBody::Expression(body) => Some(FunctionLikeBody::ExpressionStatement(body.expression)),
    };

    if let Some(body) = body {
        let mut scope = ScopeContext::new(ReferenceOrigin::Symbol((class_like_metadata.name, method_metadata.name)));
        scope.set_class_like(Some(class_like_metadata));
        scope.set_function_like(Some(method_metadata));
        scope.set_static(method.modifiers.contains_static());

        let mut method_block_context = BlockContext::new(scope, context.settings.register_super_globals);

        method_block_context.flags.set_collect_initializations(true);

        let method_artifacts = analyze_function_like(
            context,
            artifacts,
            &mut method_block_context,
            method_metadata,
            method.parameter_list,
            initializer,
            body,
            None,
        )?;

        let method_key = (class_like_metadata.name, lowercase_method_name);

        if context.dialect.is_sharp() {
            effects::summary::record(
                context,
                artifacts,
                Body::Method(class_like_metadata.name, lowercase_method_name),
                member,
                method.parameter_list.parameters.iter().map(|parameter| parameter.variable.span).collect(),
                initializer,
                body,
            );
        }

        if method_metadata.return_from_body.is_some() {
            let mut returned: Option<TUnion> = None;
            for inferred in method_artifacts.inferred_return_types {
                returned = Some(add_optional_union_type((*inferred).clone(), returned.as_ref(), context.codebase));
            }

            if let Some(returned) = returned {
                artifacts.body_returns.insert(method_key, returned);
            }
        }

        artifacts
            .method_initialized_properties
            .insert(method_key, method_block_context.definitely_initialized_properties.clone());

        artifacts.method_calls_this_methods.insert(method_key, method_block_context.definitely_called_methods.clone());

        if method_block_context.flags.calls_parent_constructor() {
            artifacts.method_calls_parent_constructor.insert(method_key, true);
        }

        if let Some(parent_initializer_name) = method_block_context.calls_parent_initializer {
            artifacts.method_calls_parent_initializer.insert(method_key, parent_initializer_name);
        }

        let is_overriding =
            context.codebase.method_is_overriding(class_like_metadata.name.as_bytes(), method_name.as_bytes());

        // An override keeps the parameter types of the method it overrides, so only a method that declares its
        // own parameters can drop a `?`.
        if context.dialect.is_sharp() && !is_overriding {
            rejected_nullable_parameter::check_rejected_nullable_parameters(
                context,
                method_metadata,
                method.parameter_list.parameters.as_slice(),
                body,
                &method_block_context,
                artifacts,
            );
        }

        if context.settings.find_unused_parameters && !is_overriding {
            // A parameter that `: super(…)` passes on is used.
            let parameters: Vec<FunctionLikeParameter<'arena>> = method
                .parameter_list
                .parameters
                .iter()
                .filter(|parameter| {
                    !initializer.is_some_and(|initializer| {
                        initializer.argument_list.arguments.iter().any(|argument| {
                            is_variable_used_in_expression(argument.value(), context, parameter.variable.name)
                        })
                    })
                })
                .cloned()
                .collect();

            unused_parameter::check_unused_params(method_metadata, &parameters, body, context);
        }
    }

    check_unused_function_template_parameters(
        context,
        method_metadata,
        method.name_span,
        "method",
        concat_word!(&class_like_metadata.original_name, "::", &method_name),
    );

    // Check for missing type hints
    for (i, parameter) in method.parameter_list.parameters.iter().enumerate() {
        missing_type_hints::check_parameter_type_hint(context, Some(class_like_metadata), method_metadata, parameter);

        missing_type_hints::check_imprecise_parameter_type_hint(context, method_metadata, parameter, i);
    }

    missing_type_hints::check_return_type_hint(
        context,
        Some(class_like_metadata),
        method_metadata,
        name,
        method.return_type_hint,
        method.span,
    );

    missing_type_hints::check_imprecise_return_type_hint(context, method_metadata, name, method.return_type_hint);

    Ok(Some(class_like_metadata))
}

/// Spec section 9.1: `: super(…)` runs as PHP's `parent::__construct(…)` before the constructor's body, so its
/// arguments are checked against the base class's constructor, and it counts as the call of that constructor. A base
/// class without a constructor takes `: super()`, which does nothing, and no arguments, as C#'s `object()` does. A named
/// constructor's `: this(…)` creates its object with the main constructor, so its arguments are checked against that
/// constructor. Semantics refuses `: this(…)` on the main constructor and `: super(…)` on a named one.
pub(crate) fn analyze_constructor_initializer<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    initializer: &ConstructorInitializer<'arena>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let is_named = block_context.scope.get_function_like().is_some_and(|method| method.flags.is_named_constructor());
    let Some(class) = block_context
        .scope
        .get_class_like()
        .filter(|class| initializer.is_super() != is_named && !class.has_incomplete_hierarchy())
    else {
        return Ok(());
    };

    let (receiver, constructed) = if is_named {
        (Expression::Self_(initializer.keyword), Some(class.name))
    } else {
        (Expression::Parent(initializer.keyword), class.direct_parent_class)
    };

    if constructed.is_some_and(|constructed| {
        context.codebase.get_declaring_method(constructed.as_bytes(), b"__construct").is_some()
    }) {
        let method = ClassLikeMemberSelector::Identifier(LocalIdentifier {
            span: initializer.keyword.span,
            value: b"__construct",
        });
        let call = StaticCall {
            class: &receiver,
            method: &method,
            argument_list: &initializer.argument_list,
            span: initializer.span(),
        };

        return analyze_static_method_call(context, block_context, artifacts, call);
    }

    if !initializer.argument_list.arguments.is_empty() {
        let message = match constructed {
            Some(base) => {
                format!(
                    "`{}` has no constructor to call: remove the arguments.",
                    display_class_like_name(context, base)
                )
            }
            None => format!(
                "`{}` has no base class to call: remove the arguments.",
                display_class_like_name(context, class.original_name)
            ),
        };

        context.collector.report_with_code(
            IssueCode::TooManyArguments,
            Issue::error(message)
                .with_annotation(Annotation::primary(initializer.argument_list.span()).with_message("Passed here.")),
        );
    }

    initializer.argument_list.analyze(context, block_context, artifacts)
}

/// Spec section 9.1: PHP# has no implicit `super()`, so the constructor of a class whose base class has one calls it
/// with `: super(…)`. A class without a constructor gets one for its initial values that are not constant, which calls
/// the base class's constructor without arguments, so it declares its own when that base class's constructor takes any.
pub(super) fn check_super_call<A>(
    context: &mut Context<'_, '_, A>,
    class: &ClassLikeMetadata,
    members: &[ClassLikeMember<'_>],
    name_span: Span,
) where
    A: Arena,
{
    let (Some(base), Some(constructor_of_base)) =
        (class.direct_parent_class, context.codebase.get_parent_constructor(class.name.as_bytes()))
    else {
        return;
    };
    // A struct can't be inherited, which `check_class_like_extends` reports, so its constructor isn't the class's to call.
    if context.codebase.get_class_like(base.as_bytes()).is_some_and(|base| base.flags.is_struct()) {
        return;
    }
    let constructor = members.iter().find_map(|member| match member {
        ClassLikeMember::Method(method) if php_method_name(method) == b"__construct" => Some(method),
        _ => None,
    });
    let class_name = display_class_like_name(context, class.original_name);
    let base_name = display_class_like_name(context, base);

    match constructor {
        Some(constructor) if constructor.initializer.is_none() => {
            context.collector.report_with_code(
                IssueCode::MissingSuperCall,
                Issue::error(format!("`{class_name}`'s constructor must call `{base_name}`'s with `: super(…)`."))
                    .with_annotation(Annotation::primary(constructor.name.span).with_message("Declared here.")),
            );
        }
        None if !constructor_of_base.parameters.is_empty()
            && members.iter().any(|member| {
                matches!(member, ClassLikeMember::Property(property) if property.initial_value_runs_in_constructor())
            }) =>
        {
            let mut parameters = Vec::new();
            let mut arguments = Vec::new();
            let mut classes = Vec::new();
            for parameter in &constructor_of_base.parameters {
                let hint = parameter.get_type_metadata().map(|hint| &hint.type_union);
                let default = match parameter.get_default_type() {
                    Some(default) => match written_default(&default.type_union, hint) {
                        Some(default) => format!(" = {default}"),
                        // PHP passes this default and every one after it, so the constructor leaves them out.
                        None => break,
                    },
                    None => String::new(),
                };
                let name = String::from_utf8_lossy(mago_bytes::trim_start_byte(parameter.get_name().0.as_bytes(), b'$'));
                let spread = if parameter.flags.is_variadic() { "..." } else { "" };
                let written_hint = match hint {
                    Some(hint) => {
                        classes.extend(hint.get_all_child_nodes().into_iter().filter_map(|node| match node {
                            TypeRef::Atomic(TAtomic::Object(object)) => object.get_name(),
                            _ => None,
                        }));

                        display_type(context, hint)
                    }
                    None => "Any?".to_owned(),
                };
                parameters.push(format!("{written_hint} {spread}{name}{default}"));
                arguments.push(format!("{spread}{name}"));
            }
            let imports = display_missing_imports(context, classes).map(|imports| format!(" {imports}")).unwrap_or_default();

            context.collector.report_with_code(
                IssueCode::MissingSuperCall,
                Issue::error(format!(
                    "`{class_name}` must declare a constructor that calls `{base_name}`'s with `: super(…)`."
                ))
                .with_annotation(Annotation::primary(name_span).with_message("Declared here."))
                .with_note(format!(
                    "`{base_name}`'s constructor takes arguments, and `{class_name}`'s initial values run in its constructor."
                ))
                .with_help(format!(
                    "Write `public {class_name}({}) : super({}) {{ }}`.{imports}",
                    parameters.join(", "),
                    arguments.join(", ")
                )),
            );
        }
        _ => {}
    }
}

/// The default value of a parameter of type `hint` as PHP# code writes it, when the default is a literal PHP# writes
/// alike. An empty PHP array is `[:]` where the parameter is a map.
fn written_default(default: &TUnion, hint: Option<&TUnion>) -> Option<String> {
    if default.is_null() {
        Some("null".to_owned())
    } else if default.is_true() {
        Some("true".to_owned())
    } else if default.is_false() {
        Some("false".to_owned())
    } else if default.is_empty_array() {
        let is_map = hint.and_then(TUnion::get_single_array).is_some_and(|array| !array.is_list());

        Some(if is_map { "[:]" } else { "[]" }.to_owned())
    } else if let Some(int) = default.get_single_literal_int_value() {
        Some(int.to_string())
    } else {
        default
            .get_single_literal_string_value()
            .and_then(|string| std::str::from_utf8(string).ok())
            .filter(|string| !string.contains(['\\', '"', '$', '{']))
            .map(|string| format!("\"{string}\""))
    }
}

/// Reports `class`'s `operator ==`, declared at `operator`, when neither `class` nor a parent declares a
/// `public int hash()`, which equal values need to hash alike.
fn check_hash<A>(context: &mut Context<'_, '_, A>, class: &ClassLikeMetadata, operator: Span)
where
    A: Arena,
{
    let has_hash = context.codebase.get_declaring_method(class.name.as_bytes(), b"hash").is_some_and(|hash| {
        hash.method_metadata.as_ref().is_some_and(|method| method.visibility.is_public() && !method.is_static)
            && hash.return_type_declaration_metadata.as_ref().is_some_and(|returned| returned.type_union.is_int())
    });
    if has_hash {
        return;
    }

    let class_name = display_class_like_name(context, class.name);
    context.collector.report_with_code(
        IssueCode::UnimplementedAbstractMethod,
        Issue::error(format!(
            "`{class_name}` declares `operator ==` without `public int hash()`: declare it in `{class_name}` or a parent, so equal values hash alike."
        ))
        .with_annotation(Annotation::primary(operator).with_message("Declared here.")),
    );
}

/// Spec section 28: a law's body is analyzed as a static method's that returns `bool`, and records its effects, so the
/// law rule refuses it when it is impure. A law is never one of the class's methods, so no method rule reaches it.
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

        let body = FunctionLikeBody::Expression(self.body.expression);
        let mut law_block_context = BlockContext::new(scope, context.settings.register_super_globals);
        analyze_function_like(
            context,
            artifacts,
            &mut law_block_context,
            law_metadata,
            &self.parameter_list,
            None,
            body,
            None,
        )?;

        effects::summary::record(
            context,
            artifacts,
            Body::Method(class_like_metadata.name, lowercase_law_name),
            word(self.name.value),
            self.parameter_list.parameters.iter().map(|parameter| parameter.variable.span).collect(),
            None,
            body,
        );

        Ok(())
    }
}

/// Decision 040: only the standard library's `[Replaces]` wraps a PHP function, and a library method names each
/// function once.
fn check_replaced_functions<A>(
    context: &mut Context<'_, '_, A>,
    class: &ClassLikeMetadata,
    method: &FunctionLikeMetadata,
) where
    A: Arena,
{
    if !context.source_file.is_standard_library {
        for attribute in method
            .attributes
            .iter()
            .filter(|attribute| attribute.name.as_bytes().eq_ignore_ascii_case(b"Sharp\\Replaces"))
        {
            context.collector.report_with_code(
                IssueCode::ReplacesOutsideLibrary,
                Issue::error(format!(
                    "Only the standard library declares which PHP functions it wraps: remove `[Replaces]` from `{}`.",
                    method.original_name
                ))
                .with_annotation(Annotation::primary(attribute.span).with_message("Written here.")),
            );
        }

        return;
    }

    let mut named: Vec<(Word, Span)> = Vec::new();
    for (function, span) in method.replaced_functions() {
        let first = named
            .iter()
            .find(|(earlier, _)| earlier.as_bytes().eq_ignore_ascii_case(function.as_bytes()))
            .map(|(_, first)| *first);
        let Some(first) = first else {
            named.push((function, span));
            continue;
        };

        context.collector.report_with_code(
            IssueCode::DuplicateDefinition,
            Issue::error(format!(
                "`{}` names `{function}` twice: name each function once.",
                display_sharp_member(display_class_like_name(context, class.original_name), method.original_name)
            ))
            .with_annotation(Annotation::primary(span).with_message("Named again here."))
            .with_annotation(Annotation::secondary(first).with_message("First named here.")),
        );
    }
}
