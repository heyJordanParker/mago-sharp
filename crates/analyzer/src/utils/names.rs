//! Helpers for rendering symbol names in user-facing diagnostics.

use std::cell::OnceCell;

use foldhash::HashMap;
use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::union::TUnion;
use mago_names::display_sharp_member;
use mago_names::kind::NameKind;
use mago_names::scope::NamespaceScope;
use mago_names::short_name;
use mago_syntax_core::utils::is_part_of_identifier;
use mago_word::Word;
use mago_word::WordMap;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::context::Context;

/// Returns the case-preserved name of a class-like as the analyzed file writes it: as [`sharp_class_like_name`] names
/// it in a `.sharp` file, and its full name in PHP. Falls back to the input if no metadata is available.
#[inline]
pub(crate) fn display_class_like_name<A>(context: &Context<'_, '_, A>, name: Word) -> Word
where
    A: Arena,
{
    if context.dialect.is_sharp() {
        word(sharp_class_like_name(context.codebase, &context.imported_names, &context.short_name_counts, name))
    } else {
        context.codebase.get_class_like(name.as_bytes()).map_or(name, |m| m.original_name)
    }
}

/// Returns the case-preserved name of the class-like `name` as a `.sharp` file names it, given the `imported_names` of
/// the file: the name the file's import gives it, as in `Rx` for `import Sharp.Text.Regex as Rx;` and `Order` for
/// `import App.Orders.Order;`. A class the file doesn't import is named by its short name, or, when another class-like
/// of `codebase` has the same short name, by its full name with `.` between its parts, so `Billing.Order` and
/// `App.Orders.Order` stay apart. `short_name_counts` keeps the count of each short name once it is first needed.
pub(crate) fn sharp_class_like_name(
    codebase: &CodebaseMetadata,
    imported_names: &WordMap<Word>,
    short_name_counts: &OnceCell<HashMap<String, u32>>,
    name: Word,
) -> String {
    let name = codebase.get_class_like(name.as_bytes()).map_or(name, |m| m.original_name);
    let full_name = mago_bytes::trim_start_byte(name.as_bytes(), b'\\');

    if let Some(imported_name) = imported_names.get(&ascii_lowercase_word(full_name)) {
        imported_name.to_string()
    } else if shares_short_name(codebase, short_name_counts, name) {
        String::from_utf8_lossy(full_name).replace('\\', ".")
    } else {
        short_name(name)
    }
}

/// Whether another class-like of the project or its vendors has the short name of the class-like `name`, compared
/// without case as PHP compares class names. PHP's built-in class-likes don't count: a `.sharp` file reaches one,
/// like `Dom\Text`, only through an import, and one file can't import two classes of one short name without an
/// alias. The prelude's `Sharp\` class-likes do count, as a `.sharp` file reaches them with no import.
fn shares_short_name(
    codebase: &CodebaseMetadata,
    short_name_counts: &OnceCell<HashMap<String, u32>>,
    name: Word,
) -> bool {
    let counts = short_name_counts.get_or_init(|| {
        let mut counts = HashMap::default();
        for (class_like, metadata) in &codebase.class_likes {
            // php-sharp#60: the prelude's `Sharp\` class-likes are built-in, yet a `.sharp` file reaches them with no
            // import.
            if !metadata.flags.is_built_in() || class_like.as_bytes().starts_with(b"sharp\\") {
                *counts.entry(short_name(class_like).to_ascii_lowercase()).or_insert(0) += 1;
            }
        }

        counts
    });

    counts.get(&short_name(name).to_ascii_lowercase()).is_some_and(|count| *count > 1)
}

/// Returns the sentence that names the `import` lines the analyzed file needs, as [`sharp_missing_imports`] writes it,
/// or `None` in a PHP file.
pub(crate) fn display_missing_imports<A>(
    context: &Context<'_, '_, A>,
    class_names: impl IntoIterator<Item = Word>,
) -> Option<String>
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return None;
    }

    sharp_missing_imports(context.codebase, &context.imported_names, &context.scope, class_names)
}

/// Returns the sentence that names the `import` lines a `.sharp` file, with the `imported_names` and `scope` of its
/// imports and namespace, needs before code that names `class_names` as [`sharp_code_class_name`] writes them
/// compiles, as `Add `import Sharp.Text.Regex;` to the file.`: one line for each class-like the file doesn't
/// [bind](binds_class_like), once each, in the order given, under its [alias](sharp_import_alias) when the file binds
/// its short name to another class-like, as in `import Vendor.Clock as VendorClock;`. Returns `None` when the file
/// binds them all.
pub(crate) fn sharp_missing_imports(
    codebase: &CodebaseMetadata,
    imported_names: &WordMap<Word>,
    scope: &NamespaceScope,
    class_names: impl IntoIterator<Item = Word>,
) -> Option<String> {
    let mut imports: Vec<String> = Vec::new();
    for class_name in class_names {
        let name = codebase.get_class_like(class_name.as_bytes()).map_or(class_name, |m| m.original_name);
        if binds_class_like(imported_names, scope, name) {
            continue;
        }

        let full_name = String::from_utf8_lossy(mago_bytes::trim_start_byte(name.as_bytes(), b'\\')).replace('\\', ".");
        let import = match sharp_import_alias(codebase, imported_names, scope, name) {
            Some(alias) => format!("`import {full_name} as {alias};`"),
            None => format!("`import {full_name};`"),
        };
        if !imports.contains(&import) {
            imports.push(import);
        }
    }

    let (last, others) = imports.split_last()?;
    let imports = if others.is_empty() { last.clone() } else { format!("{} and {last}", others.join(", ")) };

    Some(format!("Add {imports} to the file."))
}

/// Returns the name a `.sharp` file, with the `imported_names` and `scope` of its imports and namespace, imports the
/// class-like `name` under when it doesn't [bind](binds_class_like) `name` but [binds](binds_short_name) its short
/// name to another class-like, so neither import shadows the other. The alias joins the parts of the full name, each
/// from its first letter on with that letter capitalized, as a PHP# type name starts: `VendorClock` for
/// `vendor\Clock`. While the file binds the alias too, the first number from 1 that frees it follows, as in
/// `VendorClock1`, as C#'s Roslyn numbers a name it generates. Returns `None` otherwise, and for a name with no ASCII
/// letter, which no PHP# type name can be built from.
fn sharp_import_alias(
    codebase: &CodebaseMetadata,
    imported_names: &WordMap<Word>,
    scope: &NamespaceScope,
    name: Word,
) -> Option<String> {
    if binds_class_like(imported_names, scope, name)
        || !binds_short_name(codebase, imported_names, scope, &short_name(name))
    {
        return None;
    }

    let alias: String = String::from_utf8_lossy(mago_bytes::trim_start_byte(name.as_bytes(), b'\\'))
        .split('\\')
        .map(|part| {
            let mut part = part.trim_start_matches(|character: char| !character.is_ascii_alphabetic()).to_owned();
            if let Some(first) = part.get_mut(..1) {
                first.make_ascii_uppercase();
            }

            part
        })
        .collect();
    if alias.is_empty() {
        return None;
    }

    let mut free = alias.clone();
    let mut number = 0;
    while binds_short_name(codebase, imported_names, scope, &free) {
        number += 1;
        free = format!("{alias}{number}");
    }

    Some(free)
}

/// Whether a `.sharp` file with the `imported_names` and `scope` of its imports and namespace binds the short name
/// `name` to a class-like, as [`binds_class_like`] binds one: by an import, by a class-like of its namespace, or by the
/// class-like of `Sharp` it imports by default. A class-like the name would bind only by sharing the namespace counts
/// when it exists.
fn binds_short_name(
    codebase: &CodebaseMetadata,
    imported_names: &WordMap<Word>,
    scope: &NamespaceScope,
    name: &str,
) -> bool {
    let (bound, imported) = scope.resolve(NameKind::Default, name);

    [bound, format!("Sharp\\{name}").into_bytes()].into_iter().any(|class_like| {
        (imported || codebase.class_like_exists(&class_like))
            && binds_class_like(imported_names, scope, word(&class_like))
    })
}

/// Whether a `.sharp` file with the `imported_names` and `scope` of its imports and namespace binds a name to the
/// class-like `name`, as spec section 23 binds a name: by an import, under its short name or the name after `as`, by
/// declaring it, or by sharing its namespace. A class-like directly in `Sharp` is imported by default, unless the file
/// imports or declares another class-like of its short name.
fn binds_class_like(imported_names: &WordMap<Word>, scope: &NamespaceScope, name: Word) -> bool {
    let name = mago_bytes::trim_start_byte(name.as_bytes(), b'\\');
    if imported_names.contains_key(&ascii_lowercase_word(name)) {
        return true;
    }

    let (bound, imported) = scope.resolve(NameKind::Default, short_name(name));
    if bound.eq_ignore_ascii_case(name) {
        return true;
    }

    !imported
        && name.iter().rposition(|byte| *byte == b'\\').is_some_and(|end| name[..end].eq_ignore_ascii_case(b"Sharp"))
}

/// Returns the property `name`, which the codebase keys with its `$`, as the analyzed file names it in prose: `total`
/// in a `.sharp` file, and `$total` in PHP.
#[inline]
pub(crate) fn display_property_name<A>(context: &Context<'_, '_, A>, name: Word) -> Word
where
    A: Arena,
{
    if context.dialect.is_sharp() { word(mago_bytes::trim_start_byte(name.as_bytes(), b'$')) } else { name }
}

/// Returns the variable `id` as the analyzed file writes it. The analyzer keys a variable, a parameter and a read
/// through one by its PHP text, as `$order`, `$this->total` and `App\Shop\Order::$count`. A `.sharp` file writes them
/// as `order`, `this.total` and `Order.count`: a property is named without `$`, and the class before `::` as
/// [`sharp_class_like_name`] names it. PHP gets `id` unchanged. Text inside quotes, as the key of `$prices['$']`, is
/// kept as written.
#[must_use]
pub(crate) fn display_variable_name<A>(context: &Context<'_, '_, A>, id: impl AsRef<[u8]>) -> String
where
    A: Arena,
{
    let id = id.as_ref();
    if !context.dialect.is_sharp() {
        return String::from_utf8_lossy(id).into_owned();
    }

    let mut written = Vec::with_capacity(id.len());
    let mut quote = None;
    let mut index = 0;
    while let Some(&byte) = id.get(index) {
        let rest = &id[index..];
        let (text, length): (&[u8], usize) = match quote {
            Some(open) => {
                if byte == open {
                    quote = None;
                }

                (&rest[..1], 1)
            }
            None if byte == b'\'' || byte == b'"' => {
                quote = Some(byte);

                (&rest[..1], 1)
            }
            None if byte == b'$' => (b"", 1),
            None if rest.starts_with(b"?->") => (b"?.", 3),
            None if rest.starts_with(b"::") => {
                let class_start = id[..index]
                    .iter()
                    .rposition(|byte| !is_part_of_identifier(byte) && *byte != b'\\')
                    .map_or(0, |end| end + 1);
                if class_start < index
                    && !matches!(class_start.checked_sub(1).map(|before| id[before]), Some(b'$' | b'>'))
                {
                    written.truncate(written.len() - (index - class_start));
                    let class_name = sharp_class_like_name(
                        context.codebase,
                        &context.imported_names,
                        &context.short_name_counts,
                        word(&id[class_start..index]),
                    );
                    written.extend_from_slice(class_name.as_bytes());
                }

                (b".", 2)
            }
            None if rest.starts_with(b"->") => (b".", 2),
            None => (&rest[..1], 1),
        };

        written.extend_from_slice(text);
        index += length;
    }

    String::from_utf8_lossy(&written).into_owned()
}

/// Returns the case-preserved method name on the given class-like for
/// user-facing diagnostics. Falls back to the input if metadata is missing.
#[inline]
pub(crate) fn display_method_name<A>(context: &Context<'_, '_, A>, class_name: Word, method_name: Word) -> Word
where
    A: Arena,
{
    context.codebase.get_method(class_name.as_bytes(), method_name.as_bytes()).map_or(method_name, |m| m.original_name)
}

/// Returns the case-preserved name of a global function for user-facing
/// diagnostics. Falls back to the input if metadata is missing.
#[inline]
pub(crate) fn display_function_name<A>(context: &Context<'_, '_, A>, name: Word) -> Word
where
    A: Arena,
{
    context.codebase.get_function(name.as_bytes()).map_or(name, |m| m.original_name)
}

/// Returns the PHP# collection type, as in `Map<string, int>`, that `object` stands for when it is `Sharp\ListMethods`,
/// `Sharp\MapMethods` or `Sharp\SetMethods`. The analyzer checks a method call on a `List`, `Map` or `Set` against
/// those classes, and a message names the type the code wrote instead.
#[must_use]
pub(crate) fn display_sharp_collection<A>(context: &Context<'_, '_, A>, object: &TObject) -> Option<String>
where
    A: Arena,
{
    let collection = sharp_collection_name(object)?;
    let parameters = object.get_type_parameters().unwrap_or_default();
    let parameters = parameters.iter().map(|parameter| display_sharp_type(context, parameter)).collect::<Vec<_>>();

    Some(format!("{collection}<{}>", parameters.join(", ")))
}

/// Returns `union` as the analyzed file writes types: as PHP# writes it in a `.sharp` file, and by its Mago type id in
/// PHP.
#[must_use]
pub(crate) fn display_type<A>(context: &Context<'_, '_, A>, union: &TUnion) -> String
where
    A: Arena,
{
    if context.dialect.is_sharp() { display_sharp_type(context, union) } else { union.get_id().to_string() }
}

/// Returns the one type `atomic` as the analyzed file writes types: as PHP# writes it in a `.sharp` file, and by its
/// Mago type id in PHP. A union of the one type would wrap a template type's id in parentheses, as it does inside a
/// union of several types.
#[must_use]
pub(crate) fn display_atomic<A>(context: &Context<'_, '_, A>, atomic: &TAtomic) -> String
where
    A: Arena,
{
    if context.dialect.is_sharp() {
        display_sharp_type(context, &TUnion::from_atomic(atomic.clone()))
    } else {
        atomic.get_id().to_string()
    }
}

/// Returns `union` with null added as the analyzed file writes types: as PHP# writes the nullable type in a `.sharp`
/// file, and `php` in PHP.
#[must_use]
pub(crate) fn display_nullable_type<A>(context: &Context<'_, '_, A>, union: &TUnion, php: String) -> String
where
    A: Arena,
{
    if context.dialect.is_sharp() { display_sharp_type(context, &union.clone().as_nullable()) } else { php }
}

/// Returns what a condition is when it holds, or when it does not, as the analyzed file says it. A PHP# condition is a
/// `bool`, so it is `true` or `false`. PHP tests truthiness, so it is truthy or falsy.
#[must_use]
pub(crate) const fn display_truth<A>(context: &Context<'_, '_, A>, holds: bool) -> &'static str
where
    A: Arena,
{
    match (context.dialect.is_sharp(), holds) {
        (true, _) => display_bool(context, holds),
        (false, true) => "truthy",
        (false, false) => "falsy",
    }
}

/// Returns the `bool` value `value` as the analyzed file writes it in prose: `` `true` `` or `` `false` `` in a
/// `.sharp` file, as PHP# writes its `bool` literals, and true or false in PHP.
#[must_use]
pub(crate) const fn display_bool<A>(context: &Context<'_, '_, A>, value: bool) -> &'static str
where
    A: Arena,
{
    match (context.dialect.is_sharp(), value) {
        (true, true) => "`true`",
        (true, false) => "`false`",
        (false, true) => "true",
        (false, false) => "false",
    }
}

/// Returns `union`, the type of a value a message checks against `expected`, the type it must have, as the analyzed
/// file writes types. In a `.sharp` file a literal is its general type, `string` for `"text"`, as PHP# writes no
/// literal type, unless `expected` holds literals of its kind: `"up"` stays `"up"` against `"asc"|"desc"`, as the
/// literal is what fails. In PHP it is its Mago type id.
#[must_use]
pub(crate) fn display_value_type<A>(context: &Context<'_, '_, A>, union: &TUnion, expected: &TUnion) -> String
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return union.get_id().to_string();
    }

    let literal_kind = |atomic: &TAtomic| match atomic {
        TAtomic::Scalar(scalar) if scalar.is_literal_value() => Some(std::mem::discriminant(scalar)),
        _ => None,
    };
    let general = union
        .types
        .iter()
        .flat_map(|atomic| {
            let kept = literal_kind(atomic)
                .is_some_and(|kind| expected.types.iter().any(|wanted| literal_kind(wanted) == Some(kind)));
            let mut atomic = TUnion::from_atomic(atomic.clone());
            if !kept {
                atomic.widen_literals();
            }

            atomic.types.into_owned()
        })
        .collect();

    display_sharp_type(context, &TUnion::from_vec(general))
}

/// Returns `union` as PHP# writes the type: `List<int>`, `Map<string, int>`, `int?`, `(int|string)?`, `Any?`, a class
/// as [`sharp_class_like_name`] names it, a type parameter by its name, an intersection as `A & B`, a function type as
/// `Function<void(int)>` or `Function<Charge(Cart) uses Http>` with each effect named as a class is, `Class<Order>`,
/// `Object`, `Iterable<int>`, and a literal as `1` or `"text"`. A refinement that
/// PHP# cannot write is the type that holds it, such as `int` for a `positive-int` and `int|string` for an
/// `array-key`, and is named once. `numeric`, `scalar` and `never` have no PHP# name and keep Mago's. A PHP docblock's
/// `non-empty-mixed` has no PHP# name either, and keeps the name its docblock writes, as Mago's `truthy-mixed` speaks
/// of a truthiness PHP# does not have.
#[must_use]
pub(crate) fn display_sharp_type<A>(context: &Context<'_, '_, A>, union: &TUnion) -> String
where
    A: Arena,
{
    if let Some(TAtomic::Mixed(mixed)) = union.types.iter().find(|atomic| atomic.is_mixed()) {
        return if mixed.is_truthy() {
            "non-empty-mixed"
        } else if mixed.is_non_null() {
            "Any"
        } else {
            "Any?"
        }
        .to_owned();
    }

    let mut parts: Vec<String> = Vec::new();
    for atomic in union.types.iter().filter(|atomic| !atomic.is_null()) {
        let written = match atomic {
            TAtomic::Scalar(TScalar::ArrayKey) => vec!["int".to_owned(), "string".to_owned()],
            atomic => vec![display_sharp_atomic(context, atomic)],
        };
        for part in written {
            if !parts.contains(&part) {
                parts.push(part);
            }
        }
    }

    match (union.has_null(), parts.as_slice()) {
        (false, _) => parts.join("|"),
        (true, []) => "null".to_owned(),
        (true, [part]) => format!("{part}?"),
        (true, _) => format!("({})?", parts.join("|")),
    }
}

fn display_sharp_atomic<A>(context: &Context<'_, '_, A>, atomic: &TAtomic) -> String
where
    A: Arena,
{
    let written = match atomic {
        TAtomic::Array(array) => {
            let (key, value) = get_array_parameters(array, context.codebase);
            match array {
                TArray::List(_) => format!("List<{}>", display_sharp_type(context, &value)),
                TArray::Keyed(_) => {
                    format!("Map<{}, {}>", display_sharp_type(context, &key), display_sharp_type(context, &value))
                }
                TArray::Set(_) => format!("Set<{}>", display_sharp_type(context, &value)),
            }
        }
        TAtomic::Iterable(iterable) => format!("Iterable<{}>", display_sharp_type(context, iterable.get_value_type())),
        TAtomic::Object(TObject::Any) => "Object".to_owned(),
        TAtomic::Object(object) => {
            let Some(name) = object.get_name() else {
                return atomic.get_id().to_string();
            };
            let name =
                sharp_class_like_name(context.codebase, &context.imported_names, &context.short_name_counts, name);
            match object.get_type_parameters() {
                Some(parameters) if !parameters.is_empty() => {
                    let parameters: Vec<String> =
                        parameters.iter().map(|parameter| display_sharp_type(context, parameter)).collect();
                    format!("{name}<{}>", parameters.join(", "))
                }
                _ => name,
            }
        }
        TAtomic::GenericParameter(parameter) => parameter.parameter_name.to_string(),
        TAtomic::Callable(TCallable::Signature(signature)) => {
            let written = |union: Option<&TUnion>| {
                union.map_or_else(|| "Any?".to_owned(), |union| display_sharp_type(context, union))
            };
            let parameters: Vec<String> =
                signature.get_parameters().iter().map(|parameter| written(parameter.get_type_signature())).collect();
            let effects: Vec<String> = signature
                .effects
                .iter()
                .flatten()
                .map(|effect| {
                    sharp_class_like_name(
                        context.codebase,
                        &context.imported_names,
                        &context.short_name_counts,
                        *effect,
                    )
                })
                .collect();
            let uses = if effects.is_empty() { String::new() } else { format!(" uses {}", effects.join(", ")) };

            format!("Function<{}({}){uses}>", written(signature.get_return_type()), parameters.join(", "))
        }
        TAtomic::Scalar(TScalar::ClassLikeString(class_string)) => match class_string {
            TClassLikeString::Literal { value } => format!(
                "Class<{}>",
                sharp_class_like_name(context.codebase, &context.imported_names, &context.short_name_counts, *value)
            ),
            TClassLikeString::OfType { constraint, .. } => {
                format!("Class<{}>", display_sharp_atomic(context, constraint))
            }
            TClassLikeString::Generic { parameter_name, .. } => format!("Class<{parameter_name}>"),
            TClassLikeString::Any { .. } => "Class<Object>".to_owned(),
        },
        TAtomic::Scalar(scalar) => {
            if let Some(value) = scalar.get_literal_int_value() {
                value.to_string()
            } else if let Some(value) = scalar.get_literal_float_value() {
                value.to_string()
            } else if let Some(value) = scalar.get_known_literal_string_value() {
                format!("\"{}\"", String::from_utf8_lossy(value))
            } else {
                match scalar {
                    TScalar::Integer(_) => "int".to_owned(),
                    TScalar::String(_) => "string".to_owned(),
                    _ => atomic.get_id().to_string(),
                }
            }
        }
        _ => atomic.get_id().to_string(),
    };

    match atomic.get_intersection_types() {
        Some(intersection_types) if !intersection_types.is_empty() => std::iter::once(written)
            .chain(intersection_types.iter().map(|intersection_type| display_sharp_atomic(context, intersection_type)))
            .collect::<Vec<_>>()
            .join(" & "),
        _ => written,
    }
}

/// The accessor `hook_name` of the property `property_name` of the class `class_name` as PHP# names it, `Box.total.get`,
/// as C# names an accessor in its messages.
#[must_use]
pub(crate) fn display_sharp_accessor<A>(
    context: &Context<'_, '_, A>,
    class_name: Word,
    property_name: Word,
    hook_name: Word,
) -> String
where
    A: Arena,
{
    display_sharp_member(
        sharp_class_like_name(context.codebase, &context.imported_names, &context.short_name_counts, class_name),
        format_args!("{property_name}.{hook_name}"),
    )
}

/// The member `member_name` of the class `class_name` as the analyzed file names it: `Box.put` in a `.sharp` file, with
/// the class named as [`sharp_class_like_name`] names it, and `Box::put` in PHP, where a property keeps its `$`:
/// `Box::$total`.
#[must_use]
pub(crate) fn display_member<A>(
    context: &Context<'_, '_, A>,
    class_name: Word,
    member_name: impl std::fmt::Display,
) -> String
where
    A: Arena,
{
    if context.dialect.is_sharp() {
        display_sharp_member(
            sharp_class_like_name(context.codebase, &context.imported_names, &context.short_name_counts, class_name),
            member_name,
        )
    } else {
        format!("{class_name}::{member_name}")
    }
}

/// The member `member_name` of the class `class_name` as code the analyzed file writes: `Status.cases()` in a `.sharp`
/// file, by the name an import binds, and `Status::cases()` in PHP. The class is named as [`sharp_code_class_name`]
/// names it.
#[must_use]
pub(crate) fn display_code_member<A>(
    context: &Context<'_, '_, A>,
    class_name: Word,
    member_name: impl std::fmt::Display,
) -> String
where
    A: Arena,
{
    if context.dialect.is_sharp() {
        display_sharp_member(
            sharp_code_class_name(context.codebase, &context.imported_names, &context.scope, class_name),
            member_name,
        )
    } else {
        display_member(context, class_name, member_name)
    }
}

/// The class-like `name` as code in a `.sharp` file, with the `imported_names` and `scope` of its imports and
/// namespace, writes it: by the name the file's import gives it, by the [alias](sharp_import_alias) that
/// [`sharp_missing_imports`] imports it under when the file binds its short name to another class-like, or else by its
/// short name. PHP# refuses a full name in code (spec section 23), so the dotted name that tells two classes of one
/// short name apart in prose would not compile there.
#[must_use]
pub(crate) fn sharp_code_class_name(
    codebase: &CodebaseMetadata,
    imported_names: &WordMap<Word>,
    scope: &NamespaceScope,
    name: Word,
) -> String {
    let name = codebase.get_class_like(name.as_bytes()).map_or(name, |m| m.original_name);
    if let Some(imported_name) =
        imported_names.get(&ascii_lowercase_word(mago_bytes::trim_start_byte(name.as_bytes(), b'\\')))
    {
        return imported_name.to_string();
    }

    sharp_import_alias(codebase, imported_names, scope, name).unwrap_or_else(|| short_name(name))
}

/// Returns `List`, `Map` or `Set` when `object` is `Sharp\ListMethods`, `Sharp\MapMethods` or `Sharp\SetMethods`.
#[must_use]
pub(crate) fn sharp_collection_name(object: &TObject) -> Option<&'static str> {
    let name = object.get_name()?;

    if name.as_bytes().eq_ignore_ascii_case(b"Sharp\\ListMethods") {
        Some("List")
    } else if name.as_bytes().eq_ignore_ascii_case(b"Sharp\\MapMethods") {
        Some("Map")
    } else if name.as_bytes().eq_ignore_ascii_case(b"Sharp\\SetMethods") {
        Some("Set")
    } else {
        None
    }
}

/// A class under `Sharp\` that the prelude declares as a plain PHP stub.
// Deleted once the standard library's `.sharp` sources replace the prelude stubs (php-sharp issue #60).
#[must_use]
pub(crate) fn is_prelude_stub(class: Word) -> bool {
    class.as_bytes().len() > 6 && class.as_bytes()[..6].eq_ignore_ascii_case(b"sharp\\")
}

/// Produces a user-facing display string for a `FunctionLikeIdentifier`: a method reads `Order::total` in PHP and
/// `Order.total` in a `.sharp` file.
#[must_use]
pub(crate) fn display_function_like_identifier<A>(
    context: &Context<'_, '_, A>,
    identifier: &FunctionLikeIdentifier,
) -> String
where
    A: Arena,
{
    match identifier {
        FunctionLikeIdentifier::Function(name) => display_function_name(context, *name).to_string(),
        FunctionLikeIdentifier::Method(class_name, method_name) => display_member(
            context,
            display_class_like_name(context, *class_name),
            display_method_name(context, *class_name, *method_name),
        ),
        FunctionLikeIdentifier::Closure(name) => name.to_string(),
    }
}
