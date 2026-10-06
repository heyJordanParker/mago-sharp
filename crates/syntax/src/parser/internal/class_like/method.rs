use crate::T;
use crate::cst::cst::AttributeList;
use crate::cst::cst::ClassLikeMember;
use crate::cst::cst::FunctionLikeReturnTypeHint;
use crate::cst::cst::Hint;
use crate::cst::cst::Identifier;
use crate::cst::cst::Method;
use crate::cst::cst::MethodAbstractBody;
use crate::cst::cst::MethodBody;
use crate::cst::cst::MethodExpressionBody;
use crate::cst::cst::Modifier;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;
use mago_span::HasSpan;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_method_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<Method<'arena>, ParseError> {
        Ok(Method {
            attribute_lists: attributes,
            modifiers,
            function: Some(self.expect_php_keyword(T!["function"])?),
            ampersand: if self.stream.is_at(T!["&"])? { Some(self.stream.eat_span(T!["&"])?) } else { None },
            name: self.parse_local_identifier()?,
            parameter_list: self.parse_function_like_parameter_list()?,
            return_type_hint: self.parse_optional_function_like_return_type_hint()?,
            body: self.parse_method_body()?,
        })
    }

    /// Parses a PHP# class member that starts with its type: a method, whose return type comes first with no colon
    /// and no `function` keyword, or a field, `int count = 0;`. A name followed by `(` with no type before it is the
    /// constructor, `public Report(int count) {}`, a method without a return type.
    ///
    /// The type is parsed once, and the name after it decides: a name followed by `(` makes a method, anything else
    /// a field. A type has no length limit, so no fixed lookahead can decide before it. A PHP property, which starts
    /// with `var` or a `$` variable, still parses so the rest of the class does, and its PHP syntax is an error. So do
    /// a named constructor and `required` on any member but the constructor, which are one "not supported yet" error
    /// each.
    pub(crate) fn parse_sharp_member_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<ClassLikeMember<'arena>, ParseError> {
        if self.stream.is_at(T!["var"])? || self.stream.is_at(T!["$variable"])? {
            return Ok(ClassLikeMember::Property(
                self.parse_property_with_attributes_and_modifiers(attributes, modifiers)?,
            ));
        }

        // `required` written before any other modifier starts the member's modifiers.
        let modifiers = if modifiers.is_empty()
            && self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"required")
        {
            self.parse_modifier_sequence()?
        } else {
            modifiers
        };

        let hint = self.parse_type_hint()?;
        // A named constructor, `public Report.fromJson(string json) : this(…) {}` in spec section 9.1, parses whole
        // and is left out of the class, which parses on.
        if let Hint::Identifier(Identifier::Local(class)) = hint
            && self.stream.is_at(T!["."])?
            && self.stream.peek_kind(2)? == Some(T!["("])
        {
            self.stream.consume()?;
            let name = class.span.join(self.parse_local_identifier()?.span);
            self.parse_function_like_parameter_list()?;
            if self.stream.is_at(T![":"])? {
                self.stream.consume()?;
                self.parse_expression()?;
            }
            self.parse_method_body()?;

            return Err(ParseError::NotSupportedYetInSharp("A named constructor", name));
        }
        if let Hint::Identifier(Identifier::Local(name)) = hint
            && self.stream.is_at(T!["("])?
        {
            return Ok(ClassLikeMember::Method(Method {
                attribute_lists: attributes,
                modifiers,
                function: None,
                ampersand: None,
                name,
                parameter_list: self.parse_function_like_parameter_list()?,
                return_type_hint: None,
                body: self.parse_method_body()?,
            }));
        }
        // Spec section 25 marks a constructor `required`, and section 6.1 a property, which is not supported yet.
        if let Some(required) = modifiers.iter().find(|modifier| matches!(modifier, Modifier::Required(_))) {
            self.errors.push(ParseError::NotSupportedYetInSharp("`required`", required.span()));
        }
        // A bare name before `=` or `;` is a field written without its type: it is the name.
        if let Hint::Identifier(Identifier::Local(name)) = hint
            && matches!(self.stream.peek_kind(0)?, Some(T!["="] | T![";"]))
        {
            self.errors.push(ParseError::UntypedFieldInSharp(name.span));

            return Ok(ClassLikeMember::Property(self.parse_untyped_field(attributes, modifiers, name)?));
        }
        if !matches!(self.stream.peek_kind(1)?, Some(T!["("])) {
            return Ok(ClassLikeMember::Property(self.parse_property_with_hint(
                attributes,
                modifiers,
                None,
                Some(hint),
            )?));
        }

        Ok(ClassLikeMember::Method(Method {
            attribute_lists: attributes,
            modifiers,
            function: None,
            return_type_hint: Some(FunctionLikeReturnTypeHint { colon: None, hint }),
            ampersand: None,
            name: self.parse_local_identifier()?,
            parameter_list: self.parse_function_like_parameter_list()?,
            body: self.parse_method_body()?,
        }))
    }

    /// Parses a method's body. A PHP# method may also have an expression body, `=> expr;`, as spec section 7 writes it.
    fn parse_method_body(&mut self) -> Result<MethodBody<'arena>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T![";" | "?>"]) => MethodBody::Abstract(MethodAbstractBody { terminator: self.parse_terminator()? }),
            Some(T!["=>"]) if self.dialect.is_sharp() => MethodBody::Expression(MethodExpressionBody {
                arrow: self.stream.eat_span(T!["=>"])?,
                expression: self.parse_expression()?,
                semicolon: self.stream.eat_span(T![";"])?,
            }),
            _ => MethodBody::Concrete(self.parse_block()?),
        })
    }
}
