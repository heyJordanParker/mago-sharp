//! Helpers for rendering symbol names in user-facing diagnostics.

use std::cell::OnceCell;

use foldhash::HashMap;
use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeKind;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::module::ModuleMetadata;
use mago_codex::ttype::SharpType;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::reference::TReference;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::get_sharp_type;
use mago_codex::ttype::union::TUnion;
use mago_names::ResolvedNames;
use mago_names::STANDARD_EXCEPTIONS;
use mago_names::binding::sharp_operator_symbol;
use mago_names::display_sharp_member;
use mago_names::kind::NameKind;
use mago_names::scope::NamespaceScope;
use mago_names::short_name;
use mago_span::HasPosition;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::MethodCall;
use mago_syntax_core::utils::is_part_of_identifier;
use mago_word::Word;
use mago_word::WordMap;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::context::Context;
use crate::visibility::display_namespace;
use crate::visibility::list_refusing;

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
    // Spec section 22: the trait that holds an interface's default bodies is named by the interface, which PHP# code
    // writes, as in `HasLabel.label`.
    let name = codebase
        .get_class_like(name.as_bytes())
        .map_or(name, |class_like| codebase.get_defaults_interface(class_like).unwrap_or(class_like).original_name);
    let full_name = mago_bytes::trim_start_byte(name.as_bytes(), b'\\');

    if let Some(imported_name) = imported_names.get(&ascii_lowercase_word(full_name)) {
        imported_name.to_string()
    } else if shares_short_name(codebase, short_name_counts, name) {
        String::from_utf8_lossy(full_name).replace('\\', ".")
    } else {
        short_name(name)
    }
}

/// Returns the effect `effect` as a `uses` in a `.sharp` file with the `imported_names` of its imports writes it. A
/// standard effect, one of the `Sharp` namespace, is the name the file's import of it gives it, as in `Post` for
/// `import Sharp.Mail as Post;`, or else its own name, which is that effect in every `uses` whatever class the file
/// imports or declares under it, spec section 29. Any other effect is named as [`sharp_class_like_name`] names a class.
pub(crate) fn sharp_effect_name(
    codebase: &CodebaseMetadata,
    imported_names: &WordMap<Word>,
    short_name_counts: &OnceCell<HashMap<String, u32>>,
    effect: Word,
) -> String {
    if !effect.as_bytes().get(..6).is_some_and(|namespace| namespace.eq_ignore_ascii_case(b"Sharp\\")) {
        return sharp_class_like_name(codebase, imported_names, short_name_counts, effect);
    }

    imported_names.get(&ascii_lowercase_word(effect.as_bytes())).map_or_else(|| short_name(effect), ToString::to_string)
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
/// its short name to another class-like, as in `import Vendor.Clock as VendorClock;`. A line the `imports` line of
/// the file's namespace refuses (spec section 32) is left out. When it leaves out every line, the sentence names the
/// class-likes outside the list and the file of that `imports` line instead. Returns `None` when the file binds them
/// all.
pub(crate) fn sharp_missing_imports(
    codebase: &CodebaseMetadata,
    imported_names: &WordMap<Word>,
    scope: &NamespaceScope,
    class_names: impl IntoIterator<Item = Word>,
) -> Option<String> {
    let namespace = scope.namespace_name().unwrap_or_default();
    let mut imports: Vec<String> = Vec::new();
    let mut unlisted: Vec<(&ModuleMetadata, Vec<Word>)> = Vec::new();
    for class_name in class_names {
        let name = codebase.get_class_like(class_name.as_bytes()).map_or(class_name, |m| m.original_name);
        if binds_class_like(imported_names, scope, name) {
            continue;
        }

        let php_name = mago_bytes::trim_start_byte(name.as_bytes(), b'\\');
        let full_name = String::from_utf8_lossy(php_name).replace('\\', ".");
        if let Some(list) = list_refusing(codebase, namespace, php_name) {
            let full_name = word(&full_name);
            match unlisted.iter_mut().find(|(other, _)| std::ptr::eq(*other, list)) {
                Some((_, names)) if !names.contains(&full_name) => names.push(full_name),
                Some(_) => {}
                None => unlisted.push((list, vec![full_name])),
            }
            continue;
        }

        let import = match sharp_import_alias(codebase, imported_names, scope, name) {
            Some(alias) => format!("`import {full_name} as {alias};`"),
            None => format!("`import {full_name};`"),
        };
        if !imports.contains(&import) {
            imports.push(import);
        }
    }

    let Some((last, others)) = imports.split_last() else {
        let sentences: Vec<String> = unlisted
            .iter()
            .map(|(list, names)| {
                format!(
                    "{} {} outside `{}`'s list, the `imports` line of {}.",
                    and_list(names),
                    if names.len() == 1 { "is" } else { "are" },
                    display_namespace(codebase, list),
                    list.file
                )
            })
            .collect();

        return (!sentences.is_empty()).then(|| sentences.join(" "));
    };
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
/// class-like of `Sharp` or standard exception it imports by default. A class-like the name would bind only by sharing
/// the namespace counts when it exists.
fn binds_short_name(
    codebase: &CodebaseMetadata,
    imported_names: &WordMap<Word>,
    scope: &NamespaceScope,
    name: &str,
) -> bool {
    let (bound, imported) = scope.resolve(NameKind::Default, name);

    [bound, format!("Sharp\\{name}").into_bytes(), name.as_bytes().to_vec()].into_iter().any(|class_like| {
        (imported || codebase.class_like_exists(&class_like))
            && binds_class_like(imported_names, scope, word(&class_like))
    })
}

/// Whether a `.sharp` file with the `imported_names` and `scope` of its imports and namespace binds a name to the
/// class-like `name`, as spec section 23 binds a name: by an import, under its short name or the name after `as`, by
/// declaring it, by sharing its namespace, or by the [default import](imports_by_default) of `Sharp`.
fn binds_class_like(imported_names: &WordMap<Word>, scope: &NamespaceScope, name: Word) -> bool {
    let full_name = mago_bytes::trim_start_byte(name.as_bytes(), b'\\');
    if imported_names.contains_key(&ascii_lowercase_word(full_name)) {
        return true;
    }

    let (bound, _) = scope.resolve(NameKind::Default, short_name(full_name));

    bound.eq_ignore_ascii_case(full_name) || imports_by_default(scope, name)
}

/// Whether spec section 23's default import of `Sharp` and the standard exceptions binds the class-like `name` in a
/// `.sharp` file with the `scope` of its imports and namespace: `name` is directly in `Sharp` or is a standard
/// exception, and the file imports or declares no other class-like of its short name.
pub(crate) fn imports_by_default(scope: &NamespaceScope, name: Word) -> bool {
    let name = mago_bytes::trim_start_byte(name.as_bytes(), b'\\');
    let (_, imported) = scope.resolve(NameKind::Default, short_name(name));

    !imported
        && (name.iter().rposition(|byte| *byte == b'\\').is_some_and(|end| name[..end].eq_ignore_ascii_case(b"Sharp"))
            || STANDARD_EXCEPTIONS.iter().any(|exception| exception.as_bytes().eq_ignore_ascii_case(name)))
}

/// Whether the full name `name` is `Sharp\Internal`, the namespace of the engine's functions that only the standard
/// library imports (spec section 15), or a name under it, compared ignoring case.
pub(crate) fn is_sharp_internal(name: &[u8]) -> bool {
    let internal = b"Sharp\\Internal";

    name.get(..internal.len()).is_some_and(|root| root.eq_ignore_ascii_case(internal))
        && matches!(name.get(internal.len()), None | Some(b'\\'))
}

/// Whether the full name `name` is PHP's `Closure`, compared ignoring case. Spec section 14.1 has one name for every
/// function type, `Function<R(P)>`, so a `.sharp` file names no class `Closure`, as it names none it never declared.
pub(crate) fn is_php_closure(name: &[u8]) -> bool {
    mago_bytes::trim_start_byte(name, b'\\').eq_ignore_ascii_case(b"Closure")
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

/// The article a message writes before `name`: "an" before a vowel, as in "an `Environment`", and "a" otherwise.
#[must_use]
pub(crate) fn article(name: &str) -> &'static str {
    if name.starts_with(['A', 'E', 'I', 'O', 'U', 'a', 'e', 'i', 'o', 'u']) { "an" } else { "a" }
}

/// [`article`] at the start of a sentence: "An" before a vowel, and "A" otherwise.
#[must_use]
pub(crate) fn sentence_article(name: &str) -> &'static str {
    if article(name) == "an" { "An" } else { "A" }
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

/// Returns the PHP# collection type, as in `Map<string, int>`, that `object` stands for when it is one of the classes
/// [`sharp_collection_name`] names. The analyzer checks a method call on a `List`, `Map`, `Set` or `Iterable` against
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

/// The name an issue gives the PHP# extension member `metadata`, spec section 26: the member on the type it extends,
/// with no type arguments, as a call of it is written, such as `string.contains`, `List.firstOr` for an extension on
/// `List<T>` or `Class.attributes` for one on `Class<T>`, never on the static class that declares it.
#[must_use]
pub(crate) fn display_extension_member<A>(context: &Context<'_, '_, A>, metadata: &FunctionLikeMetadata) -> String
where
    A: Arena,
{
    let Some(receiver) = metadata.parameters.first().and_then(|receiver| receiver.get_type_metadata()) else {
        unreachable!("a call reaches an extension member only when its receiver has a type the value fits");
    };
    let extended = match receiver.type_union.types.as_ref() {
        [atomic] => match get_sharp_type(atomic) {
            Some(SharpType::List(_)) => "List".to_owned(),
            Some(SharpType::Set(_)) => "Set".to_owned(),
            Some(SharpType::Map(..)) => "Map".to_owned(),
            Some(SharpType::Iterable(_)) => "Iterable".to_owned(),
            Some(SharpType::ClassValue(_)) => "Class".to_owned(),
            Some(SharpType::Class(name, _)) => display_class_like_name(context, name).to_string(),
            _ => display_type(context, &receiver.type_union),
        },
        _ => display_type(context, &receiver.type_union),
    };

    format!("{extended}.{}", metadata.original_name)
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

/// Returns `union` as PHP# writes the type: `List<int>`, `Map<string, int>`, a tuple as `[Money total, int]`, `int?`,
/// `(int|string)?`, `Any?`, a class as [`sharp_class_like_name`] names it, a type parameter by its name, an
/// intersection as `A & B`, a function type as
/// `Function<void(int)>` or `Function<Charge(Cart) uses Http>` with each effect named as a class is, `Class<Order>`,
/// `Object`, `Iterable<int>`, and a literal as `1` or `"text"`. A refinement that
/// PHP# cannot write is the type that holds it, such as `int` for a `positive-int` and `int|string` for an
/// `array-key`, and is named once, as `Any` is for a PHP docblock's `non-empty-mixed`. `numeric`, `scalar` and `never`
/// have no PHP# name and keep Mago's.
#[must_use]
pub(crate) fn display_sharp_type<A>(context: &Context<'_, '_, A>, union: &TUnion) -> String
where
    A: Arena,
{
    display_sharp_union(context, union, false)
}

/// Returns `union` as a PHP# parameter writes it: as [`display_sharp_type`] does, with a pure function type written
/// `Function<int(int) uses none>`, since a parameter's function type without `uses` takes on each caller's effects,
/// spec section 29.
#[must_use]
pub(crate) fn display_sharp_parameter_type<A>(context: &Context<'_, '_, A>, union: &TUnion) -> String
where
    A: Arena,
{
    display_sharp_union(context, union, true)
}

/// Spec section 29: what a message says cannot hold a function value with an effect `declared`'s function type does
/// not list, with each type as `display` writes it: "a pure `Function<int(int)>`", "a `Function<int(int) uses Http>`",
/// or, for a union, "the pure `Function<int(int)>` of `int|Function<int(int)>`".
#[must_use]
pub(crate) fn display_function_holder(declared: &TUnion, display: &dyn Fn(&TUnion) -> String) -> String {
    let function = declared.types.iter().find_map(|atomic| match atomic {
        TAtomic::Callable(TCallable::Signature(signature)) => {
            signature.effects.as_ref().map(|effects| (atomic, effects.is_empty()))
        }
        _ => None,
    });
    let pure = if function.is_some_and(|(_, pure)| pure) { "pure " } else { "" };
    let members = declared.types.iter().filter(|atomic| !atomic.is_null()).count();

    match function {
        Some((function, _)) if members > 1 => {
            format!("the {pure}`{}` of `{}`", display(&TUnion::from_atomic(function.clone())), display(declared))
        }
        _ => format!("a {pure}`{}`", display(declared)),
    }
}

/// Returns `union` as [`display_sharp_type`] does, with each pure function type of its own written `uses none` when
/// `parameter` holds, as [`display_sharp_parameter_type`] writes it.
fn display_sharp_union<A>(context: &Context<'_, '_, A>, union: &TUnion, parameter: bool) -> String
where
    A: Arena,
{
    if let Some(TAtomic::Mixed(mixed)) = union.types.iter().find(|atomic| atomic.is_mixed()) {
        return if mixed.is_truthy() || mixed.is_non_null() { "Any" } else { "Any?" }.to_owned();
    }

    let mut parts: Vec<String> = Vec::new();
    for atomic in union.types.iter().filter(|atomic| !atomic.is_null()) {
        let written = match atomic {
            TAtomic::Scalar(TScalar::ArrayKey) => vec!["int".to_owned(), "string".to_owned()],
            atomic => vec![display_sharp_atomic(context, atomic, parameter)],
        };
        for part in written {
            if !parts.contains(&part) {
                parts.push(part);
            }
        }
    }

    // An intersection that holds null is written in parentheses, as a union is, spec section 24.
    let is_intersection = union.types.iter().any(TAtomic::has_intersection_types);
    match (union.has_null(), parts.as_slice()) {
        (false, _) => parts.join("|"),
        (true, []) => "null".to_owned(),
        (true, [part]) if !is_intersection => format!("{part}?"),
        (true, _) => format!("({})?", parts.join("|")),
    }
}

fn display_sharp_atomic<A>(context: &Context<'_, '_, A>, atomic: &TAtomic, parameter: bool) -> String
where
    A: Arena,
{
    let written = match atomic {
        // Spec section 30: a tuple as it is written, each element's type and its name when it has one.
        TAtomic::Array(TArray::List(list)) if let Some(elements) = list.tuple_elements() => {
            let elements: Vec<String> = elements
                .into_iter()
                .map(|(element, name)| match name {
                    Some(name) => format!("{} {name}", display_sharp_type(context, element)),
                    None => display_sharp_type(context, element),
                })
                .collect();

            format!("[{}]", elements.join(", "))
        }
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
        TAtomic::Iterable(iterable) if iterable.get_key_type().is_mixed() => {
            format!("Iterable<{}>", display_sharp_type(context, iterable.get_value_type()))
        }
        TAtomic::Iterable(iterable) => format!(
            "iterable<{}, {}>",
            display_sharp_type(context, iterable.get_key_type()),
            display_sharp_type(context, iterable.get_value_type())
        ),
        TAtomic::Object(TObject::Any) => "Object".to_owned(),
        TAtomic::Object(object) => {
            let Some(name) = object.get_name() else {
                return atomic.get_id().to_string();
            };
            let name =
                sharp_class_like_name(context.codebase, &context.imported_names, &context.short_name_counts, name);
            let readonly = if matches!(object, TObject::Named(named) if named.is_readonly) { "readonly " } else { "" };
            match object.get_type_parameters() {
                Some(parameters) if !parameters.is_empty() => {
                    let parameters: Vec<String> =
                        parameters.iter().map(|parameter| display_sharp_type(context, parameter)).collect();
                    // Spec section 31: a case of a generic enum writes the enum's type arguments, as in
                    // `Result<Order, PaymentError>.Ok`.
                    let case_of = object
                        .get_name()
                        .and_then(|name| context.codebase.get_class_like(name.as_bytes()))
                        .and_then(|class| class.direct_parent_class.filter(|_| class.flags.is_enum_case_class()));
                    match case_of {
                        Some(r#enum) => format!(
                            "{readonly}{}<{}>.{name}",
                            sharp_class_like_name(
                                context.codebase,
                                &context.imported_names,
                                &context.short_name_counts,
                                r#enum
                            ),
                            parameters.join(", ")
                        ),
                        None => format!("{readonly}{name}<{}>", parameters.join(", ")),
                    }
                }
                _ => format!("{readonly}{name}"),
            }
        }
        TAtomic::Reference(TReference::Symbol { name, is_readonly, .. }) => {
            let readonly = if *is_readonly { "readonly " } else { "" };
            let name =
                sharp_class_like_name(context.codebase, &context.imported_names, &context.short_name_counts, *name);

            format!("{readonly}{name}")
        }
        TAtomic::GenericParameter(parameter) if atomic.is_readonly_value() => {
            format!("readonly {}", parameter.parameter_name)
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
                    sharp_effect_name(context.codebase, &context.imported_names, &context.short_name_counts, *effect)
                })
                .collect();
            let uses = match (effects.is_empty(), parameter && signature.effects.is_some()) {
                (false, _) => format!(" uses {}", effects.join(", ")),
                (true, true) => " uses none".to_owned(),
                (true, false) => String::new(),
            };

            format!("Function<{}({}){uses}>", written(signature.get_return_type()), parameters.join(", "))
        }
        TAtomic::Scalar(TScalar::ClassLikeString(class_string)) => match class_string {
            TClassLikeString::Literal { value } => format!(
                "Class<{}>",
                sharp_class_like_name(context.codebase, &context.imported_names, &context.short_name_counts, *value)
            ),
            TClassLikeString::OfType { constraint, .. } => {
                format!("Class<{}>", display_sharp_type(context, constraint))
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
            .chain(
                intersection_types
                    .iter()
                    .map(|intersection_type| display_sharp_atomic(context, intersection_type, parameter)),
            )
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

/// The code a spec section 13 message names as the one that writes, passes or calls through a readonly value: the
/// method or accessor `function` runs, by the name it declares, `operator ==` for an operator, or `This lambda`.
#[must_use]
pub(crate) fn readonly_writer(function: Option<&FunctionLikeMetadata>) -> String {
    let Some(function) = function else {
        unreachable!("semantics refuses a statement at file level in a PHP# file, so only a body is analyzed");
    };

    if matches!(function.kind, FunctionLikeKind::Closure | FunctionLikeKind::ArrowFunction) {
        "This lambda".to_owned()
    } else if function.flags.is_operator()
        && let Some(symbol) = sharp_operator_symbol(function.original_name.as_bytes())
    {
        format!("operator {symbol}")
    } else {
        function.original_name.to_string()
    }
}

/// A PHP# method's declaration up to its parameters, as the analyzed file writes it: `totalIn(string currency)`.
#[must_use]
pub(crate) fn sharp_method_declaration<A>(context: &Context<'_, '_, A>, method: &FunctionLikeMetadata) -> String
where
    A: Arena,
{
    let parameters: Vec<String> = method
        .parameters
        .iter()
        .map(|parameter| {
            let name = parameter.get_name().0.as_bytes();
            let name = String::from_utf8_lossy(name.strip_prefix(b"$").unwrap_or(name));
            // A parameter written without its type, a parse error, shows as written.
            let declared = parameter
                .get_type_declaration_metadata()
                .map(|declared| format!("{} ", display_sharp_type(context, &declared.type_union)));

            format!("{}{name}", declared.unwrap_or_default())
        })
        .collect();

    format!("{}({})", method.original_name, parameters.join(", "))
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

/// The case of an enum with data that the class `class` runs as, written through its enum as code the analyzed file
/// writes, as `PaymentResult.Paid`. `None` for any other class.
#[must_use]
pub(crate) fn display_enum_case<A>(context: &Context<'_, '_, A>, class: &ClassLikeMetadata) -> Option<String>
where
    A: Arena,
{
    let enum_name = class.direct_parent_class.filter(|_| class.flags.is_enum_case_class())?;
    let r#enum = context.codebase.get_class_like(enum_name.as_bytes())?;
    let case = class.original_name.as_bytes().rsplit(|&byte| byte == b'\\').next().unwrap_or_default();

    Some(display_code_member(context, r#enum.original_name, word(case)))
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

/// The names `names` as an English list, each in backticks: "`A`", "`A` and `B`", "`A`, `B` and `C`".
#[must_use]
pub(crate) fn and_list(names: &[Word]) -> String {
    let names: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();

    match names.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
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

/// Returns `List`, `Map`, `Set` or `Iterable` when `object` is `Sharp\Internal\ListMethods`,
/// `Sharp\Internal\MapMethods`, `Sharp\Internal\SetMethods` or `Sharp\Internal\IterableMethods`.
#[must_use]
pub(crate) fn sharp_collection_name(object: &TObject) -> Option<&'static str> {
    let name = object.get_name()?;

    if name.as_bytes().eq_ignore_ascii_case(b"Sharp\\Internal\\ListMethods") {
        Some("List")
    } else if name.as_bytes().eq_ignore_ascii_case(b"Sharp\\Internal\\MapMethods") {
        Some("Map")
    } else if name.as_bytes().eq_ignore_ascii_case(b"Sharp\\Internal\\SetMethods") {
        Some("Set")
    } else if name.as_bytes().eq_ignore_ascii_case(b"Sharp\\Internal\\IterableMethods") {
        Some("Iterable")
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
/// `Order.total` in a `.sharp` file, and a namespace's function `Vendor\Pdf\render` reads `Pdf.render` there.
#[must_use]
pub(crate) fn display_function_like_identifier<A>(
    context: &Context<'_, '_, A>,
    identifier: &FunctionLikeIdentifier,
) -> String
where
    A: Arena,
{
    match identifier {
        FunctionLikeIdentifier::Function(name) => {
            let name = display_function_name(context, *name);
            // A `.sharp` file calls a plain PHP function through its namespace, spec section 23.
            match name.as_bytes().iter().rposition(|byte| *byte == b'\\') {
                Some(separator) if context.dialect.is_sharp() => display_member(
                    context,
                    display_class_like_name(context, word(&name.as_bytes()[..separator])),
                    String::from_utf8_lossy(&name.as_bytes()[separator + 1..]),
                ),
                None if context.dialect.is_sharp() => format!("global.{name}"),
                _ => name.to_string(),
            }
        }
        FunctionLikeIdentifier::Method(class_name, method_name) => display_member(
            context,
            display_class_like_name(context, *class_name),
            display_method_name(context, *class_name, *method_name),
        ),
        FunctionLikeIdentifier::Closure(name) => name.to_string(),
    }
}

/// The plain PHP function `member` of the namespace `namespace`, as PHP# calls one: `Pdf.render(html)` after
/// `import Vendor.Pdf;`, spec section 23. The caller reads a name that is also a class's as the class.
#[must_use]
pub(crate) fn namespace_function(
    codebase: &CodebaseMetadata,
    namespace: Word,
    member: Word,
) -> Option<&FunctionLikeMetadata> {
    codebase.get_function(&[namespace.as_bytes(), b"\\", member.as_bytes()].concat())
}

/// Whether `name`, which resolves to `full_name`, names a namespace a file imports, as `Pdf` does after
/// `import Vendor.Pdf;` when no class is named `Vendor\Pdf`. Its member is then one of the namespace's functions,
/// spec section 23, whether or not that function exists.
#[must_use]
pub(crate) fn is_imported_namespace<T>(
    resolved_names: &ResolvedNames<'_>,
    codebase: &CodebaseMetadata,
    name: &T,
    full_name: Word,
) -> bool
where
    T: HasPosition,
{
    resolved_names.is_imported(name) && !codebase.class_like_exists(full_name.as_bytes())
}

/// The namespace whose plain PHP function or constant the PHP# `name.member` names, spec section 23: PHP's root
/// namespace, the empty word, for `global`, and the namespace `name` resolves to when the file imports it, or when
/// that namespace has a function or a constant `member`, as long as no class has the namespace's name. None when
/// `name` is a class.
#[must_use]
pub(crate) fn member_namespace(
    resolved_names: &ResolvedNames<'_>,
    codebase: &CodebaseMetadata,
    name: &Identifier<'_>,
    member: &[u8],
) -> Option<Word> {
    if name.value() == b"global" {
        return Some(word(""));
    }

    let namespace = word(resolved_names.get(name));
    let full_name = namespace_member(namespace, member);
    let names_member = codebase.function_exists(full_name.as_bytes()) || codebase.constant_exists(full_name.as_bytes());

    ((is_imported_namespace(resolved_names, codebase, name, namespace) || names_member)
        && !codebase.class_like_exists(namespace.as_bytes()))
    .then_some(namespace)
}

/// The plain PHP function a PHP# `Name.member(…)` calls, spec section 23: `global.now()` calls the function `now` of
/// PHP's root namespace, and `Pdf.render(html)` after `import Vendor.Pdf;` the function `render` of that namespace,
/// where no class is named `Vendor\Pdf`. An imported namespace names its function whether or not the function exists,
/// so the call of a missing one is refused as a missing function. None for any other call, and for every call in a
/// PHP file.
#[must_use]
pub(crate) fn namespace_function_name(
    resolved_names: &ResolvedNames<'_>,
    codebase: &CodebaseMetadata,
    call: &MethodCall<'_>,
) -> Option<Word> {
    let name = resolved_names.static_call_class(call)?;
    let ClassLikeMemberSelector::Identifier(member) = &call.method else {
        return None;
    };
    let namespace = member_namespace(resolved_names, codebase, &name.name, member.value)?;

    Some(namespace_member(namespace, member.value))
}

/// The full name of the function or constant `member` of `namespace`, which [`member_namespace`] found: the bare
/// name in PHP's root namespace.
#[must_use]
pub(crate) fn namespace_member(namespace: Word, member: &[u8]) -> Word {
    if namespace.is_empty() { word(member) } else { word([namespace.as_bytes(), b"\\", member].concat()) }
}
