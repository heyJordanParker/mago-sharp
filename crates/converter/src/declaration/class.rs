//! A class's header: `public`, its type parameters, and its parents as `: A, B` (spec sections 5, 11 and 22).

use mago_span::HasSpan;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Property;

use crate::Class;
use crate::Converter;

use super::constructor::set_by_constructor;
use super::has;

impl Converter<'_, '_> {
    pub(crate) fn class(&mut self, class: &mago_syntax::cst::Class<'_>) {
        let name = String::from_utf8_lossy(class.name.value).into_owned();
        let Some(metadata) = self.input.codebase.get_class_like(self.input.names.get(&class.name)) else {
            self.refuse(class.name.span, "the analysis holds no class of this name");
            return;
        };
        if let Some(span) = has(&class.modifiers, |modifier| matches!(modifier, Modifier::Abstract(_))) {
            self.refuse(span, "an abstract class is not converted yet");
        }
        if let Some(span) = has(&class.modifiers, |modifier| matches!(modifier, Modifier::Readonly(_))) {
            self.refuse(span, "a `readonly` class is not converted yet: each of its properties becomes `{ get; }`");
        }
        if !metadata.template_type_extends_count.is_empty() || !metadata.template_type_implements_count.is_empty() {
            self.refuse(class.name.span, "`@extends` and `@implements` with type arguments are not converted yet");
        }

        self.attributes(&class.attribute_lists);
        let header_start = class.modifiers.first().map_or(class.class.span, HasSpan::span).start.offset;
        self.edits.insert(header_start, "public ");
        if let Some(parameters) = self.type_parameters(metadata, class.name.span)
            && !parameters.is_empty()
        {
            self.edits.insert(class.name.span.end.offset, parameters);
        }

        // `extends A implements B, C` is the header `: A, B, C` (spec section 22).
        let mut parents: Vec<&Identifier<'_>> = Vec::new();
        if let Some(extends) = &class.extends {
            self.edits.replace(extends.extends.span, ":");
            parents.extend(extends.types.iter());
            // The checker warns on a deprecated parent, and a `.sharp` file holds no warning (spec section 27).
            let deprecated =
                metadata.direct_parent_class.and_then(|parent| self.input.codebase.get_class_like(parent.as_bytes()));
            if let Some(parent) = deprecated.filter(|parent| parent.flags.is_deprecated()) {
                let shown = parent.original_name.as_str_lossy().into_owned();
                self.refuse(extends.extends.span, format!("`{shown}` is deprecated, and the PHP# checker warns on a class that extends it (spec section 27)"));
            }
        }
        if let Some(implements) = &class.implements {
            match parents.last() {
                Some(last) => {
                    self.edits.replace_range(last.span().end.offset, implements.implements.span.end.offset, ",");
                }
                None => self.edits.replace(implements.implements.span, ":"),
            }
            parents.extend(implements.types.iter());
        }
        for parent in parents {
            self.rename_class(parent);
        }
        self.docblock(class.span());

        let is_final = has(&class.modifiers, |modifier| matches!(modifier, Modifier::Final(_))).is_some();
        let set_by_constructor = set_by_constructor(class);
        self.class = Some(Class { name, metadata, is_final, set_by_constructor });
        for member in &class.members {
            match member {
                ClassLikeMember::Method(method) => self.method(method),
                ClassLikeMember::Property(Property::Plain(property)) => self.property(property),
                ClassLikeMember::Property(_) => {
                    self.refuse(
                        member.span(),
                        "a property with hooks is not converted yet: its hooks become accessors",
                    );
                }
                ClassLikeMember::Constant(_) => self.refuse(member.span(), "a class constant is not converted yet"),
                ClassLikeMember::TraitUse(_) => {
                    self.refuse(
                        member.span(),
                        "a trait a class uses is not converted yet: it joins the class's header",
                    );
                }
                _ => self.refuse(member.span(), "this member has no PHP# form"),
            }
        }
        self.class = None;
    }
}
