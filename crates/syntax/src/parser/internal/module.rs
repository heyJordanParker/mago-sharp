use mago_allocator::prelude::*;
use mago_span::HasSpan;

use crate::T;
use crate::cst::cst::ArrayElement;
use crate::cst::cst::Binding;
use crate::cst::cst::BindingClasses;
use crate::cst::cst::BindingTarget;
use crate::cst::cst::BindingValue;
use crate::cst::cst::ClassLikeMember;
use crate::cst::cst::ConstantAccess;
use crate::cst::cst::Expression;
use crate::cst::cst::FunctionLikeParameterList;
use crate::cst::cst::Identifier;
use crate::cst::cst::Module;
use crate::cst::cst::ModuleMember;
use crate::cst::cst::ModuleParent;
use crate::cst::sequence::Sequence;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    /// Parses a PHP# module, spec section 32: `module`, the module it overrides after `:`, and its members in braces.
    /// A member is a service binding or a law, spec section 28. It reads any other member, a statement or a class
    /// member, drops it, and reports it once at its span.
    pub(crate) fn parse_module(&mut self) -> Result<Module<'arena>, ParseError> {
        let module = self.expect_any_keyword()?;
        let parent = if self.stream.is_at(T![":"])? {
            Some(ModuleParent { colon: self.stream.consume_span()?, name: self.parse_dotted_identifier()? })
        } else {
            None
        };

        let left_brace = self.stream.eat_span(T!["{"])?;
        let mut members = self.new_vec();
        while let Some(token) = self.stream.lookahead(0)?
            && token.kind != T!["}"]
        {
            let position_before = self.stream.current_position();
            if self.is_at_binding()? {
                match self.parse_binding() {
                    Ok(binding) => members.push(ModuleMember::Binding(self.arena.alloc(binding))),
                    Err(error) => self.errors.push(error),
                }
            } else if self.is_at_local_declaration()? {
                let statement = self.parse_statement()?;
                self.errors.push(ParseError::OtherMemberInSharpModule(statement.span()));
            } else {
                self.state.members_of = "A module";
                match self.parse_classlike_member() {
                    Ok(ClassLikeMember::Law(law)) => members.push(ModuleMember::Law(law)),
                    Ok(member) => self.errors.push(ParseError::OtherMemberInSharpModule(member.span())),
                    Err(error) => self.errors.push(error),
                }
            }
            if self.stream.current_position() == position_before {
                self.errors.push(self.stream.unexpected(Some(token), &[]));
                self.stream.consume()?;
            }
        }

        Ok(Module {
            module,
            parent,
            left_brace,
            members: Sequence::new(members),
            right_brace: self.stream.eat_span(T!["}"])?,
        })
    }

    /// Whether a service binding starts here: `singleton`, `scoped` or `transient`, which stay names everywhere else,
    /// followed by the service's type.
    fn is_at_binding(&mut self) -> Result<bool, ParseError> {
        let starts_with_lifetime = self.stream.lookahead(0)?.is_some_and(|token| {
            token.kind == T![Identifier] && matches!(token.value, b"singleton" | b"scoped" | b"transient")
        });

        Ok(starts_with_lifetime && self.stream.peek_kind(1)?.is_some_and(|kind| kind.is_identifier_maybe_reserved()))
    }

    /// Parses a service binding, spec section 32: its lifetime, the service's type, the services it needs as
    /// parameters, `for` and the one class it serves, `=`, its value and `;`.
    fn parse_binding(&mut self) -> Result<Binding<'arena>, ParseError> {
        let lifetime = self.expect_any_keyword()?;
        let service = self.parse_type_hint()?;
        let parameter_list =
            if self.stream.is_at(T!["("])? { Some(self.parse_function_like_parameter_list()?) } else { None };
        let target = if self.stream.is_at(T!["for"])? {
            Some(BindingTarget { r#for: self.expect_any_keyword()?, class: self.parse_identifier()? })
        } else {
            None
        };
        let equals = self.stream.eat_span(T!["="])?;
        let value = self.parse_binding_value(parameter_list.as_ref())?;

        Ok(Binding { lifetime, service, parameter_list, target, equals, value, terminator: self.parse_terminator()? })
    }

    /// Parses a binding's value: a class when it is a bare name, or a list of bare names in brackets, that names no
    /// parameter of the binding, and otherwise one expression. A bare class name is no PHP# expression, so the two
    /// never overlap.
    fn parse_binding_value(
        &mut self,
        parameter_list: Option<&FunctionLikeParameterList<'arena>>,
    ) -> Result<BindingValue<'arena>, ParseError> {
        let expression = self.parse_expression()?;
        let class = |expression: &Expression<'arena>| match expression {
            Expression::ConstantAccess(ConstantAccess { name: Identifier::Local(name) })
                if !parameter_list.is_some_and(|list| {
                    list.parameters.iter().any(|parameter| {
                        parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name) == name.value
                    })
                }) =>
            {
                Some(Identifier::Local(*name))
            }
            _ => None,
        };

        if let Some(class) = class(expression) {
            return Ok(BindingValue::Class(class));
        }
        if let Expression::Array(array) = expression
            && !array.elements.is_empty()
            && array.colon.is_none()
        {
            let mut classes = self.new_vec();
            for element in &array.elements {
                match element {
                    ArrayElement::Value(element) => match class(element.value) {
                        Some(class) => classes.push(class),
                        None => return Ok(BindingValue::Expression(expression)),
                    },
                    _ => return Ok(BindingValue::Expression(expression)),
                }
            }

            return Ok(BindingValue::Classes(BindingClasses {
                left_bracket: array.left_bracket,
                classes: TokenSeparatedSequence::from_slices(classes.leak(), array.elements.tokens),
                right_bracket: array.right_bracket,
            }));
        }

        Ok(BindingValue::Expression(expression))
    }
}
