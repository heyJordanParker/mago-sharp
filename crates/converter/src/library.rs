//! The PHP# form of a bare call of a PHP built-in, read from declarations and never from a table of the converter's
//! own (decision 4).
//!
//! The prelude's stub of a type test, whose return is `($value is T ? true : false)`, as `is_string`'s is, gives
//! `value is T` (spec section 21). Every other built-in reaches PHP# only through a standard library member whose body
//! is one call of that function with PHP's meaning, as `count()` of a `List` is `global.count(items)`: the bridge's
//! inline forms say which function each member's body calls, and what it passes it.

use std::collections::HashMap;

use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator::is_contained_by_with_erased_template_arguments;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::union::TUnion;
use mago_sharp_bridge::InlineForms;
use mago_sharp_bridge::PhpCall;
use mago_sharp_bridge::PhpCallArgument;
use mago_word::Word;

/// The standard library's members whose body is one call of a PHP function, by that function.
#[derive(Debug, Default)]
pub struct Library {
    /// Each function's members, by its lowercase name, as PHP matches a function's name.
    members: HashMap<Vec<u8>, Vec<Member>>,
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
    #[must_use]
    pub fn new(forms: &InlineForms, codebase: &CodebaseMetadata) -> Self {
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

        Self { members }
    }

    /// The members whose body calls `function`, a PHP function's name in any case.
    #[must_use]
    pub fn members(&self, function: &[u8]) -> &[Member] {
        self.members.get(&function.to_ascii_lowercase()).map_or(&[], Vec::as_slice)
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
