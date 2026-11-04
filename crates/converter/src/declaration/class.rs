//! A class, an interface or an enum: its header, `public`, its type parameters, and its parents as `: A, B` (spec
//! sections 5, 11, 20 and 22), then each of its members.

use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_span::HasSpan;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::EnumCaseItem;
use mago_syntax::cst::Extends;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Implements;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Property;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::TraitUseSpecification;
use mago_word::ascii_lowercase_word;

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
        self.parents(metadata, None, class.extends.as_ref(), class.implements.as_ref());
        // A plain PHP trait the class uses joins the header, after the base class and the interfaces (spec section 22).
        let traits = self.traits(metadata, &class.members);
        if !traits.is_empty() {
            let last = class
                .implements
                .as_ref()
                .and_then(|implements| implements.types.last())
                .or_else(|| class.extends.as_ref().and_then(|extends| extends.types.last()));
            let (end, opening) = match last {
                Some(parent) => (parent.span().end.offset, ", "),
                None => (class.name.span.end.offset, " : "),
            };
            self.edits.insert(end, format!("{opening}{}", traits.join(", ")));
        }
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
        self.parents(metadata, None, interface.extends.as_ref(), None);
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
        self.parents(metadata, backing, None, enumeration.implements.as_ref());
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

    /// `extends A implements B, C` as the header's `: A, B, C` (spec section 22), each parent with the type arguments
    /// `@extends` or `@implements` gives it, as `: A<int>`. An enum's backing type, which ends at `backing`, comes
    /// first.
    fn parents(
        &mut self,
        metadata: &ClassLikeMetadata,
        backing: Option<u32>,
        extends: Option<&Extends<'_>>,
        implements: Option<&Implements<'_>>,
    ) {
        let mut last = backing;
        if let Some(extends) = extends {
            self.edits.replace(extends.extends.span, ":");
            for parent in extends.types.iter() {
                self.parent(metadata, parent);
                last = Some(parent.span().end.offset);
            }
        }
        if let Some(implements) = implements {
            match last {
                Some(end) => self.edits.replace_range(end, implements.implements.span.end.offset, ","),
                None => self.edits.replace(implements.implements.span, ":"),
            }
            for parent in implements.types.iter() {
                self.parent(metadata, parent);
            }
        }
    }

    /// The parent `identifier` names, by the name this file reaches it by, with its type arguments.
    fn parent(&mut self, metadata: &ClassLikeMetadata, identifier: &Identifier<'_>) {
        let Some(name) = self.class_name(identifier) else {
            return;
        };
        let resolved = ascii_lowercase_word(self.input.names.get(identifier));
        let arguments = metadata.template_extended_offsets.get(&resolved).map_or_else(
            || Some(Vec::new()),
            |arguments| arguments.iter().map(|argument| self.type_text(argument, identifier.span())).collect(),
        );
        let Some(arguments) = arguments else {
            return;
        };
        let written = if arguments.is_empty() { name } else { format!("{name}<{}>", arguments.join(", ")) };
        self.edits.replace(identifier.span(), written);
    }

    /// Each trait `members` uses, with the type arguments `@use` gives it, as the header writes it, each `use` line
    /// leaving the body. A `use` with `insteadof` or `as` has no PHP# form.
    fn traits(&mut self, metadata: &ClassLikeMetadata, members: &Sequence<'_, ClassLikeMember<'_>>) -> Vec<String> {
        let mut traits = Vec::new();
        for member in members {
            let ClassLikeMember::TraitUse(trait_use) = member else {
                continue;
            };
            if matches!(trait_use.specification, TraitUseSpecification::Concrete(_)) {
                self.refuse(member.span(), "a trait's `use` with `insteadof` or `as` has no PHP# form");
                continue;
            }
            for name in trait_use.trait_names.iter() {
                self.parent(metadata, name);
                let span = name.span();
                traits.push(self.edits.render(self.source, span.start.offset, span.end.offset));
            }
            // Its docblock, which holds the `@use` the header now writes, goes with it.
            let docblock = self.docblock_before(member.span()).map(|(span, _)| span);
            self.delete_lines(docblock.map_or(member.span(), |docblock| docblock.join(member.span())));
        }

        traits
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
                // A class's traits join its header, which `traits` writes.
                ClassLikeMember::TraitUse(_) if metadata.kind.is_class() => {}
                ClassLikeMember::TraitUse(_) => {
                    self.refuse(member.span(), "a trait an enum uses is not converted yet");
                }
                _ => self.refuse(member.span(), "this member has no PHP# form"),
            }
        }
        self.class = None;
    }
}
