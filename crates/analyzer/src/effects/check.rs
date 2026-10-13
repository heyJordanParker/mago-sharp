//! The rules effects decide: an `extern` declaration's own checks, what a `uses` names and what a static holds, and
//! the rules over solved effects.

use std::cell::OnceCell;

use foldhash::HashMap;
use mago_allocator::Arena;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::TypeRef;
use mago_codex::ttype::atomic::TAtomic;
use mago_names::display_sharp_member;
use mago_names::short_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Extern;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Program;
use mago_syntax::cst::Uses;
use mago_syntax::walker::Walker;
use mago_word::Word;
use mago_word::ascii_lowercase_word;

use crate::artifacts::AnalysisArtifacts;
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
use crate::utils::names::display_member;
use crate::utils::names::sharp_class_like_name;
use crate::utils::names::sharp_code_class_name;
use crate::utils::names::sharp_effect_name;
use crate::utils::names::sharp_missing_imports;

/// Spec section 29: a getter reads, so it has no effect and changes nothing.
pub(crate) fn getters_must_be_pure(
    effects: &Effects,
    codebase: &CodebaseMetadata,
    short_name_counts: &OnceCell<HashMap<String, u32>>,
) -> IssueCollection {
    effects
        .impure_bodies()
        .filter(|(body, ..)| matches!(body, Body::Accessor(_, _, accessor) if accessor.as_bytes() == b"get"))
        .map(|(_, member, imports, scope, impurity)| {
            let class_name = |class| sharp_class_like_name(codebase, imports, short_name_counts, class);
            let effect_name = |effect| sharp_effect_name(codebase, imports, short_name_counts, effect);
            let code_class_name = |class| sharp_code_class_name(codebase, imports, scope, class);
            let missing_import = |class| sharp_missing_imports(codebase, imports, scope, [class]);

            impure(
                IssueCode::ImpureGetter,
                format!("Getter `{member}` {}. Getters must be pure.", impurity.describe(&class_name, &effect_name)),
                impurity.span,
                "getter",
                &impurity,
                &class_name,
                &code_class_name,
                &missing_import,
            )
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
            let (target, import) = if class.is_empty() {
                (member.to_string(), None)
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
        if context.codebase.get_function(member.as_bytes()).is_none() {
            context.collector.report_with_code(
                IssueCode::NonExistentFunction,
                Issue::error(format!("Function `{written}` does not exist."))
                    .with_annotation(Annotation::primary(target)),
            );
        }

        return;
    }

    let Some(class_metadata) = context.codebase.get_class_like(class.as_bytes()) else {
        context.collector.report_with_code(
            IssueCode::NonExistentClassLike,
            Issue::error(format!("Cannot find class, interface, enum, or type alias `{written}`."))
                .with_annotation(Annotation::primary(target)),
        );

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
    let mut names = Vec::new();
    UsesWalker.walk_program(program, &mut names);

    for name in names {
        let effect = context.resolved_names.get(name);
        artifacts.symbol_references.add_reference_to_symbol(&block_context.scope, ascii_lowercase_word(effect), false);
        if is_effect(context.codebase, effect) {
            continue;
        }

        let mut foreign: Vec<String> = context
            .codebase
            .class_likes
            .values()
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
                "`{}` is not an effect: `uses` takes {} or {}.",
                String::from_utf8_lossy(name.value),
                effects.join(", "),
                short_name(events()),
            ))
            .with_annotation(Annotation::primary(name.span).with_message("Written here."))
            .with_note("An effect is a standard effect, a `foreign` class, or `Events`, the effect of `emit`."),
        );
    }
}

/// The names every `uses` of a program writes.
struct UsesWalker;

impl<'ast, 'arena> Walker<'ast, 'arena, Vec<&'ast LocalIdentifier<'arena>>> for UsesWalker {
    fn walk_in_uses(&self, uses: &'ast Uses<'arena>, names: &mut Vec<&'ast LocalIdentifier<'arena>>) {
        names.extend(uses.names.iter());
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

        let held = declared.type_union.get_all_child_nodes().into_iter().find_map(|node| match node {
            TypeRef::Atomic(TAtomic::Object(object)) => object
                .get_name()
                .and_then(|name| context.codebase.get_class_like(name.as_bytes()))
                .filter(|held| held.flags.is_foreign()),
            _ => None,
        });
        let Some(held) = held else {
            continue;
        };

        let held = sharp_class_like_name(
            context.codebase,
            &context.imported_names,
            &context.short_name_counts,
            held.original_name,
        );
        let property_name = display_member(context, class.original_name, property.name.0);
        context.collector.report_with_code(
            IssueCode::ForeignInStatic,
            Issue::error(format!("`{property_name}` is static, so it cannot hold a `{held}`."))
                .with_annotation(Annotation::primary(property.name_span.unwrap_or(declared.span)))
                .with_note("A `foreign` object reaches code only through constructors, so no static holds one.")
                .with_help(format!("Take the `{held}` as a constructor parameter instead.")),
        );
    }
}
