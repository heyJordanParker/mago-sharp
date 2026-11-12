use mago_allocator::Arena;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_names::scope::php_name;
use mago_names::short_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Use;
use mago_syntax::cst::UseItems;
use mago_syntax::cst::UseType;
use mago_word::Word;
use mago_word::concat_word;
use mago_word::word;

use crate::code::IssueCode;
use crate::context::Context;
use crate::utils::names::display_class_like_name;

/// Reports `used_name`, a class-like name the code writes at `span`, as the unknown name it is when it names a PHP#
/// class-like only in another case, with that class-like as the hint, and returns whether it reported. PHP# names are
/// case-sensitive, spec section 24, so the engine finds a PHP# class-like only by its exact name, from any code, plain
/// PHP included, and another case names nothing.
pub fn report_sharp_class_like_in_another_case<A>(
    context: &mut Context<'_, '_, A>,
    used_name: &[u8],
    span: Span,
) -> bool
where
    A: Arena,
{
    report_in_another_case(context, used_name, span, false)
}

/// Reports `used_name`, the full name a PHP# `import` line writes at `span`, as
/// [`report_sharp_class_like_in_another_case`] reports a name code writes, by the full names, as the line writes them.
pub fn report_sharp_import_in_another_case<A>(context: &mut Context<'_, '_, A>, used_name: &[u8], span: Span) -> bool
where
    A: Arena,
{
    report_in_another_case(context, used_name, span, true)
}

fn report_in_another_case<A>(context: &mut Context<'_, '_, A>, used_name: &[u8], span: Span, full: bool) -> bool
where
    A: Arena,
{
    let used_name = mago_bytes::trim_start_byte(used_name, b'\\');
    let Some(metadata) = context
        .codebase
        .symbols
        .sharp_class_like_in_another_case(used_name)
        .and_then(|key| context.codebase.get_class_like(key.as_bytes()))
    else {
        return false;
    };

    let (written, declared) = if !context.dialect.is_sharp() {
        (String::from_utf8_lossy(used_name).into_owned(), metadata.original_name.to_string())
    } else if full {
        (String::from_utf8_lossy(used_name).replace('\\', "."), metadata.original_name.to_string().replace('\\', "."))
    } else {
        (short_name(used_name), display_class_like_name(context, metadata.original_name).to_string())
    };
    context.collector.report_with_code(
        IssueCode::NonExistentClassLike,
        Issue::error(format!("Cannot find type `{written}`; did you mean `{declared}`?"))
            .with_annotation(Annotation::primary(span).with_message(format!("Declared as `{declared}`.")))
            .with_note("PHP# names are case-sensitive: code reaches a PHP# type only by the name it declares.")
            .with_help(format!("Write `{declared}`.")),
    );

    true
}

/// Checks if the used class-like name matches the canonical casing and reports if not.
pub fn check_class_like_casing<A>(context: &mut Context<'_, '_, A>, used_name: Word, span: Span)
where
    A: Arena,
{
    // A PHP# type in another case names nothing, which the code that finds no type reports, spec section 24.
    if !context.settings.check_name_casing {
        return;
    }

    let Some(metadata) = context.codebase.get_class_like(used_name.as_bytes()) else {
        return;
    };

    let canonical = metadata.original_name;
    if canonical == used_name {
        return;
    }

    let (used_name, canonical) = if context.dialect.is_sharp() {
        (used_name.to_string().replace('\\', "."), canonical.to_string().replace('\\', "."))
    } else {
        (used_name.to_string(), canonical.to_string())
    };

    // Extract the mismatched part (could be namespace or class name)
    // Compare only the parts that differ
    context.collector.report_with_code(
        IssueCode::IncorrectClassLikeCasing,
        Issue::warning(format!("Incorrect casing for class-like `{used_name}`."))
            .with_annotation(
                Annotation::primary(span).with_message(format!("Used as `{used_name}`, but defined as `{canonical}`.")),
            )
            .with_note("Using incorrect casing can cause autoloading failures on case-sensitive file systems.")
            .with_help(format!("Use the correct casing: `{canonical}`.")),
    );
}

/// Checks if the used function name matches the canonical casing and reports if not.
pub fn check_function_casing_with_metadata<A>(
    context: &mut Context<'_, '_, A>,
    metadata: &FunctionLikeMetadata,
    used_name: Word,
    span: Span,
) where
    A: Arena,
{
    if !context.settings.check_name_casing {
        return;
    }

    let canonical = metadata.original_name;

    if canonical == used_name {
        return;
    }

    let canonical_bytes = canonical.as_bytes();
    let used_bytes = used_name.as_bytes();
    if memchr::memchr(b'\\', canonical_bytes).is_none()
        && let Some(idx) = memchr::memrchr(b'\\', used_bytes)
        && &used_bytes[idx + 1..] == canonical_bytes
    {
        return;
    }

    context.collector.report_with_code(
        IssueCode::IncorrectFunctionCasing,
        Issue::warning(format!("Incorrect casing for function `{used_name}`."))
            .with_annotation(
                Annotation::primary(span).with_message(format!("Used as `{used_name}`, but defined as `{canonical}`.")),
            )
            .with_help(format!("Use the correct casing: `{canonical}`.")),
    );
}

/// Checks if the used function name matches the canonical casing and reports if not.
fn check_function_casing<A>(context: &mut Context<'_, '_, A>, used_name: Word, span: Span)
where
    A: Arena,
{
    if !context.settings.check_name_casing {
        return;
    }

    let Some(metadata) = context.codebase.get_function(used_name.as_bytes()) else {
        return;
    };

    check_function_casing_with_metadata(context, metadata, used_name, span);
}

/// Checks casing of all names imported via `use` statements.
pub fn check_use_statement_casing<A>(context: &mut Context<'_, '_, A>, r#use: &Use<'_>)
where
    A: Arena,
{
    match &r#use.items {
        UseItems::Sequence(sequence) => {
            for item in sequence.items.iter() {
                let fqn = word(mago_bytes::trim_start_byte(&php_name(&item.name), b'\\'));
                check_class_like_casing(context, fqn, item.name.span());
            }
        }
        UseItems::TypedSequence(typed_sequence) => {
            for item in typed_sequence.items.iter() {
                let fqn = word(mago_bytes::trim_start_byte(&php_name(&item.name), b'\\'));
                check_typed_use_casing(context, fqn, item.name.span(), &typed_sequence.r#type);
            }
        }
        UseItems::TypedList(typed_list) => {
            let namespace = php_name(&typed_list.namespace);
            let prefix = mago_bytes::trim_start_byte(&namespace, b'\\');
            for item in typed_list.items.iter() {
                let fqn = concat_word!(prefix, b"\\", item.name.value());
                check_typed_use_casing(context, fqn, item.name.span(), &typed_list.r#type);
            }
        }
        UseItems::MixedList(mixed_list) => {
            let namespace = php_name(&mixed_list.namespace);
            let prefix = mago_bytes::trim_start_byte(&namespace, b'\\');
            for maybe_typed_item in mixed_list.items.iter() {
                let fqn = concat_word!(prefix, b"\\", maybe_typed_item.item.name.value());
                match &maybe_typed_item.r#type {
                    Some(UseType::Function(_)) => {
                        check_function_casing(context, fqn, maybe_typed_item.item.name.span())
                    }
                    Some(UseType::Const(_)) => {} // Constants are case-sensitive
                    None => check_class_like_casing(context, fqn, maybe_typed_item.item.name.span()),
                }
            }
        }
    }
}

fn check_typed_use_casing<A>(context: &mut Context<'_, '_, A>, fqn: Word, span: Span, use_type: &UseType<'_>)
where
    A: Arena,
{
    match use_type {
        UseType::Function(_) => check_function_casing(context, fqn, span),
        UseType::Const(_) => {} // Constants are case-sensitive
    }
}
