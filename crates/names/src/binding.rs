use std::borrow::Cow;

use mago_allocator::Arena;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::ComputedProperty;
use mago_syntax::cst::Extension;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::FunctionLikeReturnTypeHint;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Keyword;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::MethodExpressionBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::ModifierSequenceExt;
use mago_syntax::cst::NamedConstructor;
use mago_syntax::cst::NullableHint;
use mago_syntax::cst::On;
use mago_syntax::cst::Operator;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::TokenSeparatedSequence;
use mago_syntax::cst::TypeParameterList;
use mago_syntax::cst::Uses;
use mago_word::Word;
use mago_word::concat_word;
use mago_word::word;

/// Whether `name`, written in a method's `uses`, names `parameter` and the parameter has a function type, nullable or
/// not, as `f` in the old `apply(Function<int(int)> f) uses f` did: a function parameter, never an effect.
fn names_function(parameter: &FunctionLikeParameter<'_>, name: &LocalIdentifier<'_>) -> bool {
    parameter.variable.name == name.value
        && matches!(
            parameter.hint,
            Some(Hint::Function(_) | Hint::Nullable(NullableHint { hint: Hint::Function(_), .. }))
        )
}

/// Whether `parameter` has a function type, nullable or not, that states no effects, as `f` in `map(Function<R(T)> f)`.
///
/// Spec section 29: each call of the method takes on the effects of the function it passes there, so the function runs
/// only inside the call. A function type that states its effects, as `Function<Charge(Cart) uses Http>`, caps them
/// instead. A promoted constructor parameter is its field, whose function type without `uses` is pure, as every
/// field's is.
#[must_use]
pub fn takes_on_effects(parameter: &FunctionLikeParameter<'_>) -> bool {
    if parameter.is_promoted_property() {
        return false;
    }

    match &parameter.hint {
        Some(Hint::Function(function)) => function.uses.is_none(),
        Some(Hint::Nullable(NullableHint { hint: Hint::Function(function), .. })) => function.uses.is_none(),
        _ => false,
    }
}

/// The superglobals PHP gives every function, which spec section 29 removes from PHP#.
pub const SUPERGLOBALS: [&[u8]; 9] =
    [b"GLOBALS", b"_SERVER", b"_GET", b"_POST", b"_FILES", b"_COOKIE", b"_SESSION", b"_REQUEST", b"_ENV"];

/// Returns the name of the PHP variable the PHP# local or parameter `name` runs as.
///
/// That is `name` itself, or `@` before a superglobal's name, as `@this` is written, which PHP code never writes, so the
/// local never reads or replaces PHP's.
#[must_use]
pub fn php_local_name(name: &[u8]) -> Cow<'_, [u8]> {
    if SUPERGLOBALS.contains(&name) { Cow::Owned([b"@", name].concat()) } else { Cow::Borrowed(name) }
}

/// Returns the PHP variable a PHP# local, parameter or `this` runs as: `total` runs as `$total`.
///
/// A name already written with `$`, which is a PHP# error of its own, is kept as written.
#[inline]
#[must_use]
pub fn php_variable_name(name: &[u8]) -> Word {
    if name.starts_with(b"$") { word(name) } else { concat_word!(b"$", name) }
}

/// Returns the PHP method a method runs as. The PHP# constructor, the one method without `function` or a return type,
/// runs as `__construct`, and every other method keeps its name.
#[inline]
#[must_use]
pub fn php_method_name<'arena>(method: &Method<'arena>) -> &'arena [u8] {
    if method.function.is_none() && method.return_type_hint.is_none() { b"__construct" } else { method.name.value }
}

/// Returns the PHP static method a PHP# operator on `operands` operands runs as, named after .NET's operator method.
///
/// `+` runs as `op_Addition`, and `-` on one operand as `op_UnaryNegation`. An operator a class cannot declare, which
/// semantics refuses, runs as none: `!=` runs `op_Equality`, and `<` and the other orderings run `op_Comparison`.
#[inline]
#[must_use]
pub fn php_operator_name(symbol: &BinaryOperator<'_>, operands: usize) -> Option<&'static [u8]> {
    Some(match symbol {
        BinaryOperator::Equal(_) => b"op_Equality",
        BinaryOperator::Spaceship(_) => b"op_Comparison",
        BinaryOperator::Addition(_) => b"op_Addition",
        BinaryOperator::Subtraction(_) if operands == 1 => b"op_UnaryNegation",
        BinaryOperator::Subtraction(_) => b"op_Subtraction",
        BinaryOperator::Multiplication(_) => b"op_Multiply",
        BinaryOperator::Division(_) => b"op_Division",
        BinaryOperator::Modulo(_) => b"op_Modulus",
        BinaryOperator::Exponentiation(_) => b"op_Exponent",
        _ => return None,
    })
}

/// Returns the symbol of the PHP# operator the PHP static method `php_name` runs as, the reverse of
/// [`php_operator_name`]: `+` for `op_Addition`, and `-` for `op_UnaryNegation` and `op_Subtraction`.
#[inline]
#[must_use]
pub fn sharp_operator_symbol(php_name: &[u8]) -> Option<&'static str> {
    Some(match php_name {
        b"op_Equality" => "==",
        b"op_Comparison" => "<=>",
        b"op_Addition" => "+",
        b"op_UnaryNegation" | b"op_Subtraction" => "-",
        b"op_Multiply" => "*",
        b"op_Division" => "/",
        b"op_Modulus" => "%",
        b"op_Exponent" => "**",
        _ => return None,
    })
}

/// The parts a method and a PHP# operator share, as the PHP method they run as: a PHP# operator is a static method.
pub struct MethodParts<'ast, 'arena> {
    /// The PHP method's name: `__construct` for the PHP# constructor, and `op_Addition` for `operator +`.
    pub name: &'arena [u8],
    /// The method's name, or the operator's symbol.
    pub name_span: Span,
    pub span: Span,
    pub attribute_lists: &'ast Sequence<'arena, AttributeList<'arena>>,
    pub modifiers: &'ast Sequence<'arena, Modifier<'arena>>,
    /// The method's PHP# type parameters. An operator and a named constructor declare none.
    pub type_parameters: Option<&'ast TypeParameterList<'arena>>,
    pub parameter_list: &'ast FunctionLikeParameterList<'arena>,
    pub return_type_hint: Option<&'ast FunctionLikeReturnTypeHint<'arena>>,
    /// The `readonly` after a method's parameters, spec section 13. An operator, a named constructor and an extension
    /// member have none: each runs as a static method, with no `this`.
    pub readonly: Option<&'ast Keyword<'arena>>,
    /// The events a method listens to. An operator and a named constructor listen to none.
    pub on: Option<&'ast On<'arena>>,
    /// The effects a method without a body allows, spec section 29. An operator and a named constructor have a body.
    pub uses: Option<&'ast Uses<'arena>>,
    pub body: &'ast MethodBody<'arena>,
    pub returns_value: bool,
    pub returns_by_reference: bool,
}

impl<'ast, 'arena> MethodParts<'ast, 'arena> {
    #[must_use]
    pub fn of_method(method: &'ast Method<'arena>) -> Self {
        Self {
            name: php_method_name(method),
            name_span: method.name.span,
            span: method.span(),
            attribute_lists: &method.attribute_lists,
            modifiers: &method.modifiers,
            type_parameters: method.type_parameters,
            parameter_list: &method.parameter_list,
            return_type_hint: method.return_type_hint.as_ref(),
            readonly: method.readonly.as_ref(),
            on: method.on,
            uses: method.uses,
            body: &method.body,
            returns_value: method.returns_value(),
            returns_by_reference: method.ampersand.is_some(),
        }
    }

    /// Whether this is `int hash()` or `string toString()`, which the language calls on a readonly value, so they are
    /// readonly by definition, spec section 13. A method of either name that takes parameters is an ordinary method.
    #[must_use]
    pub fn is_hash_or_to_string(&self) -> bool {
        let returns = |keyword: fn(&Hint<'_>) -> bool| self.return_type_hint.is_some_and(|hint| keyword(&hint.hint));

        !self.modifiers.contains_static()
            && self.parameter_list.parameters.is_empty()
            && match self.name {
                b"hash" => returns(|hint| matches!(hint, Hint::Integer(_))),
                b"toString" => returns(|hint| matches!(hint, Hint::String(_))),
                _ => false,
            }
    }

    /// The effects the method's `uses` names, spec section 29: every name but one of its function-typed parameters,
    /// which the parser refuses there.
    pub fn uses_effects(&self) -> impl Iterator<Item = &'ast LocalIdentifier<'arena>> + '_ {
        self.uses
            .into_iter()
            .flat_map(|uses| uses.names.iter())
            .filter(move |name| !self.parameter_list.parameters.iter().any(|parameter| names_function(parameter, name)))
    }

    /// The parts of an operator, named at its symbol, or none for an operator a class cannot declare, which semantics
    /// refuses.
    #[must_use]
    pub fn of_operator(operator: &'ast Operator<'arena>) -> Option<Self> {
        Some(Self {
            name: php_operator_name(&operator.symbol, operator.parameter_list.parameters.len())?,
            name_span: operator.symbol.span(),
            span: operator.span(),
            attribute_lists: &operator.attribute_lists,
            modifiers: &operator.modifiers,
            type_parameters: None,
            parameter_list: &operator.parameter_list,
            return_type_hint: Some(&operator.return_type_hint),
            readonly: None,
            on: None,
            uses: None,
            body: &operator.body,
            returns_value: true,
            returns_by_reference: false,
        })
    }

    /// The parts of a named constructor, spec section 9.1, which runs as the static method of its name. Its body runs
    /// on the object `: this(…)` creates and returns nothing.
    #[must_use]
    pub fn of_named_constructor(named_constructor: &'ast NamedConstructor<'arena>) -> Self {
        Self {
            name: named_constructor.name.value,
            name_span: named_constructor.name.span,
            span: named_constructor.span(),
            attribute_lists: &named_constructor.attribute_lists,
            modifiers: &named_constructor.modifiers,
            type_parameters: None,
            parameter_list: &named_constructor.parameter_list,
            return_type_hint: None,
            readonly: None,
            on: None,
            uses: None,
            body: &named_constructor.body,
            returns_value: false,
            returns_by_reference: false,
        }
    }

    /// The parts of an extension method, spec section 26, as the public static method of its static class it runs as:
    /// `static`, with the extension's receiver before its own parameters. Semantics refuses an extension without one
    /// receiver, whose methods have none.
    #[must_use]
    pub fn of_extension_method<A>(
        arena: &'arena A,
        extension: &'ast Extension<'arena>,
        method: &'ast Method<'arena>,
    ) -> Option<Self>
    where
        A: Arena,
        'arena: 'ast,
    {
        Some(Self {
            name: method.name.value,
            name_span: method.name.span,
            span: method.span(),
            attribute_lists: &method.attribute_lists,
            modifiers: static_modifiers(arena, extension, &method.modifiers),
            type_parameters: block_type_parameters_first(arena, extension, method.type_parameters),
            parameter_list: receiver_first(arena, extension, &method.parameter_list)?,
            return_type_hint: method.return_type_hint.as_ref(),
            readonly: None,
            on: method.on,
            uses: method.uses,
            body: &method.body,
            returns_value: method.returns_value(),
            returns_by_reference: method.ampersand.is_some(),
        })
    }

    /// The parts of an extension's computed property, spec section 26, as the public static method of its static class
    /// it runs as: `static`, with the extension's receiver as its one parameter, returning its expression.
    #[must_use]
    pub fn of_extension_property<A>(
        arena: &'arena A,
        extension: &'ast Extension<'arena>,
        property: &'ast ComputedProperty<'arena>,
    ) -> Option<Self>
    where
        A: Arena,
        'arena: 'ast,
    {
        let own = arena.alloc(FunctionLikeParameterList {
            left_parenthesis: extension.parameter_list.left_parenthesis,
            parameters: TokenSeparatedSequence::empty(),
            right_parenthesis: extension.parameter_list.right_parenthesis,
        });
        let body = &property.body;

        Some(Self {
            name: property.variable.name,
            name_span: property.variable.span,
            span: property.span(),
            attribute_lists: &property.attribute_lists,
            modifiers: static_modifiers(arena, extension, &property.modifiers),
            type_parameters: extension.type_parameters,
            parameter_list: receiver_first(arena, extension, own)?,
            return_type_hint: property
                .hint
                .as_ref()
                .map(|hint| &*arena.alloc(FunctionLikeReturnTypeHint { colon: None, hint: hint.clone() })),
            readonly: None,
            on: None,
            uses: None,
            body: arena.alloc(MethodBody::Expression(MethodExpressionBody {
                arrow: body.arrow,
                expression: body.expression,
                semicolon: body.semicolon,
            })),
            returns_value: true,
            returns_by_reference: false,
        })
    }
}

/// An extension member's modifiers with `static`, written at the `extension` keyword, as the static method it runs as.
fn static_modifiers<'arena, A>(
    arena: &'arena A,
    extension: &Extension<'arena>,
    modifiers: &Sequence<'arena, Modifier<'arena>>,
) -> &'arena Sequence<'arena, Modifier<'arena>>
where
    A: Arena,
{
    let r#static = Modifier::Static(Keyword { span: extension.extension.span, value: b"static" });

    arena.alloc(Sequence::from_slice(arena.alloc_slice_fill_iter(modifiers.iter().cloned().chain([r#static]))))
}

/// An extension member's type parameters after the extension block's, as C# 14 compiles `extension<T>(List<T> items)`:
/// the static method it runs as takes the block's first, which a call infers from its receiver.
fn block_type_parameters_first<'ast, 'arena, A>(
    arena: &'arena A,
    extension: &'ast Extension<'arena>,
    own: Option<&'ast TypeParameterList<'arena>>,
) -> Option<&'ast TypeParameterList<'arena>>
where
    A: Arena,
    'arena: 'ast,
{
    let (Some(block), Some(own)) = (extension.type_parameters, own) else {
        return extension.type_parameters.or(own);
    };
    let parameters = arena.alloc_slice_fill_iter(block.parameters.iter().chain(own.parameters.iter()).cloned());

    Some(arena.alloc(TypeParameterList {
        less_than: block.less_than,
        parameters: TokenSeparatedSequence::from_slices(parameters, own.parameters.tokens),
        greater_than: own.greater_than,
    }))
}

/// An extension member's parameters after the extension's receiver, or none for an extension without one receiver.
fn receiver_first<'arena, A>(
    arena: &'arena A,
    extension: &Extension<'arena>,
    own: &FunctionLikeParameterList<'arena>,
) -> Option<&'arena FunctionLikeParameterList<'arena>>
where
    A: Arena,
{
    let [receiver] = extension.parameter_list.parameters.as_slice() else {
        return None;
    };
    let parameters = arena.alloc_slice_fill_iter(std::iter::once(receiver).chain(own.parameters.iter()).cloned());

    Some(arena.alloc(FunctionLikeParameterList {
        left_parenthesis: own.left_parenthesis,
        parameters: TokenSeparatedSequence::from_slices(parameters, own.parameters.tokens),
        right_parenthesis: own.right_parenthesis,
    }))
}

/// What a bare PHP# name, or a type name that names a type parameter, refers to.
///
/// The parser reads `x`, `x.y` and `x.y()` the same way whatever `x` is. The binder then decides,
/// from the scopes of the file alone, what each bare name means.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum Binding {
    /// A parameter, or a local declared with `let` or `const`, whose block is still open.
    Local(Local),
    /// `this`, the object the method runs on.
    This,
    /// A class, written before `.`. Its fully qualified name is resolved like any class name.
    Class,
    /// A constant. Its fully qualified name is resolved like any constant name.
    Constant,
    /// A member of the enclosing class, read without `this.`. A bare call is never one: it calls the global function.
    Member,
    /// `field` in an accessor body: the storage of the property the accessor belongs to.
    Field,
    /// A type parameter of the enclosing method, class or interface, written as a type. Its resolved name is the name
    /// as written, as in `TItem`.
    TypeParameter {
        /// The name in the type parameter's declaration, as `TItem` in `<TItem : DatabaseEntity>`.
        declaration: Span,
    },
}

/// A PHP# scope rule a bare name breaks. The name still has its [`Binding`].
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum BindingError {
    /// A local used after the block that declares it closed. The name still binds as that local, so the scope
    /// error is the only one it causes.
    OutOfScope {
        /// The name where it is used.
        name: Span,
        /// The local, whose block has closed.
        local: Local,
    },
    /// A local declared with a name that an enclosing block of the same method already declares (C# CS0136).
    Redeclared {
        /// The name in the second declaration.
        name: Span,
        /// The declaration still open in an enclosing block.
        earlier: Local,
    },
    /// A local written where only a type goes, as in `typeof(order)`, spec section 25. The name binds as no class.
    LocalAsType {
        /// The name where it is written.
        name: Span,
        /// The local it names.
        local: Local,
    },
}

/// A local of a PHP# method: where it is declared, and how.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Local {
    /// The name in the local's declaration.
    pub declaration: Span,
    pub kind: LocalKind,
}

/// How a PHP# local is declared.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum LocalKind {
    Parameter,
    /// Declared with `let`, so it can be reassigned.
    Let,
    /// Declared with `const`, so it cannot be reassigned.
    Const,
    /// Declared by a pattern, so it exists only where `test` is true, or false when `negated` (`x is not T name`).
    Pattern {
        /// The `is` that declares it, or the pattern of the `match` arm that declares it.
        test: Span,
        negated: bool,
    },
}
