//! Conditions (spec section 21). A PHP# condition is a `bool`, so a PHP condition of another type becomes the
//! comparison PHP's truthiness makes for that type: `count != 0` for an `int`, `!items.isEmpty()` for a collection,
//! `order != null` for an object that may be null. `isset` and `empty` become the same comparisons.

use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::EmptyConstruct;
use mago_syntax::cst::Expression;
use mago_syntax::cst::If;
use mago_syntax::cst::IssetConstruct;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Variable;

use super::Kind;
use super::is_primary;
use super::kinds;
use crate::Converter;

/// The assignment PHP runs first in `condition`, when a comparison reads its value: the left side, in parentheses, of
/// a comparison, as `($at = strpos($url, '#')) !== false`, which is itself the condition or the left side of one.
fn first_compared<'condition, 'arena>(
    condition: &'condition Expression<'arena>,
) -> Option<&'condition Expression<'arena>> {
    let Expression::Binary(binary) = condition.unparenthesized() else {
        return None;
    };
    let lhs = binary.lhs.unparenthesized();
    match lhs {
        Expression::Assignment(_) if binary.operator.is_comparison() => {
            matches!(binary.lhs, Expression::Parenthesized(_)).then_some(lhs)
        }
        _ => first_compared(binary.lhs),
    }
}

/// Whether each value of `union` but null is a `bool`, an `int`, a `float` or a string, which a condition reads by its
/// type.
fn is_scalar(union: &TUnion) -> bool {
    let bare: Vec<Kind> = kinds(union).into_iter().filter(|kind| *kind != Kind::Null).collect();

    !bare.is_empty() && bare.iter().all(|kind| matches!(kind, Kind::Bool | Kind::Int | Kind::Float | Kind::String))
}

/// Why a condition of `shown`, a scalar no native declaration types, has no PHP# form.
pub(super) fn unproven_scalar(shown: &str) -> String {
    format!(
        "a condition of `{shown}` has no PHP# form: no native declaration makes it the type the analysis gives it at runtime, as a framework's PHPDoc `@property` doesn't, and PHP may hold another type there, as `1` for a `bool` or `\"0\"` for an `int`, which PHP# reads by its type (spec section 21)"
    )
}

impl Converter<'_, '_> {
    /// Whether a condition may read `value` as the scalar the analysis types it, which it holds at runtime, or refuses
    /// it. A local it reads keeps that only while every value written to it holds its type.
    pub(super) fn proven(&mut self, value: &Expression<'_>) -> bool {
        if !self.holds_type_at_runtime(value) {
            let shown = String::from_utf8_lossy(self.text(value.span())).into_owned();
            self.refuse(value.span(), unproven_scalar(&shown));
            return false;
        }
        if let Expression::Variable(Variable::Direct(variable)) = value.unparenthesized()
            && self.is_local(variable.name)
        {
            self.tested_local(variable.name, value.span());
        }

        true
    }

    /// A condition, which PHP# takes only as a `bool` (spec section 21). `top` says it stands alone, as an `if`'s does,
    /// so a comparison of several parts needs no parentheses.
    pub(super) fn condition(&mut self, condition: &Expression<'_>, top: bool) {
        if self.is(condition, &[Kind::Bool]) {
            if !self.proven(condition) {
                return;
            }
            return self.expression(condition, None);
        }
        if let Some(test) = self.truthiness(condition, false, top) {
            self.edits.replace(condition.span(), test);
        }
    }

    /// The condition of an `else if`, which refuses an assignment of a local: PHP runs it only when the conditions
    /// before it fail, and the line of its own [`Converter::assigned_condition`] writes before an `if` runs every time.
    pub(super) fn chained_condition(&mut self, condition: &Expression<'_>) {
        if let Expression::Assignment(assignment) = condition
            && let Expression::Variable(Variable::Direct(variable)) = assignment.lhs
            && assignment.operator.is_assign()
        {
            let shown = String::from_utf8_lossy(variable.name);
            return self.refuse(
                condition.span(),
                format!("an `else if` whose condition assigns `{shown}` has no PHP# form: a PHP# assignment is a statement, and a line before the `if` runs it every time, where PHP runs this one only when the conditions before it fail"),
            );
        }
        self.condition(condition, true);
    }

    /// `if ($user = $this->find($id))`, which tests the value it assigns, as the assignment on its own line and the
    /// test of the local it writes, `const user = this.find(id);` and `if (user != null)`, since a PHP# assignment is
    /// a statement. PHP runs the assignment first and then tests the local, as the two lines do. Gives whether the
    /// condition is such an assignment. An `else if` keeps it as a condition, which refuses, since its assignment runs
    /// only when the conditions before it fail.
    pub(super) fn assigned_condition(&mut self, statement: &Statement<'_>, r#if: &If<'_>) -> bool {
        let condition = r#if.condition;
        let Expression::Assignment(assignment) = condition else {
            return false;
        };
        let (Expression::Variable(Variable::Direct(variable)), true) =
            (assignment.lhs, assignment.operator.is_assign())
        else {
            return false;
        };
        let Some(union) = self.type_of(assignment.rhs).filter(|_| self.is_local(variable.name)) else {
            return false;
        };
        if !self.declaration(statement, condition) {
            self.expression(condition, None);
        }
        if is_scalar(union) && !self.proven(assignment.lhs) {
            return true;
        }
        let named = |this: &mut Self| this.variable_name(variable.name);
        let Some((test, _)) = self.test_of(union, condition.span(), named, true, false, true) else {
            return true;
        };
        let indentation = self.indentation(statement.span().start.offset);
        self.edits.delete(r#if.r#if.span.start.offset, condition.span().start.offset);
        self.edits.insert(condition.span().end.offset, format!(";\n{indentation}if ({test}"));

        true
    }

    /// `if (($at = strpos($url, '#')) !== false)`, whose condition first assigns a local a comparison then reads, as
    /// the assignment on its own line and the comparison of the local, `const at = strpos(url, "#");` and
    /// `if ((at) != false)`. PHP runs the assignment before any other part of the condition, as the line before the
    /// `if` runs it. Gives whether the condition is such a comparison.
    pub(super) fn compared_assignment(&mut self, statement: &Statement<'_>, r#if: &If<'_>) -> bool {
        let Some(assigned) = first_compared(r#if.condition) else {
            return false;
        };
        let Expression::Assignment(assignment) = assigned else {
            return false;
        };
        let (Expression::Variable(Variable::Direct(variable)), true) =
            (assignment.lhs, assignment.operator.is_assign())
        else {
            return false;
        };
        if !self.is_local(variable.name) {
            return false;
        }
        self.condition(r#if.condition, true);
        self.declared_by(statement, assignment);
        // Only `(` and spaces stand between `if` and the assignment, which the comparison reads first.
        let start = assigned.span().start.offset;
        let opened = String::from_utf8_lossy(&self.source[r#if.r#if.span.start.offset as usize..start as usize]);
        let indentation = self.indentation(statement.span().start.offset);
        let name = self.variable_name(variable.name);
        self.edits.delete(r#if.r#if.span.start.offset, start);
        self.edits.insert(assigned.span().end.offset, format!(";\n{indentation}{opened}{name}"));

        true
    }

    /// The `bool` test of PHP's truthiness of `expression`, or of its falsiness when `negated`, which is what `!` and
    /// `empty` read (spec section 21). A test of several parts reads the value more than once, so it is written only
    /// for a value [`Converter::rereads`] gives again, and in parentheses unless `top`. Refuses the type where PHP# has
    /// no such test.
    pub(super) fn truthiness(&mut self, expression: &Expression<'_>, negated: bool, top: bool) -> Option<String> {
        self.tested(expression, negated, top).map(|(test, _)| test)
    }

    /// PHP's `$value ?: $other` as `test ? value : other`, where `test` is the truthiness of `value`, which reads it
    /// again, so `value` is one [`Converter::rereads`] gives again (spec section 21).
    pub(super) fn elvis(&mut self, expression: &Expression<'_>, value: &Expression<'_>, other: &Expression<'_>) {
        if !self.rereads(value) {
            let shown = String::from_utf8_lossy(self.text(value.span())).into_owned();
            return self.refuse(
                expression.span(),
                format!("`?:` of `{shown}` has no PHP# form: `??` covers null, and PHP's truthiness test reads the value a second time, which the converter writes only for a variable or a property (spec section 21)"),
            );
        }
        let (test, value) = if self.is(value, &[Kind::Bool]) {
            if !self.proven(value) {
                return;
            }
            self.expression(value, None);
            let rendered = self.render(value);
            (rendered.clone(), rendered)
        } else {
            let Some(tested) = self.tested(value, false, false) else {
                return;
            };
            tested
        };
        self.expression(other, None);
        let other = self.render(other);
        self.edits.replace(expression.span(), format!("{test} ? {value} : {other}"));
    }

    /// Whether reading `expression` a second time gives what the first read gave: a variable, or a property of one that
    /// is never null, which every class it may be declares with no `get` hook, and no class it may be declares `__get`
    /// for. PHP runs no code between the two reads of one test, so nothing changes the value between them.
    pub(super) fn rereads(&self, expression: &Expression<'_>) -> bool {
        match expression {
            Expression::Parenthesized(parenthesized) => self.rereads(parenthesized.expression),
            Expression::Variable(Variable::Direct(_)) => true,
            Expression::Access(Access::Property(access)) => {
                let ClassLikeMemberSelector::Identifier(name) = &access.property else {
                    return false;
                };
                let property = [b"$".as_slice(), name.value].concat();
                let codebase = self.input.codebase;
                let plain = |class: &[u8]| {
                    let declared = codebase
                        .get_declaring_property(class, &property)
                        .is_some_and(|metadata| metadata.hooks.is_empty() && !metadata.flags.is_virtual_property());
                    let descendants = codebase.get_all_descendants(class);
                    declared
                        && std::iter::once(class)
                            .chain(descendants.iter().map(|descendant| descendant.as_bytes()))
                            .all(|class| codebase.get_declaring_method(class, b"__get").is_none())
                };
                self.rereads(access.object)
                    && self.type_of(access.object).is_some_and(|union| {
                        union.types.iter().all(|atomic| {
                            matches!(atomic, TAtomic::Object(TObject::Named(named)) if plain(named.name.as_bytes()))
                        })
                    })
            }
            _ => false,
        }
    }

    /// The test [`Converter::truthiness`] gives, with the PHP# text of the value it tests.
    fn tested(&mut self, expression: &Expression<'_>, negated: bool, top: bool) -> Option<(String, String)> {
        let Some(union) = self.type_of(expression) else {
            self.refuse(expression.span(), "the analysis gives this condition no type");
            return None;
        };
        // A scalar's test reads it by its type, which it holds at runtime only where a declaration makes it.
        if is_scalar(union) && !self.proven(expression) {
            return None;
        }
        let rendered = |this: &mut Self| {
            this.expression(expression, None);
            let rendered = this.render(expression);
            if is_primary(expression) { rendered } else { format!("({rendered})") }
        };
        let rereads = self.rereads(expression);
        self.test_of(union, expression.span(), rendered, rereads, negated, top)
    }

    /// The test of PHP's truthiness of a value of type `union`, at `span`, whose PHP# text `rendered` gives once the
    /// test is known to exist, with that text. `rereads` says whether a second read of the value gives the first's.
    fn test_of(
        &mut self,
        union: &TUnion,
        span: Span,
        rendered: impl FnOnce(&mut Self) -> String,
        rereads: bool,
        negated: bool,
        top: bool,
    ) -> Option<(String, String)> {
        let shown = union.get_id().to_string();
        let kinds = kinds(union);
        let nullable = kinds.contains(&Kind::Null);
        let bare: Vec<Kind> = kinds.into_iter().filter(|kind| *kind != Kind::Null).collect();
        // An object, and a class value, which names a class, are always truthy, so their test is their null check, as
        // is the test of a value that can only be null.
        let is_object = matches!(bare.as_slice(), [Kind::Object] | [Kind::Enum] | [Kind::Class] | []);
        if union.types.iter().any(mago_codex::ttype::atomic::TAtomic::is_mixed) || (is_object && !nullable) {
            self.refuse(
                span,
                format!("a condition of `{shown}` reads PHP's truthiness, which PHP# has no test for: a PHP# condition is a `bool` (spec section 21)"),
            );
            return None;
        }

        let value = rendered(self);
        // PHP's truthiness of each type, as the test for a value that is truthy, and the test for one that is not.
        let (truthy, falsy) = match bare.as_slice() {
            [Kind::Bool] => (format!("{value} == true"), format!("{value} != true")),
            [Kind::Int] => (format!("{value} != 0"), format!("{value} == 0")),
            [Kind::Float] => (format!("{value} != 0.0"), format!("{value} == 0.0")),
            [Kind::String] => {
                (format!("{value} != \"\" && {value} != \"0\""), format!("{value} == \"\" || {value} == \"0\""))
            }
            [Kind::List] | [Kind::Map] => (format!("!{value}.isEmpty()"), format!("{value}.isEmpty()")),
            _ if is_object => (format!("{value} != null"), format!("{value} == null")),
            _ => {
                self.refuse(
                    span,
                    format!("a condition of `{shown}` reads PHP's truthiness, which PHP# has no test for: a PHP# condition is a `bool` (spec section 21)"),
                );
                return None;
            }
        };
        // `bool?` compares with `true`, which a null never equals, and an object's test is its null check.
        let (truthy, falsy) = if nullable && !is_object && bare != [Kind::Bool] {
            (format!("{value} != null && {truthy}"), format!("{value} == null || {falsy}"))
        } else {
            (truthy, falsy)
        };
        let test = if negated { falsy } else { truthy };
        let parts = test.matches("&&").count() + test.matches("||").count();
        if parts > 0 && !rereads {
            self.refuse(
                span,
                format!("a condition of `{shown}` has no PHP# form here: its PHP# test reads the value more than once, which gives what PHP's one read gives only for a variable or a property"),
            );
            return None;
        }

        Some((if parts > 0 && !top { format!("({test})") } else { test }, value))
    }

    /// `isset($a, $b)` as `a != null && b != null`, a `Map` key as `map.get(key) != null`, since `get` gives null for a
    /// missing key, as PHP's `isset` reads it, and a `List` index as `list.getOrNull(i) != null` (spec section 12). `negated` writes `!isset(…)`, which replaces
    /// `expression`, as the comparisons that test the opposite, since `!` binds tighter than `!=`.
    pub(super) fn isset(&mut self, expression: &Expression<'_>, isset: &IssetConstruct<'_>, negated: bool) {
        let compared = if negated { "==" } else { "!=" };
        let mut tests = Vec::new();
        for value in &isset.values {
            let test = match value {
                Expression::ArrayAccess(access) if self.is(access.array, &[Kind::Map]) => {
                    if self.unread(access.array, "isset") {
                        return;
                    }
                    self.untyped(access.array, "an index");
                    self.untyped(access.index, "a key");
                    if self.numeric_key(access.array, access.index) {
                        return;
                    }
                    let map = self.render(access.array);
                    let map = if is_primary(access.array) { map } else { format!("({map})") };
                    format!("{map}.get({}) {compared} null", self.render(access.index))
                }
                // A `List` gives `getOrNull(i)` past its end, where PHP's `isset` is false (spec section 12).
                Expression::ArrayAccess(access)
                    if self.is(access.array, &[Kind::List]) && self.is(access.index, &[Kind::Int]) =>
                {
                    if self.unread(access.array, "isset") {
                        return;
                    }
                    self.untyped(access.array, "an index");
                    self.untyped(access.index, "an index");
                    let list = self.render(access.array);
                    let list = if is_primary(access.array) { list } else { format!("({list})") };
                    format!("{list}.getOrNull({}) {compared} null", self.render(access.index))
                }
                Expression::ArrayAccess(access) => {
                    let shown = self.shown(access.array);
                    return self.refuse(
                        value.span(),
                        format!("`isset` at an index of `{shown}` has no PHP# form: PHP# reads a missing key from a `Map`, and a missing `int` index from a `List`"),
                    );
                }
                Expression::Variable(Variable::Direct(_)) | Expression::Access(_) => {
                    if self.unread(value, "isset") {
                        return;
                    }
                    self.expression(value, None);
                    let rendered = self.render(value);
                    format!("{rendered} {compared} null")
                }
                _ => return self.refuse(value.span(), "`isset` of this expression has no PHP# form"),
            };
            tests.push(test);
        }
        let joined = tests.join(if negated { " || " } else { " && " });
        self.edits.replace(expression.span(), if tests.len() > 1 { format!("({joined})") } else { joined });
    }

    /// `empty($x)` as the test of PHP's falsiness of `x`, as `items.isEmpty()` (spec section 21), and `!empty($x)`,
    /// which `negated` writes in place of `expression`, as the test of its truthiness.
    pub(super) fn empty(&mut self, expression: &Expression<'_>, empty: &EmptyConstruct<'_>, negated: bool) {
        if self.unread(empty.value, "empty") {
            return;
        }
        if let Some(test) = self.truthiness(empty.value, !negated, false) {
            self.edits.replace(expression.span(), test);
        }
    }

    /// Refuses `place`, which `construct`, `isset` or `empty`, reads, and gives whether it did, where PHP reads a place
    /// that holds no value without an error and PHP# reads the place itself. PHP gives a typed property that holds no
    /// value as unset, asks `__isset` or `__get` of a class that declares them, and reads past a `List`'s end. So each
    /// property along `place` converts only when it always holds a value, as one with a default or a promoted one does,
    /// and no class its object may be declares `__isset` or `__get`, and an index converts for a `Map` key only.
    fn unread(&mut self, place: &Expression<'_>, construct: &str) -> bool {
        let shown = String::from_utf8_lossy(self.text(place.span())).into_owned();
        let mut current = place;
        loop {
            let (object, property) = match current {
                Expression::Access(Access::Property(access)) => (access.object, &access.property),
                Expression::Access(Access::NullSafeProperty(access)) => (access.object, &access.property),
                Expression::ArrayAccess(access) if self.is(access.array, &[Kind::Map]) => {
                    current = access.array;
                    continue;
                }
                Expression::ArrayAccess(access) => {
                    let indexed = self.shown(access.array);
                    self.refuse(
                        place.span(),
                        format!("`{construct}` at an index of `{indexed}` reads past a `List`'s end, which PHP gives as unset and PHP# refuses: it converts for a `Map` key"),
                    );
                    return true;
                }
                _ => return false,
            };
            if let Some(reason) = self.unset_property(object, property) {
                self.refuse(place.span(), format!("`{construct}` of `{shown}` {reason}"));
                return true;
            }
            current = object;
        }
    }

    /// Why PHP may read `property` of `object` as holding no value, where PHP# reads it: no default and no promotion,
    /// or a class `object` may be that declares `__isset` or `__get`.
    fn unset_property(&self, object: &Expression<'_>, property: &ClassLikeMemberSelector<'_>) -> Option<String> {
        let ClassLikeMemberSelector::Identifier(name) = property else {
            return Some("reads a property named by a variable".to_owned());
        };
        let codebase = self.input.codebase;
        let Some(union) = self.type_of(object) else {
            return Some("reads a property of a value the analysis gives no type".to_owned());
        };
        let mut classes = Vec::new();
        for atomic in union.types.iter().filter(|atomic| !atomic.is_null()) {
            let TAtomic::Object(TObject::Named(named)) = atomic else {
                return Some(format!(
                    "reads a property of `{}`, whose class the analysis doesn't know",
                    union.get_id()
                ));
            };
            classes.push(named.name);
        }
        let property = [b"$".as_slice(), name.value].concat();
        for class in classes {
            let class = class.as_bytes();
            let valued = codebase.get_declaring_property(class, &property).is_some_and(|metadata| {
                metadata.flags.has_default()
                    || metadata.flags.is_promoted_property()
                    || metadata.type_declaration_metadata.is_none()
            });
            if !valued {
                return Some("reads a property that may hold no value, which PHP gives as unset and PHP# reads: it converts for a property with a default or a promoted one".to_owned());
            }
            let descendants = codebase.get_all_descendants(class);
            let magic = std::iter::once(class).chain(descendants.iter().map(|descendant| descendant.as_bytes())).find(
                |class| {
                    [b"__isset".as_slice(), b"__get"]
                        .iter()
                        .any(|method| codebase.get_declaring_method(class, method).is_some())
                },
            );
            if let Some(magic) = magic {
                let magic = String::from_utf8_lossy(magic);
                return Some(format!(
                    "reads a property `{magic}`'s `__isset` or `__get` may answer for, which PHP asks and PHP# never does"
                ));
            }
        }

        None
    }
}
