use std::hash::Hash;

use mago_names::ResolvedNames;
use mago_syntax::cst::FunctionHint;
use mago_syntax::cst::GenericHint;
use mago_syntax::cst::Hint;
use mago_syntax::cst::IntersectionHint;
use mago_syntax::cst::NullableHint;
use mago_syntax::cst::ParenthesizedHint;
use mago_syntax::cst::UnionHint;

use crate::FingerprintOptions;
use crate::Fingerprintable;

impl Fingerprintable for Hint<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        match self {
            Hint::Identifier(id) => {
                id.fingerprint_with_hasher(hasher, resolved_names, options);
            }
            Hint::Parenthesized(p) => p.fingerprint_with_hasher(hasher, resolved_names, options),
            Hint::Nullable(n) => n.fingerprint_with_hasher(hasher, resolved_names, options),
            Hint::Union(u) => u.fingerprint_with_hasher(hasher, resolved_names, options),
            Hint::Intersection(i) => i.fingerprint_with_hasher(hasher, resolved_names, options),
            Hint::Null(_) => "null".hash(hasher),
            Hint::True(_) => "true".hash(hasher),
            Hint::False(_) => "false".hash(hasher),
            Hint::Array(_) => "array".hash(hasher),
            Hint::Callable(_) => "callable".hash(hasher),
            Hint::Static(_) => "static".hash(hasher),
            Hint::Self_(_) => "self".hash(hasher),
            Hint::Parent(_) => "parent".hash(hasher),
            Hint::Void(_) => "void".hash(hasher),
            Hint::Never(_) => "never".hash(hasher),
            Hint::Float(_) => "float".hash(hasher),
            Hint::Bool(_) => "bool".hash(hasher),
            Hint::Integer(_) => "int".hash(hasher),
            Hint::String(_) => "string".hash(hasher),
            Hint::Object(_) => "object".hash(hasher),
            Hint::Mixed(_) => "mixed".hash(hasher),
            Hint::Iterable(_) => "iterable".hash(hasher),
            Hint::Generic(g) => g.fingerprint_with_hasher(hasher, resolved_names, options),
            Hint::Function(f) => f.fingerprint_with_hasher(hasher, resolved_names, options),
        }
    }
}

impl Fingerprintable for FunctionHint<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "function".hash(hasher);
        self.return_type.fingerprint_with_hasher(hasher, resolved_names, options);
        for parameter in &self.parameters {
            parameter.fingerprint_with_hasher(hasher, resolved_names, options);
        }

        if let Some(uses) = &self.uses {
            "uses".hash(hasher);
            for effect in &uses.names {
                effect.fingerprint_with_hasher(hasher, resolved_names, options);
            }
        }
    }
}

impl Fingerprintable for GenericHint<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "generic".hash(hasher);
        self.name.value.hash(hasher);
        for argument in &self.arguments {
            argument.fingerprint_with_hasher(hasher, resolved_names, options);
        }
    }
}

impl Fingerprintable for ParenthesizedHint<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        self.hint.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}

impl Fingerprintable for NullableHint<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "nullable".hash(hasher);
        self.hint.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}

impl Fingerprintable for UnionHint<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "union".hash(hasher);
        self.left.fingerprint_with_hasher(hasher, resolved_names, options);
        self.right.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}

impl Fingerprintable for IntersectionHint<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "intersection".hash(hasher);
        self.left.fingerprint_with_hasher(hasher, resolved_names, options);
        self.right.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}
