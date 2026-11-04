//! A class, an interface or an enum: its header, `public`, its type parameters, and its parents as `: A, B` (spec
//! sections 5, 11, 20 and 22), then each of its members.

use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_span::HasSpan;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::EnumCaseItem;
use mago_syntax::cst::Extends;
use mago_syntax::cst::Implements;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Property;
use mago_syntax::cst::Sequence;

use crate::Class;
use crate::Converter;

use super::constructor::set_by_constructor;
use super::has;

impl<'input> Converter<'input, '_> {
    pub(crate) fn class(&mut self, class: &mago_syntax::cst::Class<'_>) {
        let Some(metadata) = self.declared(&class.name) else {
            return;
        };
        // A `readonly` class is each of its properties `{ get; }` (spec section 13).
        if let Some(span) = has(&class.modifiers, |modifier| matches!(modifier, Modifier::Readonly(_))) {
            self.edits.delete(span.start.offset, self.skip_whitespace(span.end.offset));
        }
        if !metadata.template_type_extends_count.is_empty() || !metadata.template_type_implements_count.is_empty() {
            self.refuse(class.name.span, "`@extends` and `@implements` with type arguments are not converted yet");
        }

        self.attributes(&class.attribute_lists);
        let header_start = class.modifiers.first().map_or(class.class.span, HasSpan::span).start.offset;
        self.header(header_start, metadata, &class.name);
        if let Some(extends) = &class.extends {
            // The checker warns on a deprecated parent, and a `.sharp` file holds no warning (spec section 27).
            let deprecated =
                metadata.direct_parent_class.and_then(|parent| self.input.codebase.get_class_like(parent.as_bytes()));
            if let Some(parent) = deprecated.filter(|parent| parent.flags.is_deprecated()) {
                let shown = parent.original_name.as_str_lossy().into_owned();
                self.refuse(extends.extends.span, format!("`{shown}` is deprecated, and the PHP# checker warns on a class that extends it (spec section 27)"));
            }
        }
        self.parents(None, class.extends.as_ref(), class.implements.as_ref());
        self.docblock(class.span());

        let is_final = has(&class.modifiers, |modifier| matches!(modifier, Modifier::Final(_))).is_some();
        self.members(metadata, &class.name, is_final, set_by_constructor(class), &class.members);
    }

    /// An interface, whose members PHP# writes with no access modifier (spec section 22).
    pub(crate) fn interface(&mut self, interface: &mago_syntax::cst::Interface<'_>) {
        let Some(metadata) = self.declared(&interface.name) else {
            return;
        };
        self.attributes(&interface.attribute_lists);
        self.header(interface.interface.span.start.offset, metadata, &interface.name);
        self.parents(None, interface.extends.as_ref(), None);
        self.docblock(interface.span());
        self.members(metadata, &interface.name, false, Vec::new(), &interface.members);
    }

    /// An enum, whose backing type and interfaces its header lists after `:` (spec section 20). Its cases keep their
    /// form, and an enum is never extended, so no method of it is `virtual`.
    pub(crate) fn enumeration(&mut self, enumeration: &mago_syntax::cst::Enum<'_>) {
        let Some(metadata) = self.declared(&enumeration.name) else {
            return;
        };
        self.attributes(&enumeration.attribute_lists);
        self.header(enumeration.r#enum.span.start.offset, metadata, &enumeration.name);
        let backing = enumeration.backing_type_hint.as_ref().map(|backing| backing.hint.span().end.offset);
        self.parents(backing, None, enumeration.implements.as_ref());
        self.docblock(enumeration.span());
        self.members(metadata, &enumeration.name, true, Vec::new(), &enumeration.members);
    }

    /// The analysis's class-like `name` declares, or a refusal.
    fn declared(&mut self, name: &LocalIdentifier<'_>) -> Option<&'input ClassLikeMetadata> {
        let metadata = self.input.codebase.get_class_like(self.input.names.get(name));
        if metadata.is_none() {
            self.refuse(name.span, "the analysis holds no class of this name");
        }

        metadata
    }

    /// `public` at `start`, where a declaration's header starts, since PHP reaches every class, and the type
    /// parameters after `name`.
    fn header(&mut self, start: u32, metadata: &ClassLikeMetadata, name: &LocalIdentifier<'_>) {
        self.edits.insert(start, "public ");
        if let Some(parameters) = self.type_parameters(metadata, name.span)
            && !parameters.is_empty()
        {
            self.edits.insert(name.span.end.offset, parameters);
        }
    }

    /// `extends A implements B, C` as the header's `: A, B, C` (spec section 22). An enum's backing type, which ends
    /// at `backing`, comes first.
    fn parents(&mut self, backing: Option<u32>, extends: Option<&Extends<'_>>, implements: Option<&Implements<'_>>) {
        let mut last = backing;
        if let Some(extends) = extends {
            self.edits.replace(extends.extends.span, ":");
            for parent in extends.types.iter() {
                self.rename_class(parent);
                last = Some(parent.span().end.offset);
            }
        }
        if let Some(implements) = implements {
            match last {
                Some(end) => self.edits.replace_range(end, implements.implements.span.end.offset, ","),
                None => self.edits.replace(implements.implements.span, ":"),
            }
            for parent in implements.types.iter() {
                self.rename_class(parent);
            }
        }
    }

    /// Each member of the class-like `metadata`, named `name`.
    fn members(
        &mut self,
        metadata: &'input ClassLikeMetadata,
        name: &LocalIdentifier<'_>,
        is_final: bool,
        set_by_constructor: Vec<Vec<u8>>,
        members: &Sequence<'_, ClassLikeMember<'_>>,
    ) {
        let name = String::from_utf8_lossy(name.value).into_owned();
        self.class = Some(Class { name, metadata, is_final, set_by_constructor });
        for member in members {
            match member {
                ClassLikeMember::Method(method) => self.method(method),
                ClassLikeMember::Property(Property::Plain(property)) => self.property(property),
                ClassLikeMember::Property(_) => {
                    self.refuse(
                        member.span(),
                        "a property with hooks is not converted yet: its hooks become accessors",
                    );
                }
                ClassLikeMember::Constant(_) if metadata.kind.is_interface() => {
                    self.refuse(member.span(), "a constant of an interface is not converted yet");
                }
                ClassLikeMember::Constant(constant) => self.class_constant(constant),
                ClassLikeMember::EnumCase(case) => {
                    self.attributes(&case.attribute_lists);
                    if let EnumCaseItem::Backed(item) = &case.item {
                        self.expression(item.value, None);
                    }
                }
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
