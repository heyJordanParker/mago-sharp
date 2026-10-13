use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::FunctionLikeReturnTypeHint;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Operator;
use mago_syntax::cst::Sequence;
use mago_word::Word;
use mago_word::concat_word;
use mago_word::word;

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

/// The parts a method and a PHP# operator share, as the PHP method they run as: a PHP# operator is a static method.
pub struct MethodParts<'ast, 'arena> {
    /// The PHP method's name: `__construct` for the PHP# constructor, and `op_Addition` for `operator +`.
    pub name: &'arena [u8],
    /// The method's name, or the operator's symbol.
    pub name_span: Span,
    pub span: Span,
    pub attribute_lists: &'ast Sequence<'arena, AttributeList<'arena>>,
    pub modifiers: &'ast Sequence<'arena, Modifier<'arena>>,
    pub parameter_list: &'ast FunctionLikeParameterList<'arena>,
    pub return_type_hint: Option<&'ast FunctionLikeReturnTypeHint<'arena>>,
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
            parameter_list: &method.parameter_list,
            return_type_hint: method.return_type_hint.as_ref(),
            body: &method.body,
            returns_value: method.returns_value(),
            returns_by_reference: method.ampersand.is_some(),
        }
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
            parameter_list: &operator.parameter_list,
            return_type_hint: Some(&operator.return_type_hint),
            body: &operator.body,
            returns_value: true,
            returns_by_reference: false,
        })
    }
}

/// What a bare PHP# name refers to.
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
