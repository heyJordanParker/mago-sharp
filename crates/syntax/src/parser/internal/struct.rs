use mago_allocator::prelude::*;

use crate::T;
use crate::cst::cst::AttributeList;
use crate::cst::cst::Modifier;
use crate::cst::cst::Struct;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Whether a PHP# struct starts here: `struct`, a name, and `:`, `{` or the `<` of type parameters. The name may
    /// be a reserved word, as a class's may, and the checks refuse it there. Everywhere else `struct` is a name, such
    /// as a local `struct`.
    pub(crate) fn is_at_struct(&mut self) -> Result<bool, ParseError<'arena>> {
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
    /// uses, its backing type and its interfaces, and class members.
    pub(crate) fn parse_struct_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<Struct<'arena>, ParseError<'arena>> {
        let r#struct = self.expect_any_keyword()?;
        let name = self.parse_local_identifier()?;
        let type_parameters = self.parse_optional_type_parameter_list()?;
        let type_parameter_names = self.type_parameter_names(type_parameters.as_ref());
        let (backing_type_hint, inheritance) = self.parse_enum_header()?;

        Ok(Struct {
            attribute_lists: attributes,
            modifiers,
            r#struct,
            name,
            type_parameters,
            backing_type_hint,
            inheritance,
            left_brace: self.stream.eat_span(T!["{"])?,
            members: self.parse_class_like_members("A struct", Some(&name), type_parameter_names)?,
            right_brace: self.stream.eat_span(T!["}"])?,
        })
    }
}
