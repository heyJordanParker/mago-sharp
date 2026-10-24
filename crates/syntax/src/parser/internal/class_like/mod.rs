use crate::T;
use crate::cst::cst::AnonymousClass;
use crate::cst::cst::AttributeList;
use crate::cst::cst::Class;
use crate::cst::cst::ClassLikeMember;
use crate::cst::cst::Enum;
use crate::cst::cst::EnumBackingTypeHint;
use crate::cst::cst::Hint;
use crate::cst::cst::Inheritance;
use crate::cst::cst::Interface;
use crate::cst::cst::LocalIdentifier;
use crate::cst::cst::Modifier;
use crate::cst::cst::Statement;
use crate::cst::cst::Trait;
use crate::cst::cst::TypeParameterList;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;

pub mod constant;
pub mod enum_case;
pub mod extension;
pub mod inheritance;
pub mod law;
pub mod member;
pub mod method;
pub mod property;
pub mod trait_use;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// A class-like's members up to its `}`. A member that fails to parse is reported and skipped, so the rest still
    /// parse. `members_of` names the owner in the messages, as `A class` or `An enum`, `name` is its name, `None` for
    /// an anonymous class, and `type_parameters_of` the type parameters it declares, as `T` or `TKey, TValue`.
    pub(crate) fn parse_class_like_members(
        &mut self,
        members_of: &'static str,
        name: Option<&LocalIdentifier<'arena>>,
        type_parameters_of: Option<std::boxed::Box<str>>,
    ) -> Result<Sequence<'arena, ClassLikeMember<'arena>>, ParseError> {
        let members_name = name.map(|name| std::boxed::Box::<[u8]>::from(name.value));
        let mut members = self.new_vec();
        // Whether the loop is skipping the tokens after an error, which report nothing until a member parses again.
        // PHP# recovers so, and PHP keeps upstream's error per skipped token.
        let mut recovering = false;
        loop {
            if matches!(self.stream.peek_kind(0)?, Some(T!["}"])) {
                break;
            }

            let position_before = self.stream.current_position();
            self.state.members_of = members_of;
            self.state.members_name.clone_from(&members_name);
            self.state.type_parameters_of.clone_from(&type_parameters_of);
            let errors_before = self.errors.len();
            match self.parse_classlike_member() {
                Ok(member) => {
                    members.push(member);
                    recovering = false;
                }
                Err(_) if recovering => self.errors.truncate(errors_before),
                Err(err) => {
                    self.errors.push(err);
                    recovering = self.dialect.is_sharp();
                }
            }
            if self.stream.current_position() == position_before {
                if let Ok(Some(token)) = self.stream.lookahead(0) {
                    if token.kind == T!["}"] {
                        break;
                    }
                    if !recovering {
                        self.errors.push(self.stream.unexpected(Some(token), &[]));
                        recovering = self.dialect.is_sharp();
                    }
                    let _ = self.stream.consume();
                } else {
                    break;
                }
            }
        }

        Ok(Sequence::new(members))
    }

    pub(crate) fn parse_interface_with_attributes(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
    ) -> Result<Interface<'arena>, ParseError> {
        self.parse_interface_with_attributes_and_modifiers(attributes, Sequence::empty())
    }

    /// A class, or in PHP# an interface, an enum, a trait or an event, whose declaration starts with modifiers, as in
    /// `public interface Foo {}`.
    pub(crate) fn parse_modified_class_like_with_attributes(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
    ) -> Result<Statement<'arena>, ParseError> {
        let modifiers = self.parse_modifier_sequence()?;
        self.read_php_type_declaration()?;
        if self.is_at_event()? {
            return Ok(Statement::Event(self.parse_event_with_attributes_and_modifiers(attributes, modifiers)?));
        }

        if self.is_at_struct()? {
            return Ok(Statement::Struct(self.parse_struct_with_attributes_and_modifiers(attributes, modifiers)?));
        }

        if self.dialect.is_sharp() {
            match self.stream.peek_kind(0)? {
                Some(T!["interface"]) | Some(T![Identifier]) if self.is_at_interface()? => {
                    return Ok(Statement::Interface(
                        self.parse_interface_with_attributes_and_modifiers(attributes, modifiers)?,
                    ));
                }
                Some(T!["enum"]) => {
                    return Ok(Statement::Enum(self.parse_enum_with_attributes_and_modifiers(attributes, modifiers)?));
                }
                Some(T!["trait"]) => {
                    return Ok(Statement::Trait(
                        self.parse_trait_with_attributes_and_modifiers(attributes, modifiers)?,
                    ));
                }
                _ => {}
            }
        }

        Ok(Statement::Class(self.parse_class_with_attributes_and_modifiers(attributes, modifiers)?))
    }

    /// Whether an interface starts here: `interface`, or in PHP# `sealed interface`, spec section 20.1. Everywhere else
    /// `sealed` is a name, and `sealed` before anything but `interface` stays a parse error.
    pub(crate) fn is_at_interface(&mut self) -> Result<bool, ParseError> {
        let is_sealed =
            self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"sealed");

        Ok(match self.stream.peek_kind(0)? {
            Some(T!["interface"]) => true,
            _ => self.dialect.is_sharp() && is_sealed && self.stream.peek_kind(1)? == Some(T!["interface"]),
        })
    }

    fn parse_interface_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<Interface<'arena>, ParseError> {
        let sealed = if self.stream.is_at(T!["interface"])? { None } else { Some(self.expect_any_keyword()?) };
        let interface = self.expect_keyword(T!["interface"])?;
        let name = self.parse_local_identifier()?;
        let type_parameters = self.parse_optional_type_parameter_list()?;
        let type_parameter_names = type_parameter_names(type_parameters.as_ref());

        Ok(Interface {
            attribute_lists: attributes,
            modifiers,
            sealed,
            interface,
            name,
            type_parameters,
            extends: self.parse_optional_extends()?,
            inheritance: self.parse_optional_inheritance()?,
            left_brace: self.stream.eat_span(T!["{"])?,
            members: self.parse_class_like_members("An interface", Some(&name), type_parameter_names)?,
            right_brace: self.stream.eat_span(T!["}"])?,
        })
    }

    pub(crate) fn parse_class_with_attributes(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
    ) -> Result<Class<'arena>, ParseError> {
        let modifiers = self.parse_modifier_sequence()?;

        self.parse_class_with_attributes_and_modifiers(attributes, modifiers)
    }

    fn parse_class_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<Class<'arena>, ParseError> {
        let class = self.expect_keyword(T!["class"])?;
        let name = self.parse_local_identifier()?;
        let type_parameters = self.parse_optional_type_parameter_list()?;
        let type_parameter_names = type_parameter_names(type_parameters.as_ref());

        Ok(Class {
            attribute_lists: attributes,
            modifiers,
            class,
            name,
            type_parameters,
            extends: self.parse_optional_extends()?,
            implements: self.parse_optional_implements()?,
            inheritance: self.parse_optional_inheritance()?,
            left_brace: self.stream.eat_span(T!["{"])?,
            members: self.parse_class_like_members("A class", Some(&name), type_parameter_names)?,
            right_brace: self.stream.eat_span(T!["}"])?,
        })
    }

    pub(crate) fn parse_anonymous_class(&mut self) -> Result<AnonymousClass<'arena>, ParseError> {
        Ok(AnonymousClass {
            new: self.expect_keyword(T!["new"])?,
            attribute_lists: self.parse_attribute_list_sequence()?,
            modifiers: self.parse_modifier_sequence()?,
            class: self.expect_keyword(T!["class"])?,
            argument_list: self.parse_optional_partial_argument_list()?,
            extends: self.parse_optional_extends()?,
            implements: self.parse_optional_implements()?,
            left_brace: self.stream.eat_span(T!["{"])?,
            members: self.parse_class_like_members("A class", None, None)?,
            right_brace: self.stream.eat_span(T!["}"])?,
        })
    }

    pub(crate) fn parse_trait_with_attributes(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
    ) -> Result<Trait<'arena>, ParseError> {
        self.parse_trait_with_attributes_and_modifiers(attributes, Sequence::empty())
    }

    fn parse_trait_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<Trait<'arena>, ParseError> {
        let r#trait = self.expect_keyword(T!["trait"])?;
        let name = self.parse_local_identifier()?;

        Ok(Trait {
            attribute_lists: attributes,
            modifiers,
            r#trait,
            name,
            left_brace: self.stream.eat_span(T!["{"])?,
            members: self.parse_class_like_members("A trait", Some(&name), None)?,
            right_brace: self.stream.eat_span(T!["}"])?,
        })
    }

    pub(crate) fn parse_enum_with_attributes(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
    ) -> Result<Enum<'arena>, ParseError> {
        self.parse_enum_with_attributes_and_modifiers(attributes, Sequence::empty())
    }

    fn parse_enum_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<Enum<'arena>, ParseError> {
        let r#enum = self.expect_keyword(T!["enum"])?;
        let name = self.parse_local_identifier()?;
        let type_parameters = self.parse_optional_type_parameter_list()?;
        let type_parameter_names = type_parameter_names(type_parameters.as_ref());
        let (backing_type_hint, inheritance) = self.parse_enum_header()?;

        Ok(Enum {
            attribute_lists: attributes,
            modifiers,
            r#enum,
            name,
            type_parameters,
            backing_type_hint,
            inheritance,
            implements: self.parse_optional_implements()?,
            left_brace: self.stream.eat_span(T!["{"])?,
            members: self.parse_class_like_members("An enum", Some(&name), type_parameter_names)?,
            right_brace: self.stream.eat_span(T!["}"])?,
        })
    }

    /// The backing type after `:`, as PHP writes `: string`, and in PHP# the interfaces, as in `: string, HasLabel` or
    /// `: HasLabel`. A leading class name, with or without type arguments, opens the interfaces, and any other type is
    /// the backing type, which `check_enum` holds to `int` or `string`.
    fn parse_enum_header(
        &mut self,
    ) -> Result<(Option<EnumBackingTypeHint<'arena>>, Option<Inheritance<'arena>>), ParseError> {
        if !matches!(self.stream.peek_kind(0)?, Some(T![":"])) {
            return Ok((None, None));
        }

        let colon = self.stream.consume_span()?;
        let hint = self.parse_type_hint()?;
        if !self.dialect.is_sharp() {
            return Ok((Some(EnumBackingTypeHint { colon, hint }), None));
        }

        if matches!(hint, Hint::Identifier(_) | Hint::Generic(_)) {
            return Ok((None, Some(self.parse_inheritance_from(colon, hint)?)));
        }

        let backing_type_hint = Some(EnumBackingTypeHint { colon, hint });
        if !matches!(self.stream.peek_kind(0)?, Some(T![","])) {
            return Ok((backing_type_hint, None));
        }

        let comma = self.stream.consume_span()?;
        let first = self.parse_type_hint()?;

        Ok((backing_type_hint, Some(self.parse_inheritance_from(comma, first)?)))
    }
}

/// The names of the type parameters `list` declares, as `T` or `TKey, TValue`, or `None` when it declares none.
pub(crate) fn type_parameter_names(list: Option<&TypeParameterList<'_>>) -> Option<std::boxed::Box<str>> {
    list.map(|list| {
        let names = list.parameters.iter().map(|parameter| String::from_utf8_lossy(parameter.name.value));

        names.collect::<std::vec::Vec<_>>().join(", ").into_boxed_str()
    })
}
