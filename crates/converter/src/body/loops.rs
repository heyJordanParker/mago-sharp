//! Loops (spec section 17): `foreach` as `for … of`, and `for`, `while` and `do … while`, which keep their form. A
//! loop's body is written in braces, and its variables are declared by the loop and scoped to it.

use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::ArrayAccess;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::DoWhile;
use mago_syntax::cst::Expression;
use mago_syntax::cst::For;
use mago_syntax::cst::ForBody;
use mago_syntax::cst::Foreach;
use mago_syntax::cst::ForeachBody;
use mago_syntax::cst::ForeachTarget;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::KeyValueArrayElement;
use mago_syntax::cst::Statement;
use mago_syntax::cst::StringPart;
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

/// Counts the places a loop's body names its key `name`, and those that read it as text or as a key: a part of `.`,
/// `.=` or an interpolated string, an index, or the key of an array element.
struct KeyReads<'name> {
    name: &'name [u8],
    named: usize,
    as_text: usize,
}

impl KeyReads<'_> {
    fn read_as_text(&mut self, expression: &Expression<'_>) {
        if matches!(expression, Expression::Variable(Variable::Direct(variable)) if variable.name == self.name) {
            self.as_text += 1;
        }
    }
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for KeyReads<'_> {
    fn walk_in_direct_variable(&mut self, variable: &'ast DirectVariable<'arena>, _: &mut ()) {
        if variable.name == self.name {
            self.named += 1;
        }
    }

    fn walk_in_binary(&mut self, binary: &'ast Binary<'arena>, _: &mut ()) {
        if matches!(binary.operator, BinaryOperator::StringConcat(_)) {
            self.read_as_text(binary.lhs);
            self.read_as_text(binary.rhs);
        }
    }

    fn walk_in_assignment(&mut self, assignment: &'ast Assignment<'arena>, _: &mut ()) {
        if matches!(assignment.operator, AssignmentOperator::Concat(_)) {
            self.read_as_text(assignment.rhs);
        }
    }

    fn walk_in_string_part(&mut self, part: &'ast StringPart<'arena>, _: &mut ()) {
        match part {
            StringPart::Expression(value) => self.read_as_text(value),
            StringPart::BracedExpression(braced) => self.read_as_text(braced.expression),
            StringPart::Literal(_) => {}
        }
    }

    fn walk_in_array_access(&mut self, access: &'ast ArrayAccess<'arena>, _: &mut ()) {
        self.read_as_text(access.index);
    }

    fn walk_in_key_value_array_element(&mut self, element: &'ast KeyValueArrayElement<'arena>, _: &mut ()) {
        self.read_as_text(element.key);
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
                _ => {
                    let reason = super::locals::destructured(target)
                        .map_or("a `foreach` into this place has no PHP# form", super::locals::unpacked);
                    return self.refuse(target.span(), reason);
                }
            }
        }

        let iterable = foreach.expression;
        let (list, map) = (self.is(iterable, &[Kind::List]), self.is(iterable, &[Kind::Map]));
        // A loop over the values of an object of a class that implements `Traversable`, as a framework's collection or a
        // generator, is PHP's own `foreach` over it in PHP# too.
        let codebase = self.input.codebase;
        let traversable = self.is(iterable, &[Kind::Object])
            && self.type_of(iterable).is_some_and(|union| {
                !union.types.is_empty()
                    && union.types.iter().all(|atomic| {
                        matches!(atomic, TAtomic::Object(TObject::Named(named))
                        if codebase.is_instance_of(named.name.as_bytes(), b"Traversable"))
                    })
            });
        let values = list || traversable || self.is(iterable, &[Kind::Collection]);
        let suffix = match (key.is_some(), values, map) {
            (false, true, _) | (true, _, true) => "",
            (false, _, true) => ".values()",
            (true, true, _) if list => ".entries()",
            _ => {
                let shown = self.shown(iterable);
                return self.refuse(
                    iterable.span(),
                    format!("`foreach` over `{shown}` has no PHP# form: PHP# loops over a `List`, a `Map`, a `Set`, an `Iterable` or the values of a `Traversable` object, and gives a key only from a `List` or a `Map` (spec section 17)"),
                );
            }
        };
        // PHP turns a numeric string key into an `int`, as `"42"` into 42, and a PHP# `Map` keeps a `string` key as a
        // string, which a PHP array crossing into it gives back for an `int` one. So a loop gives PHP's key only where
        // the keys are `int`, or where the body reads the key only as text or as a key, where the `int` and the string
        // of its digits give the same (spec section 12).
        let read_as_text = |key: &Expression<'_>| {
            let Expression::Variable(Variable::Direct(variable)) = key else {
                return false;
            };
            let mut reads = KeyReads { name: variable.name, named: 0, as_text: 0 };
            reads.walk_statement(body, &mut ());
            reads.named == reads.as_text
        };
        // A `Map` keyed by `int|string` holds each key as the PHP array gave it, and each key the body writes passes
        // [`Converter::numeric_key`], so a loop gives PHP's keys too.
        if key.is_some_and(|key| !read_as_text(key))
            && map
            && !self.keyed_by_int(iterable)
            && !self.keyed_by_both(iterable)
        {
            let shown = self.shown(iterable);
            return self.refuse(
                iterable.span(),
                format!("a keyed loop over `{shown}` reads the key other than as text or as a key, which has no PHP# form: PHP turns a numeric string key into an `int`, as `'42'` into 42, and a PHP# `Map` keeps it a string (spec section 12)"),
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
        let names: Vec<String> = variables.iter().map(|variable| self.variable_name(variable.name)).collect();
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

    /// `unset($map[$key])` as `map.remove(key);` (spec section 12), and `unset($bag[$key])` of an `ArrayAccess` object
    /// as `bag.offsetUnset(key);`, the call PHP's `unset` makes. A PHP# value is never unset, so `unset` of anything
    /// else has no PHP# form.
    pub(super) fn unset(&mut self, unset: &Unset<'_>) {
        let mut removals = Vec::new();
        for value in unset.values.iter() {
            let Expression::ArrayAccess(access) = value else {
                return self.refuse(
                    value.span(),
                    "`unset` of a variable or a property has no PHP# form: a PHP# value is never unset",
                );
            };
            let removal = if self.is(access.array, &[Kind::Map]) {
                "remove"
            } else if self.is_array_access(access.array) {
                "offsetUnset"
            } else {
                let shown = self.shown(access.array);
                let reason = if self.is(access.array, &[Kind::List]) {
                    "PHP leaves the keys after it as they are, and a PHP# `List` renumbers its elements after it (spec section 12)"
                } else {
                    "PHP# removes a key from a `Map` and an `ArrayAccess` object only (spec section 12)"
                };
                return self
                    .refuse(value.span(), format!("`unset` at an index of `{shown}` has no PHP# form: {reason}"));
            };
            self.written(access.array);
            self.untyped(access.index, "a key");
            if self.numeric_key(access.array, access.index) {
                return;
            }
            let map = self.render(access.array);
            let map = if is_primary(access.array) { map } else { format!("({map})") };
            removals.push(format!("{map}.{removal}({});", self.render(access.index)));
        }
        self.edits.replace(unset.span(), removals.join(" "));
    }
}
