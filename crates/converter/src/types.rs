//! Writes an analysis type in PHP#, with the file's own names.
//!
//! A class is written by the name the file imports it under, by its short name in the file's own namespace, bare as a
//! root class of `Builtins.sharp`, or by a new `import` line the conversion adds. A type PHP# can't write, as
//! `get_sharp_type` reads it, is the reason the file is refused.

use std::collections::BTreeMap;

use foldhash::HashMap;

use mago_codex::metadata::CodebaseMetadata;
use mago_codex::ttype::SharpType;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::atomic::scalar::int::TInteger;
use mago_codex::ttype::atomic::scalar::string::TStringCasing;
use mago_codex::ttype::get_sharp_type;
use mago_codex::ttype::union::TUnion;
use mago_names::EQUATABLE;

use crate::library::short;
use mago_names::STANDARD_ROOT_CLASSES;
use mago_word::Word;

/// The names a file reaches classes and library namespaces by: its namespace, its `import` lines, and the imports the
/// conversion adds.
#[derive(Debug)]
pub struct Imports {
    namespace: String,
    /// Each name the file's imports take, lowercase, with the name as written and the full PHP name it stands for.
    taken: HashMap<String, (String, String)>,
    /// Each import the conversion adds, by the name it takes.
    added: BTreeMap<String, String>,
}

impl Imports {
    pub fn new(namespace: &str) -> Self {
        Self { namespace: namespace.trim_matches('\\').to_owned(), taken: HashMap::default(), added: BTreeMap::new() }
    }

    /// Records that the file names `name`, a full PHP name, as `alias`, or, when PHP# reserves `alias` for a type, as
    /// its namespace's last part before it, as `AttributesAny` for `Attributes\Any`.
    pub fn add_use(&mut self, alias: &str, name: &str) {
        let name = name.trim_start_matches('\\');
        let alias = match name.rsplit('\\').nth(1) {
            Some(namespace) if reserved(alias) => format!("{namespace}{alias}"),
            _ => alias.to_owned(),
        };
        self.taken.insert(alias.to_ascii_lowercase(), (alias, name.to_owned()));
    }

    /// Whether `class`, a full PHP name, is one a `.sharp` file names bare with no import: a root class of
    /// `Builtins.sharp`, as `Throwable` and `Attribute` (spec section 23), or a class declared directly in `Sharp`.
    pub fn is_default(class: &str) -> bool {
        let class = class.trim_start_matches('\\');
        STANDARD_ROOT_CLASSES.iter().any(|root| root.eq_ignore_ascii_case(class))
            || class.rsplit_once('\\').is_some_and(|(namespace, _)| namespace == "Sharp")
    }

    /// The imports the conversion adds, each as its `import` line writes it, as `Proof.Spec` or `Cache.Key as CacheKey`.
    pub fn added(&self) -> impl Iterator<Item = String> + '_ {
        self.added.iter().map(|(alias, name)| {
            let dotted = name.replace('\\', ".");
            if short(name) == alias { dotted } else { format!("{dotted} as {alias}") }
        })
    }

    /// The name this file writes for `class`, a full PHP name, or the reason no name reaches it.
    pub fn name_of(&mut self, class: &str) -> Result<String, String> {
        let class = class.trim_start_matches('\\');
        let (namespace, short) = class.rsplit_once('\\').unwrap_or(("", class));
        let free = !self.taken.contains_key(&short.to_ascii_lowercase()) && !reserved(short);
        if let Some((alias, _)) = self.taken.values().find(|(_, taken)| taken.eq_ignore_ascii_case(class)) {
            return Ok(alias.clone());
        }
        if free && (namespace.eq_ignore_ascii_case(&self.namespace) || Self::is_default(class)) {
            return Ok(short.to_owned());
        }
        if namespace.is_empty() {
            return Err(format!(
                "`{class}` is a class of PHP's root namespace, which PHP# names only in the standard library (spec section 23)"
            ));
        }

        Ok(self.import(class))
    }

    /// The name this file writes for `name`, a class or a library's namespace, importing it under its last part, or
    /// under its namespace's last part before it when another import takes that (spec section 23).
    pub fn import(&mut self, name: &str) -> String {
        let name = name.trim_start_matches('\\');
        if let Some((alias, _)) = self.taken.values().find(|(_, taken)| taken.eq_ignore_ascii_case(name)) {
            return alias.clone();
        }
        let parts: Vec<&str> = name.split('\\').collect();
        let mut alias = short(name).to_owned();
        let mut prefix = parts.len().saturating_sub(1);
        while prefix > 0 && (self.taken.contains_key(&alias.to_ascii_lowercase()) || reserved(&alias)) {
            prefix -= 1;
            alias = format!("{}{alias}", parts[prefix]);
        }
        self.add_use(&alias, name);
        self.added.insert(alias.clone(), name.to_owned());

        alias
    }
}

/// Whether PHP# reserves `name` for a type where a plain PHP class may take it: `Any`, PHP#'s `mixed`, `Equatable`,
/// its built-in bound (spec section 11), and `Error`, its interface of every failure, which a `.sharp` file's `Error`
/// always names (spec section 31). PHP reserves every other type name itself.
fn reserved(name: &str) -> bool {
    ["Any", EQUATABLE, "Error"].iter().any(|reserved| reserved.eq_ignore_ascii_case(name))
}

/// `union` in PHP#'s spelling, or the reason PHP# can't write it.
pub fn type_text(union: &TUnion, codebase: &CodebaseMetadata, imports: &mut Imports) -> Result<String, String> {
    if union.types.iter().any(TAtomic::is_mixed) {
        let non_null = union.types.iter().all(|atomic| matches!(atomic, TAtomic::Mixed(mixed) if mixed.is_non_null()));
        return Ok(if non_null { "Any" } else { "Any?" }.to_owned());
    }

    let mut members = Vec::new();
    let mut collections = Vec::new();
    for atomic in union.types.iter().filter(|atomic| !atomic.is_null()) {
        let text = atomic_text(atomic, codebase, imports)?;
        if matches!(get_sharp_type(atomic), Some(SharpType::List(_) | SharpType::Set(_) | SharpType::Map(..))) {
            collections.push(text.clone());
        }
        if !members.contains(&text) {
            members.push(text);
        }
    }
    // A loop reads each collection its own way, so a PHP# type holds one (spec section 12).
    if let [first, second, ..] = collections.as_slice() {
        return Err(format!(
            "`{}` holds two collections, `{first}` and `{second}`, and a PHP# type holds one (spec section 12)",
            union.get_id()
        ));
    }

    Ok(match (union.has_null(), members.as_slice()) {
        (_, []) => return Err("`null` alone is no PHP# type".to_owned()),
        (false, [member]) => member.clone(),
        (false, _) => members.join("|"),
        (true, [member]) => format!("{member}?"),
        (true, _) => format!("({})?", members.join("|")),
    })
}

fn atomic_text(atomic: &TAtomic, codebase: &CodebaseMetadata, imports: &mut Imports) -> Result<String, String> {
    if atomic.get_intersection_types().is_some_and(|intersected| !intersected.is_empty()) {
        return Err(format!("the intersection type `{}` is not converted yet", atomic.get_id()));
    }
    // `get_sharp_type` reads a literal as its general type, which would widen `int|false` to `int|bool`.
    let literal = matches!(atomic, TAtomic::Scalar(scalar) if scalar.is_literal_value());
    let Some(sharp_type) = get_sharp_type(atomic).filter(|_| !literal) else {
        return Err(format!("PHP# has no type `{}`", atomic.get_id()));
    };

    Ok(match sharp_type {
        SharpType::Int => "int".to_owned(),
        SharpType::Float => "float".to_owned(),
        SharpType::Bool => "bool".to_owned(),
        SharpType::String => "string".to_owned(),
        SharpType::Void => "void".to_owned(),
        SharpType::Object => "Object".to_owned(),
        // `static` is the class the method runs on, which PHP# writes `Self` (spec section 25).
        SharpType::Class(..) if matches!(atomic, TAtomic::Object(TObject::Named(object)) if object.is_static) => {
            "Self".to_owned()
        }
        SharpType::Enum(name) => class_name(name, codebase, imports)?,
        SharpType::Class(name, arguments) => {
            let class = class_name(name, codebase, imports)?;
            // PHP takes a generic class written alone as any of its kind, and a PHP# type names its type arguments
            // (spec section 11).
            let declared = codebase.get_class_like(name.as_bytes());
            if arguments.is_empty() && declared.is_some_and(|declared| !declared.template_types.is_empty()) {
                let shown = declared.map_or(name, |declared| declared.original_name).as_str_lossy().into_owned();
                return Err(format!(
                    "`{shown}` takes type arguments, which PHP leaves out here and a PHP# type names (spec section 11): give them in PHPDoc, as in `{class}<…>`"
                ));
            }
            if arguments.is_empty() {
                class
            } else {
                let mut written = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    written.push(held(argument, atomic, codebase, imports)?);
                }
                format!("{class}<{}>", written.join(", "))
            }
        }
        SharpType::List(element) => format!("List<{}>", held(element, atomic, codebase, imports)?),
        SharpType::Set(element) => format!("Set<{}>", held(element, atomic, codebase, imports)?),
        SharpType::Map(key, value) => {
            format!("Map<{}, {}>", held(key, atomic, codebase, imports)?, held(value, atomic, codebase, imports)?)
        }
        SharpType::Iterable(value) => {
            let written = format!("Iterable<{}>", held(value, atomic, codebase, imports)?);
            // A PHP# `Iterable` reads a value's elements, and its key is no type it names (spec section 12). PHP's
            // `iterable<V>` keys by any `array-key`, which says nothing either.
            let any_key =
                |key: &TUnion| key.is_mixed() || matches!(key.types.as_ref(), [TAtomic::Scalar(TScalar::ArrayKey)]);
            if let TAtomic::Iterable(iterable) = atomic
                && !any_key(&iterable.key_type)
            {
                return Err(format!(
                    "PHP's `{}` gives its keys, and a PHP# `{written}` reads values only (spec section 12)",
                    atomic.get_id()
                ));
            }
            written
        }
        SharpType::Parameter(parameter) => parameter.parameter_name.to_string(),
        SharpType::ClassValue(TClassLikeString::Literal { value }) => {
            format!("Class<{}>", class_name(*value, codebase, imports)?)
        }
        // A class type names the class itself, a generic one with no type arguments (spec section 11).
        SharpType::ClassValue(TClassLikeString::OfType { constraint, .. }) => match constraint.types.as_ref() {
            [TAtomic::Object(TObject::Named(object))] if object.get_type_parameters().is_none() => {
                format!("Class<{}>", class_name(object.name, codebase, imports)?)
            }
            _ => format!("Class<{}>", held(constraint, atomic, codebase, imports)?),
        },
        // `Function<R(P)>` (spec section 14.1), whose parameters are plain: PHP# passes every argument by value.
        SharpType::Function(signature) => {
            let parameters = signature.get_parameters();
            if parameters
                .iter()
                .any(|parameter| parameter.is_by_reference() || parameter.is_variadic() || parameter.has_default())
            {
                return Err(format!(
                    "`{}` takes a parameter by reference, a variadic one or an optional one, and a PHP# function type lists plain parameters (spec section 14.1)",
                    atomic.get_id()
                ));
            }
            let mut written = Vec::with_capacity(parameters.len());
            for parameter in parameters {
                written.push(match parameter.get_type_signature() {
                    Some(declared) => held(declared, atomic, codebase, imports)?,
                    None => "Any?".to_owned(),
                });
            }
            let returned = match signature.get_return_type() {
                Some(returned) => held(returned, atomic, codebase, imports)?,
                None => "Any?".to_owned(),
            };
            format!("Function<{returned}({})>", written.join(", "))
        }
        // `[Money total, int count]`, each element's name optional (spec section 30).
        SharpType::Tuple(list) => {
            let mut written = Vec::new();
            for (element, name) in list.tuple_elements().unwrap_or_default() {
                let text = held(element, atomic, codebase, imports)?;
                written.push(match name {
                    Some(name) => format!("{text} {name}"),
                    None => text,
                });
            }
            format!("[{}]", written.join(", "))
        }
        SharpType::ClassValue(_) => return Err(format!("the type `{}` is not converted yet", atomic.get_id())),
    })
}

/// The first type `union` holds, as PHPDoc writes it, that narrows the PHP# type [`type_text`] writes for it, as
/// `non-empty-string` narrows `string` and `positive-int` narrows `int`, or `None` when PHP# writes `union` whole.
#[must_use]
pub fn narrowing(union: &TUnion) -> Option<String> {
    union.types.iter().find_map(|atomic| {
        let narrows = match atomic {
            TAtomic::Scalar(TScalar::String(string)) => {
                string.literal.is_some()
                    || string.is_non_empty
                    || string.is_numeric
                    || string.is_truthy
                    || string.is_callable
                    || string.casing != TStringCasing::Unspecified
            }
            TAtomic::Scalar(TScalar::Integer(integer)) => *integer != TInteger::Unspecified,
            TAtomic::Array(TArray::List(list)) => {
                if list.non_empty {
                    true
                } else {
                    return narrowing(&list.element_type);
                }
            }
            TAtomic::Array(TArray::Keyed(keyed)) => {
                if keyed.non_empty {
                    true
                } else {
                    return keyed
                        .parameters
                        .as_ref()
                        .and_then(|(key, value)| narrowing(key).or_else(|| narrowing(value)));
                }
            }
            TAtomic::Object(TObject::Named(object)) => {
                return object.get_type_parameters().unwrap_or_default().iter().find_map(narrowing);
            }
            _ => false,
        };

        narrows.then(|| atomic.get_id().to_string())
    })
}

/// `union`, a type inside `holder`, in PHP#'s spelling, or the reason PHP# can't write it, which names `holder`: a bare
/// `array` is refused for the `array-key` it holds.
fn held(
    union: &TUnion,
    holder: &TAtomic,
    codebase: &CodebaseMetadata,
    imports: &mut Imports,
) -> Result<String, String> {
    type_text(union, codebase, imports).map_err(|reason| format!("{reason}, which `{}` holds", holder.get_id()))
}

fn class_name(name: Word, codebase: &CodebaseMetadata, imports: &mut Imports) -> Result<String, String> {
    let declared = codebase.get_class_like(name.as_bytes()).map_or(name, |class| class.original_name);

    imports.name_of(&declared.as_str_lossy())
}
