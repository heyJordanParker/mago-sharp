use crate::T;
use crate::cst::cst::AttributeList;
use crate::cst::cst::EnumCase;
use crate::cst::cst::EnumCaseBackedItem;
use crate::cst::cst::EnumCaseDataItem;
use crate::cst::cst::EnumCaseItem;
use crate::cst::cst::EnumCaseUnitItem;
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
        Ok(EnumCase {
            attribute_lists: attributes,
            case: self.expect_keyword(T!["case"])?,
            item: self.parse_enum_case_item()?,
            terminator: self.parse_terminator()?,
        })
    }

    fn parse_enum_case_item(&mut self) -> Result<EnumCaseItem<'arena>, ParseError<'arena>> {
        let name = self.parse_local_identifier()?;
        if self.dialect.is_sharp() && self.stream.is_at(T!["("])? {
            let parameter_list = self.parse_function_like_parameter_list()?;

            return Ok(EnumCaseItem::Data(EnumCaseDataItem { name, parameter_list }));
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
