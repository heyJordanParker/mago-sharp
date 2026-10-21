use mago_allocator::Arena;
use std::rc::Rc;

use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::get_arraykey;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_span::HasSpan;
use mago_syntax::cst::Access;
use mago_syntax::cst::ArrayAccess;
use mago_syntax::cst::Expression;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::access::analyze_rest_of_chain;
use crate::expression::is_refused_type;
use crate::resolver::method::get_declared_collection;
use crate::resolver::method::with_declared_kinds;
use crate::resolver::static_property::StaticProperty;
use crate::resolver::static_property::StaticPropertyName;
use crate::utils::expression::array::get_array_target_type_given_index;
use crate::utils::expression::expression_is_nullsafe;
use crate::utils::expression::get_array_access_id;
use crate::utils::expression::get_block_expression_id;
use crate::utils::quoted_source;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for ArrayAccess<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let keyed_array_var_id = get_array_access_id(
            self,
            block_context.scope.get_class_like_name(),
            context.resolved_names,
            Some(context.codebase),
        );

        let extended_var_id = get_block_expression_id(self.array, context, block_context);
        let saved_narrowed_type = if matches!(self.index, Expression::UnaryPostfix(_)) {
            keyed_array_var_id.as_ref().and_then(|k| block_context.locals.get(k).cloned())
        } else {
            None
        };

        let was_inside_isset = block_context.flags.inside_isset();
        let was_inside_general_use = block_context.flags.inside_general_use();
        let was_inside_unset = block_context.flags.inside_unset();

        block_context.flags.set_inside_isset(false);
        block_context.flags.set_inside_general_use(true);
        block_context.flags.set_inside_unset(false);

        analyze_rest_of_chain(
            context,
            block_context,
            artifacts,
            self.array,
            false,
            |context, block_context, artifacts| self.index.analyze(context, block_context, artifacts),
        )?;

        block_context.flags.set_inside_isset(was_inside_isset);
        block_context.flags.set_inside_unset(was_inside_unset);
        block_context.flags.set_inside_general_use(was_inside_general_use);

        let index_type = artifacts.get_expression_type(&self.index).cloned().unwrap_or_else(get_arraykey);

        let was_inside_general_use = block_context.flags.inside_general_use();
        block_context.flags.set_inside_general_use(true);
        self.array.analyze(context, block_context, artifacts)?;
        block_context.flags.set_inside_general_use(was_inside_general_use);

        check_constant_list_index(self, &index_type, context, block_context);

        // A PHP# place is indexed as every kind of collection it is declared as, whatever it holds. A `Set` has no
        // index, so a read of one is refused even where an earlier write of the index left its type behind.
        let container_type = artifacts.get_rc_expression_type(&self.array).cloned().map(|container_type| {
            with_declared_kinds(context, block_context, artifacts, self.array, &container_type)
                .map_or(container_type, Rc::new)
        });
        let has_no_index = context.dialect.is_sharp()
            && container_type.as_ref().is_some_and(|container_type| {
                container_type.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(TArray::Set(_))))
            });

        if !has_no_index
            && let Some(keyed_array_var_id) = &keyed_array_var_id
            && block_context.has_variable(keyed_array_var_id.as_bytes())
            && let Some(array_access_type) = block_context.locals.get(keyed_array_var_id).cloned()
        {
            let is_variable_key = memchr::memmem::find(keyed_array_var_id.as_bytes(), b"[$").is_some();
            if is_variable_key || !array_access_type.possibly_undefined() {
                artifacts.set_rc_expression_type(self, Rc::clone(&array_access_type));

                return Ok(());
            }
        }

        if !has_no_index && let Some(saved_type) = saved_narrowed_type {
            let is_variable_key =
                keyed_array_var_id.as_ref().is_some_and(|k| memchr::memmem::find(k.as_bytes(), b"[$").is_some());
            if is_variable_key || !saved_type.possibly_undefined() {
                artifacts.set_rc_expression_type(self, saved_type);

                return Ok(());
            }
        }

        // Spec section 14.4: `?.` skips the rest of its chain, so past it an index reads the collection's own element,
        // and only the whole chain may be null, as a member read past it is.
        let skipped_by_null_safe = context.dialect.is_sharp()
            && artifacts.get_expression_type(self.array).is_some_and(TUnion::has_nullsafe_null);
        let container_type = container_type.map(|container_type| {
            if skipped_by_null_safe { Rc::new(container_type.to_non_nullable()) } else { container_type }
        });

        if let Some(container_type) = container_type {
            let read = |context: &mut Context<'ctx, 'arena, A>| {
                get_array_target_type_given_index(
                    context,
                    block_context,
                    self.span(),
                    self.array.span(),
                    Some(self.index.span()),
                    &container_type,
                    &index_type,
                    false,
                    extended_var_id,
                    None,
                    container_type.can_be_null() && expression_is_nullsafe(self.array),
                )
            };
            // A refused PHP# read of a container with no `List`, `Map` or `Set` member, such as an `int`, `null` or an
            // object with no index, never gives a value. It has the error type, so nothing that uses it reports the same
            // mistake again, and it is judged again each time it runs. A collection member gives its value type even when
            // the key is refused, so a wrong use of that value is still reported.
            let access_type = if context.dialect.is_sharp()
                && !container_type.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(_)))
            {
                let (access_type, issues) = context.record(read);
                let refused = issues.iter().any(|issue| issue.level == Level::Error);
                context.collector.extend(issues);

                if refused { TUnion::from_atomic(TAtomic::Error) } else { access_type }
            } else {
                read(context)
            };
            let access_type = if skipped_by_null_safe {
                let element_is_never_null = !access_type.can_be_null();
                let mut access_type = access_type.as_nullable();
                access_type.set_nullsafe_null(element_is_never_null);
                access_type
            } else {
                access_type
            };

            if let Some(keyed_array_var_id) = &keyed_array_var_id {
                let can_store_result = block_context.flags.inside_assignment() || !container_type.is_mixed();

                if !block_context.flags.inside_isset()
                    && can_store_result
                    && !is_refused_type(&access_type)
                    && memchr::memmem::find(keyed_array_var_id.as_bytes(), b"[$").is_some()
                    && !block_context.locals.contains_key(keyed_array_var_id)
                {
                    block_context.locals.insert(*keyed_array_var_id, Rc::new(access_type.clone()));
                }
            }

            artifacts.set_expression_type(self, access_type);
        } else {
            artifacts.set_expression_type(self, get_mixed());
        }

        Ok(())
    }
}

/// Refuses an index a PHP# constant value reads outside the list a constant holds, as `Billing.PLANS[5]` where `PLANS`
/// holds 2 elements: a constant's value is known before the code runs, so its index is known to miss. An index the
/// checker can't know throws `OutOfRangeException` when it runs, as in a method body.
fn check_constant_list_index<A>(
    access: &ArrayAccess<'_>,
    index_type: &TUnion,
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
) where
    A: Arena,
{
    let span = access.span();
    if context.dialect.is_sharp()
        && (block_context.flags.inside_constant_expression()
            || context.patterns.iter().any(|(_, pattern)| pattern.span().contains(&span.start)))
        && let Some(index) = index_type.get_single_literal_int_value()
        && let Expression::Access(Access::Property(read)) = access.array.unparenthesized()
        && let Some(StaticProperty {
            class: Expression::ConstantAccess(class),
            name: StaticPropertyName::Identifier(name),
            ..
        }) = StaticProperty::from_property_access(read, context.resolved_names)
        && let Some(TAtomic::Array(TArray::List(list))) = context
            .codebase
            .get_class_constant(context.resolved_names.get(&class.name), name.value)
            .and_then(|constant| constant.inferred_type.as_ref())
        && let Some(known) = list.known_elements.as_ref().filter(|_| list.element_type.is_never())
        && !usize::try_from(index).is_ok_and(|index| known.contains_key(&index))
    {
        let size = known.len();
        let elements = if size == 1 { "element" } else { "elements" };
        let list = quoted_source(context, access.array.span());
        context.collector.report_with_code(
            IssueCode::MismatchedArrayIndex,
            Issue::error(format!("`{list}` holds {size} {elements}, so it has no index {index}."))
                .with_annotation(
                    Annotation::primary(access.index.span()).with_message(format!("Index {index} is read here.")),
                )
                .with_help(if size == 0 {
                    "Read an element only once the list holds one.".to_owned()
                } else {
                    format!("Read an index from 0 to {}.", size - 1)
                }),
        );
    }
}

/// Spec section 12 types a PHP# `Map` read `V?`, because a key is often missing. A bare read throws on a missing
/// key, so a `Map` is read only where the read is handled: `??` and `?.` read a missing key as null, as `isset`
/// does, and so do `is`, `as` and `match`, whose PHP reads the index they test with `??`. A refused bare read keeps
/// the type `V` it would have when it runs, so it is reported once. A bare `List` read is allowed, because a `List`'s
/// keys run without gaps, and keeps its element type, before `?.` too. A place is the collections it is declared as, whatever
/// literal it holds, and a place that may be a `Map` is read as one. A place that may be a `Set` has no index at all,
/// which the `Set`'s own rule refuses, so this rule adds no second report to its bare read.
pub(crate) fn check_sharp_map_read<'arena, A>(
    access: &ArrayAccess<'arena>,
    context: &mut Context<'_, 'arena, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
) where
    A: Arena,
{
    let kinds = get_indexed_kinds(access, context, block_context, artifacts);
    if !kinds.map {
        return;
    }

    if block_context.flags.inside_isset() {
        if let Some(read) = artifacts.get_expression_type(access).cloned() {
            artifacts.set_expression_type(access, read.as_nullable());
        }
    } else if !kinds.set {
        context.collector.report_with_code(
            IssueCode::PossiblyUndefinedArrayIndex,
            Issue::error("A `Map` is not read by a bare index, because its key may be missing.")
                .with_annotation(Annotation::primary(access.span()).with_message("This read throws when the key is missing."))
                .with_help("Read it with `??`, as in `map[key] ?? fallback`, or with `map.get(key)`, which gives null for a missing key. `+=`, `++` and `--` read first, so write `m[k] = (m[k] ?? 0) + 1`."),
        );
    }
}

/// Spec section 12 makes a `List` index a place, as a Rust `Vec` index and a Go slice index are, so a change through
/// it changes the element the list stores. A change through a `Map` index is refused, as Go refuses `m[k].x = 5`,
/// because a `Map` read is a copy. Gives whether `access`, which a change goes through, indexes a `List` alone. A
/// place that may be a `Set` has no index, which the `Set`'s own rule refuses, so it gets no second report.
pub(crate) fn check_change_through_index<'arena, A>(
    access: &ArrayAccess<'arena>,
    context: &mut Context<'_, 'arena, A>,
    block_context: &BlockContext<'_>,
    artifacts: &AnalysisArtifacts,
) -> bool
where
    A: Arena,
{
    let kinds = get_indexed_kinds(access, context, block_context, artifacts);
    if kinds.map && !kinds.set {
        context.collector.report_with_code(
            IssueCode::InvalidPassByReference,
            Issue::error("A `Map` read is a copy: read it, change it, and store it back.")
                .with_annotation(
                    Annotation::primary(access.span()).with_message("This reads a copy of the value at this key."),
                )
                .with_note("A change to the copy never reaches the `Map`.")
                .with_help("Change a local and store it back, as in `List<int> counts = map[key] ?? []; counts.add(5); map[key] = counts;`."),
        );
    }

    !kinds.map && !kinds.set
}

/// Spec section 12: "`x[i] ?? fallback` is a compile error that names `getOrNull`", whatever the element type, since a
/// `List` index throws past either end and never reads null for a missing index. `??=` reads the index first, so its
/// left side is refused too. Gives whether `lhs`, the left side of `??`, is such an index.
pub(crate) fn check_sharp_list_coalesce<'arena, A>(
    lhs: &Expression<'arena>,
    context: &mut Context<'_, 'arena, A>,
    block_context: &BlockContext<'_>,
    artifacts: &AnalysisArtifacts,
) -> bool
where
    A: Arena,
{
    let Expression::ArrayAccess(access) = lhs.unparenthesized() else {
        return false;
    };
    if !context.dialect.is_sharp() || !get_indexed_kinds(access, context, block_context, artifacts).list {
        return false;
    }

    context.collector.report_with_code(
        IssueCode::InvalidArrayAccess,
        Issue::error(
            "A `List` index never reads null: it throws past either end, so `??` never reads a missing index.",
        )
        .with_annotation(Annotation::primary(access.span()).with_message("This index throws when it is missing."))
        .with_help("Read it with `list.getOrNull(i)`, which gives null past either end."),
    );

    true
}

/// The collections a place or a value an index reads may be.
struct IndexedKinds {
    list: bool,
    map: bool,
    set: bool,
}

/// The collections `access` indexes may be. A place is the collections it is declared as, whatever literal it holds.
fn get_indexed_kinds<'arena, A>(
    access: &ArrayAccess<'arena>,
    context: &Context<'_, 'arena, A>,
    block_context: &BlockContext<'_>,
    artifacts: &AnalysisArtifacts,
) -> IndexedKinds
where
    A: Arena,
{
    let kinds = |container: &TUnion| {
        let has = |kind: fn(&TArray) -> bool| {
            container.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(array) if kind(array)))
        };

        IndexedKinds {
            list: has(|array| matches!(array, TArray::List(_))),
            map: has(|array| matches!(array, TArray::Keyed(_))),
            set: has(|array| matches!(array, TArray::Set(_))),
        }
    };

    match get_declared_collection(context, block_context, artifacts, access.array) {
        Some(declared) => kinds(&declared),
        None => artifacts
            .get_expression_type(access.array)
            .map_or(IndexedKinds { list: false, map: false, set: false }, kinds),
    }
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use crate::code::IssueCode;
    use crate::test_analysis;

    test_analysis! {
        name = using_generic_parameter_as_index,
        code = indoc! {"
            <?php

            /**
             * @template T as string|lowercase-string
             *
             * @param array<T, bool> $old
             * @param array<T, bool> $new
             * @return array<T, bool>
             */
            function mergeThreadData(array $old, array $new): array {
                foreach ($new as $name => $value) {
                    if (!isset($old[$name]) || !$old[$name] && $value) {
                        $old[$name] = $value;
                    }
                }

                return $old;
            }
        "},
    }

    test_analysis! {
        name = negated_isset_narrows_parent_array_type,
        code = indoc! {"
            <?php

            /**
             * @return array{foo?: array{bar?: string}}
             */
            function y(): array { return []; }

            $y = y();

            if (isset($y['foo']) && !isset($y['foo']['bar'])) {
                echo $y['foo']['bar'];
                echo $y['foo']['baz'];
            }
        "},
        issues = [
            IssueCode::UndefinedStringArrayIndex,  // $y['foo']['bar']
            IssueCode::UndefinedStringArrayIndex,  // $y['foo']['baz']
        ],
    }

    test_analysis! {
        name = variable_key_narrowing_not_null,
        code = indoc! {"
            <?php

            /**
             * @param array<string, int|null> $map
             */
            function testNotNullVariableKeyNarrowing(array $map, string $key): int
            {
                if ($map[$key] !== null) {
                    return $map[$key];
                }

                return 0;
            }
        "},
    }
}
