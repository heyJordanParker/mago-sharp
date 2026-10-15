use std::hash::Hash;

use mago_names::ResolvedNames;
use mago_syntax::cst::As;
use mago_syntax::cst::BinaryPattern;
use mago_syntax::cst::ComparisonPattern;
use mago_syntax::cst::FieldsPattern;
use mago_syntax::cst::Is;
use mago_syntax::cst::ListPattern;
use mago_syntax::cst::ListPatternElement;
use mago_syntax::cst::NotPattern;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::PropertiesPattern;
use mago_syntax::cst::PropertyPattern;
use mago_syntax::cst::RestPattern;
use mago_syntax::cst::TypePattern;
use mago_syntax_core::stack::ensure_sufficient_stack;

use crate::FingerprintOptions;
use crate::Fingerprintable;

impl Fingerprintable for Is<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "is".hash(hasher);
        self.value.fingerprint_with_hasher(hasher, resolved_names, options);
        self.pattern.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}

impl Fingerprintable for As<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "as".hash(hasher);
        self.value.fingerprint_with_hasher(hasher, resolved_names, options);
        self.hint.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}

impl Fingerprintable for Pattern<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        ensure_sufficient_stack(|| match self {
            Pattern::Type(pattern) => pattern.fingerprint_with_hasher(hasher, resolved_names, options),
            Pattern::Value(value) => {
                "pattern_value".hash(hasher);
                value.fingerprint_with_hasher(hasher, resolved_names, options);
            }
            Pattern::Comparison(pattern) => pattern.fingerprint_with_hasher(hasher, resolved_names, options),
            Pattern::Not(pattern) => pattern.fingerprint_with_hasher(hasher, resolved_names, options),
            Pattern::Binary(pattern) => pattern.fingerprint_with_hasher(hasher, resolved_names, options),
            Pattern::Parenthesized(pattern) => {
                pattern.pattern.fingerprint_with_hasher(hasher, resolved_names, options);
            }
            Pattern::Properties(pattern) => pattern.fingerprint_with_hasher(hasher, resolved_names, options),
            Pattern::List(pattern) => pattern.fingerprint_with_hasher(hasher, resolved_names, options),
        });
    }
}

impl Fingerprintable for TypePattern<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "pattern_type".hash(hasher);
        self.hint.fingerprint_with_hasher(hasher, resolved_names, options);
        if let Some(fields) = self.fields {
            "fields".hash(hasher);
            fields.fingerprint_with_hasher(hasher, resolved_names, options);
        }
        if let Some(properties) = self.properties {
            "properties".hash(hasher);
            properties.fingerprint_with_hasher(hasher, resolved_names, options);
        }
        if let Some(variable) = &self.variable {
            "variable".hash(hasher);
            variable.fingerprint_with_hasher(hasher, resolved_names, options);
        }
    }
}

impl Fingerprintable for FieldsPattern<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        self.fields.len().hash(hasher);
        for field in &self.fields {
            field.fingerprint_with_hasher(hasher, resolved_names, options);
        }
    }
}

impl Fingerprintable for ComparisonPattern<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "pattern_comparison".hash(hasher);
        self.operator.fingerprint_with_hasher(hasher, resolved_names, options);
        self.value.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}

impl Fingerprintable for NotPattern<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "pattern_not".hash(hasher);
        self.pattern.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}

impl Fingerprintable for BinaryPattern<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "pattern_binary".hash(hasher);
        self.left.fingerprint_with_hasher(hasher, resolved_names, options);
        self.operator.fingerprint_with_hasher(hasher, resolved_names, options);
        self.right.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}

impl Fingerprintable for PropertiesPattern<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "pattern_properties".hash(hasher);
        self.properties.len().hash(hasher);
        for property in &self.properties {
            property.fingerprint_with_hasher(hasher, resolved_names, options);
        }
    }
}

impl Fingerprintable for PropertyPattern<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        self.name.fingerprint_with_hasher(hasher, resolved_names, options);
        self.pattern.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}

impl Fingerprintable for ListPattern<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "pattern_list".hash(hasher);
        self.elements.len().hash(hasher);
        for element in &self.elements {
            match element {
                ListPatternElement::Pattern(pattern) => {
                    pattern.fingerprint_with_hasher(hasher, resolved_names, options)
                }
                ListPatternElement::Rest(rest) => rest.fingerprint_with_hasher(hasher, resolved_names, options),
            }
        }
    }
}

impl Fingerprintable for RestPattern<'_> {
    #[inline]
    fn fingerprint_with_hasher<H>(
        &self,
        hasher: &mut H,
        resolved_names: &ResolvedNames,
        options: &FingerprintOptions<'_>,
    ) where
        H: std::hash::Hasher,
    {
        "pattern_rest".hash(hasher);
        self.hint.fingerprint_with_hasher(hasher, resolved_names, options);
        self.variable.fingerprint_with_hasher(hasher, resolved_names, options);
    }
}
