//! Conditions (spec section 21). A PHP# condition is a `bool`, so a PHP condition of another type becomes the
//! comparison PHP's truthiness makes for that type: `count != 0` for an `int`, `!items.isEmpty()` for a collection,
//! `order != null` for an object that may be null. `isset` and `empty` become the same comparisons.

use mago_codex::ttype::TType;
use mago_span::HasSpan;
use mago_syntax::cst::EmptyConstruct;
use mago_syntax::cst::Expression;
use mago_syntax::cst::IssetConstruct;
use mago_syntax::cst::Variable;

use super::Kind;
use super::is_primary;
use super::kinds;
use crate::Converter;

impl Converter<'_, '_> {
    /// A condition, which PHP# takes only as a `bool` (spec section 21). `top` says it stands alone, as an `if`'s does,
    /// so a comparison of several parts needs no parentheses.
    pub(super) fn condition(&mut self, condition: &Expression<'_>, top: bool) {
        if self.is(condition, &[Kind::Bool]) {
            return self.expression(condition, None);
        }
        if let Some(test) = self.truthiness(condition, false, top) {
            self.edits.replace(condition.span(), test);
        }
    }

    /// The `bool` test of PHP's truthiness of `expression`, or of its falsiness when `negated`, which is what `!` and
    /// `empty` read (spec section 21). A test of several parts reads the value more than once, so it is written only
    /// for a local, and in parentheses unless `top`. Refuses the type where PHP# has no such test.
    pub(super) fn truthiness(&mut self, expression: &Expression<'_>, negated: bool, top: bool) -> Option<String> {
        let Some(union) = self.type_of(expression) else {
            self.refuse(expression.span(), "the analysis gives this condition no type");
            return None;
        };
        let shown = union.get_id().to_string();
        let kinds = kinds(union);
        let nullable = kinds.contains(&Kind::Null);
        let bare: Vec<Kind> = kinds.into_iter().filter(|kind| *kind != Kind::Null).collect();
        // An object is always truthy, so its test is its null check, as is the test of a value that can only be null.
        let is_object = matches!(bare.as_slice(), [Kind::Object] | [Kind::Enum] | []);
        if union.types.iter().any(mago_codex::ttype::atomic::TAtomic::is_mixed) || (is_object && !nullable) {
            self.refuse(
                expression.span(),
                format!("a condition of `{shown}` reads PHP's truthiness, which PHP# has no test for: a PHP# condition is a `bool` (spec section 21)"),
            );
            return None;
        }

        self.expression(expression, None);
        let rendered = self.render(expression);
        let value = if is_primary(expression) { rendered } else { format!("({rendered})") };
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
                    expression.span(),
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
        if parts > 0 && !matches!(expression, Expression::Variable(Variable::Direct(_))) {
            self.refuse(
                expression.span(),
                format!("a condition of `{shown}` is not converted yet: its PHP# test reads the value more than once, which the converter writes only for a local"),
            );
            return None;
        }

        Some(if parts > 0 && !top { format!("({test})") } else { test })
    }

    /// `isset($a, $b)` as `a != null && b != null`, and a `Map` key as `map.get(key) != null`, since `get` gives null
    /// for a missing key, as PHP's `isset` reads it (spec section 12). `negated` writes `!isset(…)`, which replaces
    /// `expression`, as the comparisons that test the opposite, since `!` binds tighter than `!=`.
    pub(super) fn isset(&mut self, expression: &Expression<'_>, isset: &IssetConstruct<'_>, negated: bool) {
        let compared = if negated { "==" } else { "!=" };
        let mut tests = Vec::new();
        for value in &isset.values {
            let test = match value {
                Expression::ArrayAccess(access) if self.is(access.array, &[Kind::Map]) => {
                    self.untyped(access.array, "an index");
                    self.untyped(access.index, "a key");
                    let map = self.render(access.array);
                    let map = if is_primary(access.array) { map } else { format!("({map})") };
                    format!("{map}.get({}) {compared} null", self.render(access.index))
                }
                Expression::ArrayAccess(access) => {
                    let shown = self.shown(access.array);
                    return self.refuse(
                        value.span(),
                        format!("`isset` at an index of `{shown}` is not converted yet: PHP# reads a missing key only from a `Map`"),
                    );
                }
                Expression::Variable(Variable::Direct(_)) | Expression::Access(_) => {
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
        if let Some(test) = self.truthiness(empty.value, !negated, false) {
            self.edits.replace(expression.span(), test);
        }
    }
}
