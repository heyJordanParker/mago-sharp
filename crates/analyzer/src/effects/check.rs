//! The rules effects decide: an `extern` declaration's own checks, and the rules over solved effects.

use std::cell::OnceCell;

use foldhash::HashMap;
use mago_allocator::Arena;
use mago_codex::metadata::CodebaseMetadata;
use mago_names::display_sharp_member;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Extern;
use mago_word::Word;
use mago_word::word;

use crate::code::IssueCode;
use crate::context::Context;
use crate::effects::Body;
use crate::effects::Effect;
use crate::effects::Effects;
use crate::effects::Impurity;
use crate::effects::Runs;
use crate::utils::names::namespace_function;
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
            // A plain PHP function is declared through its namespace, spec section 23: `global.trim`, or `Pdf.render`
            // after `import Vendor.Pdf;`.
            let (target, import) = if class.is_empty() {
                match member.as_bytes().iter().rposition(|byte| *byte == b'\\') {
                    Some(separator) => {
                        let namespace = word(&member.as_bytes()[..separator]);
                        let function = String::from_utf8_lossy(&member.as_bytes()[separator + 1..]);

                        (format!("{}.{function}", code_class_name(namespace)), missing_import(namespace))
                    }
                    None => (format!("global.{member}"), None),
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
