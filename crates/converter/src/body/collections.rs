//! Array literals as `List` and `Map` (spec section 12).

use mago_codex::ttype::TType;
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
                    self.edits.replace(element.double_arrow, ":");
                    self.expression(element.value, None);
                }
                ArrayElement::Value(element) => self.expression(element.value, None),
                ArrayElement::Variadic(_) => self.refuse(element.span(), "a spread is not converted yet"),
                ArrayElement::Missing(_) => self.refuse(element.span(), "a skipped element has no PHP# form"),
            }
        }
    }
}
