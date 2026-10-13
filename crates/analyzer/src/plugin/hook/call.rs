//! Call hooks for function and method call events.

use mago_names::ResolvedNames;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::NullSafeMethodCall;

use crate::plugin::context::HookContext;
use crate::plugin::hook::ExpressionHookResult;
use crate::plugin::hook::HookResult;
use crate::plugin::provider::Provider;

/// Hook trait for intercepting function call analysis.
///
/// This hook receives the real CST function call node and full mutable context,
/// allowing hooks to inspect calls, report issues, modify analysis state,
/// and optionally skip analysis with a custom return type.
pub trait FunctionCallHook: Provider {
    /// Called before a function call is analyzed.
    ///
    /// Return `ExpressionHookResult::Continue` to proceed with normal analysis,
    /// `ExpressionHookResult::Skip` to skip analysis (type will be `mixed`), or
    /// `ExpressionHookResult::SkipWithType(ty)` to skip with a custom return type.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] if the underlying plugin implementation propagates one.
    fn before_function_call(
        &self,
        _call: &FunctionCall<'_>,
        _context: &mut HookContext<'_, '_>,
    ) -> HookResult<ExpressionHookResult> {
        Ok(ExpressionHookResult::Continue)
    }

    /// Called after a function call has been analyzed.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] if the underlying plugin implementation propagates one.
    fn after_function_call(&self, _call: &FunctionCall<'_>, _context: &mut HookContext<'_, '_>) -> HookResult<()> {
        Ok(())
    }
}

/// Hook trait for intercepting method call analysis.
///
/// This hook receives the real CST method call node and full mutable context,
/// allowing hooks to inspect calls, report issues, modify analysis state,
/// and optionally skip analysis with a custom return type.
pub trait MethodCallHook: Provider {
    /// Called before a method call is analyzed.
    ///
    /// Return `ExpressionHookResult::Continue` to proceed with normal analysis,
    /// `ExpressionHookResult::Skip` to skip analysis (type will be `mixed`), or
    /// `ExpressionHookResult::SkipWithType(ty)` to skip with a custom return type.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] if the underlying plugin implementation propagates one.
    fn before_method_call(
        &self,
        _call: &MethodCall<'_>,
        _context: &mut HookContext<'_, '_>,
    ) -> HookResult<ExpressionHookResult> {
        Ok(ExpressionHookResult::Continue)
    }

    /// Called after a method call has been analyzed.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] if the underlying plugin implementation propagates one.
    fn after_method_call(&self, _call: &MethodCall<'_>, _context: &mut HookContext<'_, '_>) -> HookResult<()> {
        Ok(())
    }
}

/// The parts of a static method call, as [`StaticMethodCallHook`] sees them.
///
/// PHP writes a static call as a [`StaticMethodCall`](mago_syntax::cst::StaticMethodCall) node,
/// `Class::method()`. PHP# writes it as a [`MethodCall`] node, `Class.method()`, whose object the
/// binder bound to a class. Both reach the hook as these parts, so no hook reads a `::` token that
/// the source does not contain.
#[derive(Debug, Clone, Copy)]
pub struct StaticCall<'ast, 'arena> {
    pub class: &'ast Expression<'arena>,
    pub method: &'ast ClassLikeMemberSelector<'arena>,
    pub argument_list: &'ast ArgumentList<'arena>,
    pub span: Span,
}

impl<'ast, 'arena> StaticCall<'ast, 'arena> {
    /// Returns the static call a PHP# method call writes, `Class.method()` when
    /// [`ResolvedNames::static_call_class`] finds its class, `super.method()`, which PHP writes `parent::method()`, or
    /// `Self.method()`, which PHP writes `static::method()`. Returns `None` for an instance call.
    ///
    /// Call it on a method call of a PHP# file only: the lexer reads PHP's `self` as the keyword `Self` is, and PHP's
    /// `Self->method()` is an instance call.
    #[must_use]
    pub fn from_method_call(call: &'ast MethodCall<'arena>, resolved_names: &ResolvedNames<'_>) -> Option<Self> {
        let is_super = matches!(call.object, Expression::Parent(keyword) if keyword.value == b"super");
        let is_self = matches!(call.object, Expression::Self_(keyword) if keyword.value == b"Self");

        (is_super || is_self || resolved_names.static_call_class(call).is_some()).then(|| Self {
            class: call.object,
            method: &call.method,
            argument_list: &call.argument_list,
            span: call.span(),
        })
    }
}

/// Hook trait for intercepting static method call analysis.
///
/// This hook receives the [`StaticCall`] parts of the call and full mutable context,
/// allowing hooks to inspect calls, report issues, modify analysis state,
/// and optionally skip analysis with a custom return type.
pub trait StaticMethodCallHook: Provider {
    /// Called before a static method call is analyzed.
    ///
    /// Return `ExpressionHookResult::Continue` to proceed with normal analysis,
    /// `ExpressionHookResult::Skip` to skip analysis (type will be `mixed`), or
    /// `ExpressionHookResult::SkipWithType(ty)` to skip with a custom return type.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] if the underlying plugin implementation propagates one.
    fn before_static_method_call(
        &self,
        _call: &StaticCall<'_, '_>,
        _context: &mut HookContext<'_, '_>,
    ) -> HookResult<ExpressionHookResult> {
        Ok(ExpressionHookResult::Continue)
    }

    /// Called after a static method call has been analyzed.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] if the underlying plugin implementation propagates one.
    fn after_static_method_call(
        &self,
        _call: &StaticCall<'_, '_>,
        _context: &mut HookContext<'_, '_>,
    ) -> HookResult<()> {
        Ok(())
    }
}

/// Hook trait for intercepting nullsafe method call analysis.
pub trait NullSafeMethodCallHook: Provider {
    /// Called before a nullsafe method call is analyzed.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] if the underlying plugin implementation propagates one.
    fn before_nullsafe_method_call(
        &self,
        _call: &NullSafeMethodCall<'_>,
        _context: &mut HookContext<'_, '_>,
    ) -> HookResult<ExpressionHookResult> {
        Ok(ExpressionHookResult::Continue)
    }

    /// Called after a nullsafe method call has been analyzed.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] if the underlying plugin implementation propagates one.
    fn after_nullsafe_method_call(
        &self,
        _call: &NullSafeMethodCall<'_>,
        _context: &mut HookContext<'_, '_>,
    ) -> HookResult<()> {
        Ok(())
    }
}
