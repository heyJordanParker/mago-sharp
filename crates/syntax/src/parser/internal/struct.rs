use mago_allocator::prelude::*;
use mago_span::HasSpan;

use crate::T;
use crate::cst::cst::AttributeList;
use crate::cst::cst::Hint;
use crate::cst::cst::Inheritance;
use crate::cst::cst::Modifier;
use crate::cst::cst::Struct;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::parser::internal::class_like::type_parameter_names;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether a PHP# struct starts here: `struct`, a name, and `:`, `{` or the `<` of type parameters. The name may
    /// be a reserved word, as a class's may, and the checks refuse it there. Everywhere else `struct` is a name, such
    /// as a local `struct`.
    pub(crate) fn is_at_struct(&mut self) -> Result<bool, ParseError> {
        let starts_with_struct =
            self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"struct");
        let names_a_struct =
            self.stream.peek_kind(1)?.is_some_and(|kind| kind.is_identifier_maybe_reserved() && !kind.is_infix());

        Ok(self.dialect.is_sharp()
            && starts_with_struct
            && names_a_struct
            && matches!(self.stream.peek_kind(2)?, Some(T![":" | "{" | "<"])))
    }

    /// Parses a PHP# struct, spec section 10: `struct`, its name, the type parameters a class takes, the header an enum
    /// uses and class members.
    pub(crate) fn parse_struct_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<Struct<'arena>, ParseError> {
        let r#struct = self.expect_any_keyword()?;
        let name = self.parse_local_identifier()?;
        let type_parameters = self.parse_optional_type_parameter_list()?;
        let type_parameter_names = type_parameter_names(type_parameters.as_ref());

        Ok(Struct {
            attribute_lists: attributes,
            modifiers,
            r#struct,
            name,
            type_parameters,
            inheritance: self.parse_struct_header()?,
            left_brace: self.stream.eat_span(T!["{"])?,
            members: self.parse_class_like_members("A struct", Some(&name), type_parameter_names)?,
            right_brace: self.stream.eat_span(T!["}"])?,
        })
    }

    /// A struct's header, `: Interface, Other`, each a class name with or without type arguments. A type that names no
    /// interface, such as `string`, is refused where it stands, and the interfaces after it still parse.
    fn parse_struct_header(&mut self) -> Result<Option<Inheritance<'arena>>, ParseError> {
        if !self.stream.is_at(T![":"])? {
            return Ok(None);
        }

        let mut separator = self.stream.eat_span(T![":"])?;
        loop {
            match self.parse_type_hint()? {
                first @ (Hint::Identifier(_) | Hint::Generic(_)) => {
                    return Ok(Some(self.parse_inheritance_from(separator, first)?));
                }
                hint => self.errors.push(ParseError::StructHeaderTypeInSharp(hint.span())),
            }
            if !self.stream.is_at(T![","])? {
                return Ok(None);
            }
            separator = self.stream.eat_span(T![","])?;
        }
    }
}
