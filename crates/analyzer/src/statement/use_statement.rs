use mago_allocator::Arena;
use mago_codex::metadata::CodebaseMetadata;
use mago_names::NAMED_EFFECTS;
use mago_names::scope::php_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Use;
use mago_syntax::cst::UseItems;
use mago_syntax::cst::UseType;
use mago_word::concat_word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use mago_bytes::BytesDisplay;
use mago_bytes::trim_start_byte;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Use<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        _block_context: &mut BlockContext<'ctx>,
        _artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        match &self.items {
            UseItems::Sequence(sequence) => {
                for item in sequence.items.iter() {
                    let name = php_name(&item.name);
                    check_class_like_import(context, &item.name, trim_start_byte(&name, b'\\'));
                }
            }
            UseItems::TypedSequence(typed_sequence) => {
                for item in typed_sequence.items.iter() {
                    let name = php_name(&item.name);
                    check_typed_import(context, &item.name, trim_start_byte(&name, b'\\'), &typed_sequence.r#type);
                }
            }
            UseItems::TypedList(typed_list) => {
                let namespace = php_name(&typed_list.namespace);
                let prefix = trim_start_byte(&namespace, b'\\');
                for item in typed_list.items.iter() {
                    let fqn = concat_word!(prefix, b"\\", item.name.value());
                    check_typed_import(context, &item.name, fqn.as_bytes(), &typed_list.r#type);
                }
            }
            UseItems::MixedList(mixed_list) => {
                let namespace = php_name(&mixed_list.namespace);
                let prefix = trim_start_byte(&namespace, b'\\');
                for maybe_typed_item in mixed_list.items.iter() {
                    let fqn = concat_word!(prefix, b"\\", maybe_typed_item.item.name.value());
                    check_maybe_typed_import(
                        context,
                        &maybe_typed_item.item.name,
                        fqn.as_bytes(),
                        maybe_typed_item.r#type.as_ref(),
                    );
                }
            }
        }

        Ok(())
    }
}

fn check_class_like_import<A>(context: &mut Context<'_, '_, A>, name: &Identifier<'_>, fqn: &[u8])
where
    A: Arena,
{
    if !context.codebase.class_like_exists(fqn) && !context.codebase.namespace_exists(fqn) {
        report_non_existent_import(context, name.span(), fqn, "class, interface, trait, or enum");
    }
}

/// Spec section 23: the last part of a PHP# `import` is a class, or the namespace of a plain PHP library's functions
/// and constants. A single function or constant is read through its namespace instead, a namespace of classes only is
/// imported class by class, and an import that names nothing is refused. A capitalized last part names a class, as
/// spec section 24 capitalizes every type, so `import Log;` never names PHP's `log()`.
pub(crate) fn check_sharp_import<A>(context: &mut Context<'_, '_, A>, name: &Identifier<'_>)
where
    A: Arena,
{
    // A standard effect that is a name and no class, as `Sharp.Mail`, is imported to rename it, spec section 29.
    let fqn = php_name(name);
    if context.codebase.class_like_exists(&fqn)
        || holds_function_or_constant(context.codebase, &fqn)
        || NAMED_EFFECTS.iter().any(|effect| effect.as_bytes().eq_ignore_ascii_case(&fqn))
    {
        return;
    }

    let (namespace, last) = match fqn.iter().rposition(|byte| *byte == b'\\') {
        Some(separator) => (Some(&fqn[..separator]), &fqn[separator + 1..]),
        None => (None, &*fqn),
    };
    let namespace = namespace.map(|namespace| String::from_utf8_lossy(namespace).replace('\\', "."));
    let short_namespace = namespace.as_deref().map(|namespace| namespace.rsplit('.').next().unwrap_or(namespace));
    let last_name = String::from_utf8_lossy(last);

    let (code, issue) = if last.first().is_some_and(u8::is_ascii_lowercase) && context.codebase.function_exists(&fqn) {
        // A function of PHP's root namespace is called through `global`, which no file imports.
        let message = match (&namespace, short_namespace) {
            (Some(namespace), Some(short)) => format!("import `{namespace}`, and call `{short}.{last_name}(…)`"),
            _ => format!("call `global.{last_name}(…)`"),
        };

        (
            IssueCode::FunctionImport,
            Issue::error(format!("An import names a class or a namespace, not a function: {message}."))
                .with_annotation(Annotation::primary(name.span()).with_message("Imports one function.")),
        )
    } else if context.codebase.constant_exists(&fqn) {
        let message = match (&namespace, short_namespace) {
            (Some(namespace), Some(short)) => format!("import `{namespace}`, and read `{short}.{last_name}`"),
            _ => format!("read `global.{last_name}`"),
        };

        (
            IssueCode::FunctionImport,
            Issue::error(format!("An import names a class or a namespace, not a constant: {message}."))
                .with_annotation(Annotation::primary(name.span()).with_message("Imports one constant.")),
        )
    } else if let Some(classes) = classes_of_namespace(context, &fqn) {
        let written = String::from_utf8_lossy(&fqn).replace('\\', ".");

        (
            IssueCode::NonExistentUseImport,
            Issue::error(format!(
                "`{written}` holds classes only, and an import names a class or a namespace of functions: import the class, as in {classes}."
            ))
            .with_annotation(Annotation::primary(name.span()).with_message("Imports a namespace of classes.")),
        )
    } else {
        report_non_existent_import(context, name.span(), &fqn, "class, interface, trait, or enum");

        return;
    };

    context.collector.report_with_code(code, issue);
}

/// Whether the namespace `namespace` holds a plain PHP function or constant directly, as `Vendor\Pdf` holds `render`.
fn holds_function_or_constant(codebase: &CodebaseMetadata, namespace: &[u8]) -> bool {
    let in_namespace = |name: &[u8]| {
        name.len() > namespace.len()
            && name[..namespace.len()].eq_ignore_ascii_case(namespace)
            && name[namespace.len()] == b'\\'
            && !name[namespace.len() + 1..].contains(&b'\\')
    };

    codebase.function_likes.keys().any(|(class, function)| class.is_empty() && in_namespace(function.as_bytes()))
        || codebase.constants.keys().any(|constant| in_namespace(constant.as_bytes()))
}

/// The imports of the class-likes directly in the namespace `namespace`, as `` `import Sharp.Json.Json;` ``, joined
/// with `or` in the order of their names, or `None` when it holds none.
fn classes_of_namespace<A>(context: &Context<'_, '_, A>, namespace: &[u8]) -> Option<String>
where
    A: Arena,
{
    let mut imports: Vec<String> = context
        .codebase
        .class_likes
        .values()
        .map(|class| class.original_name.as_bytes())
        .filter(|class| {
            class
                .iter()
                .rposition(|byte| *byte == b'\\')
                .is_some_and(|end| class[..end].eq_ignore_ascii_case(namespace))
        })
        .map(|class| format!("`import {};`", String::from_utf8_lossy(class).replace('\\', ".")))
        .collect();
    imports.sort_unstable();

    (!imports.is_empty()).then(|| imports.join(" or "))
}

/// Spec section 15: PHP# code listens with `Events.on` and raises events with `emit`, so only the standard library
/// imports `Sharp.Internal`, the namespace of the engine's functions, or a name under it.
pub(crate) fn check_internal_import<A>(context: &mut Context<'_, '_, A>, name: &Identifier<'_>)
where
    A: Arena,
{
    let fqn = php_name(name);
    let internal = b"Sharp\\Internal";
    if context.source_file.is_standard_library
        || !fqn.get(..internal.len()).is_some_and(|root| root.eq_ignore_ascii_case(internal))
        || !matches!(fqn.get(internal.len()), None | Some(b'\\'))
    {
        return;
    }

    context.collector.report_with_code(
        IssueCode::InternalOutsideLibrary,
        Issue::error("`Sharp.Internal` holds the engine's functions, and only the standard library imports it.")
            .with_annotation(Annotation::primary(name.span()).with_message("Imported here."))
            .with_help("Listen with `Events.on`, and raise an event with `emit`."),
    );
}

fn check_typed_import<A>(context: &mut Context<'_, '_, A>, name: &Identifier<'_>, fqn: &[u8], use_type: &UseType<'_>)
where
    A: Arena,
{
    match use_type {
        UseType::Function(_) => {
            if !context.codebase.function_exists(fqn) {
                report_non_existent_import(context, name.span(), fqn, "function");
            }
        }
        UseType::Const(_) => {
            if !context.codebase.constant_exists(fqn) {
                report_non_existent_import(context, name.span(), fqn, "constant");
            }
        }
    }
}

fn check_maybe_typed_import<A>(
    context: &mut Context<'_, '_, A>,
    name: &Identifier<'_>,
    fqn: &[u8],
    use_type: Option<&UseType<'_>>,
) where
    A: Arena,
{
    match use_type {
        Some(UseType::Function(_)) => {
            if !context.codebase.function_exists(fqn) {
                report_non_existent_import(context, name.span(), fqn, "function");
            }
        }
        Some(UseType::Const(_)) => {
            if !context.codebase.constant_exists(fqn) {
                report_non_existent_import(context, name.span(), fqn, "constant");
            }
        }
        None => {
            if !context.codebase.class_like_exists(fqn) && !context.codebase.namespace_exists(fqn) {
                report_non_existent_import(context, name.span(), fqn, "class, interface, trait, or enum");
            }
        }
    }
}

fn report_non_existent_import<A>(context: &mut Context<'_, '_, A>, span: Span, fqn: &[u8], symbol_type: &str)
where
    A: Arena,
{
    let fqn = if context.dialect.is_sharp() {
        String::from_utf8_lossy(fqn).replace('\\', ".")
    } else {
        BytesDisplay(fqn).to_string()
    };
    context.collector.report_with_code(
        IssueCode::NonExistentUseImport,
        Issue::error(format!("Imported {symbol_type} `{fqn}` does not exist."))
            .with_annotation(Annotation::primary(span).with_message(format!("`{fqn}` not found")))
            .with_help(format!("Ensure that the {symbol_type} `{fqn}` is defined in the codebase.")),
    );
}
