//! Array literals as `List` and `Map` (spec section 12).

use mago_codex::ttype::SharpType;
use mago_codex::ttype::TType;
use mago_codex::ttype::get_sharp_type;
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

    /// A literal's `elements`, each of which lands in the value type of `expected`, the type its place declares. PHP
    /// numbers the elements of `['a', 'b']` from 0, so where only a `Map` is declared they are its keys, `[0: 'a', 1:
    /// 'b']`, since `[a, b]` is a `List` literal (spec section 12). A literal of keys and elements without one in such a
    /// place refuses, as PHP numbers those after the greatest `int` key.
    pub(super) fn elements<'element, 'arena>(
        &mut self,
        literal: &Expression<'_>,
        elements: impl Iterator<Item = &'element ArrayElement<'arena>> + Clone,
        expected: Option<&TUnion>,
    ) where
        'arena: 'element,
    {
        let kinds = expected.map(kinds).unwrap_or_default();
        let in_map = kinds.contains(&Kind::Map) && !kinds.contains(&Kind::List) && !kinds.contains(&Kind::Collection);
        let numbered = in_map && elements.clone().all(|element| matches!(element, ArrayElement::Value(_)));
        if in_map && !numbered && elements.clone().any(|element| matches!(element, ArrayElement::Value(_))) {
            return self.refuse(
                literal.span(),
                "a literal of keys and of elements without one has no PHP# form where a `Map` is declared: PHP numbers each element after the greatest `int` key, and a PHP# `Map` literal names every key (spec section 12)",
            );
        }
        let value = expected.and_then(element_type);
        for (index, element) in elements.enumerate() {
            match element {
                ArrayElement::KeyValue(element) => {
                    self.expression(element.key, None);
                    self.edits.replace_range(element.key.span().end.offset, element.double_arrow.end.offset, ":");
                    self.expression(element.value, value.as_ref());
                }
                ArrayElement::Value(element) => {
                    self.expression(element.value, value.as_ref());
                    // The key goes before the element's own text, and an insert at its start would land inside a
                    // rewrite that renders the element, as `typeof(0: Order)`, so the element's span takes both.
                    if numbered {
                        let text = self.render(element.value);
                        self.edits.replace(element.value.span(), format!("{index}: {text}"));
                    }
                }
                // A `List` spread appends as PHP's does. A `Map` spread keeps each key as written, where PHP's
                // renumbers an `int` key, and a numeric string key is one to PHP, as `'42'` is 42 (spec section 12).
                ArrayElement::Variadic(spread) => {
                    self.expression(spread.value, None);
                    if !self.is(spread.value, &[Kind::List]) {
                        let shown = self.shown(spread.value);
                        self.refuse(
                            element.span(),
                            format!("a spread of `{shown}` has no PHP# form: a `List` spread appends, and a `Map` spread keeps each key, where PHP renumbers an `int` key and reads `'42'` as one (spec section 12)"),
                        );
                    }
                }
                ArrayElement::Missing(_) => self.refuse(element.span(), "a skipped element has no PHP# form"),
            }
        }
    }
}

/// The type of a value in a collection of the type `collection`, a `List`'s element or a `Map`'s value, where it holds
/// one collection.
fn element_type(collection: &TUnion) -> Option<TUnion> {
    let [atomic] = collection.types.iter().filter(|atomic| !atomic.is_null()).collect::<Vec<_>>()[..] else {
        return None;
    };
    match get_sharp_type(atomic)? {
        SharpType::List(element) | SharpType::Set(element) | SharpType::Map(_, element) => Some(element.clone()),
        _ => None,
    }
}
