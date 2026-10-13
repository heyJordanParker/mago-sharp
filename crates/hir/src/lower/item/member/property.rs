use mago_allocator::Arena;
use mago_span::HasSpan;
use mago_syntax::cst;

use mago_flags::U8Flags;

use crate::ir::delimited::Delimited;
use crate::ir::expression::Expression;
use crate::ir::item::member::hook::Hook;
use crate::ir::item::member::hook::HookBody;
use crate::ir::item::member::hook::HookBodyKind;
use crate::ir::item::member::property::HookedProperty;
use crate::ir::item::member::property::Property;
use crate::ir::name::Name;
use crate::ir::variable::DirectVariable;
use crate::lower::Lowering;

impl<'scratch, 'arena, S, A> Lowering<'_, 'scratch, 'arena, S, A>
where
    S: Arena,
    A: Arena,
{
    /// Lowers a plain property declaration into one node per declared property,
    /// in source order, so `public $a, $b;` yields two nodes.
    pub(crate) fn lower_plain_property(
        &mut self,
        property: &'scratch cst::PlainProperty<'scratch>,
    ) -> Vec<Property<'arena, (), (), ()>> {
        let attributes = self.lower_attribute_lists(&property.attribute_lists);
        let modifiers = self.lower_modifiers(&property.modifiers);
        let r#type = property.hint.as_ref().map(|hint| self.lower_type(hint));
        let version_constraint = self.lower_version_constraint(&property.attribute_lists);
        let document = self.phpdoc_resolution.get(property.span());
        let annotation = self.lower_item_annotation(document.as_ref(), None);
        let flattened = property.items.len() > 1;

        property
            .items
            .iter()
            .map(|item| {
                let (variable, default_value) = self.lower_property_item(item);

                Property {
                    span: item.span(),
                    annotation,
                    attributes,
                    version_constraint,
                    modifiers,
                    r#type,
                    variable,
                    default_value,
                    flattened,
                }
            })
            .collect()
    }

    pub(crate) fn lower_hooked_property(
        &mut self,
        property: &'scratch cst::HookedProperty<'scratch>,
    ) -> HookedProperty<'arena, (), (), ()> {
        let attributes = self.lower_attribute_lists(&property.attribute_lists);
        let modifiers = self.lower_modifiers(&property.modifiers);
        let r#type = property.hint.as_ref().map(|hint| self.lower_type(hint));
        let version_constraint = self.lower_version_constraint(&property.attribute_lists);
        let document = self.phpdoc_resolution.get(property.span());
        let annotation = self.lower_item_annotation(document.as_ref(), None);
        let (variable, default_value) = self.lower_property_item(&property.item);
        let hooks = self.lower_property_hooks(&property.hook_list);

        HookedProperty {
            span: property.span(),
            annotation,
            attributes,
            version_constraint,
            modifiers,
            r#type,
            variable,
            default_value,
            hooks,
        }
    }

    /// Lowers a PHP# computed property as the hooked property it runs as: one `get => expr;` hook, at its arrow.
    pub(crate) fn lower_computed_property(
        &mut self,
        property: &'scratch cst::ComputedProperty<'scratch>,
    ) -> HookedProperty<'arena, (), (), ()> {
        let document = self.phpdoc_resolution.get(property.span());
        let get = Hook {
            span: property.body.span(),
            annotation: None,
            attributes: &[],
            version_constraint: &[],
            flags: U8Flags::new(),
            modifiers: &[],
            name: Name { span: property.body.arrow, value: self.interner.intern(b"get") },
            parameters: None,
            body: Some(HookBody {
                span: property.body.span(),
                kind: HookBodyKind::Expression(self.arena.alloc(self.lower_expression(property.body.expression))),
            }),
        };

        HookedProperty {
            span: property.span(),
            annotation: self.lower_item_annotation(document.as_ref(), None),
            attributes: self.lower_attribute_lists(&property.attribute_lists),
            version_constraint: self.lower_version_constraint(&property.attribute_lists),
            modifiers: self.lower_modifiers(&property.modifiers),
            r#type: property.hint.as_ref().map(|hint| self.lower_type(hint)),
            variable: self.lower_direct_variable(&property.variable),
            default_value: None,
            hooks: Delimited { span: property.body.span(), items: self.arena.alloc_slice_copy(&[get]) },
        }
    }

    fn lower_property_item(
        &mut self,
        item: &'scratch cst::PropertyItem<'scratch>,
    ) -> (DirectVariable<'arena>, Option<&'arena Expression<'arena, (), (), ()>>) {
        match item {
            cst::PropertyItem::Abstract(item) => (self.lower_direct_variable(&item.variable), None),
            cst::PropertyItem::Concrete(item) => {
                let variable = self.lower_direct_variable(&item.variable);
                let value = self.arena.alloc(self.lower_expression(item.value));

                (variable, Some(value))
            }
        }
    }
}
