use std::sync::Arc;

use mago_word::Word;
use mago_word::concat_word;

use crate::metadata::CodebaseMetadata;
use crate::ttype::TType;
use crate::ttype::TypeRef;
use crate::ttype::atomic::TAtomic;
use crate::ttype::atomic::reference::TReference;
use crate::ttype::get_array_parameters;
use crate::ttype::union::TUnion;

/// `key-of<T>`: the keys of the array `T`, or, as PHP#'s `keyof Order` writes it, the names of the class `Order`'s
/// members, spec section 11.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TKeyOf(Arc<TUnion>);

impl TKeyOf {
    #[must_use]
    pub fn new(object: Arc<TUnion>) -> Self {
        Self(object)
    }

    #[inline]
    #[must_use]
    pub fn get_target_type(&self) -> &TUnion {
        &self.0
    }

    #[inline]
    pub fn get_target_type_mut(&mut self) -> &mut TUnion {
        Arc::make_mut(&mut self.0)
    }

    /// Whether this holds the names of a class's members, as PHP#'s `keyof Order` and `keyof T` do, spec section 11,
    /// rather than an array's keys. A member's name is a string only `nameof` gives, so a member name type holds no
    /// other string, and it is a non-empty `string` wherever a `string` is taken.
    #[must_use]
    pub fn names_members(&self) -> bool {
        self.0.types.iter().all(|target| match target {
            TAtomic::Object(_) | TAtomic::Reference(TReference::Symbol { .. }) => true,
            TAtomic::GenericParameter(parameter) => !parameter
                .constraint
                .types
                .iter()
                .any(|bound| matches!(bound, TAtomic::Array(_) | TAtomic::Iterable(_))),
            _ => false,
        })
    }

    #[must_use]
    pub fn get_key_of_targets(
        target_types: &[TAtomic],
        codebase: &CodebaseMetadata,
        retain_generics: bool,
    ) -> Option<TUnion> {
        let mut key_types = vec![];

        for target in target_types {
            match target {
                TAtomic::Array(array) => {
                    let (array_key_type, _) = get_array_parameters(array, codebase);

                    key_types.extend(array_key_type.types.iter().cloned());
                }
                TAtomic::Iterable(iterable) => {
                    key_types.extend(iterable.get_key_type().types.iter().cloned());
                }
                TAtomic::GenericParameter(parameter) => {
                    if retain_generics {
                        key_types.push(TAtomic::GenericParameter(parameter.clone()));
                    } else if let Some(generic_key_types) =
                        Self::get_key_of_targets(parameter.get_constraint().types.as_ref(), codebase, retain_generics)
                    {
                        key_types.extend(generic_key_types.types.into_owned());
                    }
                }
                _ => {}
            }
        }

        if key_types.is_empty() { None } else { Some(TUnion::from_vec(key_types)) }
    }
}

impl TType for TKeyOf {
    fn get_child_nodes(&self) -> Vec<TypeRef<'_>> {
        vec![TypeRef::Union(&self.0)]
    }

    fn needs_population(&self) -> bool {
        self.0.needs_population()
    }

    fn is_expandable(&self) -> bool {
        true
    }

    fn get_id(&self) -> Word {
        concat_word!(b"key-of<", self.0.get_id(), b">")
    }
}
