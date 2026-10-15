use mago_names::ResolvedNames;
use mago_syntax::cst::GenericClass;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::ObjectInitializer;
use mago_syntax::cst::With;

use crate::FingerprintOptions;
use crate::Fingerprintable;
use std::hash::Hash;

impl Fingerprintable for Instantiation<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "new".hash(hasher);
        self.class.fingerprint_with_hasher(hasher, resolved_names, options);
        self.type_arguments.fingerprint_with_hasher(hasher, resolved_names, options);
        if let Some(constructor) = &self.constructor {
            constructor.name.fingerprint_with_hasher(hasher, resolved_names, options);
        }
        self.argument_list.fingerprint_with_hasher(hasher, resolved_names, options);
        self.initializer.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}

impl Fingerprintable for With<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "with".hash(hasher);
        self.object.fingerprint_with_hasher(hasher, resolved_names, options);
        self.initializer.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}

impl Fingerprintable for ObjectInitializer<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "initializer".hash(hasher);
        self.values.len().hash(hasher);
        for value in &self.values {
            value.name.fingerprint_with_hasher(hasher, resolved_names, options);
            value.value.fingerprint_with_hasher(hasher, resolved_names, options);
        }
    }
}

impl Fingerprintable for GenericClass<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "generic_class".hash(hasher);
        self.class.fingerprint_with_hasher(hasher, resolved_names, options);
        self.type_arguments.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}
