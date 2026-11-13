//! Method return type provider trait.

use mago_codex::metadata::CodebaseMetadata;
use mago_codex::ttype::union::TUnion;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::concat_word;
use mago_word::starts_with_ignore_case;

use crate::plugin::context::InvocationInfo;
use crate::plugin::context::ProviderContext;
use crate::plugin::provider::Provider;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodTarget {
    pub class: &'static [u8],
    pub method: &'static [u8],
}

impl MethodTarget {
    #[inline]
    #[must_use]
    pub const fn exact(class: &'static [u8], method: &'static [u8]) -> Self {
        Self { class, method }
    }

    #[inline]
    #[must_use]
    pub const fn all_methods(class: &'static [u8]) -> Self {
        Self { class, method: b"*" }
    }

    #[inline]
    #[must_use]
    pub const fn any_class(method: &'static [u8]) -> Self {
        Self { class: b"*", method }
    }

    /// Whether the target names the method `method_name` of `class_name`, by the key rule, spec section 24: a class-like
    /// by its key, and a method by its class's rule, so the target `get` names no `Get` of a class with exact names.
    #[must_use]
    pub fn matches(&self, codebase: &CodebaseMetadata, class_name: &[u8], method_name: &[u8]) -> bool {
        self.matches_class(codebase, class_name) && self.matches_method(codebase, class_name, method_name)
    }

    fn matches_class(&self, codebase: &CodebaseMetadata, class_name: &[u8]) -> bool {
        if self.class == b"*" {
            return true;
        }

        // A built-in target's class prefix names a namespace of PHP or a plain PHP library, found in any case.
        if self.class.last() == Some(&b'*') {
            starts_with_ignore_case(class_name, &self.class[..self.class.len() - 1])
        } else {
            codebase.class_key(class_name) == codebase.class_key(self.class)
        }
    }

    fn matches_method(&self, codebase: &CodebaseMetadata, class_name: &[u8], method_name: &[u8]) -> bool {
        if self.method == b"*" {
            return true;
        }

        let exact = codebase.get_class_like(class_name).is_some_and(|class| class.exact_names);
        let (written, name) = match self.method.strip_suffix(b"*") {
            Some(prefix) => (prefix, method_name.get(..prefix.len())),
            None => (self.method, Some(method_name)),
        };

        name.is_some_and(|name| if exact { name == written } else { name.eq_ignore_ascii_case(written) })
    }

    #[must_use]
    pub fn is_exact(&self) -> bool {
        !self.class.contains(&b'*') && !self.method.contains(&b'*')
    }

    #[must_use]
    pub fn index_key(&self) -> Option<Word> {
        if self.is_exact() {
            Some(concat_word!(ascii_lowercase_word(self.class), b"::", ascii_lowercase_word(self.method)))
        } else {
            None
        }
    }
}

pub trait MethodReturnTypeProvider: Provider {
    fn targets() -> &'static [MethodTarget]
    where
        Self: Sized;

    fn get_return_type(
        &self,
        context: &ProviderContext<'_, '_, '_>,
        class_name: &[u8],
        method_name: &[u8],
        invocation: &InvocationInfo<'_, '_, '_>,
    ) -> Option<TUnion>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target's method prefix, as `get*`, names each method that starts with it, in any case for a class without exact
    /// names, and no method shorter than it.
    #[test]
    fn a_method_prefix_names_each_method_that_starts_with_it() {
        let codebase = CodebaseMetadata::new();
        let target = MethodTarget::any_class(b"get*");

        assert!(target.matches(&codebase, b"Lib\\Box", b"getName"));
        assert!(target.matches(&codebase, b"Lib\\Box", b"GETNAME"));
        assert!(!target.matches(&codebase, b"Lib\\Box", b"ge"));
        assert!(!target.matches(&codebase, b"Lib\\Box", b"setName"));
    }
}
