//! The PHP# form of a bare call of a PHP built-in, read from its declaration and never from a table of the converter's
//! own.
//!
//! The prelude's stub of a type test, whose return is `($value is T ? true : false)`, as `is_string`'s is, gives
//! `value is T` (spec section 21). Every other built-in reaches PHP# through a standard library member, which this
//! slice doesn't convert, so its call keeps the file plain PHP.

use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::union::TUnion;

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
