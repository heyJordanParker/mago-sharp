use std::borrow::Cow;

use mago_syntax::dialect::Dialect;

use crate::metadata::CodebaseMetadata;
use crate::ttype::atomic::TAtomic;
use crate::ttype::comparator::ComparisonResult;
use crate::ttype::comparator::generic_comparator::update_failed_result_from_nested;
use crate::ttype::comparator::union_comparator;
use crate::ttype::get_arraykey;
use crate::ttype::get_backing_key_type;
use crate::ttype::get_iterable_parameters;

pub fn is_contained_by(
    codebase: &CodebaseMetadata,
    input_type_part: &TAtomic,
    container_type_part: &TAtomic,
    inside_assertion: bool,
    atomic_comparison_result: &mut ComparisonResult,
) -> bool {
    let Some((container_k, container_v)) = get_iterable_parameters(container_type_part, codebase) else {
        return false;
    };

    let Some((mut input_k, input_v)) = get_iterable_parameters(input_type_part, codebase) else {
        return false;
    };

    // `iterable<V>` desugars to `iterable<mixed, V>` whose *array* arm is
    // `array<array-key, V>` (PHP arrays can only hold array-key keys).
    if matches!(input_type_part, TAtomic::Iterable(_))
        && matches!(container_type_part, TAtomic::Array(_))
        && input_k.is_mixed()
    {
        input_k = get_arraykey();
    }

    // A PHP# `Map` keyed by a backed enum is also an iterable of the backing values, which plain PHP receives.
    let dialect = if atomic_comparison_result.sharp_rules { Dialect::Sharp } else { Dialect::Php };
    if let Cow::Owned(backing_k) = get_backing_key_type(&input_k, codebase, dialect)
        && !union_comparator::is_contained_by(
            codebase,
            &input_k,
            &container_k,
            false,
            input_k.ignore_falsable_issues(),
            inside_assertion,
            &mut atomic_comparison_result.nested(),
        )
    {
        input_k = backing_k;
    }

    let mut all_types_contain = true;

    for (input, container) in [(&input_k, &container_k), (&input_v, &container_v)] {
        let mut nested_comparison_result = atomic_comparison_result.nested();
        if !union_comparator::is_contained_by(
            codebase,
            input,
            container,
            false,
            input.ignore_falsable_issues(),
            inside_assertion,
            &mut nested_comparison_result,
        ) {
            all_types_contain = false;

            update_failed_result_from_nested(atomic_comparison_result, &nested_comparison_result);
        }
    }

    all_types_contain
}
