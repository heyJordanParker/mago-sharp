use strum::Display;

use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::cst::attribute::AttributeList;
use crate::cst::cst::block::Block;
use crate::cst::cst::expression::Expression;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::identifier::LocalIdentifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::modifier::Modifier;
use crate::cst::cst::terminator::Terminator;
use crate::cst::cst::type_hint::Hint;
use crate::cst::cst::variable::DirectVariable;

use crate::cst::sequence::Sequence;
use crate::cst::sequence::TokenSeparatedSequence;

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum Property<'arena> {
    Plain(PlainProperty<'arena>),
    Hooked(HookedProperty<'arena>),
    Computed(ComputedProperty<'arena>),
}

/// Represents a class-like property declaration in PHP.
///
/// # Examples
///
/// ```php
/// <?php
///
/// class Example {
///    public $foo;
///    protected $bar = 42;
/// }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PlainProperty<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    pub modifiers: Sequence<'arena, Modifier<'arena>>,
    pub var: Option<Keyword<'arena>>,
    pub hint: Option<Hint<'arena>>,
    pub items: TokenSeparatedSequence<'arena, PropertyItem<'arena>>,
    pub terminator: Terminator<'arena>,
}

/// Represents a class-like property declaration in PHP with hooks.
///
/// # Examples
///
/// ```php
/// <?php
///
/// class Example {
///   private $_foo;
///
///   public $foo {
///     get() {
///        return $this->_foo;
///     }
///     set($value) {
///       $this->_foo = $value;
///     }
///   }
/// }
/// ```
///
/// A PHP# auto-property writes its initial value after its accessors, as C# does:
///
/// ```csharp
/// public int views { get; private set; } = 0;
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct HookedProperty<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    pub modifiers: Sequence<'arena, Modifier<'arena>>,
    pub var: Option<Keyword<'arena>>,
    pub hint: Option<Hint<'arena>>,
    pub item: PropertyItem<'arena>,
    pub hook_list: PropertyHookList<'arena>,
    pub initial_value: Option<PropertyInitialValue<'arena>>,
}

/// A PHP# computed property, which runs its expression on every read, as PHP's `get => expr;` hook does:
///
/// ```csharp
/// public string slug => Str.slug(name);
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ComputedProperty<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    pub modifiers: Sequence<'arena, Modifier<'arena>>,
    pub hint: Option<Hint<'arena>>,
    pub variable: DirectVariable<'arena>,
    pub body: PropertyHookConcreteExpressionBody<'arena>,
}

/// The initial value a PHP# auto-property writes after its accessors: `= 0;`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PropertyInitialValue<'arena> {
    pub equals: Span,
    pub value: &'arena Expression<'arena>,
    pub semicolon: Span,
}

/// Represents a property item in a class-like property declaration in PHP.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum PropertyItem<'arena> {
    Abstract(PropertyAbstractItem<'arena>),
    Concrete(PropertyConcreteItem<'arena>),
}

/// Represents an abstract property item in a class-like property declaration in PHP.
///
/// # Examples
///
/// ```php
/// <?php
///
/// class Example {
///    public $foo;
/// }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PropertyAbstractItem<'arena> {
    pub variable: DirectVariable<'arena>,
}

/// Represents a concrete property item in a class-like property declaration in PHP.
///
/// # Examples
///
/// ```php
/// <?php
///
/// class Example {
///   public $foo = 42;
/// }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PropertyConcreteItem<'arena> {
    pub variable: DirectVariable<'arena>,
    pub equals: Span,
    pub value: &'arena Expression<'arena>,
}

/// Represents a list of property hooks in a class-like property declaration in PHP.
///
/// # Examples
///
/// ```php
/// <?php
///
/// class Example {
///   public $foo {
///     get() {
///       return $this->bar;
///     }
///     set($value) {
///       $this->bar = $value;
///     }
///   }
/// }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PropertyHookList<'arena> {
    pub left_brace: Span,
    pub hooks: Sequence<'arena, PropertyHook<'arena>>,
    pub right_brace: Span,
}

/// Represents a property hook in a class-like property declaration in PHP.
///
/// # Examples
///
/// ```php
/// <?php
///
/// class Example {
///   public $foo {
///     get() {
///       return $this->bar;
///     }
///   }
/// }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PropertyHook<'arena> {
    pub attribute_lists: Sequence<'arena, AttributeList<'arena>>,
    pub modifiers: Sequence<'arena, Modifier<'arena>>,
    pub ampersand: Option<Span>,
    pub name: LocalIdentifier<'arena>,
    pub parameter_list: Option<FunctionLikeParameterList<'arena>>,
    pub body: PropertyHookBody<'arena>,
}

/// Represents the body of a property hook in a class-like property declaration in PHP.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum PropertyHookBody<'arena> {
    Abstract(PropertyHookAbstractBody),
    Concrete(PropertyHookConcreteBody<'arena>),
}

/// Represents an abstract body of a property hook in a class-like property declaration in PHP.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PropertyHookAbstractBody {
    pub semicolon: Span,
}

/// Represents a concrete body of a property hook in a class-like property declaration in PHP.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum PropertyHookConcreteBody<'arena> {
    Block(Block<'arena>),
    Expression(PropertyHookConcreteExpressionBody<'arena>),
}

/// Represents an expression body of a property hook in a class-like property declaration in PHP.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PropertyHookConcreteExpressionBody<'arena> {
    pub arrow: Span,
    pub expression: &'arena Expression<'arena>,
    pub semicolon: Span,
}

impl<'arena> Property<'arena> {
    #[must_use]
    pub fn modifiers(&self) -> &Sequence<'arena, Modifier<'arena>> {
        match &self {
            Property::Hooked(h) => &h.modifiers,
            Property::Plain(p) => &p.modifiers,
            Property::Computed(c) => &c.modifiers,
        }
    }

    #[must_use]
    pub fn var(&self) -> Option<&Keyword<'arena>> {
        match &self {
            Property::Hooked(h) => h.var.as_ref(),
            Property::Plain(p) => p.var.as_ref(),
            Property::Computed(_) => None,
        }
    }

    /// Returns the first variable in this property declaration.
    ///
    /// # Panics
    ///
    /// Panics if the property declaration has no variables. This indicates a bug in the parser,
    /// as valid PHP properties must have at least one variable.
    #[must_use]
    #[allow(clippy::expect_used)]
    pub fn first_variable(&self) -> &DirectVariable<'arena> {
        self.variables()
            .first()
            .expect("expected property to have at least 1 item. this is a bug in mago. please report it.")
    }

    pub fn variables(&self) -> Vec<&DirectVariable<'arena>> {
        match &self {
            Property::Plain(inner) => inner.items.iter().map(PropertyItem::variable).collect(),
            Property::Hooked(inner) => vec![inner.item.variable()],
            Property::Computed(inner) => vec![&inner.variable],
        }
    }

    #[must_use]
    pub fn hint(&self) -> Option<&Hint<'arena>> {
        match &self {
            Property::Hooked(h) => h.hint.as_ref(),
            Property::Plain(p) => p.hint.as_ref(),
            Property::Computed(c) => c.hint.as_ref(),
        }
    }

    /// Returns the initial value of the first property: its default, or the PHP# initial value written after the
    /// accessors of an auto-property.
    #[must_use]
    pub fn initial_value(&self) -> Option<&'arena Expression<'arena>> {
        match &self {
            Property::Plain(plain) => match plain.items.first() {
                Some(PropertyItem::Concrete(item)) => Some(item.value),
                _ => None,
            },
            Property::Hooked(hooked) => match &hooked.item {
                PropertyItem::Concrete(item) => Some(item.value),
                PropertyItem::Abstract(_) => hooked.initial_value.as_ref().map(|initial_value| initial_value.value),
            },
            Property::Computed(_) => None,
        }
    }
}

impl PropertyHookList<'_> {
    /// Returns `true` when the list has no `set` hook, which makes a PHP# auto-property get-only (spec section 6.1).
    #[must_use]
    pub fn is_get_only(&self) -> bool {
        !self.hooks.iter().any(|hook| hook.name.value == b"set")
    }
}

impl<'arena> PropertyItem<'arena> {
    #[must_use]
    pub fn variable(&self) -> &DirectVariable<'arena> {
        match &self {
            PropertyItem::Abstract(item) => &item.variable,
            PropertyItem::Concrete(item) => &item.variable,
        }
    }
}

impl HasSpan for Property<'_> {
    fn span(&self) -> Span {
        match &self {
            Property::Plain(inner) => inner.span(),
            Property::Hooked(inner) => inner.span(),
            Property::Computed(inner) => inner.span(),
        }
    }
}

impl HasSpan for ComputedProperty<'_> {
    fn span(&self) -> Span {
        let start = match (self.attribute_lists.first(), self.modifiers.first(), &self.hint) {
            (Some(attribute_list), _, _) => attribute_list.span(),
            (None, Some(modifier), _) => modifier.span(),
            (None, None, Some(hint)) => hint.span(),
            (None, None, None) => self.variable.span(),
        };

        Span::between(start, self.body.span())
    }
}

impl HasSpan for PlainProperty<'_> {
    fn span(&self) -> Span {
        if let Some(attribute_list) = self.attribute_lists.first() {
            return attribute_list.span().join(self.terminator.span());
        }

        match (self.modifiers.first(), &self.var) {
            (Some(modifier), Some(var)) => {
                if var.span().start < modifier.span().start {
                    return var.span().join(self.terminator.span());
                }

                return modifier.span().join(self.terminator.span());
            }
            (Some(modifier), _) => return modifier.span().join(self.terminator.span()),
            (_, Some(var)) => return var.span().join(self.terminator.span()),
            _ => {}
        }

        if let Some(type_hint) = &self.hint {
            return type_hint.span().join(self.terminator.span());
        }

        if let Some(item) = self.items.first() {
            return item.span().join(self.terminator.span());
        }

        self.terminator.span()
    }
}

impl HasSpan for HookedProperty<'_> {
    fn span(&self) -> Span {
        let end = self.initial_value.as_ref().map_or_else(|| self.hook_list.span(), HasSpan::span);

        if let Some(attribute_list) = self.attribute_lists.first() {
            return Span::between(attribute_list.span(), end);
        }

        match (self.modifiers.first(), &self.var) {
            (Some(modifiers), Some(var)) => {
                if var.span().start < modifiers.span().start {
                    return Span::between(var.span(), end);
                }

                return Span::between(modifiers.span(), end);
            }
            (Some(modifiers), _) => return Span::between(modifiers.span(), end),
            (_, Some(var)) => return Span::between(var.span(), end),
            _ => {}
        }

        if let Some(type_hint) = &self.hint {
            return Span::between(type_hint.span(), end);
        }

        Span::between(self.item.span(), end)
    }
}

impl HasSpan for PropertyInitialValue<'_> {
    fn span(&self) -> Span {
        Span::between(self.equals, self.semicolon)
    }
}

impl HasSpan for PropertyItem<'_> {
    fn span(&self) -> Span {
        match self {
            PropertyItem::Abstract(item) => item.span(),
            PropertyItem::Concrete(item) => item.span(),
        }
    }
}

impl HasSpan for PropertyAbstractItem<'_> {
    fn span(&self) -> Span {
        self.variable.span()
    }
}

impl HasSpan for PropertyConcreteItem<'_> {
    fn span(&self) -> Span {
        Span::between(self.variable.span(), self.value.span())
    }
}

impl HasSpan for PropertyHookList<'_> {
    fn span(&self) -> Span {
        Span::between(self.left_brace, self.right_brace)
    }
}

impl HasSpan for PropertyHook<'_> {
    fn span(&self) -> Span {
        if let Some(attributes) = self.attribute_lists.first() {
            return Span::between(attributes.span(), self.body.span());
        }

        if let Some(modifier) = self.modifiers.first() {
            return Span::between(modifier.span(), self.body.span());
        }

        if let Some(ampersand) = self.ampersand {
            return Span::between(ampersand, self.body.span());
        }

        Span::between(self.name.span(), self.body.span())
    }
}

impl HasSpan for PropertyHookBody<'_> {
    fn span(&self) -> Span {
        match self {
            PropertyHookBody::Abstract(body) => body.span(),
            PropertyHookBody::Concrete(body) => body.span(),
        }
    }
}

impl HasSpan for PropertyHookAbstractBody {
    fn span(&self) -> Span {
        self.semicolon
    }
}

impl HasSpan for PropertyHookConcreteBody<'_> {
    fn span(&self) -> Span {
        match self {
            PropertyHookConcreteBody::Block(body) => body.span(),
            PropertyHookConcreteBody::Expression(body) => body.span(),
        }
    }
}

impl HasSpan for PropertyHookConcreteExpressionBody<'_> {
    fn span(&self) -> Span {
        Span::between(self.arrow, self.semicolon)
    }
}
