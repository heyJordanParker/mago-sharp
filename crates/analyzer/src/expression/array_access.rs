use mago_allocator::Arena;
use std::rc::Rc;

use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::get_arraykey;
use mago_codex::ttype::get_mixed;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::ArrayAccess;
use mago_syntax::cst::Expression;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::resolver::method::get_declared_collection;
use crate::utils::expression::array::get_array_target_type_given_index;
use crate::utils::expression::expression_is_nullsafe;
use crate::utils::expression::get_array_access_id;
use crate::utils::expression::get_block_expression_id;

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

        self.index.analyze(context, block_context, artifacts)?;

        block_context.flags.set_inside_isset(was_inside_isset);
        block_context.flags.set_inside_unset(was_inside_unset);
        block_context.flags.set_inside_general_use(was_inside_general_use);

        let index_type = artifacts.get_expression_type(&self.index).cloned().unwrap_or_else(get_arraykey);

        let was_inside_general_use = block_context.flags.inside_general_use();
        block_context.flags.set_inside_general_use(true);
        self.array.analyze(context, block_context, artifacts)?;
        block_context.flags.set_inside_general_use(was_inside_general_use);

        if let Some(keyed_array_var_id) = &keyed_array_var_id
            && block_context.has_variable(keyed_array_var_id.as_bytes())
            && let Some(array_access_type) = block_context.locals.get(keyed_array_var_id).cloned()
        {
            let is_variable_key = memchr::memmem::find(keyed_array_var_id.as_bytes(), b"[$").is_some();
            if is_variable_key || !array_access_type.possibly_undefined() {
                artifacts.set_rc_expression_type(self, Rc::clone(&array_access_type));

                return Ok(());
            }
        }

        if let Some(saved_type) = saved_narrowed_type {
            let is_variable_key =
                keyed_array_var_id.as_ref().is_some_and(|k| memchr::memmem::find(k.as_bytes(), b"[$").is_some());
            if is_variable_key || !saved_type.possibly_undefined() {
                artifacts.set_rc_expression_type(self, saved_type);

                return Ok(());
            }
        }

        let container_type = artifacts.get_rc_expression_type(&self.array).cloned();

        if let Some(container_type) = container_type {
            let access_type = get_array_target_type_given_index(
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
            );

            if let Some(keyed_array_var_id) = &keyed_array_var_id {
                let can_store_result = block_context.flags.inside_assignment() || !container_type.is_mixed();

                if !block_context.flags.inside_isset()
                    && can_store_result
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

/// Spec section 12 types a PHP# `Map` read `V?`, because a key is often missing. A bare read throws on a missing
/// key, so a `Map` is read only where the read is handled: `??` and `?.` read a missing key as null, as `isset`
/// does, and so do `is`, `as` and `match`, whose PHP reads the index they test with `??`. A refused bare read keeps
/// the type `V` it would have when it runs, so it is reported once. A `List` read stays bare, because a `List`'s keys
/// run without gaps. A place is the collection it is declared as, whatever literal it holds.
pub(crate) fn check_sharp_map_read<'arena, A>(
    access: &ArrayAccess<'arena>,
    context: &mut Context<'_, 'arena, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
) where
    A: Arena,
{
    let is_map = match get_declared_collection(context, block_context, artifacts, access.array) {
        Some(collection) => collection.is_keyed(),
        None => artifacts.get_expression_type(access.array).is_some_and(|container| {
            container.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(TArray::Keyed(_))))
        }),
    };
    if !is_map {
        return;
    }

    if !block_context.flags.inside_isset() {
        context.collector.report_with_code(
            IssueCode::PossiblyUndefinedArrayIndex,
            Issue::error("A `Map` is not read by a bare index, because its key may be missing.")
                .with_annotation(Annotation::primary(access.span()).with_message("This read throws when the key is missing."))
                .with_help("Read it with `??`, as in `map[key] ?? fallback`, or with `map.get(key)`, which gives null for a missing key. `+=`, `++` and `--` read first, so write `m[k] = (m[k] ?? 0) + 1`."),
        );
    } else if let Some(read) = artifacts.get_expression_type(access).cloned() {
        artifacts.set_expression_type(access, read.as_nullable());
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
