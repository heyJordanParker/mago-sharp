use crate::T;
use crate::cst::cst::AttributeList;
use crate::cst::cst::EnumCase;
use crate::cst::cst::EnumCaseBackedItem;
use crate::cst::cst::EnumCaseBacking;
use crate::cst::cst::EnumCaseBody;
use crate::cst::cst::EnumCaseDataItem;
use crate::cst::cst::EnumCaseItem;
use crate::cst::cst::EnumCaseMembers;
use crate::cst::cst::EnumCaseUnitItem;
use crate::cst::cst::Modifier;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_enum_case_with_attributes(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
    ) -> Result<EnumCase<'arena>, ParseError<'arena>> {
        self.parse_enum_case_with_attributes_and_modifiers(attributes, Sequence::empty())
    }

    /// A case, after its attributes and, in PHP#, a machine case's `final`, spec section 20.2. A PHP# case takes a
    /// block of members in place of its `;`, which the checks keep to a machine's cases.
    pub(crate) fn parse_enum_case_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<EnumCase<'arena>, ParseError<'arena>> {
        let case = self.expect_keyword(T!["case"])?;
        let item = self.parse_enum_case_item()?;
        let body = if self.dialect.is_sharp() && self.stream.is_at(T!["{"])? {
            EnumCaseBody::Members(EnumCaseMembers {
                left_brace: self.stream.eat_span(T!["{"])?,
                members: self.parse_class_like_members("A case", Some(item.name()), None)?,
                right_brace: self.stream.eat_span(T!["}"])?,
            })
        } else {
            EnumCaseBody::Terminator(self.parse_terminator()?)
        };

        Ok(EnumCase { attribute_lists: attributes, modifiers, case, item, body })
    }

    fn parse_enum_case_item(&mut self) -> Result<EnumCaseItem<'arena>, ParseError<'arena>> {
        let name = self.parse_local_identifier()?;
        if self.dialect.is_sharp() && self.stream.is_at(T!["("])? {
            let parameter_list = self.parse_function_like_parameter_list()?;
            let backing = if self.stream.is_at(T!["="])? {
                Some(EnumCaseBacking { equals: self.stream.eat_span(T!["="])?, value: self.parse_expression()? })
            } else {
                None
            };

            return Ok(EnumCaseItem::Data(EnumCaseDataItem { name, parameter_list, backing }));
        }

        Ok(match self.stream.peek_kind(0)? {
            Some(T!["="]) => {
                let equals = self.stream.eat_span(T!["="])?;
                let value = self.parse_expression()?;

                EnumCaseItem::Backed(EnumCaseBackedItem { name, equals, value })
            }
            _ => EnumCaseItem::Unit(EnumCaseUnitItem { name }),
        })
    }
}
