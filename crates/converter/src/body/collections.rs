//! Array literals as `List` and `Map` (spec section 12).

use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::Expression;

use super::Kind;
use super::kinds;
use crate::Converter;

impl Converter<'_, '_> {
    /// `[]`, which PHP# writes `[:]` where a `Map` is declared, and refuses where the place holds a `List` or a `Map`,
    /// since nothing picks one.
    pub(super) fn empty_array(&mut self, expression: &Expression<'_>, expected: Option<&TUnion>) {
        let kinds = expected.map(kinds).unwrap_or_default();
        let (list, map) = (kinds.contains(&Kind::List), kinds.contains(&Kind::Map));
        if list && map {
            let shown = expected.map(|union| union.get_id().to_string()).unwrap_or_default();
            return self.refuse(
                expression.span(),
                format!("`[]` lands in `{shown}`, which holds a `List` or a `Map`, and PHP# writes an empty one as `[]` or `[:]` (spec section 12)"),
            );
        }
        self.edits.replace(expression.span(), if map { "[:]" } else { "[]" });
    }

    pub(super) fn elements<'element, 'arena>(&mut self, elements: impl Iterator<Item = &'element ArrayElement<'arena>>)
    where
        'arena: 'element,
    {
        for element in elements {
            match element {
                ArrayElement::KeyValue(element) => {
                    self.expression(element.key, None);
                    self.edits.replace_range(element.key.span().end.offset, element.double_arrow.end.offset, ":");
                    self.expression(element.value, None);
                }
                ArrayElement::Value(element) => self.expression(element.value, None),
                // A `List` spread appends as PHP's does. A `Map` spread keeps each key where PHP's renumbers an `int`
                // key, so only one keyed by `string` keeps PHP's meaning (spec section 12).
                ArrayElement::Variadic(spread) => {
                    self.expression(spread.value, None);
                    let keyed_by_string = self.type_of(spread.value).is_some_and(|union| {
                        union.types.iter().all(|atomic| {
                            matches!(atomic, TAtomic::Array(TArray::Keyed(keyed))
                                if keyed.parameters.as_ref().is_some_and(|(key, _)| key.is_string()))
                        })
                    });
                    if !self.is(spread.value, &[Kind::List]) && !keyed_by_string {
                        let shown = self.shown(spread.value);
                        self.refuse(
                            element.span(),
                            format!("a spread of `{shown}` has no PHP# form: a `List` spread appends, and a `Map` spread keeps an `int` key PHP renumbers (spec section 12)"),
                        );
                    }
                }
                ArrayElement::Missing(_) => self.refuse(element.span(), "a skipped element has no PHP# form"),
            }
        }
    }
}
