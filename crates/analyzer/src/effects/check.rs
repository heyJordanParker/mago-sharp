//! The rules effects decide: an `extern` declaration's own checks, what a `uses` names, what a static holds and where
//! a function parameter goes, and the rules over solved effects.

use std::cell::OnceCell;

use foldhash::HashMap;
use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::TypeRef;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::Binding;
use mago_names::binding::MethodParts;
use mago_names::binding::php_variable_name;
use mago_names::display_sharp_member;
use mago_names::short_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Closure;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Extern;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Method;
use mago_syntax::cst::Program;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::Return;
use mago_syntax::cst::Uses;
use mago_syntax::walker::Walker;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::artifacts::AnalysisArtifacts;
use crate::artifacts::CallTarget;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::effects::Body;
use crate::effects::Effect;
use crate::effects::Effects;
use crate::effects::Impurity;
use crate::effects::NAMED_EFFECTS;
use crate::effects::Runs;
use crate::effects::events;
use crate::effects::is_effect;
use crate::effects::summary::parameter_index;
use crate::statement::function_like::FunctionLikeBody;
use crate::utils::names::display_function_like_identifier;
use crate::utils::names::display_member;
use crate::utils::names::display_sharp_type;
use crate::utils::names::is_imported_namespace;
use crate::utils::names::namespace_function;
use crate::utils::names::sharp_class_like_name;
use crate::utils::names::sharp_code_class_name;
use crate::utils::names::sharp_collection_name;
use crate::utils::names::sharp_effect_name;
use crate::utils::names::sharp_missing_imports;

/// Spec section 6.1: a `get` body may have any effect, and a read carries it, but it never writes `this`: neither a
/// write of a field of `this` nor a call of a method that writes one. Unlike a method marked `readonly` (section 13),
/// it may call a plain PHP method and an unmarked PHP# one. A `lazy` property keeps its value through the bridge's
/// write, which is the language's, not the body's.
pub(crate) fn getters_never_write_this(
    effects: &Effects,
    codebase: &CodebaseMetadata,
    short_name_counts: &OnceCell<HashMap<String, u32>>,
) -> IssueCollection {
    effects
        .bodies_changing_this()
        .filter(|(body, ..)| matches!(body, Body::Accessor(_, _, accessor) if accessor.as_bytes() == b"get"))
        .map(|(_, member, imports, _, impurity)| {
            let class_name = |class| sharp_class_like_name(codebase, imports, short_name_counts, class);
            let effect_name = |effect| sharp_effect_name(codebase, imports, short_name_counts, effect);

            Issue::error(format!(
                "Getter `{member}` {}. A getter never writes `this`.",
                impurity.describe(&class_name, &effect_name)
            ))
            .with_code(IssueCode::GetterWritesThis.as_str())
            .with_annotation(Annotation::primary(impurity.span))
            .with_note("A getter may have any effect, but it never changes its object.")
            .with_help("Write the change in a method, and call it before the read.")
        })
        .collect()
}

/// Spec section 28: a law holds only over pure code, which is the code Lean reads.
pub(crate) fn laws_must_be_pure(
    effects: &Effects,
    codebase: &CodebaseMetadata,
    short_name_counts: &OnceCell<HashMap<String, u32>>,
) -> IssueCollection {
    effects
        .impure_bodies()
        .filter(|(body, ..)| {
            matches!(body, Body::Method(class, method)
                if codebase.get_class_like(class.as_bytes()).is_some_and(|class| class.laws.contains_key(method)))
        })
        .map(|(_, member, imports, scope, impurity)| {
            let class_name = |class| sharp_class_like_name(codebase, imports, short_name_counts, class);
            let effect_name = |effect| sharp_effect_name(codebase, imports, short_name_counts, effect);
            let code_class_name = |class| sharp_code_class_name(codebase, imports, scope, class);
            let missing_import = |class| sharp_missing_imports(codebase, imports, scope, [class]);

            impure(
                IssueCode::ImpureLaw,
                format!(
                    "Law `{member}` {}. Laws hold only over pure code.",
                    impurity.describe(&class_name, &effect_name)
                ),
                impurity.span,
                "law",
                &impurity,
                &class_name,
                &code_class_name,
                &missing_import,
            )
        })
        .collect()
}

/// Spec section 29: every implementation is held to what its declaration allows, so a lambda or a method value that a
/// place's function type holds has only the effects that type lists. The refusal has the code a value of the wrong
/// type gets in that place.
pub(crate) fn function_values_must_fit_their_types(
    effects: &Effects,
    codebase: &CodebaseMetadata,
    short_name_counts: &OnceCell<HashMap<String, u32>>,
) -> IssueCollection {
    effects
        .unfit_function_values()
        .map(|(value, name, imports, scope, impurity)| {
            let class_name = |class| sharp_class_like_name(codebase, imports, short_name_counts, class);
            let effect_name = |effect| sharp_effect_name(codebase, imports, short_name_counts, effect);
            let code_class_name = |class| sharp_code_class_name(codebase, imports, scope, class);
            let missing_import = |class| sharp_missing_imports(codebase, imports, scope, [class]);
            let method = || format!("`{}`", display_sharp_member(class_name(name.0), name.1));
            let reason = match (&value.runs, impurity.effect) {
                (Runs::Body(Body::Lambda(_)), _) => {
                    format!("This lambda {}", impurity.describe(&class_name, &effect_name))
                }
                (Runs::Body(_), _) => format!("{} {}", method(), impurity.describe(&class_name, &effect_name)),
                (Runs::Foreign { .. }, Some(Effect::Foreign(effect))) => {
                    format!("{} has the effect `{}`", method(), effect_name(effect))
                }
                (Runs::Foreign { .. }, _) => format!("{} has no `extern` declaration", method()),
            };
            let body = if matches!(value.runs, Runs::Body(Body::Lambda(_))) { "lambda" } else { "method" };

            let issue = impure(
                value.code,
                format!("{reason}, so a `{}` cannot hold it.", value.declared),
                value.span,
                body,
                &impurity,
                &class_name,
                &code_class_name,
                &missing_import,
            );

            match value.runs {
                Runs::Body(_) => issue.with_annotation(Annotation::secondary(impurity.span)),
                Runs::Foreign { .. } => issue,
            }
        })
        .collect()
}

/// Spec section 29: a method without a body is pure unless it says `uses`, and every implementation is held to what
/// the declaration allows.
pub(crate) fn implementations_must_fit_their_declarations(
    effects: &Effects,
    codebase: &CodebaseMetadata,
    short_name_counts: &OnceCell<HashMap<String, u32>>,
) -> IssueCollection {
    effects
        .unfit_implementations(codebase)
        .map(|((class, member), ancestor, declaration, imports, scope, impurity)| {
            let class_name = |class| sharp_class_like_name(codebase, imports, short_name_counts, class);
            let effect_name = |effect| sharp_effect_name(codebase, imports, short_name_counts, effect);
            let code_class_name = |class| sharp_code_class_name(codebase, imports, scope, class);
            let missing_import = |class| sharp_missing_imports(codebase, imports, scope, [class]);
            let ancestor =
                codebase.get_class_like(ancestor.as_bytes()).map_or(ancestor, |ancestor| ancestor.original_name);
            let declared = display_sharp_member(class_name(ancestor), declaration.original_name);
            let allows = if declaration.uses.is_empty() {
                format!("the pure `{declared}`")
            } else {
                let uses: Vec<String> = declaration.uses.iter().map(|effect| effect_name(*effect)).collect();

                format!("`uses {}` on `{declared}`", uses.join(", "))
            };

            impure(
                IssueCode::IncompatibleEffects,
                format!(
                    "`{}` {}, so {allows} cannot accept it.",
                    display_sharp_member(class_name(class), member),
                    impurity.describe(&class_name, &effect_name)
                ),
                impurity.span,
                "method",
                &impurity,
                &class_name,
                &code_class_name,
                &missing_import,
            )
        })
        .collect()
}

/// Spec section 13: through a readonly value only methods marked `readonly` run, so a call of an unmarked one is an
/// error that names the fix: the marker, when the method changes nothing in `this`, or the write that keeps it off.
pub(crate) fn readonly_values_call_readonly_methods(
    effects: &Effects,
    codebase: &CodebaseMetadata,
    short_name_counts: &OnceCell<HashMap<String, u32>>,
) -> IssueCollection {
    effects
        .readonly_calls()
        .map(|(call, imports, change)| {
            let class_name = |class| sharp_class_like_name(codebase, imports, short_name_counts, class);
            let fix = match &change {
                None => format!(
                    "{} does not change {}, so declare it `{} readonly`.",
                    call.method, call.class, call.declaration
                ),
                Some(change) => {
                    let place = BytesDisplay(change.cause.1.as_bytes());
                    match change.path.last() {
                        None => format!("{} changes {}: it writes `{place}`.", call.method, call.class),
                        Some(body) => format!(
                            "{} changes {}: it reaches `{}`, which writes `{place}`.",
                            call.method,
                            call.class,
                            display_sharp_member(class_name(body.0), body.1)
                        ),
                    }
                }
            };

            let issue =
                Issue::error(format!("{} cannot call {}: {} is readonly.", call.caller, call.call, call.receiver))
                    .with_code(IssueCode::MissingReadonlyMarker.as_str())
                    .with_annotation(Annotation::primary(call.span))
                    .with_help(fix);

            match change {
                Some(change) => issue.with_annotation(Annotation::secondary(change.span)),
                None => issue,
            }
        })
        .collect()
}

/// The error at `span` on the `impurity` of the `body` it refuses, a getter, a law, a lambda or a method, with the
/// `extern` to write when the callee has none, or how to call a plain PHP property's code from outside `body`.
/// `class_name` names each class in prose, `code_class_name` names it in the `extern`, and `missing_import` names the
/// import the `extern` needs when the body's file doesn't bind its class.
#[allow(clippy::too_many_arguments)]
fn impure(
    code: IssueCode,
    message: String,
    span: Span,
    body: &str,
    impurity: &Impurity,
    class_name: &dyn Fn(Word) -> String,
    code_class_name: &dyn Fn(Word) -> String,
    missing_import: &dyn Fn(Word) -> Option<String>,
) -> Issue {
    let issue = Issue::error(message).with_code(code.as_str()).with_annotation(Annotation::primary(span));

    match impurity.effect {
        Some(Effect::Unknown(Some((class, member)))) => {
            // A plain PHP function is declared through its namespace, spec section 23: `global.trim`, which the cause
            // already writes, or `Pdf.render` after `import Vendor.Pdf;`.
            let (target, import) = if class.is_empty() {
                match member.as_bytes().iter().rposition(|byte| *byte == b'\\') {
                    Some(separator) => {
                        let namespace = word(&member.as_bytes()[..separator]);
                        let function = String::from_utf8_lossy(&member.as_bytes()[separator + 1..]);

                        (format!("{}.{function}", code_class_name(namespace)), missing_import(namespace))
                    }
                    None => (member.to_string(), None),
                }
            } else {
                let name = code_class_name(class);
                let target = if member.is_empty() { name } else { display_sharp_member(&name, member) };

                (target, missing_import(class))
            };
            let import = import.map(|import| format!(" {import}")).unwrap_or_default();

            issue.with_help(format!(
                "Declare it in a .sharp file: `extern {target};` when it has no effect, or name its effects after `uses`.{import}"
            ))
        }
        Some(Effect::Unknown(None)) => {
            let (class, property) = impurity.cause;
            let class = class_name(class);
            let property = display_sharp_member(&class, property);

            issue.with_help(format!(
                "Property `{property}` holds plain PHP code, which no `extern` can declare. Call it outside the {body}, or through a method of `{class}` that an `extern` declares."
            ))
        }
        _ => issue,
    }
}

/// Spec section 29: an `extern` declaration declares plain PHP that exists, once in the whole project.
pub(crate) fn check_extern<A>(context: &mut Context<'_, '_, A>, r#extern: &Extern<'_>)
where
    A: Arena,
{
    let span = r#extern.span();
    let Some((declarations, own)) = context.codebase.externs.values().find_map(|declarations| {
        declarations.iter().find(|declaration| declaration.span == span).map(|own| (declarations, own))
    }) else {
        return;
    };

    let written = String::from_utf8_lossy(r#extern.target.value());
    if let Some(first) = declarations.first()
        && first.span != span
    {
        context.collector.report_with_code(
            IssueCode::DuplicateExtern,
            Issue::error(format!("`{written}` already has an `extern` declaration."))
                .with_annotation(Annotation::primary(span))
                .with_annotation(Annotation::secondary(first.span).with_message("First declared here.")),
        );

        return;
    }

    let target = r#extern.target.span();
    let (class, member) = own.target;
    if class.is_empty() {
        // Spec section 23 names a function of PHP's root namespace as `global.now`, in an `extern` as in a call.
        if !written.starts_with("global.") {
            context.collector.report_with_code(
                IssueCode::BareFunctionCall,
                Issue::error(format!(
                    "An `extern` names a plain PHP function through its namespace: write `extern global.{written}`."
                ))
                .with_annotation(Annotation::primary(target)),
            );
        } else if context.codebase.get_function(member.as_bytes()).is_none() {
            context.collector.report_with_code(
                IssueCode::NonExistentFunction,
                Issue::error(format!("Function `{written}` does not exist."))
                    .with_annotation(Annotation::primary(target)),
            );
        }

        return;
    }

    let Some(class_metadata) = context.codebase.get_class_like(class.as_bytes()) else {
        // `extern Pdf.render` after `import Vendor.Pdf;` declares the function `render` of that namespace, spec
        // section 23.
        if namespace_function(context.codebase, class, member).is_some() {
            return;
        }

        let issue = if !member.is_empty()
            && is_imported_namespace(context.resolved_names, context.codebase, &r#extern.target, class)
        {
            (IssueCode::NonExistentFunction, format!("Function `{written}` does not exist."))
        } else {
            (IssueCode::NonExistentClassLike, format!("Cannot find class, interface, enum, or type alias `{written}`."))
        };
        context.collector.report_with_code(issue.0, Issue::error(issue.1).with_annotation(Annotation::primary(target)));

        return;
    };

    if class_metadata.flags.is_sharp() {
        context.collector.report_with_code(
            IssueCode::ExternOnSharp,
            Issue::error(format!("`{written}` is PHP# code, so it needs no `extern`: PHP# infers its effects."))
                .with_annotation(Annotation::primary(target)),
        );
    } else if own.event && !class_metadata.kind.is_class() {
        // Spec section 15: only a class is an event, so `emit` never takes a value plain PHP fills with a non-event.
        let kind = if class_metadata.kind.is_interface() {
            "an interface"
        } else if class_metadata.kind.is_trait() {
            "a trait"
        } else {
            "an enum"
        };

        context.collector.report_with_code(
            IssueCode::InvalidEvent,
            Issue::error(format!("`extern event` marks a class, and `{written}` is {kind}."))
                .with_annotation(Annotation::primary(target)),
        );
    } else if !member.is_empty() && context.codebase.get_declaring_method(class.as_bytes(), member.as_bytes()).is_none()
    {
        context.collector.report_with_code(
            IssueCode::NonExistentMethod,
            Issue::error(format!("Method `{written}` does not exist.")).with_annotation(Annotation::primary(target)),
        );
    }
}

/// Spec section 29: each name a `uses` writes is an effect. Any file may declare a `foreign` class, so the check
/// reads the codebase, and the file depends on each class it names.
pub(crate) fn check_uses<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    program: &Program<'_>,
) where
    A: Arena,
{
    let mut found = UsesNames::default();
    UsesWalker.walk_program(program, &mut found);

    for name in found.names {
        let effect = context.resolved_names.get(name);
        artifacts.symbol_references.add_reference_to_symbol(&block_context.scope, ascii_lowercase_word(effect), false);
        if is_effect(context.codebase, effect) {
            continue;
        }

        // The message names the `foreign` classes the file can name without another import: the standard library's,
        // its namespace's and its imports'. The file depends on each class it could name, so the message follows a
        // class that becomes `foreign` or stops being one, as a compile from scratch gives it.
        let namespace = context.scope.namespace_name().unwrap_or_default();
        let nameable: Vec<_> = context
            .codebase
            .class_likes
            .values()
            .filter(|class| {
                let name = class.original_name.as_bytes();
                let class_namespace = name.iter().rposition(|byte| *byte == b'\\').map_or(&b""[..], |end| &name[..end]);

                class_namespace.eq_ignore_ascii_case(b"Sharp")
                    || class_namespace.eq_ignore_ascii_case(namespace)
                    || context.imported_names.contains_key(&ascii_lowercase_word(name))
            })
            .collect();
        for class in &nameable {
            artifacts.symbol_references.add_reference_to_symbol(&block_context.scope, class.name, false);
        }
        let mut foreign: Vec<String> = nameable
            .into_iter()
            .filter(|class| class.flags.is_foreign())
            .map(|class| {
                sharp_class_like_name(
                    context.codebase,
                    &context.imported_names,
                    &context.short_name_counts,
                    class.original_name,
                )
            })
            .collect();
        foreign.sort_unstable();
        let effects: Vec<String> = NAMED_EFFECTS.iter().map(short_name).chain(foreign).collect();

        context.collector.report_with_code(
            IssueCode::InvalidEffect,
            Issue::error(format!(
                "`{}` is not an effect: `uses` takes {}, {} or a `foreign` class.",
                String::from_utf8_lossy(name.value),
                effects.join(", "),
                short_name(events()),
            ))
            .with_annotation(Annotation::primary(name.span).with_message("Written here."))
            .with_note("An effect is a standard effect, a `foreign` class, or `Events`, the effect of `emit`."),
        );
    }
}

/// The names every effect `uses` of a program writes. A method with a body names none, and a function parameter named
/// in any method's is no effect, as the semantics checker holds them to.
struct UsesWalker;

/// The names [`UsesWalker`] collects, and the `uses` of the methods it read already.
#[derive(Default)]
struct UsesNames<'ast, 'arena> {
    names: Vec<&'ast LocalIdentifier<'arena>>,
    methods: Vec<Span>,
}

impl<'ast, 'arena> Walker<'ast, 'arena, UsesNames<'ast, 'arena>> for UsesWalker {
    fn walk_in_method(&self, method: &'ast Method<'arena>, found: &mut UsesNames<'ast, 'arena>) {
        let Some(uses) = method.uses else {
            return;
        };

        found.methods.push(uses.span());
        if method.is_abstract() {
            found.names.extend(MethodParts::of_method(method).uses_effects());
        }
    }

    fn walk_in_uses(&self, uses: &'ast Uses<'arena>, found: &mut UsesNames<'ast, 'arena>) {
        if !found.methods.contains(&uses.span()) {
            found.names.extend(uses.names.iter());
        }
    }
}

/// Spec section 29: a `foreign` object reaches code only through constructors, so a static property of `class` holds
/// none, alone or inside a collection.
pub(crate) fn check_foreign_statics<A>(context: &mut Context<'_, '_, A>, class: &ClassLikeMetadata)
where
    A: Arena,
{
    for property in class.properties.values() {
        if !property.flags.is_static() {
            continue;
        }
        let Some(declared) = &property.type_declaration_metadata else {
            continue;
        };

        report_foreign_in_static(
            context,
            class.original_name,
            property.name.0,
            &declared.type_union,
            property.name_span.unwrap_or(declared.span),
        );
    }
}

/// Spec section 29: a write of a value of `value_type` at `span` to the static `property` of `class` stores no
/// `foreign` object, whatever the static's declared type, as `Cache.loose = this.store` into an `Any?` would.
pub(crate) fn check_foreign_static_write<A>(
    context: &mut Context<'_, '_, A>,
    class: Word,
    property: Word,
    value_type: &TUnion,
    span: Span,
) where
    A: Arena,
{
    let class = context.codebase.get_class_like(class.as_bytes()).map_or(class, |metadata| metadata.original_name);

    report_foreign_in_static(context, class, property, value_type, span);
}

/// Reports `foreign-in-static` at `span` when `held_type`, a type the static `property` of `class` holds, holds a
/// `foreign` object, alone or inside a collection.
fn report_foreign_in_static<A>(
    context: &mut Context<'_, '_, A>,
    class: Word,
    property: Word,
    held_type: &TUnion,
    span: Span,
) where
    A: Arena,
{
    let held = held_type.get_all_child_nodes().into_iter().find_map(|node| match node {
        TypeRef::Atomic(TAtomic::Object(object)) => object
            .get_name()
            .and_then(|name| context.codebase.get_class_like(name.as_bytes()))
            .filter(|held| held.flags.is_foreign()),
        _ => None,
    });
    let Some(held) = held else {
        return;
    };

    let held = sharp_class_like_name(
        context.codebase,
        &context.imported_names,
        &context.short_name_counts,
        held.original_name,
    );
    let property_name = display_member(context, class, property);
    context.collector.report_with_code(
        IssueCode::ForeignInStatic,
        Issue::error(format!("`{property_name}` is static, so it cannot hold a `{held}`."))
            .with_annotation(Annotation::primary(span))
            .with_note("A `foreign` object reaches code only through constructors, so no static holds one.")
            .with_help(format!("Take the `{held}` as a constructor parameter instead.")),
    );
}

/// Spec section 29: a function parameter whose type states no effects takes on each caller's, so its function runs only
/// inside the method's call, as Swift's closure parameters do without `@escaping`. Refuses each place the method body
/// `code` lets one of `metadata`'s escape, through the parameter itself, a local that holds it or a lambda that captures
/// it: stored in a field, a collection or a new object, returned, or passed where it may be kept. Calling it, and
/// passing it on to a PHP# function parameter that takes on effects or to a plain PHP callable, keep it inside the call.
pub(crate) fn report_escaping_functions<'ast, 'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    artifacts: &AnalysisArtifacts,
    metadata: &FunctionLikeMetadata,
    parameters: &'ast FunctionLikeParameterList<'arena>,
    code: FunctionLikeBody<'ast, 'arena>,
) where
    A: Arena,
{
    let taking: Vec<(Span, usize, Via)> = metadata
        .parameters
        .iter()
        .zip(parameters.parameters.iter())
        .enumerate()
        .filter(|(_, (parameter, _))| parameter.flags.takes_on_effects())
        .map(|(index, (_, declared))| (declared.variable.span, index, Via::Parameter))
        .collect();
    if taking.is_empty() {
        return;
    }

    // A local the body sets to one of them, or to a lambda that captures one, holds it too, so the walk repeats until
    // no local joins, and the last walk reports with every such local known.
    let mut escapes = Escapes { context: &*context, artifacts, taking, in_lambda: 0, found: Vec::new(), joined: false };
    loop {
        escapes.joined = false;
        escapes.found.clear();
        walk_escapes(code, &mut escapes);
        if !escapes.joined {
            break;
        }
    }

    let found = escapes.found;
    for escape in found {
        let parameter = &metadata.parameters[escape.parameter];
        let name = BytesDisplay(parameter.name.0.as_bytes().strip_prefix(b"$").unwrap_or(parameter.name.0.as_bytes()));
        // The parameter's own function type, which ends in its `>` and may go on with `?`, with a place for effects.
        let suggested = escape.suggested.unwrap_or_else(|| {
            let written = parameter
                .type_metadata
                .as_ref()
                .map(|declared| display_sharp_type(context, &declared.type_union))
                .unwrap_or_default();
            let end = written.rfind('>').unwrap_or(written.len());

            format!("{} uses …{}", &written[..end], &written[end..])
        });
        let (code, verb, at) = match &escape.place {
            Place::Field => (IssueCode::InvalidPropertyAssignmentValue, "stored in a field".to_owned(), "Stored here."),
            Place::Collection => (IssueCode::InvalidArrayElement, "stored in a collection".to_owned(), "Stored here."),
            Place::NewObject => (IssueCode::InvalidArgument, "stored in a new object".to_owned(), "Passed here."),
            Place::Kept(Some(callee)) => {
                (IssueCode::InvalidArgument, format!("passed to `{callee}`, which may keep it"), "Passed here.")
            }
            Place::Kept(None) => {
                (IssueCode::InvalidArgument, "passed to a function value, which may keep it".to_owned(), "Passed here.")
            }
            Place::Return => (IssueCode::InvalidReturnStatement, "returned".to_owned(), "Returned here."),
        };
        let message = match escape.via {
            Via::Parameter => format!("`{name}` can't be {verb}: its type states no effects."),
            Via::Local => format!(
                "This local can't be {verb}: it holds `{name}`, a function parameter whose type states no effects."
            ),
            Via::Lambda => format!(
                "This lambda can't be {verb}: it captures `{name}`, a function parameter whose type states no effects."
            ),
        };
        context.collector.report_with_code(
            code,
            Issue::error(message)
                .with_annotation(Annotation::primary(escape.span).with_message(at))
                .with_note("A method takes on the effects of each function it is given, so that function runs only inside the method's call.")
                .with_help(format!("Take a `{suggested}`, whose type states the effects it may have.")),
        );
    }
}

/// Walks the body `code` once for [`report_escaping_functions`]. An expression body is what the method returns.
fn walk_escapes<'arena, A>(code: FunctionLikeBody<'_, 'arena>, escapes: &mut Escapes<'_, '_, 'arena, A>)
where
    A: Arena,
{
    match code {
        FunctionLikeBody::Statements(statements, _) => {
            for statement in statements {
                EscapeWalker.walk_statement(statement, escapes);
            }
        }
        FunctionLikeBody::Expression(expression) => {
            escapes.check(expression, Place::Return);
            EscapeWalker.walk_expression(expression, escapes);
        }
        FunctionLikeBody::ExpressionStatement(expression) => EscapeWalker.walk_expression(expression, escapes),
    }
}

/// Where a value a body holds goes, for [`report_escaping_functions`]: each is a place a function may be kept past the
/// method's call. `Kept` names the method, function or property it is passed to, or none for a function value.
enum Place {
    Field,
    Collection,
    NewObject,
    Kept(Option<String>),
    Return,
}

/// What holds a function parameter that escapes: the parameter itself, a local set to it, or a lambda that captures
/// either.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Via {
    Parameter,
    Local,
    Lambda,
}

/// One function parameter that escapes at `span`: the parameter's index, the place, what holds it, and the type the
/// place states when that type states its effects.
struct Escape {
    span: Span,
    parameter: usize,
    place: Place,
    via: Via,
    suggested: Option<String>,
}

/// The state of [`EscapeWalker`]'s walk of one body: where each parameter that takes on effects is declared, and each
/// local that holds one, with the parameter's index and what holds it; how many lambdas enclose the walk; what
/// escaped; and whether a local joined during this walk.
struct Escapes<'analysis, 'ctx, 'arena, A>
where
    A: Arena,
{
    context: &'analysis Context<'ctx, 'arena, A>,
    artifacts: &'analysis AnalysisArtifacts,
    taking: Vec<(Span, usize, Via)>,
    in_lambda: usize,
    found: Vec<Escape>,
    joined: bool,
}

impl<'ctx, 'arena, A> Escapes<'_, 'ctx, 'arena, A>
where
    A: Arena,
{
    /// The parameter that takes on effects which `value` holds, by its index, and what holds it. A conditional or `??`
    /// holds what either of its sides holds.
    fn holds(&self, value: &Expression<'arena>) -> Option<(usize, Via)> {
        match value.unparenthesized() {
            Expression::Conditional(conditional) => {
                conditional.then.and_then(|then| self.holds(then)).or_else(|| self.holds(conditional.r#else))
            }
            Expression::Binary(binary) if matches!(binary.operator, BinaryOperator::NullCoalesce(_)) => {
                self.holds(binary.lhs).or_else(|| self.holds(binary.rhs))
            }
            Expression::ArrowFunction(_) | Expression::Closure(_) => {
                let mut captured = Captured { escapes: self, found: None };
                CaptureWalker.walk_expression(value.unparenthesized(), &mut captured);

                captured.found.map(|parameter| (parameter, Via::Lambda))
            }
            expression => self.parameter(expression),
        }
    }

    /// The index of the parameter that takes on effects which `expression` names, itself or through a local that holds
    /// it, and what holds it.
    fn parameter(&self, expression: &Expression<'arena>) -> Option<(usize, Via)> {
        let declaration = self.local(expression)?;

        self.taking.iter().find(|(declared, ..)| *declared == declaration).map(|(_, index, via)| (*index, *via))
    }

    /// Where the local or parameter `expression` names is declared.
    fn local(&self, expression: &Expression<'arena>) -> Option<Span> {
        let name = match expression {
            Expression::ConstantAccess(name) => &name.name,
            Expression::Identifier(name) => name,
            _ => return None,
        };
        let Some(Binding::Local(local)) = self.context.resolved_names.binding(name) else {
            return None;
        };

        Some(local.declaration)
    }

    /// Records the local declared at `declaration` as holding what `value` holds, when that is a parameter that takes on
    /// effects or a lambda that captures one.
    fn join(&mut self, declaration: Span, value: &Expression<'arena>) {
        let Some((parameter, _)) = self.holds(value) else {
            return;
        };

        if !self.taking.iter().any(|(declared, ..)| *declared == declaration) {
            self.taking.push((declaration, parameter, Via::Local));
            self.joined = true;
        }
    }

    /// The type an object's field that `lhs` writes declares, as `this.spare`'s. A static's write names none.
    fn field_type(&self, lhs: &Expression<'arena>) -> Option<&'ctx TUnion> {
        let Expression::Access(Access::Property(PropertyAccess {
            object,
            property: ClassLikeMemberSelector::Identifier(name),
            ..
        })) = lhs
        else {
            return None;
        };
        let class =
            self.artifacts.get_expression_type(object)?.types.iter().find_map(TAtomic::get_object_or_enum_name)?;
        let codebase: &'ctx CodebaseMetadata = self.context.codebase;

        codebase.get_property_type(class.as_bytes(), php_variable_name(name.value).as_bytes())
    }

    /// Records `value` as escaping to `place` when it holds a parameter that takes on effects.
    fn check(&mut self, value: &Expression<'arena>, place: Place) {
        self.check_stated(value, place, None);
    }

    /// As [`Self::check`], where `stated` is the type of the place, which names the type to take instead when it is a
    /// function type that states its effects.
    fn check_stated(&mut self, value: &Expression<'arena>, place: Place, stated: Option<&TUnion>) {
        let Some((parameter, via)) = self.holds(value) else {
            return;
        };

        let suggested =
            stated.filter(|stated| states_effects(stated)).map(|stated| display_sharp_type(self.context, stated));
        self.found.push(Escape { span: value.span(), parameter, place, via, suggested });
    }

    /// Checks each argument of the call at `span` against the parameter it is given to in each function or method the
    /// analysis resolved the call to. A call resolved to none runs a function value, which may keep it.
    fn check_arguments(&mut self, span: Span, arguments: &ArgumentList<'arena>) {
        let targets = self.artifacts.call_targets.get(&(span.start.offset, span.end.offset));
        for (position, argument) in arguments.arguments.iter().enumerate() {
            if self.holds(argument.value()).is_none() {
                continue;
            }

            let kept = targets.map_or(Some((None, None)), |targets| {
                targets.iter().find_map(|target| self.keeper(target, argument, position))
            });
            if let Some((callee, stated)) = kept {
                self.check_stated(argument.value(), Place::Kept(callee), stated);
            }
        }
    }

    /// Who may keep `argument`, given at `position` to `target`, with the type the parameter states: none when the
    /// parameter is a PHP# one that takes on effects or a plain PHP `callable` or `Closure`, and the callee for any
    /// other. A function a property holds, a magic method and a closure declare no parameter the checker reads, so each
    /// may keep it.
    fn keeper(
        &self,
        target: &CallTarget,
        argument: &Argument<'arena>,
        position: usize,
    ) -> Option<(Option<String>, Option<&'ctx TUnion>)> {
        let codebase: &'ctx CodebaseMetadata = self.context.codebase;
        let (metadata, sharp, callee) = match target {
            CallTarget::FunctionLike { callee: callee @ FunctionLikeIdentifier::Method(class, method), .. } => {
                let metadata = codebase.get_method(class.as_bytes(), method.as_bytes());
                // A collection's method is written on its PHP# type, as `List.add`.
                let callee = sharp_collection_name(&TObject::new_named(*class)).map_or_else(
                    || display_function_like_identifier(self.context, callee),
                    |collection| {
                        display_sharp_member(collection, metadata.map_or(*method, |method| method.original_name))
                    },
                );

                (
                    metadata,
                    codebase.get_class_like(class.as_bytes()).is_some_and(|class| class.flags.is_sharp()),
                    callee,
                )
            }
            CallTarget::FunctionLike { callee: callee @ FunctionLikeIdentifier::Function(function), .. } => (
                codebase.get_function(function.as_bytes()),
                false,
                display_function_like_identifier(self.context, callee),
            ),
            CallTarget::Property { class, property } | CallTarget::MagicMethod { class, method: property, .. } => {
                return Some((Some(display_member(self.context, *class, property)), None));
            }
            CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Closure(_), .. } => return Some((None, None)),
        };
        let parameter = metadata
            .and_then(|metadata| metadata.parameters.get(parameter_index(argument, position, &metadata.parameters)));
        let stated =
            parameter.and_then(|parameter| parameter.type_metadata.as_ref()).map(|declared| &declared.type_union);
        let keeps_inside = parameter.is_some_and(|parameter| parameter.flags.takes_on_effects())
            || !sharp && stated.is_some_and(holds_a_callable);

        (!keeps_inside).then_some((Some(callee), stated))
    }
}

/// Whether every type of `union` is a function type that states its effects, as `Function<Charge(Cart) uses Http>`.
fn states_effects(union: &TUnion) -> bool {
    union.types.iter().all(|atomic| match atomic {
        TAtomic::Callable(TCallable::Signature(signature)) => {
            signature.effects.as_ref().is_some_and(|effects| !effects.is_empty())
        }
        TAtomic::Null => true,
        _ => false,
    }) && holds_a_callable(union)
}

/// Whether a parameter of the type `union` takes a function: a function type, or plain PHP's `callable` or `Closure`,
/// which the codebase reads as one.
fn holds_a_callable(union: &TUnion) -> bool {
    union.types.iter().any(|atomic| matches!(atomic, TAtomic::Callable(_)))
}

/// Walks a method body for [`report_escaping_functions`], checking each place a value can escape to. A `return` in a
/// lambda returns from the lambda, so the lambda's own place decides whether what it captures escapes.
struct EscapeWalker;

impl<'ast, 'arena, A> Walker<'ast, 'arena, Escapes<'_, '_, 'arena, A>> for EscapeWalker
where
    A: Arena,
{
    fn walk_in_assignment(&self, assignment: &'ast Assignment<'arena>, escapes: &mut Escapes<'_, '_, 'arena, A>) {
        let place = match assignment.lhs.unparenthesized() {
            Expression::ArrayAccess(_) => Place::Collection,
            lhs => match escapes.local(lhs) {
                Some(declaration) => return escapes.join(declaration, assignment.rhs),
                None => Place::Field,
            },
        };
        let stated = escapes.field_type(assignment.lhs);
        escapes.check_stated(assignment.rhs, place, stated);
    }

    fn walk_in_local_declaration(
        &self,
        local_declaration: &'ast LocalDeclaration<'arena>,
        escapes: &mut Escapes<'_, '_, 'arena, A>,
    ) {
        escapes.join(local_declaration.name.span, local_declaration.value);
    }

    fn walk_in_array_element(
        &self,
        array_element: &'ast ArrayElement<'arena>,
        escapes: &mut Escapes<'_, '_, 'arena, A>,
    ) {
        if let Some(value) = array_element.get_value() {
            escapes.check(value, Place::Collection);
        }
    }

    fn walk_in_return(&self, r#return: &'ast Return<'arena>, escapes: &mut Escapes<'_, '_, 'arena, A>) {
        if escapes.in_lambda == 0
            && let Some(value) = r#return.value
        {
            escapes.check(value, Place::Return);
        }
    }

    fn walk_in_instantiation(
        &self,
        instantiation: &'ast Instantiation<'arena>,
        escapes: &mut Escapes<'_, '_, 'arena, A>,
    ) {
        // The constructor `new` runs, a named one or the class's main one, whose parameter types name what to take.
        let codebase = escapes.context.codebase;
        let name = instantiation.constructor.as_ref().map_or(&b"__construct"[..], |constructor| constructor.name.value);
        let constructor = escapes
            .artifacts
            .get_expression_type(instantiation)
            .and_then(|created| created.types.iter().find_map(TAtomic::get_object_or_enum_name))
            .and_then(|class| codebase.get_method(class.as_bytes(), name));
        let arguments = instantiation.argument_list.iter().flat_map(|arguments| arguments.arguments.iter());
        for (position, argument) in arguments.enumerate() {
            let stated = constructor
                .and_then(|constructor| {
                    constructor.parameters.get(parameter_index(argument, position, &constructor.parameters))
                })
                .and_then(|parameter| parameter.type_metadata.as_ref())
                .map(|declared| &declared.type_union);
            escapes.check_stated(argument.value(), Place::NewObject, stated);
        }
    }

    fn walk_in_call(&self, call: &'ast Call<'arena>, escapes: &mut Escapes<'_, '_, 'arena, A>) {
        escapes.check_arguments(call.span(), call.get_argument_list());
    }

    fn walk_in_closure(&self, _: &'ast Closure<'arena>, escapes: &mut Escapes<'_, '_, 'arena, A>) {
        escapes.in_lambda += 1;
    }

    fn walk_out_closure(&self, _: &'ast Closure<'arena>, escapes: &mut Escapes<'_, '_, 'arena, A>) {
        escapes.in_lambda -= 1;
    }

    fn walk_in_arrow_function(&self, _: &'ast ArrowFunction<'arena>, escapes: &mut Escapes<'_, '_, 'arena, A>) {
        escapes.in_lambda += 1;
    }

    fn walk_out_arrow_function(&self, _: &'ast ArrowFunction<'arena>, escapes: &mut Escapes<'_, '_, 'arena, A>) {
        escapes.in_lambda -= 1;
    }
}

/// The first parameter that takes on effects a lambda's body names, which [`CaptureWalker`] finds.
struct Captured<'escapes, 'analysis, 'ctx, 'arena, A>
where
    A: Arena,
{
    escapes: &'escapes Escapes<'analysis, 'ctx, 'arena, A>,
    found: Option<usize>,
}

struct CaptureWalker;

impl<'ast, 'arena, A> Walker<'ast, 'arena, Captured<'_, '_, '_, 'arena, A>> for CaptureWalker
where
    A: Arena,
{
    fn walk_in_expression(&self, expression: &'ast Expression<'arena>, captured: &mut Captured<'_, '_, '_, 'arena, A>) {
        if captured.found.is_none() {
            captured.found = captured.escapes.parameter(expression).map(|(parameter, _)| parameter);
        }
    }
}
