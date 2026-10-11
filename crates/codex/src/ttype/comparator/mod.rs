use mago_word::Word;

use crate::ttype::atomic::TAtomic;
use crate::ttype::atomic::scalar::int::TInteger;
use crate::ttype::template::TemplateBound;
use crate::ttype::union::TUnion;

mod callable_comparator;
mod class_string_comparator;
mod derived_comparator;
mod generic_comparator;
mod integer_comparator;
mod iterable_comparator;
mod resource_comparator;
mod scalar_comparator;

pub(super) mod array_comparator;
pub(super) mod object_comparator;

pub mod atomic_comparator;
pub mod union_comparator;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonResult {
    pub type_coerced: Option<bool>,
    pub type_coerced_from_nested_mixed: Option<bool>,
    pub type_coerced_from_as_mixed: Option<bool>,
    pub replacement_union_type: Option<TUnion>,
    pub replacement_atomic_type: Option<TAtomic>,
    pub type_variable_lower_bounds: Vec<(Word, TemplateBound)>,
    pub type_variable_upper_bounds: Vec<(Word, TemplateBound)>,
    /// Whether the comparison follows PHP#'s rules: a `nonnull` container refuses a value that may be null, as PHP#'s
    /// `Any` does, and a `float` container refuses an `int`. The caller sets it for a `.sharp` file. A PHP file keeps
    /// upstream's rules, which let any `mixed` into `nonnull` and an `int` into a `float`.
    pub sharp_rules: bool,
    /// The `int` a failed comparison refused only because a `float` was expected, under `sharp_rules`.
    pub int_for_float: Option<TInteger>,
}

impl Default for ComparisonResult {
    fn default() -> Self {
        Self::new()
    }
}

impl ComparisonResult {
    #[must_use]
    pub fn new() -> Self {
        Self {
            type_coerced: None,
            type_coerced_from_nested_mixed: None,
            type_coerced_from_as_mixed: None,
            replacement_union_type: None,
            replacement_atomic_type: None,
            type_variable_lower_bounds: vec![],
            type_variable_upper_bounds: vec![],
            sharp_rules: false,
            int_for_float: None,
        }
    }

    /// A result whose comparison follows PHP#'s rules when `sharp_rules` is set.
    #[must_use]
    pub fn with_sharp_rules(sharp_rules: bool) -> Self {
        Self { sharp_rules, ..Self::new() }
    }
}

#[cfg(test)]
mod tests {

    use mago_allocator::LocalArena;
    use std::borrow::Cow;
    use std::collections::HashSet;

    use mago_database::Database;
    use mago_database::DatabaseReader;
    use mago_database::file::File;
    use mago_names::resolver::NameResolver;
    use mago_syntax::parser::parse_file;
    use mago_word::WordSet;
    use mago_word::word;

    use crate::metadata::CodebaseMetadata;
    use crate::populator::populate_codebase;
    use crate::reference::SymbolReferences;
    use crate::scanner::scan_program;
    use crate::ttype::atomic::TAtomic;
    use crate::ttype::atomic::object::TObject;
    use crate::ttype::comparator::ComparisonResult;
    use crate::ttype::comparator::union_comparator::is_contained_by;
    use std::sync::Arc;

    use crate::misc::GenericParent;
    use crate::ttype::atomic::generic::TGenericParameter;
    use crate::ttype::atomic::scalar::int::TInteger;
    use crate::ttype::get_float;
    use crate::ttype::get_int;
    use crate::ttype::get_named_object;
    use crate::ttype::get_null;
    use crate::ttype::get_string;
    use crate::ttype::union::TUnion;

    pub(crate) fn create_test_codebase(code: &'static str) -> CodebaseMetadata {
        let file = File::ephemeral(Cow::Borrowed(b"code.php"), Cow::Borrowed(code.as_bytes()));
        let config =
            mago_database::DatabaseConfiguration::new(std::path::Path::new("/"), vec![], vec![], vec![], vec![])
                .into_static();
        let database = Database::single(file, config);

        let mut codebase = CodebaseMetadata::new();
        let arena = LocalArena::new();
        for file in database.files() {
            let program = parse_file(&arena, &file);
            assert!(!program.has_errors(), "Parse failed: {:?}", program.errors);
            let resolved_names = NameResolver::new(&arena).resolve(program);
            let program_codebase =
                scan_program(&arena, &file, program, &resolved_names, mago_php_version::PHPVersion::LATEST);

            codebase.extend(program_codebase);
        }

        populate_codebase(&mut codebase, &mut SymbolReferences::new(), WordSet::default(), HashSet::default());

        codebase
    }

    pub(crate) fn assert_is_contained_by(
        codebase: &CodebaseMetadata,
        input: &TUnion,
        container: &TUnion,
        expected: bool,
        comparison_result: &mut ComparisonResult,
    ) {
        let is_contained_by = is_contained_by(codebase, input, container, false, false, false, comparison_result);

        assert_eq!(is_contained_by, expected);
    }

    #[test]
    fn test_order_is_not_important() {
        let code = "
            <?php

            interface DateTimeInterface {}

            class DateTime implements DateTimeInterface {}
        ";

        let codebase = create_test_codebase(code);

        let datetime_interface_type = TUnion::from_vec(vec![TAtomic::Object(TObject::new_named(word("DateTime")))]);
        let datetime_interface_null_type =
            TUnion::from_vec(vec![TAtomic::Object(TObject::new_named(word("DateTimeInterface"))), TAtomic::Null]);
        let null_datetime_interface_type =
            TUnion::from_vec(vec![TAtomic::Null, TAtomic::Object(TObject::new_named(word("DateTimeInterface")))]);

        let mut first_comparison_result = ComparisonResult::new();
        let mut second_comparison_result = ComparisonResult::new();

        let first_is_contained_by = is_contained_by(
            &codebase,
            &datetime_interface_null_type,
            &datetime_interface_type,
            false,
            false,
            false,
            &mut first_comparison_result,
        );

        let second_is_contained_by = is_contained_by(
            &codebase,
            &null_datetime_interface_type,
            &datetime_interface_type,
            false,
            false,
            false,
            &mut second_comparison_result,
        );

        assert!(!first_is_contained_by);
        assert!(!second_is_contained_by);

        assert_eq!(first_comparison_result.type_coerced, Some(true));
        assert_eq!(second_comparison_result.type_coerced, Some(true));

        assert_eq!(first_comparison_result, second_comparison_result);
    }

    #[test]
    fn test_union_order_with_multiple_coercible_types() {
        let code = "
            <?php

            interface A {}
            interface B {}

            class C implements A, B {}
        ";

        let codebase = create_test_codebase(code);

        let c_type = TUnion::from_vec(vec![TAtomic::Object(TObject::new_named(word("C")))]);
        let a_b_type = TUnion::from_vec(vec![
            TAtomic::Object(TObject::new_named(word("A"))),
            TAtomic::Object(TObject::new_named(word("B"))),
        ]);
        let b_a_type = TUnion::from_vec(vec![
            TAtomic::Object(TObject::new_named(word("B"))),
            TAtomic::Object(TObject::new_named(word("A"))),
        ]);

        let mut first_comparison_result = ComparisonResult::new();
        let mut second_comparison_result = ComparisonResult::new();

        let first_is_contained_by =
            is_contained_by(&codebase, &a_b_type, &c_type, false, false, false, &mut first_comparison_result);
        let second_is_contained_by =
            is_contained_by(&codebase, &b_a_type, &c_type, false, false, false, &mut second_comparison_result);

        assert!(!first_is_contained_by);
        assert!(!second_is_contained_by);
        assert_eq!(first_comparison_result.type_coerced, Some(true));
        assert_eq!(second_comparison_result.type_coerced, Some(true));
        assert_eq!(first_comparison_result, second_comparison_result);
    }

    #[test]
    fn test_union_order_with_non_coercible_types() {
        let code = "
            <?php

            class Foo {}
            class Bar {}
        ";

        let codebase = create_test_codebase(code);

        let foo_type = TUnion::from_vec(vec![TAtomic::Object(TObject::new_named(word("Foo")))]);
        let bar_null_type = TUnion::from_vec(vec![TAtomic::Object(TObject::new_named(word("Bar"))), TAtomic::Null]);
        let null_bar_type = TUnion::from_vec(vec![TAtomic::Null, TAtomic::Object(TObject::new_named(word("Bar")))]);

        let mut first_comparison_result = ComparisonResult::new();
        let mut second_comparison_result = ComparisonResult::new();

        let first_is_contained_by =
            is_contained_by(&codebase, &bar_null_type, &foo_type, false, false, false, &mut first_comparison_result);
        let second_is_contained_by =
            is_contained_by(&codebase, &null_bar_type, &foo_type, false, false, false, &mut second_comparison_result);

        assert!(!first_is_contained_by);
        assert!(!second_is_contained_by);
        assert_eq!(first_comparison_result.type_coerced, None);
        assert_eq!(second_comparison_result.type_coerced, None);
        assert_eq!(first_comparison_result, second_comparison_result);
    }

    #[test]
    fn test_union_order_with_mixed_coercion() {
        let code = "
            <?php

            interface ParentInterface {}
            class Child implements ParentInterface {}
            class Unrelated {}
        ";

        let codebase = create_test_codebase(code);

        let child_type = TUnion::from_vec(vec![TAtomic::Object(TObject::new_named(word("Child")))]);
        let parent_unrelated_type = TUnion::from_vec(vec![
            TAtomic::Object(TObject::new_named(word("ParentInterface"))),
            TAtomic::Object(TObject::new_named(word("Unrelated"))),
        ]);
        let unrelated_parent_type = TUnion::from_vec(vec![
            TAtomic::Object(TObject::new_named(word("Unrelated"))),
            TAtomic::Object(TObject::new_named(word("ParentInterface"))),
        ]);

        let mut first_comparison_result = ComparisonResult::new();
        let mut second_comparison_result = ComparisonResult::new();

        let first_is_contained_by = is_contained_by(
            &codebase,
            &parent_unrelated_type,
            &child_type,
            false,
            false,
            false,
            &mut first_comparison_result,
        );
        let second_is_contained_by = is_contained_by(
            &codebase,
            &unrelated_parent_type,
            &child_type,
            false,
            false,
            false,
            &mut second_comparison_result,
        );

        assert!(!first_is_contained_by);
        assert!(!second_is_contained_by);
        assert_eq!(first_comparison_result.type_coerced, Some(true));
        assert_eq!(second_comparison_result.type_coerced, Some(true));
        assert_eq!(first_comparison_result, second_comparison_result);
    }

    #[test]
    fn test_the_error_type_fits_every_type_both_ways() {
        let codebase = CodebaseMetadata::default();
        let error = TUnion::from_atomic(TAtomic::Error);

        for other in [get_int(), get_null(), get_named_object(word("Foo"), None)] {
            assert_is_contained_by(&codebase, &error, &other, true, &mut ComparisonResult::new());
            assert_is_contained_by(&codebase, &other, &error, true, &mut ComparisonResult::new());
        }
    }

    /// A template's value fits where its constraint fits, so the refused `int` is named only when every part of the
    /// constraint that does not fit is that `int`, whichever part comes last.
    #[test]
    fn a_template_names_the_refused_int_only_when_its_whole_constraint_is_refused_for_it() {
        let codebase = CodebaseMetadata::default();
        let template = |constraint: TUnion| {
            TUnion::from_atomic(TAtomic::GenericParameter(TGenericParameter::new(
                word("T"),
                Arc::new(constraint),
                GenericParent::ClassLike(word("Box")),
            )))
        };
        let string = get_string().get_single().clone();
        let int = get_int().get_single().clone();

        let mut int_result = ComparisonResult::with_sharp_rules(true);
        assert_is_contained_by(&codebase, &template(get_int()), &get_float(), false, &mut int_result);
        assert_eq!(int_result.int_for_float, Some(TInteger::Unspecified));

        for constraint in [vec![string.clone(), int.clone()], vec![int, string]] {
            let mut result = ComparisonResult::with_sharp_rules(true);
            assert_is_contained_by(
                &codebase,
                &template(TUnion::new(Cow::Owned(constraint))),
                &get_float(),
                false,
                &mut result,
            );
            assert_eq!(result.int_for_float, None);
        }
    }
}
