//! The rules effects decide: an `extern` declaration's own checks, what a `uses` names and what a static holds, and
//! the rules over solved effects.

use std::cell::OnceCell;

use foldhash::HashMap;
use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::TypeRef;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::MethodParts;
use mago_names::display_sharp_member;
use mago_names::short_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Extern;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Method;
use mago_syntax::cst::Program;
use mago_syntax::cst::Uses;
use mago_syntax::walker::Walker;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::effects::Body;
use crate::effects::Effect;
use crate::effects::Effects;
use crate::effects::Impurity;
use crate::effects::Runs;
use crate::effects::events;
use crate::effects::is_effect;
use crate::utils::names::display_member;
use crate::utils::names::is_imported_namespace;
use crate::utils::names::namespace_function;
use crate::utils::names::sharp_class_like_name;
use crate::utils::names::sharp_code_class_name;
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
                &[],
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
            let allowed: Vec<String> = value.allowed.iter().map(|effect| effect_name(*effect)).collect();

            let issue = impure(
                value.code,
                format!("{reason}, so a `{}` cannot hold it.", value.declared),
                value.span,
                body,
                &impurity,
                &allowed,
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
            let uses: Vec<String> = declaration.uses.iter().map(|effect| effect_name(*effect)).collect();
            let allows = if uses.is_empty() {
                format!("the pure `{declared}`")
            } else {
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
                &uses,
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
/// `extern` to write when the callee has none, or how to call a plain PHP property's code from outside `body`. The
/// `extern` names the callee's class, or its function, and the effects `allowed` lists, as the place's `uses` names
/// them. `class_name` names each class in prose, `code_class_name` names it in the `extern`, and `missing_import`
/// names the import the `extern` needs when the body's file doesn't bind its class.
#[allow(clippy::too_many_arguments)]
fn impure(
    code: IssueCode,
    message: String,
    span: Span,
    body: &str,
    impurity: &Impurity,
    allowed: &[String],
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
                (code_class_name(class), missing_import(class))
            };
            let import = import.map(|import| format!(" {import}")).unwrap_or_default();

            issue.with_help(if allowed.is_empty() {
                format!(
                    "Declare it in a .sharp file: `extern {target};` when it has no effect, or name its effects after `uses`.{import}"
                )
            } else {
                format!("Declare its effect in a .sharp file, such as `extern {target} uses {};`.{import}", allowed.join(", "))
            })
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
        let mut effects: Vec<String> = nameable
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
        effects.sort_unstable();
        effects.push(short_name(events()));

        context.collector.report_with_code(
            IssueCode::InvalidEffect,
            Issue::error(format!(
                "`{}` is not an effect: `uses` takes {} or a `foreign` class.",
                String::from_utf8_lossy(name.value),
                effects.join(", "),
            ))
            .with_annotation(Annotation::primary(name.span).with_message("Written here."))
            .with_note("An effect is a `foreign` class, as the standard library's `Clock` is, or `Events`, the effect of `emit`."),
        );
    }
}

/// The names every effect `uses` of a program writes. A method's `uses` also names its function parameters, with or
/// without a body, which are no effect, and a method with a body names only those, as the semantics checker holds it
/// to.
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

/// Spec section 29: a module binding's lifetime decides how often a `foreign` object is created, and a static outlives
/// every binding, so a static property of `class` holds none, alone or inside a collection.
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
            .with_note(
                "A module binding decides how often a `foreign` object is created, and a static outlives every binding, so no static holds one.",
            )
            .with_help(format!("Take the `{held}` as a constructor or method parameter instead.")),
    );
}
