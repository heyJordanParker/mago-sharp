use crate::T;
use crate::cst::cst::AttributeList;
use crate::cst::cst::ComputedProperty;
use crate::cst::cst::DirectVariable;
use crate::cst::cst::Hint;
use crate::cst::cst::HookedProperty;
use crate::cst::cst::Keyword;
use crate::cst::cst::LocalIdentifier;
use crate::cst::cst::Modifier;
use crate::cst::cst::PlainProperty;
use crate::cst::cst::Property;
use crate::cst::cst::PropertyAbstractItem;
use crate::cst::cst::PropertyBehaviorList;
use crate::cst::cst::PropertyConcreteItem;
use crate::cst::cst::PropertyHook;
use crate::cst::cst::PropertyHookAbstractBody;
use crate::cst::cst::PropertyHookBody;
use crate::cst::cst::PropertyHookConcreteBody;
use crate::cst::cst::PropertyHookConcreteExpressionBody;
use crate::cst::cst::PropertyHookList;
use crate::cst::cst::PropertyInitialValue;
use crate::cst::cst::PropertyItem;
use crate::cst::sequence::Sequence;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::parser::Parser;
use mago_allocator::prelude::*;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_property_with_attributes_and_modifiers(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
    ) -> Result<Property<'arena>, ParseError> {
        let var = if self.stream.is_at(T!["var"])? { Some(self.expect_php_keyword(T!["var"])?) } else { None };
        let hint = self.parse_optional_type_hint()?;

        self.parse_property_with_hint(attributes, modifiers, var, hint)
    }

    /// Parses the rest of a property once its `var` keyword and type hint are parsed.
    pub(crate) fn parse_property_with_hint(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
        var: Option<Keyword<'arena>>,
        hint: Option<Hint<'arena>>,
    ) -> Result<Property<'arena>, ParseError> {
        // A computed property, `public string slug => expr;` in spec section 6.1.
        if self.dialect.is_sharp() && self.stream.peek_kind(1)? == Some(T!["=>"]) {
            return Ok(Property::Computed(ComputedProperty {
                attribute_lists: attributes,
                modifiers,
                hint,
                variable: self.parse_property_variable()?,
                body: self.parse_property_hook_concrete_expression_body()?,
            }));
        }

        let item = self.parse_property_item()?;

        let next = self.stream.peek_kind(0)?;
        if matches!(next, Some(T!["{"])) {
            return self.parse_hooked_property(attributes, modifiers, var, hint, item);
        }

        Ok(Property::Plain(PlainProperty {
            attribute_lists: attributes,
            modifiers,
            var,
            hint,
            items: {
                let mut items = self.new_vec_of(item);
                let mut commas = self.new_vec();
                if matches!(next, Some(T![","])) {
                    commas.push(self.stream.consume()?);

                    loop {
                        let item = self.parse_property_item()?;
                        items.push(item);

                        match self.stream.peek_kind(0)? {
                            Some(T![","]) => {
                                commas.push(self.stream.consume()?);
                            }
                            _ => {
                                break;
                            }
                        }
                    }
                }

                TokenSeparatedSequence::new(items, commas)
            },
            terminator: self.parse_terminator()?,
        }))
    }

    /// Parses the rest of a PHP# field written without its type, whose name the caller parsed as a type.
    pub(crate) fn parse_untyped_field(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
        name: LocalIdentifier<'arena>,
    ) -> Result<Property<'arena>, ParseError> {
        let variable = DirectVariable { span: name.span, name: name.value };
        let item = if self.stream.is_at(T!["="])? {
            PropertyItem::Concrete(PropertyConcreteItem {
                variable,
                equals: self.stream.eat_span(T!["="])?,
                value: self.parse_expression()?,
            })
        } else {
            PropertyItem::Abstract(PropertyAbstractItem { variable })
        };

        Ok(Property::Plain(PlainProperty {
            attribute_lists: attributes,
            modifiers,
            var: None,
            hint: None,
            items: TokenSeparatedSequence::new(self.new_vec_of(item), self.new_vec()),
            terminator: self.parse_terminator()?,
        }))
    }

    /// Parses the rest of a PHP# property written without its type, whose name the caller parsed as a type.
    pub(crate) fn parse_untyped_property(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
        name: LocalIdentifier<'arena>,
    ) -> Result<Property<'arena>, ParseError> {
        let item = PropertyItem::Abstract(PropertyAbstractItem {
            variable: DirectVariable { span: name.span, name: name.value },
        });

        self.parse_hooked_property(attributes, modifiers, None, None, item)
    }

    /// Parses a property's accessors, from its `{`, and the PHP# initial value after them.
    fn parse_hooked_property(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
        modifiers: Sequence<'arena, Modifier<'arena>>,
        var: Option<Keyword<'arena>>,
        hint: Option<Hint<'arena>>,
        item: PropertyItem<'arena>,
    ) -> Result<Property<'arena>, ParseError> {
        let hook_list = self.parse_property_hook_list()?;
        let behaviors = self.parse_optional_behavior_list()?;
        let initial_value = if self.dialect.is_sharp() && self.stream.is_at(T!["="])? {
            Some(PropertyInitialValue {
                equals: self.stream.eat_span(T!["="])?,
                value: self.parse_expression()?,
                semicolon: self.stream.eat_span(T![";"])?,
            })
        } else {
            None
        };

        Ok(Property::Hooked(HookedProperty {
            attribute_lists: attributes,
            modifiers,
            var,
            hint,
            item,
            hook_list,
            behaviors,
            initial_value,
        }))
    }

    fn parse_property_item(&mut self) -> Result<PropertyItem<'arena>, ParseError> {
        Ok(match self.stream.peek_kind(1)? {
            Some(T!["="]) => PropertyItem::Concrete(self.parse_property_concrete_item()?),
            _ => PropertyItem::Abstract(self.parse_property_abstract_item()?),
        })
    }

    fn parse_property_abstract_item(&mut self) -> Result<PropertyAbstractItem<'arena>, ParseError> {
        Ok(PropertyAbstractItem { variable: self.parse_property_variable()? })
    }

    fn parse_property_concrete_item(&mut self) -> Result<PropertyConcreteItem<'arena>, ParseError> {
        Ok(PropertyConcreteItem {
            variable: self.parse_property_variable()?,
            equals: self.stream.eat_span(T!["="])?,
            value: self.parse_expression()?,
        })
    }

    /// Parses the name a property declares. A PHP# field names it bare, and a `$` variable there is PHP syntax.
    fn parse_property_variable(&mut self) -> Result<DirectVariable<'arena>, ParseError> {
        if !self.dialect.is_sharp() {
            return self.parse_direct_variable();
        }

        if self.stream.peek_kind(0)?.is_some_and(|kind| kind.is_identifier_maybe_reserved()) {
            return self.parse_bare_variable();
        }

        // The error stands, and the PHP property still parses so the rest of the class does.
        let variable = self.parse_direct_variable()?;
        self.errors.push(ParseError::PhpSyntaxInSharp(T!["$variable"], variable.span));

        Ok(variable)
    }

    /// Whether a PHP# `via` clause starts here, spec section 6.4.
    fn is_at_via(&mut self) -> Result<bool, ParseError> {
        Ok(self.dialect.is_sharp()
            && self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"via"))
    }

    /// Parses a PHP# property's behaviors after its accessors, spec section 6.4: `via` and types separated by
    /// commas, ending with `;` unless an initial value follows.
    fn parse_optional_behavior_list(&mut self) -> Result<Option<PropertyBehaviorList<'arena>>, ParseError> {
        if !self.is_at_via()? {
            return Ok(None);
        }

        let via = self.stream.consume_span()?;
        let mut behaviors = self.new_vec();
        let mut commas = self.new_vec();
        behaviors.push(self.parse_type_hint()?);
        while self.stream.is_at(T![","])? {
            commas.push(self.stream.consume()?);
            behaviors.push(self.parse_type_hint()?);
        }
        let semicolon = if self.stream.is_at(T![";"])? { Some(self.stream.consume_span()?) } else { None };

        Ok(Some(PropertyBehaviorList { via, behaviors: TokenSeparatedSequence::new(behaviors, commas), semicolon }))
    }

    /// Skips a `via` clause on the constructor parameter `name`, which section 9 refuses, and reports it once where it
    /// starts. A comma also ends the parameter, so a behavior there is a name followed by `,` or `)`.
    pub(crate) fn skip_parameter_behaviors(&mut self, name: &[u8]) -> Result<(), ParseError> {
        if !self.is_at_via()? {
            return Ok(());
        }

        let via = self.stream.consume_span()?;
        let name = String::from_utf8_lossy(name).into();
        self.errors.push(ParseError::BehaviorParameterInSharp(name, via));
        self.parse_local_identifier()?;
        while self.stream.is_at(T![","])?
            && self.stream.peek_kind(1)? == Some(T![Identifier])
            && matches!(self.stream.peek_kind(2)?, Some(T![","] | T![")"]))
        {
            self.stream.consume()?;
            self.parse_local_identifier()?;
        }

        Ok(())
    }

    pub(crate) fn parse_optional_property_hook_list(&mut self) -> Result<Option<PropertyHookList<'arena>>, ParseError> {
        Ok(match self.stream.peek_kind(0)? {
            Some(T!["{"]) => Some(self.parse_property_hook_list()?),
            _ => None,
        })
    }

    fn parse_property_hook_list(&mut self) -> Result<PropertyHookList<'arena>, ParseError> {
        Ok(PropertyHookList {
            left_brace: self.stream.eat_span(T!["{"])?,
            hooks: {
                let mut hooks = self.new_vec();
                loop {
                    if matches!(self.stream.peek_kind(0)?, Some(T!["}"])) {
                        break;
                    }

                    let hook = self.parse_property_hook()?;
                    hooks.push(hook);
                }

                Sequence::new(hooks)
            },
            right_brace: self.stream.eat_span(T!["}"])?,
        })
    }

    fn parse_property_hook(&mut self) -> Result<PropertyHook<'arena>, ParseError> {
        Ok(PropertyHook {
            attribute_lists: self.parse_attribute_list_sequence()?,
            ampersand: if self.stream.is_at(T!["&"])? { Some(self.stream.eat_span(T!["&"])?) } else { None },
            modifiers: self.parse_modifier_sequence()?,
            name: self.parse_local_identifier()?,
            parameter_list: self.parse_optional_function_like_parameter_list()?,
            body: self.parse_property_hook_body()?,
        })
    }

    fn parse_property_hook_body(&mut self) -> Result<PropertyHookBody<'arena>, ParseError> {
        let next = self.stream.lookahead(0)?.ok_or_else(|| self.stream.unexpected(None, &[]))?;

        Ok(match next.kind {
            T![";"] => PropertyHookBody::Abstract(self.parse_property_hook_abstract_body()?),
            T!["{"] | T!["=>"] => PropertyHookBody::Concrete(self.parse_property_hook_concrete_body()?),
            _ => return Err(self.stream.unexpected(Some(next), T![";", "{", "=>"])),
        })
    }

    fn parse_property_hook_abstract_body(&mut self) -> Result<PropertyHookAbstractBody, ParseError> {
        Ok(PropertyHookAbstractBody { semicolon: self.stream.eat_span(T![";"])? })
    }

    fn parse_property_hook_concrete_body(&mut self) -> Result<PropertyHookConcreteBody<'arena>, ParseError> {
        let next = self.stream.lookahead(0)?.ok_or_else(|| self.stream.unexpected(None, &[]))?;

        Ok(match next.kind {
            T!["{"] => PropertyHookConcreteBody::Block(self.parse_block()?),
            T!["=>"] => PropertyHookConcreteBody::Expression(self.parse_property_hook_concrete_expression_body()?),
            _ => return Err(self.stream.unexpected(Some(next), T!["{", "=>"])),
        })
    }

    fn parse_property_hook_concrete_expression_body(
        &mut self,
    ) -> Result<PropertyHookConcreteExpressionBody<'arena>, ParseError> {
        Ok(PropertyHookConcreteExpressionBody {
            arrow: self.stream.eat_span(T!["=>"])?,
            expression: self.parse_expression()?,
            semicolon: self.stream.eat_span(T![";"])?,
        })
    }
}
