//! The rules effects decide: an `extern` declaration's own checks, what a `uses` names, what a static holds and where
//! a function parameter goes, and the rules over solved effects.

use std::sync::Arc;

use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::r#extern::ExternMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::TypeRef;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::callable::TCallableSignature;
use mago_codex::ttype::atomic::generic::TGenericParameter;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::named::TNamedObject;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::expander::StaticClassType;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::expander::expand_union;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::Binding;
use mago_names::binding::MethodParts;
use mago_names::binding::php_variable_name;
use mago_names::display_sharp_member;
use mago_names::scope::NamespaceScope;
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
use mago_syntax::cst::ExternNamespaced;
use mago_syntax::cst::FunctionHint;
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
use mago_word::WordMap;
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
use crate::effects::Runs;
use crate::effects::events;
use crate::effects::holding_declaration;
use crate::effects::is_effect;
use crate::effects::summary::parameter_index;
use crate::resolver::extension::collection_member_kind;
use crate::statement::class_like::r#struct::data_enum;
use crate::statement::function_like::FunctionLikeBody;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_function_like_identifier;
use crate::utils::names::display_member;
use crate::utils::names::display_sharp_parameter_type;
use crate::utils::names::display_sharp_type;
use crate::utils::names::is_imported_namespace;
use crate::utils::names::is_sharp_namespace;
use crate::utils::names::namespace_function;
use crate::utils::names::sharp_class_like_name;
use crate::utils::names::sharp_code_class_name;
use crate::utils::names::sharp_effect_name;
use crate::utils::names::sharp_missing_imports;
use crate::utils::quoted_source;

/// Spec section 6.1: a `get` body may have any effect, and a read carries it, but it never writes `this`: neither a
/// write of a field of `this` nor a call of a method that writes one. Unlike a method marked `readonly` (section 13),
/// it may call a plain PHP method and an unmarked PHP# one. A `lazy` property keeps its value through the bridge's
/// write, which is the language's, not the body's.
pub(crate) fn getters_never_write_this(effects: &Effects, codebase: &CodebaseMetadata) -> IssueCollection {
    effects
        .bodies_changing_this()
        .filter(|(body, ..)| matches!(body, Body::Accessor(_, _, accessor) if accessor.as_bytes() == b"get"))
        .map(|(_, member, imports, _, impurity)| {
            let class_name = |class| sharp_class_like_name(codebase, imports, class);

            Issue::error(format!(
                "Getter `{member}` {}. A getter never writes `this`.",
                impurity.describe_change(&class_name)
            ))
            .with_code(IssueCode::GetterWritesThis.as_str())
            .with_annotation(Annotation::primary(impurity.span))
            .with_note("A getter may have any effect, but it never changes its object.")
            .with_help("Write the change in a method, and call it before the read.")
        })
        .collect()
}

/// Spec sections 10, 20 and 29: a struct and an enum are values, so their methods, `get` bodies and operators have no
/// effects, as a Swift value type's computed members are pure by convention and Rust's `Copy` values carry no I/O. A
/// data case is its enum's. A change of state is no effect, so only an effect is refused, naming the member and the
/// effect: "`Money.inEuros` has the effect `Http`, and a struct has no effects", or "`Money.check` may have any effect"
/// for a plain PHP call whose `extern` line says `uses any`.
pub(crate) fn values_have_no_effects(effects: &Effects, codebase: &CodebaseMetadata) -> IssueCollection {
    effects
        .bodies_with_effects()
        .filter_map(|(body, member, imports, _, impurity)| {
            let (Body::Method(class, _) | Body::Accessor(class, _, _)) = body else {
                return None;
            };
            if let Body::Accessor(_, _, accessor) = body
                && accessor.as_bytes() != b"get"
            {
                return None;
            }
            let metadata = codebase.get_class_like(class)?;
            // An enum with a case that carries data runs as a class, and each such case as its final subclass.
            let (kind, value_class) = if metadata.flags.is_struct() {
                ("a struct", metadata.original_name)
            } else if metadata.kind.is_enum() {
                ("an enum", metadata.original_name)
            } else {
                ("an enum", data_enum(codebase, metadata)?)
            };

            let class_name = |class| sharp_class_like_name(codebase, imports, class);
            let effect_name = |effect| sharp_effect_name(codebase, imports, effect);
            let member = display_sharp_member(class_name(value_class), member);
            let has = match impurity.effect?.named() {
                Some(effect) => format!("has the effect `{}`", effect_name(effect)),
                None => "may have any effect".to_owned(),
            };

            Some(
                Issue::error(format!("`{member}` {has}, and {kind} has no effects."))
                    .with_code(IssueCode::IncompatibleEffects.as_str())
                    .with_annotation(Annotation::primary(impurity.span).with_message("The effect comes from here."))
                    .with_note(format!("`{member}` {}.", impurity.describe(&class_name, &effect_name)))
                    .with_help("Do the work in a class that takes the value, and keep the value's members pure."),
            )
        })
        .collect()
}

/// Spec section 1: an attribute is data, so creating one has no effect: the constructor a PHP# attribute class runs, the
/// defaults of its parameters, which run for each argument the attribute leaves out, and the initial values of the
/// class and of each class it extends have no effect, so a plain PHP call whose `extern` line names an effect or says
/// `uses any` is refused there, and reading a PHP# class's attributes with `attributes<T>()` is pure. A change of state
/// is no effect. The refusal names the code and the effect: "The constructor of `Column` calls `Store.load`, which has
/// the effect `Database`, and an attribute has no effects".
pub(crate) fn attributes_have_no_effects(effects: &Effects, codebase: &CodebaseMetadata) -> IssueCollection {
    effects
        .bodies_with_effects()
        .filter_map(|(body, member, imports, _, impurity)| {
            let constructor = |method: Word| method.as_bytes() == b"__construct";
            let (class, runs_constructor) = match body {
                Body::Method(class, method) | Body::Default(class, method, _) if constructor(method) => (class, true),
                Body::InitialValue(class, _) => (class, false),
                _ => return None,
            };
            let declaring = codebase.get_class_like(class)?;
            let attribute = created_attribute(codebase, declaring, runs_constructor)?;

            let class_name = |class| sharp_class_like_name(codebase, imports, class);
            let effect_name = |effect| sharp_effect_name(codebase, imports, effect);
            let code = match body {
                Body::Method(..) => format!("constructor of `{}`", class_name(declaring.original_name)),
                Body::Default(_, _, parameter) => format!(
                    "default of `{}` in the constructor of `{}`",
                    parameter.as_bytes().strip_prefix(b"$").map(String::from_utf8_lossy).unwrap_or_default(),
                    class_name(declaring.original_name)
                ),
                _ => {
                    format!("initial value of `{}`", display_sharp_member(class_name(declaring.original_name), member))
                }
            };
            let runs = if attribute == declaring.original_name {
                String::new()
            } else {
                format!(", which the attribute `{}` runs,", class_name(attribute))
            };

            Some(
                Issue::error(format!(
                    "The {code}{runs} {}, and an attribute has no effects.",
                    impurity.describe(&class_name, &effect_name)
                ))
                .with_code(IssueCode::IncompatibleEffects.as_str())
                .with_annotation(Annotation::primary(impurity.span).with_message("The effect comes from here."))
                .with_note("Reflection creates an attribute each time code reads it, so reading one stays pure.")
                .with_help("Give the attribute constants, and do the work in the code that reads it."),
            )
        })
        .collect()
}

/// The PHP# attribute class whose creation runs `declaring`'s constructor, when `constructor` says so, or else its
/// initial values: `declaring` itself, or else the first by name of the classes that extend it, which runs
/// `declaring`'s constructor only when it declares none of its own.
fn created_attribute(codebase: &CodebaseMetadata, declaring: &ClassLikeMetadata, constructor: bool) -> Option<Word> {
    let creates = |attribute: &ClassLikeMetadata| {
        attribute.flags.is_sharp()
            && attribute.attribute_flags.is_some()
            && (!constructor
                || codebase
                    .get_declaring_method_class(attribute.name, word("__construct"))
                    .is_some_and(|runs| codebase.class_key(runs) == codebase.class_key(declaring.name)))
    };
    if creates(declaring) {
        return Some(declaring.original_name);
    }
    let mut descendants: Vec<&ClassLikeMetadata> = codebase
        .get_all_descendants(declaring.name)
        .iter()
        .filter_map(|descendant| codebase.get_class_like(*descendant))
        .filter(|descendant| creates(descendant))
        .collect();
    descendants.sort_by_key(|descendant| descendant.original_name.as_bytes());

    descendants.first().map(|attribute| attribute.original_name)
}

/// Spec section 28: a law holds only over pure code, which is the code Lean reads.
pub(crate) fn laws_must_be_pure(effects: &Effects, codebase: &CodebaseMetadata) -> IssueCollection {
    effects
        .impure_bodies()
        .filter(|(body, ..)| {
            matches!(body, Body::Method(class, method)
                if codebase.get_class_like(*class).is_some_and(|class| class.laws.contains_key(method)))
        })
        .map(|(_, member, imports, scope, impurity)| {
            let class_name = |class| sharp_class_like_name(codebase, imports, class);
            let effect_name = |effect| sharp_effect_name(codebase, imports, effect);
            let code_class_name = |class| sharp_code_class_name(codebase, imports, scope, class);
            let missing_import = |class| sharp_missing_imports(codebase, imports, scope, [class]);

            // A change has no effect, so its law is refused for the change, spec section 28.
            let holds = if impurity.effect.is_some() { "pure code" } else { "code that changes nothing" };

            impure(
                IssueCode::ImpureLaw,
                format!(
                    "Law `{member}` {}. Laws hold only over {holds}.",
                    impurity.describe(&class_name, &effect_name)
                ),
                impurity.span,
                "law",
                &impurity,
                &[],
                &code_class_name,
                &missing_import,
            )
        })
        .collect()
}

/// Spec section 29: every implementation is held to what its declaration allows, so a lambda or a method value that a
/// place's function type holds has only the effects that type lists. The refusal has the code a value of the wrong
/// type gets in that place.
pub(crate) fn function_values_must_fit_their_types(effects: &Effects, codebase: &CodebaseMetadata) -> IssueCollection {
    effects
        .unfit_function_values(codebase)
        .map(|(value, name, imports, scope, impurity)| {
            let class_name = |class| sharp_class_like_name(codebase, imports, class);
            let effect_name = |effect| sharp_effect_name(codebase, imports, effect);
            let code_class_name = |class| sharp_code_class_name(codebase, imports, scope, class);
            let missing_import = |class| sharp_missing_imports(codebase, imports, scope, [class]);
            // A plain PHP function's value has no class, spec section 14.3.
            let method = || {
                if name.0.is_empty() {
                    format!("`{}`", written_function(name.1, &code_class_name).0)
                } else {
                    format!("`{}`", display_sharp_member(class_name(name.0), name.1))
                }
            };
            let reason = match (&value.runs, impurity.effect) {
                (Runs::Body(Body::Lambda(_)), _) => {
                    format!("This lambda {}", impurity.describe(&class_name, &effect_name))
                }
                (Runs::Body(_), _) => format!("{} {}", method(), impurity.describe(&class_name, &effect_name)),
                (Runs::Foreign { .. }, Some(Effect::Foreign(effect))) => {
                    format!("{} has the effect `{}`", method(), effect_name(effect))
                }
                (Runs::Foreign { .. }, _) => format!("{} may have any effect", method()),
                (Runs::Undeclared { .. }, _) => unreachable!("a value of undeclared plain PHP is refused on its own"),
            };
            let body = if matches!(value.runs, Runs::Body(Body::Lambda(_))) { "lambda" } else { "method" };
            let allowed: Vec<String> = value.allowed.iter().map(|effect| effect_name(*effect)).collect();

            let issue = impure(
                value.code,
                format!("{reason}, so {} cannot hold it.", value.holder),
                value.span,
                body,
                &impurity,
                &allowed,
                &code_class_name,
                &missing_import,
            );

            match value.runs {
                Runs::Body(_) => issue.with_annotation(Annotation::secondary(impurity.span)),
                Runs::Foreign { .. } | Runs::Undeclared { .. } => issue,
            }
        })
        .collect()
}

/// Spec section 29: a method without a body is pure unless it says `uses`, and every implementation is held to what
/// the declaration allows.
pub(crate) fn implementations_must_fit_their_declarations(
    effects: &Effects,
    codebase: &CodebaseMetadata,
) -> IssueCollection {
    effects
        .unfit_implementations(codebase)
        .map(|((class, member), ancestor, declaration, imports, scope, impurity)| {
            let class_name = |class| sharp_class_like_name(codebase, imports, class);
            let effect_name = |effect| sharp_effect_name(codebase, imports, effect);
            let code_class_name = |class| sharp_code_class_name(codebase, imports, scope, class);
            let missing_import = |class| sharp_missing_imports(codebase, imports, scope, [class]);
            let ancestor = codebase.get_class_like(ancestor).map_or(ancestor, |ancestor| ancestor.original_name);
            let declared = display_sharp_member(class_name(ancestor), declaration.original_name);
            let uses: Vec<String> = declaration.uses.iter().map(|effect| effect_name(*effect)).collect();
            let allows = allows(&declared, &uses);

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
                &code_class_name,
                &missing_import,
            )
        })
        .collect()
}

/// What the declaration `declared` with the effects `uses` allows, as a message names it: "the pure `Pinger.ping`" or
/// "`uses Http, Clock` on `Fetcher.fetch`".
fn allows(declared: &str, uses: &[String]) -> String {
    if uses.is_empty() {
        format!("the pure `{declared}`")
    } else {
        format!("`uses {}` on `{declared}`", uses.join(", "))
    }
}

/// Spec section 29: a declaration without a body is held to what each declaration it overrides allows, as an
/// implementation is, so a sub-interface may narrow its parent's `uses` but never widen it. The refusal names the
/// first ancestor whose declaration the `uses` goes past, at `name_span`, the name of `own`.
pub(crate) fn check_declaration_uses<A>(
    context: &mut Context<'_, '_, A>,
    class: &ClassLikeMetadata,
    own: &FunctionLikeMetadata,
    name_span: Span,
) where
    A: Arena,
{
    let codebase = context.codebase;
    let wider = class.all_parent_classes.iter().chain(&class.all_parent_interfaces).find_map(|ancestor| {
        let declaration = holding_declaration(codebase, *ancestor, own.name)?;
        let wider: Vec<Word> = own
            .uses
            .iter()
            .filter(|effect| {
                let effect = codebase.class_key(**effect);
                !declaration.uses.iter().any(|allowed| codebase.class_key(*allowed) == effect)
            })
            .copied()
            .collect();

        (!wider.is_empty()).then_some((*ancestor, declaration, wider))
    });
    let Some((ancestor, declaration, wider)) = wider else {
        return;
    };

    let class_name = |class| sharp_class_like_name(codebase, &context.imported_names, class);
    let effect_name = |effect| sharp_effect_name(codebase, &context.imported_names, effect);
    let declared = display_sharp_member(
        class_name(codebase.get_class_like(ancestor).map_or(ancestor, |ancestor| ancestor.original_name)),
        declaration.original_name,
    );
    let allowed: Vec<String> = declaration.uses.iter().map(|effect| effect_name(*effect)).collect();
    let allows = allows(&declared, &allowed);
    let wider: Vec<String> = wider.iter().map(|effect| format!("`{}`", effect_name(*effect))).collect();

    context.collector.report_with_code(
        IssueCode::IncompatibleEffects,
        Issue::error(format!(
            "`{}` uses {}, so {allows} cannot accept it.",
            display_sharp_member(class_name(class.original_name), own.original_name),
            wider.join(", "),
        ))
        .with_annotation(Annotation::primary(name_span).with_message("Declared here."))
        .with_annotation(Annotation::secondary(declaration.name_span.unwrap_or(declaration.span))),
    );
}

/// Spec section 13: through a readonly value only methods marked `readonly` run, so a call of an unmarked one is an
/// error that names the fix: the marker, when the method changes nothing in `this`, or the write that keeps it off.
pub(crate) fn readonly_values_call_readonly_methods(effects: &Effects, codebase: &CodebaseMetadata) -> IssueCollection {
    effects
        .readonly_calls()
        .map(|(call, imports, change)| {
            let class_name = |class| sharp_class_like_name(codebase, imports, class);
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
/// `extern` line to write when the callee's says `uses any`. The line names the callee, a method or a function, and
/// the effects `allowed` lists, as the place's `uses` names them, or `none`. `code_class_name` names each class in the
/// line, and `missing_import` names the import the line needs when the body's file doesn't bind its class.
#[allow(clippy::too_many_arguments)]
fn impure(
    code: IssueCode,
    message: String,
    span: Span,
    body: &str,
    impurity: &Impurity,
    allowed: &[String],
    code_class_name: &dyn Fn(Word) -> String,
    missing_import: &dyn Fn(Word) -> Option<String>,
) -> Issue {
    let issue = Issue::error(message).with_code(code.as_str()).with_annotation(Annotation::primary(span));

    match impurity.effect {
        Some(Effect::Any) if impurity.every_effect => {
            issue.with_help("It runs code no `uses` can name, so call it where no `uses` limits the effects.")
        }
        Some(Effect::Any) => {
            let (class, member) = impurity.cause;
            let (target, import) = if class.is_empty() {
                let (target, namespace) = written_function(member, code_class_name);

                (target, namespace.and_then(missing_import))
            } else {
                (display_sharp_member(code_class_name(class), member), missing_import(class))
            };
            let import = import.map(|import| format!(" {import}")).unwrap_or_default();
            let uses = if allowed.is_empty() { "none".to_owned() } else { allowed.join(", ") };

            issue.with_help(format!(
                "Its `extern` line says `uses any`. State its effects in place of `any`, or call it outside the {body}, as with `extern {target} uses {uses};`.{import}"
            ))
        }
        _ => issue,
    }
}

/// Spec section 29: every plain PHP class, method and function PHP# code reaches has an `extern` line, which states its
/// effects, so no effect goes unwritten. A call takes the line of the class that declares the method it calls, so the
/// refusal names that class, or the function, as "`Mailchimp` has no `extern` line", with the line to write.
pub(crate) fn plain_php_has_extern_lines(effects: &Effects, codebase: &CodebaseMetadata) -> IssueCollection {
    effects
        .undeclared_uses()
        .map(|(span, (class, function), imports, scope)| {
            let class_name = |class| sharp_class_like_name(codebase, imports, class);
            let code_class_name = |class| sharp_code_class_name(codebase, imports, scope, class);
            let missing_import = |class| sharp_missing_imports(codebase, imports, scope, [class]);
            let (shown, target, import) = if class.is_empty() {
                let (target, namespace) = written_function(function, &code_class_name);

                (written_function(function, &class_name).0, target, namespace.and_then(missing_import))
            } else if let Some(alias) = namespace_mate_alias(codebase, imports, scope, class) {
                (class_name(class), alias.1, Some(alias.0))
            } else {
                (class_name(class), code_class_name(class), missing_import(class))
            };
            let import = import.map(|import| format!(" {import}")).unwrap_or_default();

            Issue::error(format!("`{shown}` has no `extern` line."))
                .with_code(IssueCode::IncompatibleEffects.as_str())
                .with_annotation(Annotation::primary(span).with_message("Plain PHP is reached here."))
                .with_note("Every plain PHP class and function PHP# code reaches states its effects on an `extern` line.")
                .with_help(format!(
                    "Declare it in a .sharp file, such as `extern {target} uses any;`, and name its effects in place of `any` when you know them.{import}"
                ))
        })
        .collect()
}

/// The import and the name an `extern` line takes for `class`, a plain PHP class of the file's own namespace that the
/// file names without an import, spec section 23: a bare name in an `extern` line that no import binds is a function,
/// so the line names the class through an alias of its namespace's last part and its name, as `DemoCell` for
/// `Demo.Cell`.
fn namespace_mate_alias(
    codebase: &CodebaseMetadata,
    imports: &WordMap<Word>,
    scope: &NamespaceScope,
    class: Word,
) -> Option<(String, String)> {
    let name = codebase.get_class_like(class).map_or(class, |metadata| metadata.original_name);
    let full = String::from_utf8_lossy(name.as_bytes().strip_prefix(b"\\").unwrap_or(name.as_bytes())).into_owned();
    let (namespace, short) = full.rsplit_once('\\')?;
    let own = scope.namespace_name().is_some_and(|own| own.eq_ignore_ascii_case(namespace.as_bytes()));
    if !own || imports.contains_key(&ascii_lowercase_word(full.as_bytes())) {
        return None;
    }
    let alias = format!("{}{short}", namespace.rsplit('\\').next().unwrap_or(namespace));

    Some((format!("Add `import {} as {alias};` to the file.", full.replace('\\', ".")), alias))
}

/// The plain PHP function `function`, as the cause of a call of it names it, written as the code names it through its
/// namespace, spec section 23: `Pdf.render` after `import Vendor.Pdf;`, with the namespace it is named through, and
/// one of PHP's built-ins by its bare name.
fn written_function(function: Word, code_class_name: &dyn Fn(Word) -> String) -> (String, Option<Word>) {
    match function.as_bytes().iter().rposition(|byte| *byte == b'\\') {
        Some(separator) => {
            let namespace = word(&function.as_bytes()[..separator]);
            let name = String::from_utf8_lossy(&function.as_bytes()[separator + 1..]);

            (format!("{}.{name}", code_class_name(namespace)), Some(namespace))
        }
        None => (function.to_string(), None),
    }
}

/// What an `extern` line names as its code writes it, as `Mailer.send`, and `Builder.where` for a line that writes the
/// class's type parameters before its member, as `Builder<TModel>.where`.
pub(crate) fn written_target(r#extern: &Extern<'_>) -> String {
    match &r#extern.member {
        Some(member) => format!("{}.{}", BytesDisplay(r#extern.target.value()), BytesDisplay(member.value)),
        None => String::from_utf8_lossy(r#extern.target.value()).into_owned(),
    }
}

/// Spec section 29: an `extern` declaration declares plain PHP that exists, once in the whole project.
pub(crate) fn check_extern<A>(context: &mut Context<'_, '_, A>, r#extern: &Extern<'_>)
where
    A: Arena,
{
    if let Some(namespaced) = &r#extern.namespaced {
        check_extern_namespace(context, r#extern, namespaced);

        return;
    }
    // An `extern` names `Class.member` or `Namespace.function`, so a target of three parts or more is a full name,
    // which spec section 23 keeps in `import` lines, as `Vendor.Pdf.render`.
    let target = r#extern.target.value();
    if let Some(member_dot) = target.iter().rposition(|byte| *byte == b'.')
        && let Some(owner_dot) = target[..member_dot].iter().rposition(|byte| *byte == b'.')
    {
        let (import, written) = (BytesDisplay(&target[..member_dot]), BytesDisplay(&target[owner_dot + 1..]));
        context.collector.report_with_code(
            IssueCode::NonExistentClassLike,
            Issue::error(format!(
                "Full names appear only in `import` lines: add `import {import};` and write `{written}`."
            ))
            .with_annotation(Annotation::primary(r#extern.target.span()).with_message("Full name used here.")),
        );

        return;
    }

    let span = r#extern.span();
    let Some((declarations, own)) = context.codebase.externs.values().find_map(|declarations| {
        declarations.iter().find(|declaration| declaration.span == span).map(|own| (declarations, own))
    }) else {
        return;
    };

    let written = written_target(r#extern);
    if let Some(first) = declarations.first()
        && first.span != span
    {
        let message = match (first.is_standard_library, first.uses_any, first.effects.as_slice()) {
            (false, ..) => format!("`{written}` already has an `extern` declaration."),
            (true, true, _) => {
                format!("`{written}` is already declared by the standard library, with `uses any`: delete this line.")
            }
            (true, false, []) => {
                format!("`{written}` is already declared pure by the standard library: delete this line.")
            }
            (true, false, effects) => format!(
                "`{written}` is already declared by the standard library, with `uses {}`: delete this line.",
                effects.iter().map(short_name).collect::<Vec<_>>().join(", ")
            ),
        };
        context.collector.report_with_code(
            IssueCode::DuplicateExtern,
            Issue::error(message)
                .with_annotation(Annotation::primary(span))
                .with_annotation(Annotation::secondary(first.span).with_message("First declared here.")),
        );

        return;
    }

    let target = r#extern.target.span();
    let (class, member) = own.target;
    if class.is_empty() {
        // Spec section 23 names a plain PHP function through its namespace, in an `extern` as in a call, and the
        // standard library alone names one of PHP's root namespace through `global`, as `extern global.now`.
        let declared = context.source_file.is_standard_library
            && written.starts_with("global.")
            && context.codebase.function_exists(member);
        if !declared {
            context.collector.report_with_code(
                IssueCode::NonExistentFunction,
                Issue::error(format!("Function `{written}` does not exist."))
                    .with_annotation(Annotation::primary(target)),
            );
        }

        return;
    }

    // An `extern` is keyed by its class's lowercase name, as plain PHP finds the class, and the code finds the class by
    // its name as written, which names a PHP# class only exactly, spec section 24.
    let Some(class_metadata) = context.codebase.get_class_like(word(context.resolved_names.get(&r#extern.target)))
    else {
        // `extern Pdf.render` after `import Vendor.Pdf;` declares the function `render` of that namespace, spec
        // section 23, as `extern Foundation.now` declares Laravel's `now()`.
        if namespace_function(context.codebase, class, member).is_some() {
            check_extern_signature(context, r#extern, own, &written);

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
    } else if !member.is_empty() && context.codebase.get_declaring_method(class, member).is_none() {
        context.collector.report_with_code(
            IssueCode::NonExistentMethod,
            Issue::error(format!("Method `{written}` does not exist.")).with_annotation(Annotation::primary(target)),
        );
    } else if let Some(declaring_class) = (!member.is_empty())
        .then(|| context.codebase.get_declaring_method_class(class, member))
        .flatten()
        .filter(|declaring_class| !declaring_class.as_bytes().eq_ignore_ascii_case(class_metadata.name.as_bytes()))
    {
        // Spec section 29: a call takes the line of the class that declares its method, so a line of a method the class
        // only inherits would declare nothing.
        let declaring_class = display_class_like_name(context, declaring_class);
        let member = BytesDisplay(r#extern.member_name().unwrap_or_default());
        context.collector.report_with_code(
            IssueCode::NonExistentMethod,
            Issue::error(format!(
                "`{written}` is a method of `{declaring_class}`, so `extern {declaring_class}.{member}` declares it."
            ))
            .with_annotation(Annotation::primary(target).with_message("Inherited here.")),
        );
    } else {
        check_extern_signature(context, r#extern, own, &written);
    }
}

/// Spec section 23: the signature an `extern` line states fits the library's native types, never its docblock. Each
/// parameter's type crosses to the declared one, a backed case as its value, each declared parameter after the stated
/// ones has a default, and the return type takes the declared one, a `List` or a `Map` the array the library declares.
/// A generic class's type parameters are written as the class declares them. An untyped declaration takes any type.
fn check_extern_signature<A>(
    context: &mut Context<'_, '_, A>,
    r#extern: &Extern<'_>,
    own: &ExternMetadata,
    written: &str,
) where
    A: Arena,
{
    let codebase = context.codebase;
    let Some(parameter_list) = &r#extern.parameter_list else {
        return;
    };
    let class = own.target.0;
    let Some((declared, stated)) = codebase
        .extern_declaration(own)
        .and_then(|key| Some((codebase.function_likes.get(&key)?, codebase.extern_signatures.get(&key)?)))
    else {
        return;
    };
    let parameter_name = |name: Word| {
        let name = name.as_bytes();

        String::from_utf8_lossy(name.strip_prefix(b"$").unwrap_or(name)).into_owned()
    };
    let class_metadata = (!class.is_empty()).then(|| codebase.get_class_like(class)).flatten();
    // The class's own type, its type parameters as its type arguments, is what the declaration's `self` and `static`
    // are, so `static` fits a stated `Builder<TModel>`.
    let own_type = class_metadata.map(|class| {
        let arguments = class
            .template_types
            .iter()
            .map(|(name, template)| {
                TUnion::from_atomic(TAtomic::GenericParameter(TGenericParameter::new(
                    *name,
                    Arc::new(template.constraint.clone()),
                    template.defining_entity,
                )))
            })
            .collect::<Vec<_>>();

        TNamedObject::new_with_type_parameters(class.name, (!arguments.is_empty()).then_some(arguments))
    });
    let expanded = |type_union: &TUnion| {
        let mut type_union = type_union.clone();
        let options = TypeExpansionOptions {
            self_class: class_metadata.map(|class| class.name),
            static_class_type: own_type
                .clone()
                .map_or(StaticClassType::None, |own| StaticClassType::Object(TObject::Named(own))),
            ..Default::default()
        };
        expand_union(codebase, &mut type_union, &options);

        type_union
    };
    let dialect = context.dialect;
    let fits = |input: &TUnion, container: &TUnion| {
        let mut result = ComparisonResult::for_dialect(dialect);

        union_comparator::is_contained_by(codebase, input, container, false, false, false, &mut result)
    };

    if let Some(type_parameters) = &r#extern.type_parameters
        && let Some(class_metadata) = class_metadata
    {
        let declared_names: Vec<&[u8]> = class_metadata.template_types.keys().map(|name| name.as_bytes()).collect();
        let written_names: Vec<&[u8]> =
            type_parameters.parameters.iter().map(|parameter| parameter.name.value).collect();
        if declared_names != written_names {
            let class_name = display_class_like_name(context, class_metadata.original_name);
            let names = declared_names.iter().map(|name| String::from_utf8_lossy(name)).collect::<Vec<_>>().join(", ");
            let message = if declared_names.is_empty() {
                format!("`{class_name}` declares no type parameters, so its `extern` line writes `{class_name}` alone.")
            } else {
                format!(
                    "`{class_name}` declares the type parameters `{names}`, so its `extern` line writes `{class_name}<{names}>`."
                )
            };
            context.collector.report_with_code(
                IssueCode::InvalidTemplateParameter,
                Issue::error(message).with_annotation(Annotation::primary(type_parameters.span())),
            );
        }
    }

    for (index, (parameter, written_parameter)) in
        stated.parameters.iter().zip(parameter_list.parameters.iter()).enumerate()
    {
        let Some(native) = declared
            .parameters
            .get(index)
            .or_else(|| declared.parameters.last().filter(|last| last.flags.is_variadic()))
        else {
            context.collector.report_with_code(
                IssueCode::IncompatibleParameterCount,
                Issue::error(format!(
                    "`{written}` has no parameter where its `extern` line states `{}`.",
                    parameter_name(parameter.name.0)
                ))
                .with_annotation(
                    Annotation::primary(written_parameter.span()).with_message("No parameter takes this."),
                ),
            );

            break;
        };
        if let (Some(default), Some(default_type), Some(stated_type)) =
            (&written_parameter.default_value, &parameter.default_type, &parameter.type_metadata)
            && !fits(&default_type.type_union, &expanded(&stated_type.type_union))
        {
            let name = parameter_name(parameter.name.0);
            context.collector.report_with_code(
                IssueCode::InvalidParameterDefaultValue,
                Issue::error(format!("Default value for parameter `{name}` is not assignable to its declared type."))
                    .with_annotation(Annotation::primary(default.value.span()).with_message(format!(
                        "This default value has type `{}`",
                        display_sharp_type(context, &default_type.type_union)
                    )))
                    .with_annotation(Annotation::secondary(written_parameter.span()).with_message(format!(
                        "Parameter `{name}` is declared with type `{}`",
                        display_sharp_type(context, &stated_type.type_union)
                    )))
                    .with_note("A call that leaves the parameter out passes its default to plain PHP.")
                    .with_help("Change the default value to match the declared type."),
            );
        }
        let (Some(native_type), Some(stated_type)) = (&native.type_declaration_metadata, &parameter.type_metadata)
        else {
            continue;
        };
        let native_type = expanded(&native_type.type_union);
        // Spec section 7: a function value crosses as the closure it runs as, so a stated `callable` takes a native
        // `\Closure` too.
        let closure = TUnion::from_atomic(TAtomic::Callable(TCallable::Signature(TCallableSignature::mixed(true))));
        let crosses = expanded(&stated_type.type_union).types.iter().all(|atomic| {
            let backing = match atomic {
                TAtomic::Object(TObject::Enum(object)) => Some(object.name),
                _ => None,
            }
            .and_then(|name| codebase.get_class_like(name)?.enum_type.clone());

            fits(&TUnion::from_atomic(atomic.clone()), &native_type)
                || backing.is_some_and(|backing| fits(&TUnion::from_atomic(backing), &native_type))
                || (atomic.is_callable() && fits(&closure, &native_type))
        });
        if !crosses {
            context.collector.report_with_code(
                IssueCode::IncompatibleParameterType,
                Issue::error(format!(
                    "`{}` does not cross to `{written}`'s `{}`, which the library declares `{}`.",
                    quoted_source(context, written_parameter.span()),
                    parameter_name(native.name.0),
                    display_sharp_type(context, &native_type),
                ))
                .with_annotation(Annotation::primary(written_parameter.span()).with_message("Stated here."))
                .with_note("A stated type fits the library's native type, so plain PHP takes each value the line lets a call pass.")
                .with_help(format!("State a type `{}` takes.", display_sharp_type(context, &native_type))),
            );
        }
    }
    if let Some(native) = declared
        .parameters
        .iter()
        .skip(stated.parameters.len())
        .find(|native| !native.flags.has_default() && !native.flags.is_variadic())
    {
        context.collector.report_with_code(
            IssueCode::IncompatibleParameterCount,
            Issue::error(format!(
                "`{written}` takes `{}`, which has no default, so its `extern` line states it.",
                parameter_name(native.name.0)
            ))
            .with_annotation(Annotation::primary(parameter_list.span()).with_message("Stated here.")),
        );
    }
    if let (Some(native), Some(stated), Some(hint)) = (
        &declared.return_type_declaration_metadata,
        &stated.return_type_declaration_metadata,
        &r#extern.return_type_hint,
    ) {
        let (native, stated) = (expanded(&native.type_union), expanded(&stated.type_union));
        let takes = native.types.iter().all(|atomic| {
            fits(&TUnion::from_atomic(atomic.clone()), &stated)
                || (atomic.is_array() && stated.types.iter().any(TAtomic::is_array))
        });
        if !takes {
            context.collector.report_with_code(
                IssueCode::IncompatibleReturnType,
                Issue::error(format!(
                    "`{written}` returns `{}`, which the stated `{}` does not take.",
                    display_sharp_type(context, &native),
                    quoted_source(context, hint.span()),
                ))
                .with_annotation(Annotation::primary(hint.span()).with_message("Stated here."))
                .with_help(format!("State a type that takes `{}`.", display_sharp_type(context, &native))),
            );
        }
    }
}

/// Spec section 23: `extern namespace WordPress for WP_Post;` names a library's class, function or constant of PHP's
/// root namespace, and gives its folder a namespace, which no other line gives another folder. A namespace a class
/// already has is one only a folder that holds a class of it takes, as Laravel's `helpers.php` sits beside the classes
/// of `Illuminate\Foundation`.
fn check_extern_namespace<A>(context: &mut Context<'_, '_, A>, r#extern: &Extern<'_>, namespaced: &ExternNamespaced<'_>)
where
    A: Arena,
{
    let span = r#extern.span();
    let codebase = context.codebase;
    let own =
        codebase.extern_namespaces.iter().find(|declaration| declaration.span == span).unwrap_or_else(|| {
            unreachable!("the scan of the analyzed file records each `extern namespace` it declares")
        });
    let written = String::from_utf8_lossy(r#extern.target.value());
    let name_written = String::from_utf8_lossy(namespaced.name.value());

    // A PHP# file always declares a namespace, so a class of PHP's root namespace is a plain PHP one.
    let Some(folder) = codebase.extern_namespace_folder(own) else {
        context.collector.report_with_code(
            IssueCode::NonExistentClassLike,
            Issue::error(format!(
                "Cannot find the plain PHP class, function or constant `{name_written}` outside any namespace."
            ))
            .with_annotation(Annotation::primary(own.name_span))
            .with_help("`extern namespace` names a class, function or constant a library declares outside any namespace, as `extern namespace WordPress for WP_Post;` or `extern namespace Illuminate.Foundation for abort;`."),
        );

        return;
    };

    // A namespace a class has already names it, so a folder takes it only when it holds a class of it.
    let namespace = own.namespace.as_bytes();
    let holders: Vec<&ClassLikeMetadata> = codebase
        .class_likes
        .values()
        .map(|class_like| &**class_like)
        .filter(|class_like| {
            let name = class_like.original_name;

            name.as_bytes()
                .iter()
                .rposition(|byte| *byte == b'\\')
                .is_some_and(|end| name.as_bytes()[..end].eq_ignore_ascii_case(namespace))
        })
        .collect();
    if let Some(holder) = holders.first()
        && !holders.iter().any(|holder| codebase.folder(holder.span.file_id) == Some(folder))
    {
        context.collector.report_with_code(
            IssueCode::DuplicateExtern,
            Issue::error(format!(
                "`{written}` is already the namespace of `{}`, whose folder doesn't hold `{name_written}`.",
                display_class_like_name(context, holder.original_name)
            ))
            .with_annotation(Annotation::primary(r#extern.target.span()))
            .with_annotation(
                Annotation::secondary(holder.name_span.unwrap_or(holder.span)).with_message("Declared here."),
            )
            .with_help("Give the library a namespace no class has, or the namespace of the classes beside it."),
        );

        return;
    }

    let earlier =
        codebase.extern_namespaces.iter().take_while(|declaration| *declaration != own).find_map(|declaration| {
            let same_folder = codebase.extern_namespace_folder(declaration)? == folder;
            let same_namespace = declaration.namespace.as_bytes().eq_ignore_ascii_case(namespace);

            (same_folder || same_namespace).then_some((declaration, same_folder))
        });
    if let Some((first, same_folder)) = earlier {
        let (message, help) = if same_folder {
            let namespace = String::from_utf8_lossy(first.namespace.as_bytes()).replace('\\', ".");

            (
                format!("The folder of `{name_written}` already takes the namespace `{namespace}`."),
                "Delete this line: a folder takes one namespace.",
            )
        } else {
            (
                format!("`{written}` is already the namespace of the folder of `{}`.", first.name),
                "Give this library another namespace.",
            )
        };
        context.collector.report_with_code(
            IssueCode::DuplicateExtern,
            Issue::error(message)
                .with_annotation(Annotation::primary(span))
                .with_annotation(Annotation::secondary(first.span).with_message("First declared here."))
                .with_help(help),
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

    // A method states its effects only without a body, and states none by writing no `uses`, so a `uses none` on one
    // changes nothing and warns, as Swift compiles a method's `throws(Never)`.
    for none in found.none_on_method {
        context.collector.report_with_code(
            IssueCode::InvalidEffect,
            Issue::warning("A method without `uses` is pure already, so delete `uses none`.")
                .with_annotation(Annotation::primary(none.span).with_message("Written here.")),
        );
    }

    for none in found.beside_none {
        context.collector.report_with_code(
            IssueCode::InvalidEffect,
            Issue::error("`none` states that the function has no effect, so `uses` lists nothing beside it.")
                .with_annotation(Annotation::primary(none.span).with_message("Written here."))
                .with_help("Delete `none` to allow the other effects, or delete them to keep the function pure."),
        );
    }

    for any in found.beside_any {
        context.collector.report_with_code(
            IssueCode::InvalidEffect,
            Issue::error("`any` states that the plain PHP may have any effect, so `uses` lists nothing beside it.")
                .with_annotation(Annotation::primary(any.span).with_message("Written here."))
                .with_help("Delete `any` to declare only the other effects, or delete them to allow any effect."),
        );
    }

    for name in found.names {
        let effect = context.resolved_names.get(name);
        artifacts.symbol_references.add_reference_to_symbol(
            &block_context.scope,
            context.codebase.class_key(word(effect)),
            false,
        );
        if is_effect(context.codebase, word(effect)) {
            continue;
        }

        // The message names the `foreign` classes the file can name without another import: the standard library's,
        // in any of its namespaces (spec section 29), its namespace's and its imports'. The file depends on each class
        // it could name, so the message follows a class that becomes `foreign` or stops being one, as a compile from
        // scratch gives it.
        let namespace = context.scope.namespace_name().unwrap_or_default();
        let nameable: Vec<_> = context
            .codebase
            .class_likes
            .values()
            .filter(|class| {
                let name = class.original_name;
                let class_namespace = name
                    .as_bytes()
                    .iter()
                    .rposition(|byte| *byte == b'\\')
                    .map_or(&b""[..], |end| &name.as_bytes()[..end]);

                is_sharp_namespace(class_namespace)
                    || class_namespace.eq_ignore_ascii_case(namespace)
                    || context.imported_names.contains_key(&ascii_lowercase_word(name.as_bytes()))
            })
            .collect();
        for class in &nameable {
            artifacts.symbol_references.add_reference_to_symbol(&block_context.scope, class.name, false);
        }
        let mut effects: Vec<String> = nameable
            .into_iter()
            .filter(|class| class.flags.is_foreign())
            .map(|class| sharp_effect_name(context.codebase, &context.imported_names, class.original_name))
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

/// The names every effect `uses` of a program writes. A method with a body names none, and a function parameter named
/// in any method's is no effect, as the semantics checker holds them to. A function type's `uses none` names none, and
/// is refused beside another effect. A method without a body is pure without `uses`, so its `uses none` is refused. An
/// `extern` line's `uses none` and `uses any` name none, and each is refused beside another effect.
struct UsesWalker;

/// The names [`UsesWalker`] collects, each `none` or `any` written beside another effect, each `none` on a method
/// without a body, and the `uses` of the methods, function types and `extern` lines it read already.
#[derive(Default)]
struct UsesNames<'ast, 'arena> {
    names: Vec<&'ast LocalIdentifier<'arena>>,
    beside_none: Vec<&'ast LocalIdentifier<'arena>>,
    beside_any: Vec<&'ast LocalIdentifier<'arena>>,
    none_on_method: Vec<&'ast LocalIdentifier<'arena>>,
    read: Vec<Span>,
}

/// Whether `name` is the `none` that states a function type is pure.
fn is_none(name: &LocalIdentifier<'_>) -> bool {
    name.value == b"none"
}

impl<'ast, 'arena> Walker<'ast, 'arena, UsesNames<'ast, 'arena>> for UsesWalker {
    fn walk_in_method(&self, method: &'ast Method<'arena>, found: &mut UsesNames<'ast, 'arena>) {
        let Some(uses) = method.uses else {
            return;
        };

        found.read.push(uses.span());
        if method.is_abstract() {
            let parts = MethodParts::of_method(method);
            let effects: Vec<_> = parts.uses_effects().collect();
            let beside = effects.len() > 1;
            for name in effects {
                match (is_none(name), beside) {
                    (true, true) => found.beside_none.push(name),
                    (true, false) => found.none_on_method.push(name),
                    (false, _) => found.names.push(name),
                }
            }
        }
    }

    fn walk_in_function_hint(&self, function_hint: &'ast FunctionHint<'arena>, found: &mut UsesNames<'ast, 'arena>) {
        let Some(uses) = &function_hint.uses else {
            return;
        };

        found.read.push(uses.span());
        if uses.names.len() > 1
            && let Some(none) = uses.names.iter().find(|name| is_none(name))
        {
            found.beside_none.push(none);
        }
        found.names.extend(uses.names.iter().filter(|name| !is_none(name)));
    }

    fn walk_in_extern(&self, r#extern: &'ast Extern<'arena>, found: &mut UsesNames<'ast, 'arena>) {
        let Some(uses) = &r#extern.uses else {
            return;
        };

        found.read.push(uses.span());
        let beside = uses.names.len() > 1;
        for name in uses.names.iter() {
            match (name.value, beside) {
                (b"none" | b"any", false) => {}
                (b"none", true) => found.beside_none.push(name),
                (b"any", true) => found.beside_any.push(name),
                _ => found.names.push(name),
            }
        }
    }

    fn walk_in_uses(&self, uses: &'ast Uses<'arena>, found: &mut UsesNames<'ast, 'arena>) {
        if !found.read.contains(&uses.span()) {
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
    let class = context.codebase.get_class_like(class).map_or(class, |metadata| metadata.original_name);

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
            .and_then(|name| context.codebase.get_class_like(name))
            .filter(|held| held.flags.is_foreign()),
        _ => None,
    });
    let Some(held) = held else {
        return;
    };

    let held = sharp_class_like_name(context.codebase, &context.imported_names, held.original_name);
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
        // The parameter's own function type, stated pure.
        let pure = || {
            parameter
                .type_metadata
                .as_ref()
                .map(|declared| display_sharp_parameter_type(context, &declared.type_union))
                .unwrap_or_default()
        };
        let help = match escape.suggested {
            Suggested::Stated(stated) => format!("Take a `{stated}`, whose type states the effects it may have."),
            Suggested::Pure => format!("Take a `{}`, whose type states that it is pure.", pure()),
            Suggested::PureOrStated => format!("Take a `{}`, or state its effects in place of `none`.", pure()),
        };
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
            Via::Parameter => format!("`{name}` can't be {verb}: it takes on each caller's effects."),
            Via::Local => {
                format!("This local can't be {verb}: it holds `{name}`, so it takes on each caller's effects.")
            }
            Via::Lambda => {
                format!("This lambda can't be {verb}: it captures `{name}`, so it takes on each caller's effects.")
            }
        };
        context.collector.report_with_code(
            code,
            Issue::error(message)
                .with_annotation(Annotation::primary(escape.span).with_message(at))
                .with_note("A method takes on the effects of each function it is given, so that function runs only inside the method's call.")
                .with_help(help),
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
        FunctionLikeBody::Law(_) => unreachable!("only a method's body is walked for functions that escape"),
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

/// One function parameter that escapes at `span`: the parameter's index, the place, what holds it, and the type to
/// take instead.
struct Escape {
    span: Span,
    parameter: usize,
    place: Place,
    via: Via,
    suggested: Suggested,
}

/// The type an escaping function parameter takes instead: the type of the place it goes to, which states its effects,
/// the parameter's own type stated pure when the place is a pure function type, and otherwise either.
enum Suggested {
    Stated(String),
    Pure,
    PureOrStated,
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

        codebase.get_property_type(class, php_variable_name(name.value))
    }

    /// Records `value` as escaping to `place` when it holds a parameter that takes on effects.
    fn check(&mut self, value: &Expression<'arena>, place: Place) {
        self.check_stated(value, place, None);
    }

    /// As [`Self::check`], where `stated` is the type of the place, which decides the type to take instead when it is a
    /// function type.
    fn check_stated(&mut self, value: &Expression<'arena>, place: Place, stated: Option<&TUnion>) {
        let Some((parameter, via)) = self.holds(value) else {
            return;
        };

        let suggested = match stated.map(|stated| (stated, lists_effects(stated))) {
            Some((stated, Some(true))) => Suggested::Stated(display_sharp_type(self.context, stated)),
            Some((_, Some(false))) => Suggested::Pure,
            _ => Suggested::PureOrStated,
        };
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
                let metadata = codebase.get_method(*class, *method);
                // A collection's method is written on its PHP# type, as `List.add`.
                let collection = metadata.and_then(|metadata| collection_member_kind(*class, metadata));
                let callee = collection.map_or_else(
                    || display_function_like_identifier(self.context, callee),
                    |collection| {
                        display_sharp_member(collection.name(), metadata.map_or(*method, |method| method.original_name))
                    },
                );

                (metadata, codebase.get_class_like(*class).is_some_and(|class| class.flags.is_sharp()), callee)
            }
            CallTarget::FunctionLike { callee: callee @ FunctionLikeIdentifier::Function(function), .. } => {
                (codebase.get_function(*function), false, display_function_like_identifier(self.context, callee))
            }
            CallTarget::Property { class, property } | CallTarget::MagicMethod { class, method: property, .. } => {
                return Some((Some(display_member(self.context, *class, property)), None));
            }
            CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Closure(_), .. } => return Some((None, None)),
        };
        // An extension's member takes its receiver as its first parameter, so the call's arguments fill those after it.
        let parameter = metadata.and_then(|metadata| {
            let parameters = match metadata.parameters.split_first() {
                Some((_, parameters)) if metadata.flags.is_extension_method() => parameters,
                _ => metadata.parameters.as_slice(),
            };

            parameters.get(parameter_index(argument, position, parameters))
        });
        let stated =
            parameter.and_then(|parameter| parameter.type_metadata.as_ref()).map(|declared| &declared.type_union);
        let keeps_inside = parameter.is_some_and(|parameter| parameter.flags.takes_on_effects())
            || !sharp && stated.is_some_and(holds_a_callable);

        (!keeps_inside).then_some((Some(callee), stated))
    }
}

/// Whether every PHP# function type of `union`, whose other types are only null, lists effects, as
/// `Function<Charge(Cart) uses Http>`, or none does, as `Function<Charge(Cart)>`. `None` for any other type, and for a
/// union of both.
fn lists_effects(union: &TUnion) -> Option<bool> {
    let lists: Vec<bool> = union
        .types
        .iter()
        .filter(|atomic| !matches!(atomic, TAtomic::Null))
        .map(|atomic| match atomic {
            TAtomic::Callable(TCallable::Signature(signature)) => {
                signature.effects.as_ref().map(|effects| !effects.is_empty())
            }
            _ => None,
        })
        .collect::<Option<_>>()?;

    lists.first().copied().filter(|first| lists.iter().all(|each| each == first))
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
        // A tuple a declaration unpacks, as in `const [a, b] = pair;`, is no function, so its locals hold none.
        for name in local_declaration.target.names() {
            escapes.join(name.span, local_declaration.value);
        }
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
            .and_then(|class| codebase.get_method(class, word(name)));
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
