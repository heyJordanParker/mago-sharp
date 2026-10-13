use crate::T;
use crate::cst::cst::DirectVariable;
use crate::cst::cst::FunctionLikeParameter;
use crate::cst::cst::FunctionLikeParameterDefaultValue;
use crate::cst::cst::FunctionLikeParameterList;
use crate::cst::cst::Hint;
use crate::cst::cst::Identifier;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_optional_function_like_parameter_list(
        &mut self,
    ) -> Result<Option<FunctionLikeParameterList<'arena>>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T!["("]) => Some(self.parse_function_like_parameter_list()?),
            _ => None,
        })
    }

    pub(crate) fn parse_function_like_parameter_list(
        &mut self,
    ) -> Result<FunctionLikeParameterList<'arena>, ParseError> {
        let result = self.parse_comma_separated_sequence(T!["("], T![")"], Self::parse_function_like_parameter)?;

        Ok(FunctionLikeParameterList {
            left_parenthesis: result.open,
            parameters: result.sequence,
            right_parenthesis: result.close,
        })
    }

    /// Parses a PHP# lambda's parameters, which may leave out their types: `(a, b)` or `(Order order)`.
    pub(crate) fn parse_lambda_parameter_list(&mut self) -> Result<FunctionLikeParameterList<'arena>, ParseError> {
        let result = self.parse_comma_separated_sequence(T!["("], T![")"], |parser| parser.parse_parameter(false))?;

        Ok(FunctionLikeParameterList {
            left_parenthesis: result.open,
            parameters: result.sequence,
            right_parenthesis: result.close,
        })
    }

    pub(crate) fn parse_function_like_parameter(&mut self) -> Result<FunctionLikeParameter<'arena>, ParseError> {
        self.parse_parameter(true)
    }

    /// Parses a parameter. A PHP# method's parameter must have a type, and a lambda's may leave it out.
    fn parse_parameter(&mut self, typed: bool) -> Result<FunctionLikeParameter<'arena>, ParseError> {
        let attribute_lists = self.parse_attribute_list_sequence()?;
        let modifiers = self.parse_modifier_sequence()?;
        let hint = self.parse_optional_type_hint()?;

        // In PHP#, a bare name before `,`, `)` or `=` is a parameter written without its type: it is the name.
        let untyped = match &hint {
            Some(Hint::Identifier(Identifier::Local(name)))
                if self.dialect.is_sharp()
                    && matches!(self.stream.peek_kind(0)?, Some(T![","] | T![")"] | T!["="])) =>
            {
                if typed {
                    self.errors.push(ParseError::UntypedParameterInSharp(name.span));
                }

                Some(DirectVariable { span: name.span, name: name.value })
            }
            _ => None,
        };

        let parameter = FunctionLikeParameter {
            attribute_lists,
            modifiers,
            hint: if untyped.is_some() { None } else { hint },
            ampersand: if self.stream.is_at(T!["&"])? { Some(self.stream.eat_span(T!["&"])?) } else { None },
            ellipsis: if self.stream.is_at(T!["..."])? { Some(self.stream.eat_span(T!["..."])?) } else { None },
            variable: match untyped {
                Some(variable) => variable,
                None if self.dialect.is_sharp() && self.stream.is_at(T![Identifier])? => self.parse_bare_variable()?,
                None => self.parse_direct_variable()?,
            },
            default_value: self.parse_optional_function_like_parameter_default_value()?,
            hooks: self.parse_optional_property_hook_list()?,
        };
        if parameter.hooks.is_some() {
            self.skip_via_clause(true)?;
        }

        Ok(parameter)
    }

    fn parse_optional_function_like_parameter_default_value(
        &mut self,
    ) -> Result<Option<FunctionLikeParameterDefaultValue<'arena>>, ParseError> {
        Ok(if self.stream.is_at(T!["="])? {
            let equals = self.stream.eat_span(T!["="])?;
            Some(FunctionLikeParameterDefaultValue { equals, value: self.parse_expression()? })
        } else {
            None
        })
    }
}
