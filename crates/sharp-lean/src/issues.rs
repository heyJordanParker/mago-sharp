//! Every issue the Lean step reports. Each one refuses the `.sharp` file it sits in.

use mago_analyzer::code::IssueCode;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::Span;

use crate::library;
use crate::library::Stop;
use crate::package;
use crate::package::Message;
use crate::runner::Refusal;
use crate::runner::Refused;
use crate::translate::Law;
use crate::translate::Unmodeled;
use crate::translate::proof::Case;
use crate::translate::proof::LawCall;

pub(crate) const FLOAT: &str = "A law cannot reason about float arithmetic, which rounds; use int.";
pub(crate) const OVERRIDABLE: &str = "A law cannot know which implementation runs.";
pub(crate) const IDENTITY: &str = "A law sees an object as its values, so it has no identity to compare.";
pub(crate) const PROPERTY_WRITE: &str = "A law sees an object as its values, so nothing changes it in place.";
pub(crate) const TRY_CATCH: &str = "An exception is a bug, and a law reasons about values.";
pub(crate) const TYPE_TEST: &str = "It reads types while the code runs.";
pub(crate) const STATIC_PROPERTY: &str = "It is state outside the arguments.";
pub(crate) const LAZY: &str = "A lazy sequence has no value to reason about.";
pub(crate) const EXTERN: &str = "Its body is native code, which a law cannot read.";
pub(crate) const RESERVED_NAME: &str =
    "The checker gives every class a member of that name, so a law cannot reach a member that has it.";
pub(crate) const PLAIN_PHP: &str = "A law cannot see inside plain PHP.";
pub(crate) const COLLECTION: &str = "A law cannot reason about this use of a collection.";
pub(crate) const LIBRARY: &str = "A law cannot reason about the standard library's code.";
pub(crate) const LOOP: &str = "Write the method as recursion that calls itself on a smaller value.";
pub(crate) const RECURSION: &str = "Write the method as recursion that calls itself on a smaller value: a list's rest from a list pattern, or an int one step towards a bound a condition tested.";
pub(crate) const NAMED_PATTERN: &str =
    "Write the test as the condition of an if or a ?:, and read the value it names in the branch it decides.";
pub(crate) const LAMBDA: &str = "A law cannot reason about this lambda.";
pub(crate) const INHERITANCE: &str = "A law cannot reason about inheritance.";
pub(crate) const INTERFACE_METHOD: &str =
    "A law cannot reason about an interface method, because the implementation that runs is chosen at run time.";
pub(crate) const ACCESSOR: &str = "A law cannot reason about an accessor's body.";
pub(crate) const CONSTRUCTOR_BODY: &str = "A law cannot reason about a constructor's body.";
pub(crate) const NOT_TRANSLATED: &str = "A law cannot reason about it.";

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

/// A law that reaches a declaration of an accepted file the translation left out: a bug in the checker.
pub(crate) fn untranslated(law: &Law, place: &str) -> Issue {
    unproven(
        law,
        format!(
            "Law {} is not proven: the checker left out {place}, which the law reaches. This is a bug in the PHP# checker; report it to its maintainers.",
            law.name
        ),
    )
}

/// A case of `law`'s proof that the automatic steps did not close, with its conditions and the goal still to show in
/// PHP#, and the call of the law itself on a smaller value when the case knows one and makes no such call.
pub(crate) fn unproven_case(law: &Law, case: &Case, goal: Option<&str>) -> Issue {
    let goal = goal.unwrap_or("the law's claim");
    let issue = Issue::error(format!("law {} is not proven", law.name))
        .with_code(IssueCode::LawUnproven.as_str())
        .with_annotation(Annotation::primary(law.span))
        .with_note(format!("{}still to show: {goal}", when(&case.conditions)));

    match &case.smaller {
        Some((parameter, call)) => issue.with_help(format!("the law for a smaller {parameter} is the call {call}")),
        None => issue,
    }
}

/// A call in `law`'s body of a law whose `when` condition the case of the call does not show, with the part of the
/// condition still to show in PHP#.
pub(crate) fn unshown_condition(law: &Law, call: &LawCall, goal: Option<&str>) -> Issue {
    let goal = goal.unwrap_or("its `when` condition");

    Issue::error(format!("law {} is not proven", law.name))
        .with_code(IssueCode::LawUnproven.as_str())
        .with_annotation(Annotation::primary(call.span).with_message("This call needs its law's `when` condition."))
        .with_note(format!("{}still to show for the call {}: {goal}", when(&call.conditions), call.written))
}

/// A law whose proof calls the law `called` states, which is not proven.
pub(crate) fn through(law: &Law, called: &str) -> Issue {
    Issue::error(format!("law {} is not proven", law.name))
        .with_code(IssueCode::LawUnproven.as_str())
        .with_annotation(Annotation::primary(law.span))
        .with_note(format!("it calls the law {}, which is not proven", place_of(called)))
}

/// A call of a law that calls back the law it is in, through the laws it calls.
pub(crate) fn law_cycle(span: Span, called: &str) -> Issue {
    Issue::error("a law calls itself only on a smaller value")
        .with_code(IssueCode::LawRecursion.as_str())
        .with_annotation(
            Annotation::primary(span).with_message(format!("{} calls back the law this call is in.", place_of(called))),
        )
}

/// A law whose proof the checker wrote wrong: a bug in the checker, never in the project.
pub(crate) fn rejected_proof(law: &Law) -> Issue {
    unproven(
        law,
        format!(
            "Law {} is not proven: the checker wrote a proof of it that does not check. This is a bug in the PHP# checker; report it to its maintainers.",
            law.name
        ),
    )
}

/// A law whose generated module Lean refused: a bug in the checker's translation, never in the project.
pub(crate) fn generated_module_refused(law: &Law, module: &str, line: u32) -> Issue {
    let place = if line == 0 { String::new() } else { format!(", at line {line}") };

    unproven(
        law,
        format!(
            "Law {} is not proven: the module {module} the checker generated for it is wrong{place}. This is a bug in the PHP# checker; report it to its maintainers.",
            law.name
        ),
    )
}

/// `when …, ` with `conditions` joined by `&&`, or nothing when there are none.
fn when(conditions: &[String]) -> String {
    if conditions.is_empty() { String::new() } else { format!("when {}, ", conditions.join(" && ")) }
}

/// `Money.addKeepsCurrency` of the statement `App.Shared.Money.addKeepsCurrency`.
fn place_of(statement: &str) -> String {
    let mut parts = statement.rsplitn(3, '.');
    match (parts.next(), parts.next()) {
        (Some(law), Some(class)) => format!("{class}.{law}"),
        _ => statement.to_owned(),
    }
}

/// A fact that breaks a structure rule, on the code that holds it: `This import of App.Tenant.Store.Receipts breaks
/// the structure rule neverReachesTenant in app/Shared/Module.lean.`
pub(crate) fn broken_rule(span: Span, fact: &str, rule: &str, rules: &str) -> Issue {
    Issue::error(format!("This {fact} breaks the structure rule {rule} in {rules}."))
        .with_code(IssueCode::BrokenRule.as_str())
        .with_annotation(Annotation::primary(span))
}

/// A rule of the `Module.lean` `rules` that the runner did not run, on each file of the namespace it constrains, since
/// the rule cannot check them.
pub(crate) fn refused_rule(span: Span, rules: &str, namespace: &str, rule: &str, refused: &Refused) -> Issue {
    let reason = match &refused.refusal {
        Refusal::Gap => "it uses sorry. Write the rule out".to_owned(),
        Refusal::Axiom(axiom) => format!("it uses the axiom {axiom}, which Lean's kernel does not check"),
        Refusal::Unsafe(name) => format!("it reaches {name}, which is unsafe, so Lean's kernel does not check it"),
        Refusal::ImplementedBy(name) => {
            format!("it reaches {name}, whose compiled code is not the definition Lean checked")
        }
        Refusal::Reserved(name) => format!(
            "it reaches {name}, which makes a namespace from a string. Name the namespace by its full name, as App.Shared.Schema"
        ),
        Refusal::Unevaluable(message) => format!("Lean cannot evaluate it: {}", one_line(message)),
        Refusal::Stopped(stop) => stopped(*stop, "the rule"),
    };

    Issue::error(format!(
        "This file is in {namespace}, whose structure rules {rules} holds, and its rule {rule} at line {}, column {} does not run: {reason}.",
        refused.line,
        refused.column + 1
    ))
    .with_code(IssueCode::BrokenRule.as_str())
    .with_annotation(Annotation::primary(span))
}

/// An error of a `Module.lean` Lean rejected, on each file of the namespace its rules constrain, since no rule of it can
/// check them.
pub(crate) fn rejected_rules(span: Span, rules: &str, namespace: &str, error: &Message) -> Issue {
    rejected_lean_error(
        span,
        format!("This file is in {namespace}, whose structure rules {rules} holds, and Lean rejected {rules}"),
        "",
        error,
    )
}

/// An error of the workspace Lean file `imported` that a `Module.lean` imports, on each file of the namespace its rules
/// constrain.
pub(crate) fn rejected_rules_import(
    span: Span,
    rules: &str,
    namespace: &str,
    imported: &str,
    error: &Message,
) -> Issue {
    rejected_lean_error(
        span,
        format!(
            "This file is in {namespace}, whose structure rules {rules} holds, and {rules} imports {imported}, which Lean rejected"
        ),
        " at",
        error,
    )
}

/// An error of a Lean file in a PSR-4 folder that no Lean file imports and Lean rejected, such as one that holds a
/// `@[rule]` but is no `Module.lean`, on each file of the namespace of its folder.
pub(crate) fn rejected_lean(span: Span, file: &str, namespace: &str, error: &Message) -> Issue {
    rejected_lean_error(span, format!("This file is in {namespace}, and Lean rejected {file}"), "", error)
}

/// `rejection` followed by where Lean reported `error`, after `at`, with its column counted from 1, and its whole
/// message, each line joined to the next by one space. An error of a whole file, at line 0, has no place.
fn rejected_lean_error(span: Span, rejection: String, at: &str, error: &Message) -> Issue {
    let place = if error.pos.line == 0 {
        String::new()
    } else {
        format!("{at} line {}, column {}", error.pos.line, error.pos.column + 1)
    };

    Issue::error(format!("{rejection}{place}: {}.", one_line(&error.data)))
        .with_code(IssueCode::BrokenRule.as_str())
        .with_annotation(Annotation::primary(span))
}

/// Why Lean did not finish a file or a rule: it ran past the processor-time limit Mago sets on each Lean process, or it
/// waited until Mago's deadline. `what` names what to make end sooner, such as "the rule".
pub(crate) fn stopped(stop: Stop, what: &str) -> String {
    match stop {
        Stop::Processor => format!(
            "it ran past {} processor seconds, the limit the checker sets on each Lean process. Make {what} end sooner",
            library::processor_seconds()
        ),
        Stop::Waited => {
            format!("it waited {} seconds without finishing. Make {what} end sooner", library::deadline().as_secs())
        }
    }
}

/// Lean's `message` on one line: each line trimmed and joined to the next by one space, without its final period.
fn one_line(message: &str) -> String {
    let lines: Vec<&str> = message.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let message = lines.join(" ");

    message.strip_suffix('.').unwrap_or(&message).to_owned()
}

/// Why the workspace Lean file `file`, in the PSR-4 folder of `namespace`, cannot be built: Lean or Mago owns the root of
/// `namespace`, in this case or in another, which names the same build folder on a disk that ignores case.
pub(crate) fn owned_root(namespace: &str, file: &str) -> String {
    let first = namespace.split('.').next().unwrap_or(namespace);
    let kept = match package::owner(namespace) {
        Some(owner) if owner != first => format!(
            "`{first}` names the same folder as `{owner}` on a disk that ignores case, and `{owner}` is a name Lean or the checker keeps"
        ),
        _ => format!("`{first}` is a name Lean or the checker keeps"),
    };

    format!(
        "{kept}, so Lean files under the `{}\\` root, such as {file}, can't be built. Move them under one of your own namespaces",
        namespace.replace('.', "\\")
    )
}

/// A Lean file under a root Lean or Mago owns, on each file under its folder, since none of its rules can run.
pub(crate) fn owned_lean(span: Span, namespace: &str, file: &str) -> Issue {
    Issue::error(format!("{}.", owned_root(namespace, file)))
        .with_code(IssueCode::BrokenRule.as_str())
        .with_annotation(Annotation::primary(span))
}

/// A `Module.lean` in no PSR-4 folder, on each file under its folder, since none of its rules can run.
pub(crate) fn no_rules_module(span: Span, rules: &str) -> Issue {
    Issue::error(format!(
        "{rules} is in no PSR-4 folder of composer.json, so its structure rules have no Lean module name and cannot run."
    ))
    .with_code(IssueCode::BrokenRule.as_str())
    .with_annotation(Annotation::primary(span))
}

/// No `lake` is on the `PATH`, and Mago could not install Lean, with what failed.
pub(crate) fn lake_not_found(span: Span, failure: &str) -> Issue {
    Issue::error(format!(
        "Laws and structure rules need Lean 4.34.1, and no lake is on the PATH. {failure}. Run vendor/bin/mago compile again to retry."
    ))
    .with_code(IssueCode::MissingLean.as_str())
    .with_annotation(Annotation::primary(span))
}

/// No `lake` is on the `PATH`, and the Mago build pins no elan for its target, so it cannot install Lean. The message
/// describes the build, never the host, and gives only advice true on every host it runs on: a musl build also runs on
/// glibc Linux, where Lean runs, and on Alpine, where neither Mago's glibc build nor Lean starts.
pub(crate) fn uninstallable_lean(span: Span) -> Issue {
    let reason = if cfg!(target_env = "musl") {
        "This musl build of the checker can't install Lean, because Lean has no musl build. On glibc Linux, the checker's glibc build installs Lean itself.".to_owned()
    } else {
        format!("This {} {} build of the checker can't install Lean.", std::env::consts::OS, std::env::consts::ARCH)
    };

    Issue::error(format!("Laws and structure rules need Lean 4.34.1, and no lake is on the PATH. {reason}"))
        .with_code(IssueCode::MissingLean.as_str())
        .with_annotation(Annotation::primary(span))
}

pub(crate) fn elan_failed(span: Span, message: &str) -> Issue {
    Issue::error(format!("Laws and structure rules need Lean 4.34.1, and elan could not install it: {message}."))
        .with_code(IssueCode::MissingLean.as_str())
        .with_annotation(Annotation::primary(span))
}

fn unproven(law: &Law, message: String) -> Issue {
    Issue::error(message).with_code(IssueCode::LawUnproven.as_str()).with_annotation(Annotation::primary(law.span))
}
