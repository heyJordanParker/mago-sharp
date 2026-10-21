//! Every issue the Lean step reports. Each one refuses the `.sharp` file it sits in.

use mago_analyzer::code::IssueCode;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::Span;

use crate::library;
use crate::library::Stop;
use crate::package::Refusal;
use crate::package::place_of;
use crate::translate::Law;
use crate::translate::Unmodeled;
use crate::translate::proof::Case;
use crate::translate::proof::LawCall;

pub(crate) const FLOAT: &str = "A law cannot reason about float arithmetic, which rounds; use int.";
pub(crate) const OVERRIDABLE: &str = "A law cannot know which implementation runs.";
pub(crate) const IDENTITY: &str = "A law sees an object as its values, so it has no identity to compare.";
pub(crate) const PROPERTY_WRITE: &str = "A law sees an object as its values, so nothing changes it in place.";
pub(crate) const EXCEPTION: &str =
    "Return a value that tells the failure apart, such as `null` or an enum case, in place of the exception.";
pub(crate) const TYPE_TEST: &str = "It reads types while the code runs.";
pub(crate) const STATIC_PROPERTY: &str = "It is state outside the arguments.";
pub(crate) const LAZY: &str = "A lazy sequence has no value to reason about.";
pub(crate) const EXTERN: &str = "Its body is native code, which a law cannot read.";
pub(crate) const RESERVED_NAME: &str =
    "The checker gives every class a member of that name, so a law cannot reach a member that has it.";
pub(crate) const PLAIN_PHP: &str =
    "Write the code the law reaches in PHP#, or pass the value plain PHP computes to the law as a parameter.";
pub(crate) const COLLECTION: &str =
    "Hold the values in a List, and read it with `count()`, `map`, `filter`, a list pattern or a spread.";
pub(crate) const LIBRARY: &str = "Write the code in a PHP# class of the project: of the standard library, a law reaches a List's `count()`, `map` and `filter`, and `Math.max` and `Math.min` on ints.";
pub(crate) const LOOP: &str = "Write the method as recursion that calls itself on a smaller value.";
pub(crate) const RECURSION: &str = "Write the method as recursion that calls itself on a smaller value: a list's rest from a list pattern, or an int one step towards a bound a condition tested.";
pub(crate) const NAMED_PATTERN: &str =
    "Write the test as the condition of an if or a ?:, and read the value it names in the branch it decides.";
pub(crate) const LAMBDA: &str = "Pass a lambda only to a List's `map` or `filter`, with a body that cannot throw, and call a PHP# method in place of any other function value.";
pub(crate) const INHERITANCE: &str =
    "Declare the members the law reaches in the class itself, in place of inheriting them.";
pub(crate) const INTERFACE_METHOD: &str = "Call the method on a value of a class that implements the interface, since a law cannot know which implementation runs.";
pub(crate) const ACCESSOR: &str = "Compute the value in a method, and give the property no accessor body.";
pub(crate) const CONSTRUCTOR_BODY: &str = "Write the constructor with promoted properties and no body, and compute the values in a static method that calls `new`.";
pub(crate) const TYPE: &str = "Give it a type a law reasons about: int, bool, string, a List, or a PHP# class, enum or interface, with or without `?`.";
pub(crate) const MISSING_TYPE: &str = "Write its type.";
pub(crate) const TUPLE: &str = "Hold the values in a class with one property for each, in place of a tuple.";
pub(crate) const STATEMENT: &str = "Write the method with the statements a law reaches: `let` and `const`, assignments to locals, `if`, `match`, `return`, and loops that only count or sum.";
pub(crate) const LOCAL_ASSIGNMENT: &str = "Assign the value to a local declared with `let`.";
pub(crate) const UPDATE: &str = "Update an int with `+=`, `-=`, `*=`, `/=` or `%=`, or a string with `+=`.";
pub(crate) const OPERATOR: &str = "Write it with the operators a law reaches: `+ - * / %` on ints, `+` on strings, `< <= > >=` on ints and strings, `== !=`, `! && ||` and `??`.";
pub(crate) const CLASS_OPERATOR: &str = "Compare the values the objects hold, such as their properties.";
pub(crate) const TEMPLATE: &str = "Join the strings with `+`.";
pub(crate) const NESTED_ASSIGNMENT: &str =
    "Assign the value in a statement of its own, before the expression that reads it.";
pub(crate) const EXPRESSION: &str = "Write the value with what a law reaches: literals, locals, operators, `new`, calls of PHP# methods, `match`, `is`, `?:` and `??`.";
pub(crate) const CONSTANT: &str = "Declare the value as a constant of a PHP# class, and read it as `Class.NAME`.";
pub(crate) const MATCH_SUBJECT: &str = "Compare the value with `==` in a `when` guard.";
pub(crate) const COMPARISON_PATTERN: &str = "Compare the value with `<`, `<=`, `>` or `>=` in a `when` guard.";
pub(crate) const GUARD: &str =
    "Write the guard without calls or arithmetic, which can throw, and compute such a value in the arm.";
pub(crate) const PATTERN_VALUE: &str = "Write the pattern's value as a literal or a constant.";
pub(crate) const CONSTANT_VALUE: &str = "Write the constant's value without calls or arithmetic, which can throw.";
pub(crate) const MEMBER_NAME: &str = "Name the member in the code.";
pub(crate) const BODYLESS: &str = "A law reads a method's body, and this method has none it can read.";
pub(crate) const ARGUMENT: &str = "Pass each argument by its position.";
pub(crate) const DEFAULT_ARGUMENT: &str = "Pass every argument, the ones with a default value included.";
pub(crate) const CASE_FIELD: &str =
    "A law tells the cases of an enum apart, and reads none of the data a case carries.";
pub(crate) const MEMBER: &str = "Read a property the class declares, or call one of its methods.";
pub(crate) const RECEIVER: &str = "Call it on a value of a PHP# class, enum or interface.";
pub(crate) const LAW_MATCH: &str =
    "Match a parameter of the law, a value a pattern of its body names, or a property of one.";
pub(crate) const SELF_HOLDING: &str =
    "A law reasons only about a class whose properties never hold a value of the class itself.";

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
/// PHP#, or the law's claim as written when the goal holds a part PHP# writes no other way, and the call of the law
/// itself on a smaller value when the case knows one and makes no such call.
pub(crate) fn unproven_case(law: &Law, case: &Case, goal: Option<&str>) -> Issue {
    let goal = goal.unwrap_or(&law.proof.claim);
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

/// A law whose generated module Lean did not build: it stopped at the checker's limit, or Lean rejected it, which is a
/// bug in the checker's translation, never in the project. The debug log names the module and the line.
pub(crate) fn generated_module_refused(law: &Law, module: &str, refusal: &Refusal) -> Issue {
    let reason = match refusal {
        Refusal::Stopped(stop) => format!("{}.", stopped(*stop, "the code it reaches")),
        Refusal::Rejected(line) => {
            tracing::debug!("The law checker rejected the module {module} of law {}, at line {line}.", law.name);

            "the checker translated the code it reaches wrongly. This is a bug in the PHP# checker; report it to its maintainers.".to_owned()
        }
    };

    unproven(law, format!("Law {} is not proven: {reason}", law.name))
}

/// `when …, ` with `conditions` joined by `&&`, or nothing when there are none.
fn when(conditions: &[String]) -> String {
    if conditions.is_empty() { String::new() } else { format!("when {}, ", conditions.join(" && ")) }
}

/// Why Lean did not finish a module: it ran past the processor-time limit Mago sets on each Lean process, or it waited
/// until Mago's deadline. `what` names what to make end sooner, such as "the code it reaches".
fn stopped(stop: Stop, what: &str) -> String {
    match stop {
        Stop::Processor => format!(
            "it ran past {} processor seconds, the limit the checker sets on each check. Make {what} end sooner",
            library::processor_seconds()
        ),
        Stop::Waited => {
            format!("it waited {} seconds without finishing. Make {what} end sooner", library::deadline().as_secs())
        }
    }
}

/// The law checker could not be installed or does not start. The message names it only as the law checker, which is
/// all a PHP# developer needs to know of it, and the debug log holds what failed.
pub(crate) fn uninstalled_checker(span: Span) -> Issue {
    Issue::error(
        "The law checker could not be installed. Run vendor/bin/mago compile again to retry, with MAGO_LOG=debug to see why.",
    )
    .with_code(IssueCode::LawCheckerUnavailable.as_str())
    .with_annotation(Annotation::primary(span))
}

/// No `lake` is on the `PATH`, and the Mago build pins no elan for its target, so it cannot install Lean. The message
/// describes the build, never the host, and gives only advice true on every host it runs on: a musl build also runs on
/// glibc Linux, where Lean runs, and on Alpine, where neither Mago's glibc build nor Lean starts.
pub(crate) fn uninstallable_lean(span: Span) -> Issue {
    let reason = if cfg!(target_env = "musl") {
        "this musl build of the checker can't install it, because the law checker has no musl build. On glibc Linux, the checker's glibc build installs it itself.".to_owned()
    } else {
        format!("this {} {} build of the checker can't install it.", std::env::consts::OS, std::env::consts::ARCH)
    };

    Issue::error(format!("Laws need the law checker, and {reason}"))
        .with_code(IssueCode::LawCheckerUnavailable.as_str())
        .with_annotation(Annotation::primary(span))
}

fn unproven(law: &Law, message: String) -> Issue {
    Issue::error(message).with_code(IssueCode::LawUnproven.as_str()).with_annotation(Annotation::primary(law.span))
}
