use strum::Display;

use mago_database::file::File;
use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::Sequence;
use crate::cst::cst::class_like::law::Law;
use crate::cst::cst::expression::Expression;
use crate::cst::cst::function_like::parameter::FunctionLikeParameterList;
use crate::cst::cst::identifier::Identifier;
use crate::cst::cst::keyword::Keyword;
use crate::cst::cst::terminator::Terminator;
use crate::cst::cst::type_hint::Hint;
use crate::cst::sequence::TokenSeparatedSequence;

/// Represents a PHP# module, spec section 32: how the container builds the services of its folder's namespace.
///
/// It has no name, because the folder names it. It binds services, and it states the laws that span its namespace's
/// classes, spec section 28. An environment module or a test module names the module it overrides after `:`.
///
/// Example:
///
/// ```csharp
/// module
/// {
///     singleton Mailer = SmtpMailer;
///     law receiptMatchesRefunds(int paid, int refunded) => Receipts.refundable(paid, refunded) == Refunds.remaining(paid, refunded);
/// }
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Module<'arena> {
    pub module: Keyword<'arena>,
    pub parent: Option<ModuleParent<'arena>>,
    pub left_brace: Span,
    pub members: Sequence<'arena, ModuleMember<'arena>>,
    pub right_brace: Span,
}

/// The module an environment module or a test module overrides, as in `module : App.Shop`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ModuleParent<'arena> {
    pub colon: Span,
    pub name: Identifier<'arena>,
}

/// A member of a module.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum ModuleMember<'arena> {
    Law(Law<'arena>),
    Binding(&'arena Binding<'arena>),
}

/// A service binding, spec section 32: its lifetime, the service, the services it needs as parameters, the one class
/// it serves after `for`, and its value.
///
/// Example:
///
/// ```csharp
/// singleton Mailer for SendReceipt = QueueMailer;
/// singleton RedisStore(Environment environment) = new RedisStore(environment.require("REDIS_URL"));
/// ```
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Binding<'arena> {
    /// `singleton`, `scoped` or `transient`, which stay names everywhere else.
    pub lifetime: Keyword<'arena>,
    pub service: Hint<'arena>,
    pub parameter_list: Option<FunctionLikeParameterList<'arena>>,
    pub target: Option<BindingTarget<'arena>>,
    pub equals: Span,
    pub value: BindingValue<'arena>,
    pub terminator: Terminator<'arena>,
}

/// How long a bound service lives, spec section 32.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Lifetime {
    /// As long as the host runs the app.
    Singleton,
    /// One scope, which the host starts and ends, such as one HTTP request.
    Scoped,
    /// A new service each time a class needs one.
    Transient,
}

/// The one class a binding serves, as in `for SendReceipt`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct BindingTarget<'arena> {
    pub r#for: Keyword<'arena>,
    pub class: Identifier<'arena>,
}

/// A binding's value: a class, built through its main constructor, a list of classes for a `List` service, or one
/// expression over the binding's parameters.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Display)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
pub enum BindingValue<'arena> {
    Class(Identifier<'arena>),
    Classes(BindingClasses<'arena>),
    Expression(&'arena Expression<'arena>),
}

/// The classes of a `List` service, as in `[TaxRule, DiscountRule]`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct BindingClasses<'arena> {
    pub left_bracket: Span,
    pub classes: TokenSeparatedSequence<'arena, Identifier<'arena>>,
    pub right_bracket: Span,
}

impl Module<'_> {
    /// Whether `file` is where a namespace's own module is declared, spec section 32: its folder's `Module.sharp`.
    #[inline]
    #[must_use]
    pub fn is_declared_in(file: &File) -> bool {
        file.name.rsplit(|&byte| byte == b'/').next() == Some(b"Module.sharp".as_slice())
    }

    /// The environment `file` holds a module for, spec section 32: `Production` for `Module.Production.sharp`.
    #[inline]
    #[must_use]
    pub fn environment_of(file: &File) -> Option<&[u8]> {
        let name = file.name.rsplit(|&byte| byte == b'/').next()?;
        let environment = name.strip_prefix(b"Module.")?.strip_suffix(b".sharp")?;

        (!environment.is_empty() && !environment.contains(&b'.')).then_some(environment)
    }
}

impl Binding<'_> {
    /// How long the service lives, from the binding's first word.
    #[inline]
    #[must_use]
    pub fn lifetime(&self) -> Lifetime {
        match self.lifetime.value {
            b"singleton" => Lifetime::Singleton,
            b"scoped" => Lifetime::Scoped,
            _ => Lifetime::Transient,
        }
    }
}

impl HasSpan for Module<'_> {
    fn span(&self) -> Span {
        Span::between(self.module.span(), self.right_brace)
    }
}

impl HasSpan for ModuleParent<'_> {
    fn span(&self) -> Span {
        Span::between(self.colon, self.name.span())
    }
}

impl HasSpan for ModuleMember<'_> {
    fn span(&self) -> Span {
        match self {
            ModuleMember::Law(law) => law.span(),
            ModuleMember::Binding(binding) => binding.span(),
        }
    }
}

impl HasSpan for Binding<'_> {
    fn span(&self) -> Span {
        Span::between(self.lifetime.span(), self.terminator.span())
    }
}

impl HasSpan for BindingTarget<'_> {
    fn span(&self) -> Span {
        Span::between(self.r#for.span(), self.class.span())
    }
}

impl HasSpan for BindingValue<'_> {
    fn span(&self) -> Span {
        match self {
            BindingValue::Class(class) => class.span(),
            BindingValue::Classes(classes) => classes.span(),
            BindingValue::Expression(expression) => expression.span(),
        }
    }
}

impl HasSpan for BindingClasses<'_> {
    fn span(&self) -> Span {
        Span::between(self.left_bracket, self.right_bracket)
    }
}
