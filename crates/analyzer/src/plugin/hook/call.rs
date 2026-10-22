//! Call hooks for function and method call events.

use mago_codex::metadata::CodebaseMetadata;
use mago_names::ResolvedNames;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::NullSafeMethodCall;
use mago_word::Word;
use mago_word::word;

use crate::plugin::context::HookContext;
use crate::plugin::hook::ExpressionHookResult;
use crate::plugin::hook::HookResult;
use crate::plugin::provider::Provider;
use crate::utils::names::namespace_function_name;

/// A call of a function by its name, as [`FunctionCallHook`] sees it.
///
/// It is PHP's `name(…)`, and PHP#'s `Pdf.render(…)` and `Foundation.abort(…)`, which call a plain PHP function through
/// its namespace. Every check that finds a call by its function's name reads this view, so the PHP# call gets each check
/// PHP's call gets.
#[derive(Debug, Clone, Copy)]
pub struct NamedCall<'ast, 'arena> {
    /// The full name of the function the call runs.
    pub name: Word,
    pub argument_list: &'ast ArgumentList<'arena>,
    pub span: Span,
}

impl<'ast, 'arena> NamedCall<'ast, 'arena> {
    /// Returns the view of `call` when it calls a function by its name, or `None` for any other call.
    pub(crate) fn from_call(
        resolved_names: &ResolvedNames<'_>,
        codebase: &CodebaseMetadata,
        call: &'ast Call<'arena>,
    ) -> Option<Self> {
        let (name, argument_list) = match call {
            Call::Function(FunctionCall { function: Expression::Identifier(identifier), argument_list }) => {
                (function_name(resolved_names, codebase, identifier), argument_list)
            }
            Call::Method(method_call) => {
                (namespace_function_name(resolved_names, codebase, method_call)?, &method_call.argument_list)
            }
            _ => return None,
        };

        Some(Self { name, argument_list, span: call.span() })
    }

    /// Whether the call runs one of `functions`, each a full name in any case.
    #[must_use]
    pub fn calls(&self, functions: &[&[u8]]) -> bool {
        functions.iter().any(|function| self.name.as_bytes().eq_ignore_ascii_case(function))
    }
}

/// The function PHP's `name(…)` runs: a name the file imports or writes fully qualified as the binder resolved it, and
/// any other name in the file's namespace when the namespace has that function, else in the root namespace when the
/// root has it. A function neither has keeps the name in the file's namespace.
fn function_name(resolved_names: &ResolvedNames<'_>, codebase: &CodebaseMetadata, identifier: &Identifier<'_>) -> Word {
    let full_name = resolved_names.get(identifier);
    if resolved_names.is_imported(identifier) {
        return word(full_name);
    }

    let name = identifier.value();
    if !codebase.function_exists(full_name) && !name.contains(&b'\\') && codebase.function_exists(name) {
        return word(name);
    }

    word(full_name)
}

/// Hook trait for intercepting function call analysis.
///
/// This hook receives the [`NamedCall`] view of a call of a function by its name and full mutable context,
/// allowing hooks to inspect calls, report issues, modify analysis state,
/// and optionally skip analysis with a custom return type. A PHP# call of a plain PHP function through its namespace,
/// `Foundation.abort(…)`, reaches the hook as PHP's `abort(…)` does.
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
        _call: &NamedCall<'_, '_>,
        _context: &mut HookContext<'_, '_>,
    ) -> HookResult<ExpressionHookResult> {
        Ok(ExpressionHookResult::Continue)
    }

    /// Called after a function call has been analyzed.
    ///
    /// # Errors
    ///
    /// Returns [`HookError`] if the underlying plugin implementation propagates one.
    fn after_function_call(&self, _call: &NamedCall<'_, '_>, _context: &mut HookContext<'_, '_>) -> HookResult<()> {
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
    /// [`ResolvedNames::static_call_class`] finds its class, `super.method()`, which PHP writes `parent::method()`,
    /// `super<HasLabel>.method()`, which calls an interface's default body, or `Self.method()`, which PHP writes
    /// `static::method()`. Returns `None` for an instance call.
    ///
    /// Call it on a method call of a PHP# file only: the lexer reads PHP's `self` as the keyword `Self` is, and PHP's
    /// `Self->method()` is an instance call.
    #[must_use]
    pub fn from_method_call(call: &'ast MethodCall<'arena>, resolved_names: &ResolvedNames<'_>) -> Option<Self> {
        let is_super = matches!(call.object, Expression::Parent(keyword) if keyword.value == b"super")
            || matches!(call.object, Expression::SuperOf(_));
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
