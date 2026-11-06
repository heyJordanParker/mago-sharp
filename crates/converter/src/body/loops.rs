//! Loops (spec section 17): `foreach` as `for … of`, and `for`, `while` and `do … while`, which keep their form. A
//! loop's body is written in braces, and its variables are declared by the loop and scoped to it.

use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::DoWhile;
use mago_syntax::cst::Expression;
use mago_syntax::cst::For;
use mago_syntax::cst::ForBody;
use mago_syntax::cst::Foreach;
use mago_syntax::cst::ForeachBody;
use mago_syntax::cst::ForeachTarget;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Unset;
use mago_syntax::cst::Variable;
use mago_syntax::cst::While;
use mago_syntax::cst::WhileBody;
use mago_syntax::walker::MutWalker;

use super::Kind;
use super::is_primary;
use crate::Converter;

/// Counts the places a body names the variable `name`, and those that are the collection of a loop over its values
/// alone.
pub(crate) struct IterableUses<'name> {
    name: &'name [u8],
    named: usize,
    looped: usize,
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for IterableUses<'_> {
    fn walk_in_foreach(&mut self, foreach: &'ast Foreach<'arena>, _: &mut ()) {
        if let (ForeachTarget::Value(_), Expression::Variable(Variable::Direct(variable))) =
            (&foreach.target, foreach.expression)
            && variable.name == self.name
        {
            self.looped += 1;
        }
    }

    fn walk_in_direct_variable(&mut self, variable: &'ast DirectVariable<'arena>, _: &mut ()) {
        if variable.name == self.name {
            self.named += 1;
        }
    }
}

impl Converter<'_, '_> {
    /// `foreach ($items as $item)` as `for (const item of items)`, a `Map`'s key and value as `[key, value]`, its
    /// values alone through `values()`, and a `List`'s index through `entries()` (spec section 17).
    pub(super) fn foreach(&mut self, statement: &Statement<'_>, foreach: &Foreach<'_>) {
        let ForeachBody::Statement(body) = &foreach.body else {
            return self.refuse(
                statement.span(),
                "a `foreach` with `:` and `endforeach` has no PHP# form: write its body in braces",
            );
        };
        let (key, value) = match &foreach.target {
            ForeachTarget::Value(target) => (None, target.value),
            ForeachTarget::KeyValue(target) => (Some(target.key), target.value),
        };
        let mut variables = Vec::new();
        for target in key.into_iter().chain([value]) {
            match target {
                Expression::Variable(Variable::Direct(variable)) => variables.push(variable),
                Expression::UnaryPrefix(unary) if matches!(unary.operator, UnaryPrefixOperator::Reference(_)) => {
                    return self.refuse(
                        target.span(),
                        "a `foreach` by reference has no PHP# form: PHP# passes every value as it is, so the loop writes the collection back",
                    );
                }
                _ => return self.refuse(target.span(), "a `foreach` into a pattern is not converted yet"),
            }
        }

        let iterable = foreach.expression;
        let (list, map) = (self.is(iterable, &[Kind::List]), self.is(iterable, &[Kind::Map]));
        let suffix = match (key.is_some(), list || self.is(iterable, &[Kind::Collection]), map) {
            (false, true, _) | (true, _, true) => "",
            (false, _, true) => ".values()",
            (true, true, _) if list => ".entries()",
            _ => {
                let shown = self.shown(iterable);
                return self.refuse(
                    iterable.span(),
                    format!("`foreach` over `{shown}` is not converted yet: PHP# loops over a `List`, a `Map`, a `Set` or an `Iterable`, and gives a key only from a `List` or a `Map` (spec section 17)"),
                );
            }
        };
        // PHP turns a numeric string key into an `int`, as `"42"` into 42, and a PHP# `Map` keeps the key it was given,
        // so a loop reads a `Map`'s keys as PHP's does only where they are `int` (spec section 12).
        if key.is_some() && map && !self.keyed_by_int(iterable) {
            let shown = self.shown(iterable);
            return self.refuse(
                iterable.span(),
                format!("a keyed loop over `{shown}` has no PHP# form: PHP turns a numeric string key into an `int`, as `'42'` into 42, and a PHP# `Map` keeps it as written (spec section 12)"),
            );
        }
        self.expression(iterable, None);
        let source = self.render(iterable);
        let source = if suffix.is_empty() || is_primary(iterable) { source } else { format!("({source})") };

        // The loop declares its variables right after its `(`, where `const` or `let` goes.
        let at = foreach.left_parenthesis.end.offset;
        for variable in &variables {
            if !self.loop_local(variable, iterable, at, statement.span()) {
                return;
            }
        }
        let names: Vec<String> = variables.iter().map(|variable| Self::variable_name(variable.name)).collect();
        let target = if names.len() == 1 { names.join("") } else { format!("[{}]", names.join(", ")) };
        self.edits.replace(foreach.foreach.span, "for");
        self.edits.replace_range(at, foreach.right_parenthesis.start.offset, format!("{target} of {source}{suffix}"));
        self.braced(body);
    }

    /// Refuses `parameter`, of the declared type `union`, when it is an `iterable` the PHP# type `Iterable<V>` takes and
    /// the body `walk` visits reads it other than by a loop over its values, or a constructor stores it as a property,
    /// and gives whether it did. A PHP# `Iterable<V>` parameter takes a new sequence of the values it is given,
    /// numbered from 0, so a keyed loop, a return, a call that passes it on, `instanceof` and a method call see other
    /// keys and another class than PHP's (spec section 12).
    pub(crate) fn observes_iterable(
        &mut self,
        parameter: &FunctionLikeParameter<'_>,
        union: Option<&TUnion>,
        walk: impl FnOnce(&mut IterableUses<'_>),
    ) -> bool {
        if !union.is_some_and(|union| union.types.iter().any(TAtomic::is_iterable)) {
            return false;
        }
        let mut uses = IterableUses { name: parameter.variable.name, named: 0, looped: 0 };
        walk(&mut uses);
        let read = match (parameter.is_promoted_property(), uses.named == uses.looped) {
            (true, _) => "the constructor stores as a property",
            (false, false) => "the body reads other than by a loop over its values",
            (false, true) => return false,
        };
        let shown = String::from_utf8_lossy(parameter.variable.name);
        self.refuse(
            parameter.variable.span,
            format!("`{shown}` is an `iterable` {read}, and a PHP# `Iterable<V>` parameter takes a new sequence of its values, numbered from 0, so its keys and class are another's (spec section 12)"),
        );

        true
    }

    /// Whether every key the analysis gives `value`, an array, is an `int`.
    pub(super) fn keyed_by_int(&self, value: &Expression<'_>) -> bool {
        self.type_of(value).is_some_and(|union| {
            union.types.iter().all(|atomic| match atomic {
                TAtomic::Array(TArray::Keyed(keyed)) => keyed.parameters.as_ref().is_some_and(|(key, _)| {
                    key.types.iter().all(|key| matches!(key, TAtomic::Scalar(TScalar::Integer(_))))
                }),
                TAtomic::Array(TArray::List(_)) => true,
                _ => false,
            })
        })
    }

    /// A `for` loop, which keeps its form and declares the local its initialization assigns, scoped to the loop.
    pub(super) fn for_loop(&mut self, statement: &Statement<'_>, r#for: &For<'_>) {
        let ForBody::Statement(body) = &r#for.body else {
            return self
                .refuse(statement.span(), "a `for` with `:` and `endfor` has no PHP# form: write its body in braces");
        };
        if r#for.initializations.iter().count() > 1 || r#for.conditions.iter().count() > 1 {
            return self.refuse(
                statement.span(),
                "a `for` with several initializations or conditions is not converted yet: PHP#'s declares one local and takes one condition",
            );
        }

        if let Some(locals) = self.locals() {
            locals.blocks.push(statement.span());
        }
        for initialization in r#for.initializations.iter() {
            if !self.declaration(statement, initialization) {
                self.expression(initialization, None);
            }
        }
        for condition in r#for.conditions.iter() {
            self.condition(condition, true);
        }
        for increment in r#for.increments.iter() {
            self.expression(increment, None);
        }
        self.braced(body);
        if let Some(locals) = self.locals() {
            locals.blocks.pop();
        }
    }

    pub(super) fn while_loop(&mut self, statement: &Statement<'_>, r#while: &While<'_>) {
        let WhileBody::Statement(body) = &r#while.body else {
            return self.refuse(
                statement.span(),
                "a `while` with `:` and `endwhile` has no PHP# form: write its body in braces",
            );
        };
        self.condition(r#while.condition, true);
        self.braced(body);
    }

    pub(super) fn do_while(&mut self, r#do: &DoWhile<'_>) {
        self.braced(r#do.statement);
        self.condition(r#do.condition, true);
    }

    /// `break` and `continue`, which leave the innermost loop. PHP's count of loops to leave has no PHP# form: a PHP#
    /// `break` names the loop it leaves (spec section 17).
    pub(super) fn leave(&mut self, level: Option<&Expression<'_>>) {
        if let Some(level) = level {
            self.refuse(
                level.span(),
                "a count of loops to leave is not converted yet: a PHP# `break` or `continue` names the loop it leaves (spec section 17)",
            );
        }
    }

    /// `unset($map[$key])` as `map.remove(key);` (spec section 12). A PHP# value is never unset, so `unset` of
    /// anything else has no PHP# form.
    pub(super) fn unset(&mut self, unset: &Unset<'_>) {
        let mut removals = Vec::new();
        for value in unset.values.iter() {
            let Expression::ArrayAccess(access) = value else {
                return self.refuse(
                    value.span(),
                    "`unset` of a variable or a property has no PHP# form: a PHP# value is never unset",
                );
            };
            if !self.is(access.array, &[Kind::Map]) {
                let shown = self.shown(access.array);
                return self.refuse(
                    value.span(),
                    format!(
                        "`unset` at an index of `{shown}` is not converted yet: PHP# removes a key only from a `Map`"
                    ),
                );
            }
            self.written(access.array);
            self.untyped(access.index, "a key");
            let map = self.render(access.array);
            let map = if is_primary(access.array) { map } else { format!("({map})") };
            removals.push(format!("{map}.remove({});", self.render(access.index)));
        }
        self.edits.replace(unset.span(), removals.join(" "));
    }
}
