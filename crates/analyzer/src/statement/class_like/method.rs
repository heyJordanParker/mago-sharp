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
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::union::TUnion;

use mago_names::binding::MethodParts;
use mago_names::display_sharp_member;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Law;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::ModifierSequenceExt;
use mago_syntax::cst::Operator;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::effects;
use crate::effects::Body;
use crate::error::AnalysisError;
use crate::statement::attributes::AttributeTarget;
use crate::statement::attributes::analyze_attributes;
use crate::statement::function_like::FunctionLikeBody;
use crate::statement::function_like::analyze_function_like;
use crate::statement::function_like::check_unused_function_template_parameters;
use crate::statement::function_like::rejected_nullable_parameter;
use crate::statement::function_like::unused_parameter;
use crate::utils::missing_type_hints;
use crate::utils::names::display_class_like_name;

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
        analyze_method(&MethodParts::of_method(self), word(self.name.value), context, block_context, artifacts)
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
        if let Some(class) = analyze_method(&method, member, context, block_context, artifacts)?
            && matches!(self.symbol, BinaryOperator::Equal(_))
        {
            check_hash(context, class, self.operator.span);
        }

        Ok(())
    }
}

/// Analyzes a method, which messages name as `member` of its class, and returns its class when the method was
/// analyzed.
fn analyze_method<'ctx, 'arena, A>(
    method: &MethodParts<'_, 'arena>,
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
            unused_parameter::check_unused_params(
                method_metadata,
                method.parameter_list.parameters.as_slice(),
                body,
                context,
            );
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
            body,
            None,
        )?;

        effects::summary::record(
            context,
            artifacts,
            Body::Method(class_like_metadata.name, lowercase_law_name),
            word(self.name.value),
            self.parameter_list.parameters.iter().map(|parameter| parameter.variable.span).collect(),
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
