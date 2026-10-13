//! Every issue the Lean step reports. Each one refuses the `.sharp` file it sits in.

use mago_analyzer::code::IssueCode;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::Span;

use crate::translate::Law;
use crate::translate::Unmodeled;

pub(crate) const FLOAT: &str = "Lean cannot prove facts about float arithmetic; use int.";
pub(crate) const OVERRIDABLE: &str = "Lean cannot see which implementation runs.";
pub(crate) const IDENTITY: &str = "Classes keep no identity in Lean.";
pub(crate) const PROPERTY_WRITE: &str = "Lean models objects as values.";
pub(crate) const TRY_CATCH: &str = "An exception is a bug, and a law reasons about values.";
pub(crate) const TYPE_TEST: &str = "It reads types while the code runs.";
pub(crate) const STATIC_PROPERTY: &str = "It is state outside the arguments.";
pub(crate) const LAZY: &str = "A lazy sequence has no value to reason about.";
pub(crate) const EXTERN: &str = "Its body is native code, which Lean cannot read.";
pub(crate) const RESERVED_NAME: &str = "Lean declares a member of that name for every structure and inductive.";
pub(crate) const PLAIN_PHP: &str = "Lean cannot see inside plain PHP.";
pub(crate) const COLLECTION: &str = "Mago does not translate collections to Lean.";
pub(crate) const LIBRARY: &str = "Mago does not translate the standard library to Lean.";
pub(crate) const LOOP: &str = "Mago does not translate loops to Lean.";
pub(crate) const RECURSION: &str = "Mago does not translate recursion to Lean.";
pub(crate) const LAMBDA: &str = "Mago does not translate lambdas to Lean.";
pub(crate) const INHERITANCE: &str = "Mago does not translate inheritance to Lean.";
pub(crate) const ACCESSOR: &str = "Mago does not translate accessor bodies to Lean.";
pub(crate) const CONSTRUCTOR_BODY: &str = "Mago does not translate constructor bodies to Lean.";
pub(crate) const NOT_TRANSLATED: &str = "Mago does not translate it to Lean.";

/// A construct the translation does not model, which `law` reaches: on the construct when the law's file holds it, and
/// else on the law, pointing to the construct.
pub(crate) fn unmodeled(law: &Law, construct: &Unmodeled) -> Issue {
    let issue = Issue::error(format!(
        "Law {} reaches {} in {}. {}",
        law.name, construct.what, construct.place, construct.reason
    ))
    .with_code(IssueCode::UnmodeledConstruct.as_str());

    if construct.span.file_id == law.span.file_id {
        issue.with_annotation(Annotation::primary(construct.span))
    } else {
        issue
            .with_annotation(Annotation::primary(law.span))
            .with_annotation(Annotation::secondary(construct.span).with_message(construct.what.clone()))
    }
}

/// A law that reaches a declaration whose file the analysis refused, so no translation of it exists.
pub(crate) fn refused_reach(law: &Law, place: &str) -> Issue {
    unproven(
        law,
        format!(
            "Law {} is not proven: it reaches {place}, whose file has errors. Fix them, then run vendor/bin/mago compile again.",
            law.name
        ),
    )
}

/// A law that reaches a declaration of an accepted file the translation left out: a bug in Mago.
pub(crate) fn untranslated(law: &Law, place: &str) -> Issue {
    unproven(
        law,
        format!(
            "Law {} is not proven: Mago did not translate {place}, which the law reaches. This is a bug in Mago; report it to Mago's maintainers.",
            law.name
        ),
    )
}

/// A law whose proof file the PSR-4 map of `composer.json` names no Lean module for.
pub(crate) fn no_module(law: &Law, file: &str) -> Issue {
    unproven(
        law,
        format!(
            "Law {} is not proven: {file} is in no PSR-4 folder of composer.json, so its proof file has no Lean module name.",
            law.name
        ),
    )
}

pub(crate) fn gap(law: &Law, proof: &str, line: u32) -> Issue {
    unproven(
        law,
        format!(
            "Law {} is not proven: {proof} line {line} is a gap (sorry). Write the proof there, or change the law.",
            law.name
        ),
    )
}

pub(crate) fn rejected(law: &Law, proof: &str, line: u32, message: &str) -> Issue {
    let (first, rest) = message.split_once('\n').unwrap_or((message, ""));
    let issue = unproven(
        law,
        format!("Law {} is not proven: Lean rejected its proof in {proof} line {line}: {first}", law.name),
    );

    if rest.trim().is_empty() { issue } else { issue.with_note(message.to_owned()) }
}

/// A package's law no theorem of its proof file has the type of. Mago adds a proof to a project's proof file, and never
/// writes under `vendor/`.
pub(crate) fn no_proof(law: &Law, proof: &str) -> Issue {
    unproven(
        law,
        format!(
            "Law {} has no proof in {proof}. Its package ships no proof for it, so report it to the package's maintainers.",
            law.name
        ),
    )
}

pub(crate) fn native_decide(law: &Law, proof: &str) -> Issue {
    unproven(
        law,
        format!(
            "Law {} is not proven: its proof in {proof} uses native_decide, which trusts compiled code instead of Lean's kernel. Use decide.",
            law.name
        ),
    )
}

pub(crate) fn axiom(law: &Law, proof: &str, axiom: &str) -> Issue {
    unproven(
        law,
        format!(
            "Law {} is not proven: its proof in {proof} uses the axiom {axiom}, which Lean's kernel does not check. Prove it without the axiom.",
            law.name
        ),
    )
}

/// A law whose generated module Lean refused: a bug in Mago's translation, never in the project.
pub(crate) fn generated_module_refused(law: &Law, module: &str, line: u32, message: &str) -> Issue {
    let first = message.lines().next().unwrap_or(message);

    unproven(
        law,
        format!(
            "Law {} is not proven: Lean refused the module {module} that Mago generated for it, at line {line}: {first}. This is a bug in Mago; report it to Mago's maintainers.",
            law.name
        ),
    )
    .with_note(message.to_owned())
}

/// An error in a proof file that no theorem for a law holds.
pub(crate) fn rejected_file(span: Span, proof: &str, line: u32, message: &str) -> Issue {
    let first = message.lines().next().unwrap_or(message);

    Issue::error(format!("Lean rejected {proof} line {line}: {first}"))
        .with_code(IssueCode::UnprovenLaw.as_str())
        .with_annotation(Annotation::primary(span))
        .with_note(message.to_owned())
}

/// A proof of a law the `.sharp` file no longer states, on the class's name.
pub(crate) fn deleted_law(class: Span, proof: &str, line: u32, statement: &str, source: &str) -> Issue {
    Issue::error(format!(
        "{proof} line {line} proves {statement}, which {source} no longer states. Delete the proof, or restore the law."
    ))
    .with_code(IssueCode::NonExistentLaw.as_str())
    .with_annotation(Annotation::primary(class))
}

pub(crate) fn lake_not_found(span: Span) -> Issue {
    Issue::error(
        "Laws and structure rules need Lean 4.34.1, and lake was not found. Install elan from https://lean-lang.org/install, then run vendor/bin/mago compile again.",
    )
    .with_code(IssueCode::MissingLean.as_str())
    .with_annotation(Annotation::primary(span))
}

pub(crate) fn elan_failed(span: Span, message: &str) -> Issue {
    Issue::error(format!("Laws and structure rules need Lean 4.34.1, and elan could not install it: {message}."))
        .with_code(IssueCode::MissingLean.as_str())
        .with_annotation(Annotation::primary(span))
}

fn unproven(law: &Law, message: String) -> Issue {
    Issue::error(message).with_code(IssueCode::UnprovenLaw.as_str()).with_annotation(Annotation::primary(law.span))
}
