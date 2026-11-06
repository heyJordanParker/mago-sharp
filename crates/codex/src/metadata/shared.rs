use std::ops::Deref;
use std::ops::DerefMut;
use std::sync::Arc;

/// An entry of a [`CodebaseMetadata`](super::CodebaseMetadata) map that every copy of the codebase shares until one
/// of them changes it.
///
/// Cloning the codebase copies a pointer per entry, and the first write to an entry through [`DerefMut`] copies that
/// entry alone. So a codebase built on the prelude, as each analysis is, copies only the prelude entries it changes.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Shared<T>(Arc<T>);

impl<T> Shared<T> {
    #[inline]
    #[must_use]
    pub fn new(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl<T: Clone> Shared<T> {
    /// The entry's value, copied out only when another copy of the codebase still shares it.
    #[inline]
    #[must_use]
    pub fn into_inner(self) -> T {
        Arc::unwrap_or_clone(self.0)
    }
}

impl<T> Clone for Shared<T> {
    #[inline]
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> From<T> for Shared<T> {
    #[inline]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> Deref for Shared<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Clone> DerefMut for Shared<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }
}

#[cfg(feature = "serde")]
impl<T: serde::Serialize> serde::Serialize for Shared<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        T::serialize(&self.0, serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Shared<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_shares_its_entry_until_it_writes_and_then_owns_it_alone() {
        let original = Shared::new(vec![1]);
        let mut copy = original.clone();
        assert!(Arc::ptr_eq(&original.0, &copy.0));

        copy.push(2);

        assert_eq!(*original, [1]);
        assert_eq!(*copy, [1, 2]);
        assert!(!Arc::ptr_eq(&original.0, &copy.0));
    }
}
