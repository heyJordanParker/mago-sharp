//! The PHP# form of a bare call of a PHP built-in, read from declarations and never from a table of the converter's
//! own (decision 4).
//!
//! The prelude's stub of a type test, whose return is `($value is T ? true : false)`, as `is_string`'s is, gives
//! `value is T` (spec section 21). Every other built-in reaches PHP# only through a standard library member whose body
//! is one call of that function with PHP's meaning, as `count()` of a `List` is `global.count(items)`: the bridge's
//! inline forms say which function each member's body calls, and what it passes it.
//!
//! A plain PHP library's function, constant or class of PHP's root namespace is reached through the namespace an
//! `extern namespace` line gives its folder (spec section 23), and the library also says which namespace that is.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use mago_casing::to_pascal_case;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::function_like::FunctionLikeKind;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator::is_contained_by_with_erased_template_arguments;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::union::TUnion;
use mago_composer::ComposerPackage;
use mago_sharp_bridge::InlineForms;
use mago_sharp_bridge::PhpCall;
use mago_sharp_bridge::PhpCallArgument;
use mago_word::Word;

/// What the converter reads of the project's libraries.
///
/// It holds the standard library's members whose body is one call of a PHP function, by that function, and its
/// constants whose value is one of PHP's own, by that constant. It also holds the namespace each folder of a plain PHP
/// library's names of PHP's root namespace takes.
#[derive(Debug, Default)]
pub struct Library {
    /// Each function's members, by its lowercase name, as PHP matches a function's name.
    members: HashMap<Vec<u8>, Vec<Member>>,
    /// The class and the name of the constant whose value is each of PHP's own constants, as `Int.MAX` is
    /// `PHP_INT_MAX`'s, by PHP's name, which PHP matches in its case.
    constants: HashMap<Vec<u8>, (Word, Word)>,
    /// The namespace, as PHP writes it, that an `extern namespace` line the converter writes gives each folder, as
    /// [`CodebaseMetadata::folder`] names it, that holds a function, constant or class of PHP's root namespace.
    folder_namespaces: HashMap<Word, String>,
}

/// A standard library member whose body is one call of a PHP function.
#[derive(Debug, Clone)]
pub struct Member {
    /// The class that declares it, as its declaration writes it.
    pub class: Word,
    pub name: Word,
    pub shape: Shape,
    /// The function its body calls, and what it passes it.
    pub call: PhpCall,
    /// How many of its slots it reads in the order a call writes them, the receiver first.
    pub in_order: usize,
}

/// How a call writes a member: `Class.name(…)`, `receiver.name(…)` for an extension method, or `receiver.name` for an
/// extension property (spec section 26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Static,
    Method,
    Property,
}

impl Library {
    /// The library's members among `forms` whose body is one call of a PHP function, with their declarations from
    /// `codebase`. A member that runs on an object of its own class takes no PHP value as that object, so it is none.
    /// Each folder's namespace is read from `codebase` and the `composer.json` files above the folder in `workspace`.
    #[must_use]
    pub fn new(forms: &InlineForms, codebase: &CodebaseMetadata, workspace: &Path) -> Self {
        let mut members: HashMap<Vec<u8>, Vec<Member>> = HashMap::new();
        for (class, member, form) in forms.iter() {
            let (Some(call), Some(declaring), Some(method)) =
                (form.php_call(), codebase.get_class_like(class), codebase.get_method(class, member))
            else {
                continue;
            };
            let shape = if method.flags.is_extension_property() {
                Shape::Property
            } else if method.flags.is_extension_method() {
                Shape::Method
            } else if method.method_metadata.as_ref().is_some_and(|metadata| metadata.is_static) {
                Shape::Static
            } else {
                continue;
            };
            members.entry(call.function.as_bytes().to_ascii_lowercase()).or_default().push(Member {
                class: declaring.original_name,
                name: method.original_name,
                shape,
                call: call.clone(),
                in_order: form.in_order(),
            });
        }
        for found in members.values_mut() {
            found.sort_by(|a, b| (a.class.as_bytes(), a.name.as_bytes()).cmp(&(b.class.as_bytes(), b.name.as_bytes())));
        }
        let constants = codebase
            .class_likes
            .values()
            .flat_map(|class| {
                class.constants.values().filter_map(|constant| {
                    constant.php_constant().map(|php| (php.as_bytes().to_vec(), (class.original_name, constant.name)))
                })
            })
            .collect();

        Self { members, constants, folder_namespaces: folder_namespaces(codebase, workspace) }
    }

    /// The namespace, as PHP writes it, that the converter gives `folder`'s functions, constants and classes of PHP's
    /// root namespace, or `None` when the folder's classes have no namespace and no Composer package names it.
    #[must_use]
    pub fn folder_namespace(&self, folder: Word) -> Option<&str> {
        self.folder_namespaces.get(&folder).map(String::as_str)
    }

    /// The members whose body calls `function`, a PHP function's name in any case.
    #[must_use]
    pub fn members(&self, function: &[u8]) -> &[Member] {
        self.members.get(&function.to_ascii_lowercase()).map_or(&[], Vec::as_slice)
    }

    /// The class and the name of the constant whose value is PHP's own constant `constant`.
    #[must_use]
    pub fn constant(&self, constant: &[u8]) -> Option<(Word, Word)> {
        self.constants.get(constant).copied()
    }
}

/// How `member` runs `function` with another meaning than PHP's, whatever the call passes: its return holds less than
/// the function's, a conditional return counted by both branches, so a `false` failure value is no form, or it feeds a
/// by-reference parameter of the function, but the `ref` receiver of an extension member (spec section 26).
#[must_use]
pub fn differs(member: &Member, function: &FunctionLikeMetadata, codebase: &CodebaseMetadata) -> Option<String> {
    let method = codebase.get_method(member.class.as_bytes(), member.name.as_bytes())?;
    let returned = |metadata: &Option<mago_codex::metadata::ttype::TypeMetadata>| {
        metadata.as_ref().map_or_else(get_mixed, |metadata| metadata.type_union.clone())
    };
    let (theirs, ours) = (returned(&function.return_type_metadata), returned(&method.return_type_metadata));
    for branch in branches(&theirs) {
        if !is_contained_by_with_erased_template_arguments(codebase, &branch, &ours, &mut ComparisonResult::new()) {
            return Some(format!("returns `{}`, where PHP's returns `{}`", ours.get_id(), branch.get_id()));
        }
    }

    let receiver = usize::from(member.shape != Shape::Static);
    member.call.arguments.iter().zip(&function.parameters).find_map(|(argument, parameter)| match argument {
        PhpCallArgument::Slot(slot) if parameter.flags.is_by_reference() && *slot >= receiver => {
            Some(format!("passes a value to `{}`, which PHP takes by reference", parameter.get_name().0.as_str_lossy()))
        }
        _ => None,
    })
}

/// The namespace each folder that holds a plain PHP function, constant or class of PHP's root namespace takes: the
/// first, in alphabetical order, of the namespaces of the folder's classes, as Laravel's `abort()` in
/// `Illuminate/Foundation/helpers.php` takes `Illuminate\Foundation`, or, when no class of the folder has one, the
/// namespace of the Composer package that holds the folder.
fn folder_namespaces(codebase: &CodebaseMetadata, workspace: &Path) -> HashMap<Word, String> {
    let mut beside: HashMap<Word, String> = HashMap::new();
    let mut roots: HashSet<Word> = HashSet::new();
    for class in codebase.class_likes.values().filter(|class| !class.flags.is_module()) {
        let Some(folder) = codebase.folder(class.span.file_id) else {
            continue;
        };
        let name = class.original_name.as_str_lossy();
        match name.rsplit_once('\\') {
            Some((namespace, _)) => {
                let first = beside.entry(folder).or_insert_with(|| namespace.to_owned());
                if namespace.to_ascii_lowercase() < first.to_ascii_lowercase() {
                    namespace.clone_into(first);
                }
            }
            None if !class.flags.is_sharp() => {
                roots.insert(folder);
            }
            None => {}
        }
    }
    let functions = codebase
        .function_likes
        .values()
        .filter(|function| function.kind == FunctionLikeKind::Function)
        .map(|function| (function.original_name, function.span));
    let constants = codebase.constants.values().map(|constant| (constant.name, constant.span));
    for (name, span) in functions.chain(constants) {
        if !name.as_bytes().contains(&b'\\')
            && let Some(folder) = codebase.folder(span.file_id)
        {
            roots.insert(folder);
        }
    }

    roots
        .into_iter()
        .filter_map(|folder| {
            let namespace =
                beside.get(&folder).cloned().or_else(|| package_namespace(workspace, &folder.as_str_lossy()))?;

            Some((folder, namespace))
        })
        .collect()
}

/// The namespace of the Composer package whose `composer.json` is the nearest one at or above `folder`, and no higher
/// than `workspace` for a folder in it: its name's, as `Acme\StringHelpers` for `acme/string-helpers`, or, for a
/// `composer.json` that names no package, as a project's may not, its first PSR-4 namespace in alphabetical order.
fn package_namespace(workspace: &Path, folder: &str) -> Option<String> {
    let folder = workspace.join(folder);
    let inside = folder.starts_with(workspace);
    let contents = folder
        .ancestors()
        .take_while(|at| !inside || at.starts_with(workspace))
        .find_map(|at| std::fs::read_to_string(at.join("composer.json")).ok())?;
    let package: ComposerPackage = contents.parse().ok()?;
    if let Some(name) = package.name {
        let parts: Vec<String> =
            name.0.split('/').map(|part| String::from_utf8_lossy(&to_pascal_case(part)).into_owned()).collect();

        return Some(parts.join("\\"));
    }

    package
        .autoload?
        .psr_4
        .into_keys()
        .map(|prefix| prefix.trim_matches('\\').to_owned())
        .filter(|prefix| !prefix.is_empty())
        .min()
}

/// The last part of a full PHP name.
#[must_use]
pub fn short(name: &str) -> &str {
    name.rsplit('\\').next().unwrap_or(name)
}

/// Each type `union` may be: a conditional type's two branches, as a return of `($a is int ? int : float)` is
/// either, and `union` itself otherwise.
fn branches(union: &TUnion) -> Vec<TUnion> {
    match union.types.as_ref() {
        [TAtomic::Conditional(conditional)] => {
            branches(&conditional.then).into_iter().chain(branches(&conditional.otherwise)).collect()
        }
        _ => vec![union.clone()],
    }
}

/// The type the prelude's type test `function` tests for: one parameter, and a return of `($value is T ? true :
/// false)`.
pub fn type_test(function: &FunctionLikeMetadata) -> Option<&TUnion> {
    if function.parameters.len() != 1 {
        return None;
    }
    let returned = &function.return_type_metadata.as_ref()?.type_union;
    let [TAtomic::Conditional(conditional)] = returned.types.as_ref() else {
        return None;
    };
    let is_literal = |union: &TUnion, value: bool| matches!(union.types.as_ref(), [TAtomic::Scalar(TScalar::Bool(boolean))] if boolean.value == Some(value));
    if conditional.negated || !is_literal(&conditional.then, true) || !is_literal(&conditional.otherwise, false) {
        return None;
    }

    Some(&conditional.target)
}
