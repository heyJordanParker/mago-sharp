use mago_database::file::FileType;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MetadataFlags(u64);

impl MetadataFlags {
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const ABSTRACT: MetadataFlags = MetadataFlags(1 << 0);
    pub const FINAL: MetadataFlags = MetadataFlags(1 << 1);
    pub const READONLY: MetadataFlags = MetadataFlags(1 << 3);
    pub const DEPRECATED: MetadataFlags = MetadataFlags(1 << 4);
    pub const ENUM_INTERFACE: MetadataFlags = MetadataFlags(1 << 5);
    pub const POPULATED: MetadataFlags = MetadataFlags(1 << 6);
    pub const INTERNAL: MetadataFlags = MetadataFlags(1 << 7);
    pub const CONSISTENT_CONSTRUCTOR: MetadataFlags = MetadataFlags(1 << 11);
    pub const CONSISTENT_TEMPLATES: MetadataFlags = MetadataFlags(1 << 12);
    pub const UNCHECKED: MetadataFlags = MetadataFlags(1 << 13);
    pub const USER_DEFINED: MetadataFlags = MetadataFlags(1 << 14);
    pub const BUILTIN: MetadataFlags = MetadataFlags(1 << 15);
    pub const HAS_YIELD: MetadataFlags = MetadataFlags(1 << 16);
    pub const MUST_USE: MetadataFlags = MetadataFlags(1 << 17);
    pub const HAS_THROW: MetadataFlags = MetadataFlags(1 << 18);
    pub const PURE: MetadataFlags = MetadataFlags(1 << 19);
    pub const IGNORE_NULLABLE_RETURN: MetadataFlags = MetadataFlags(1 << 20);
    pub const IGNORE_FALSABLE_RETURN: MetadataFlags = MetadataFlags(1 << 21);
    pub const INHERITS_DOCS: MetadataFlags = MetadataFlags(1 << 22);
    pub const NO_NAMED_ARGUMENTS: MetadataFlags = MetadataFlags(1 << 23);
    pub const BACKED_ENUM_CASE: MetadataFlags = MetadataFlags(1 << 24);
    pub const UNIT_ENUM_CASE: MetadataFlags = MetadataFlags(1 << 25);
    pub const BY_REFERENCE: MetadataFlags = MetadataFlags(1 << 26);
    pub const VARIADIC: MetadataFlags = MetadataFlags(1 << 27);
    pub const PROMOTED_PROPERTY: MetadataFlags = MetadataFlags(1 << 28);
    pub const HAS_DEFAULT: MetadataFlags = MetadataFlags(1 << 29);
    pub const VIRTUAL_PROPERTY: MetadataFlags = MetadataFlags(1 << 30);
    pub const ASYMMETRIC_PROPERTY: MetadataFlags = MetadataFlags(1 << 31);
    pub const STATIC: MetadataFlags = MetadataFlags(1 << 32);
    pub const WRITEONLY: MetadataFlags = MetadataFlags(1 << 33);
    // Bit 34 was MAGIC_PROPERTY; magic properties are now identified by their entry in
    // `ClassLikeMetadata::magic_properties` instead of a flag.
    pub const MAGIC_METHOD: MetadataFlags = MetadataFlags(1 << 35);
    pub const API: MetadataFlags = MetadataFlags(1 << 36);
    pub const MUTATION_FREE: MetadataFlags = MetadataFlags(1 << 37);
    pub const EXTERNAL_MUTATION_FREE: MetadataFlags = MetadataFlags(1 << 38);
    pub const SUSPENDS_FIBER: MetadataFlags = MetadataFlags(1 << 39);
    pub const EXPERIMENTAL: MetadataFlags = MetadataFlags(1 << 40);
    pub const POLYFILL: MetadataFlags = MetadataFlags(1 << 41);
    pub const PATCH: MetadataFlags = MetadataFlags(1 << 42);
    pub const EXTERNAL: MetadataFlags = MetadataFlags(1 << 43);
    /// Declared in a PHP# file.
    pub const SHARP: MetadataFlags = MetadataFlags(1 << 44);
    /// A PHP# module, spec section 32: the class-like `<Namespace>\Module` its `Module.sharp` declares, which holds only
    /// laws and never runs.
    pub const MODULE: MetadataFlags = MetadataFlags(1 << 45);
    /// A PHP# named constructor, spec section 9.1: the public static method it runs as, which creates its class's
    /// object with the main constructor and is called as `new Class.name(…)`.
    pub const NAMED_CONSTRUCTOR: MetadataFlags = MetadataFlags(1 << 46);
    /// A PHP# property declared with `init`, spec section 6.1: it is set only while the object is created, and runs as
    /// PHP's `set` hook or set visibility.
    pub const INIT: MetadataFlags = MetadataFlags(1 << 47);
    /// A PHP# event, spec section 15: the final class an `event` declaration runs as.
    pub const EVENT: MetadataFlags = MetadataFlags(1 << 48);
    /// A PHP# extension method, spec section 26: the public static method of a static class it runs as, which takes
    /// the extension's receiver first and is called as a method of the receiver.
    pub const EXTENSION_METHOD: MetadataFlags = MetadataFlags(1 << 49);
    /// A PHP# extension's computed property, spec section 26: the public static method of a static class it runs as,
    /// which takes only the extension's receiver and is read as a property of the receiver.
    pub const EXTENSION_PROPERTY: MetadataFlags = MetadataFlags(1 << 50);
    /// A PHP# `foreign` class, spec section 29: an effect, which code has by holding one of its objects.
    pub const FOREIGN: MetadataFlags = MetadataFlags(1 << 51);
    /// A PHP# struct, spec section 10: a final class whose object never changes and compares by value.
    pub const STRUCT: MetadataFlags = MetadataFlags(1 << 52);
    /// A PHP# property each creation of its object must set, spec sections 6.1 and 10: one written `required`, or a
    /// struct's public `init` property with no initial value that its main constructor does not take. What a
    /// constructor assigns is its own `MethodMetadata::assigned_properties`.
    pub const REQUIRED: MetadataFlags = MetadataFlags(1 << 53);
    /// A PHP# operator, spec section 19: the public static method it runs as, named after .NET's operator method, as
    /// `op_Addition` for `operator +`. It is readonly by definition, spec section 13.
    pub const OPERATOR: MetadataFlags = MetadataFlags(1 << 54);
    /// A PHP# enum case that runs as a class, spec section 20: the final subclass of its enum, named in the enum's
    /// namespace, that a case of an enum with a case that carries data runs as.
    pub const ENUM_CASE_CLASS: MetadataFlags = MetadataFlags(1 << 55);
    /// A PHP# sealed interface, spec section 20.1: its permitted inheritors are the classes of its namespace that
    /// implement it, which the populator gathers.
    pub const SEALED: MetadataFlags = MetadataFlags(1 << 56);
    /// A PHP# property whose `set` or `init` is `internal`, spec section 5: code outside its class's namespace and the
    /// namespaces below it can't write it.
    pub const INTERNAL_SET: MetadataFlags = MetadataFlags(1 << 57);
    /// A PHP# class-like of a project: a file whose nearest `composer.json` says `"type": "project"`, so no other
    /// package uses it, and spec section 5's too-wide warning runs on it.
    pub const PROJECT: MetadataFlags = MetadataFlags(1 << 58);
    /// A PHP# declaration or member written `internal`, spec section 5: it reaches its namespace and every namespace
    /// below it. `INTERNAL` stays PHPDoc's `@internal`.
    pub const INTERNAL_REACH: MetadataFlags = MetadataFlags(1 << 59);
    /// A PHP# field, spec section 6: storage that is never a property, as `ReflectionProperty::isField()` tells.
    pub const FIELD: MetadataFlags = MetadataFlags(1 << 60);
    /// A PHP# static whose initial value runs as code, once, when its class's statics are first used, spec section 6,
    /// as `Property::initial_value_runs_after_class` decides. A throw there leaves it unset.
    pub const RUNTIME_INITIAL_VALUE: MetadataFlags = MetadataFlags(1 << 61);
    /// A PHP# function parameter whose type states no effects, spec section 29, as `f` in `map(Function<R(T)> f)`: each
    /// call of the method takes on the effects of the function it passes there, which the type does not limit.
    pub const TAKES_ON_EFFECTS: MetadataFlags = MetadataFlags(1 << 62);
}

impl MetadataFlags {
    #[inline]
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    #[inline]
    #[must_use]
    pub const fn empty() -> Self {
        MetadataFlags(0)
    }

    #[inline]
    pub const fn insert(&mut self, other: MetadataFlags) {
        self.0 |= other.0;
    }

    #[inline]
    pub const fn set(&mut self, other: MetadataFlags, value: bool) {
        if value {
            self.insert(other);
        } else {
            self.0 &= !other.0;
        }
    }

    #[inline]
    #[must_use]
    pub const fn contains(self, other: MetadataFlags) -> bool {
        (self.0 & other.0) == other.0
    }

    #[inline]
    pub const fn remove(&mut self, other: MetadataFlags) {
        self.0 &= !other.0;
    }
}

impl MetadataFlags {
    #[inline]
    #[must_use]
    pub const fn is_deprecated(self) -> bool {
        self.contains(Self::DEPRECATED)
    }

    #[inline]
    #[must_use]
    pub const fn is_abstract(self) -> bool {
        self.contains(Self::ABSTRACT)
    }

    #[inline]
    #[must_use]
    pub const fn is_final(self) -> bool {
        self.contains(Self::FINAL)
    }

    #[inline]
    #[must_use]
    pub const fn is_sharp(self) -> bool {
        self.contains(Self::SHARP)
    }

    #[inline]
    #[must_use]
    pub const fn is_module(self) -> bool {
        self.contains(Self::MODULE)
    }

    #[inline]
    #[must_use]
    pub const fn is_named_constructor(self) -> bool {
        self.contains(Self::NAMED_CONSTRUCTOR)
    }

    #[inline]
    #[must_use]
    pub const fn is_operator(self) -> bool {
        self.contains(Self::OPERATOR)
    }

    #[inline]
    #[must_use]
    pub const fn is_init(self) -> bool {
        self.contains(Self::INIT)
    }

    #[inline]
    #[must_use]
    pub const fn is_event(self) -> bool {
        self.contains(Self::EVENT)
    }

    #[inline]
    #[must_use]
    pub const fn is_enum_case_class(self) -> bool {
        self.contains(Self::ENUM_CASE_CLASS)
    }

    #[inline]
    #[must_use]
    pub const fn is_sealed(self) -> bool {
        self.contains(Self::SEALED)
    }

    #[inline]
    #[must_use]
    pub const fn is_extension_method(self) -> bool {
        self.contains(Self::EXTENSION_METHOD)
    }

    #[inline]
    #[must_use]
    pub const fn is_extension_property(self) -> bool {
        self.contains(Self::EXTENSION_PROPERTY)
    }

    #[inline]
    #[must_use]
    pub const fn is_foreign(self) -> bool {
        self.contains(Self::FOREIGN)
    }

    #[inline]
    #[must_use]
    pub const fn is_struct(self) -> bool {
        self.contains(Self::STRUCT)
    }

    #[inline]
    #[must_use]
    pub const fn is_required(self) -> bool {
        self.contains(Self::REQUIRED)
    }

    #[inline]
    #[must_use]
    pub const fn has_yield(self) -> bool {
        self.contains(Self::HAS_YIELD)
    }

    #[inline]
    #[must_use]
    pub const fn must_use(self) -> bool {
        self.contains(Self::MUST_USE)
    }

    #[inline]
    #[must_use]
    pub const fn is_pure(self) -> bool {
        self.contains(Self::PURE)
    }

    #[inline]
    #[must_use]
    pub const fn has_consistent_constructor(self) -> bool {
        self.contains(Self::CONSISTENT_CONSTRUCTOR)
    }

    #[inline]
    #[must_use]
    pub const fn has_consistent_templates(self) -> bool {
        self.contains(Self::CONSISTENT_TEMPLATES)
    }

    #[inline]
    #[must_use]
    pub const fn is_user_defined(self) -> bool {
        self.contains(Self::USER_DEFINED)
    }

    #[inline]
    #[must_use]
    pub const fn is_built_in(self) -> bool {
        self.contains(Self::BUILTIN)
    }

    #[inline]
    #[must_use]
    pub const fn is_internal(self) -> bool {
        self.contains(Self::INTERNAL)
    }

    #[inline]
    #[must_use]
    pub const fn is_internal_set(self) -> bool {
        self.contains(Self::INTERNAL_SET)
    }

    #[inline]
    #[must_use]
    pub const fn is_project(self) -> bool {
        self.contains(Self::PROJECT)
    }

    #[inline]
    #[must_use]
    pub const fn is_internal_reach(self) -> bool {
        self.contains(Self::INTERNAL_REACH)
    }

    #[inline]
    #[must_use]
    pub const fn is_field(self) -> bool {
        self.contains(Self::FIELD)
    }

    #[inline]
    #[must_use]
    pub const fn has_runtime_initial_value(self) -> bool {
        self.contains(Self::RUNTIME_INITIAL_VALUE)
    }

    #[inline]
    #[must_use]
    pub const fn takes_on_effects(self) -> bool {
        self.contains(Self::TAKES_ON_EFFECTS)
    }

    #[inline]
    #[must_use]
    pub const fn is_populated(self) -> bool {
        self.contains(Self::POPULATED)
    }

    #[inline]
    #[must_use]
    pub const fn is_readonly(self) -> bool {
        self.contains(Self::READONLY)
    }

    #[inline]
    #[must_use]
    pub const fn is_writeonly(self) -> bool {
        self.contains(Self::WRITEONLY)
    }

    #[inline]
    #[must_use]
    pub const fn is_enum_interface(self) -> bool {
        self.contains(Self::ENUM_INTERFACE)
    }

    #[inline]
    #[must_use]
    pub const fn is_unchecked(self) -> bool {
        self.contains(Self::UNCHECKED)
    }

    #[inline]
    #[must_use]
    pub const fn ignore_nullable_return(self) -> bool {
        self.contains(Self::IGNORE_NULLABLE_RETURN)
    }

    #[inline]
    #[must_use]
    pub const fn ignore_falsable_return(self) -> bool {
        self.contains(Self::IGNORE_FALSABLE_RETURN)
    }

    #[inline]
    #[must_use]
    pub const fn inherits_docs(self) -> bool {
        self.contains(Self::INHERITS_DOCS)
    }

    #[inline]
    #[must_use]
    pub const fn forbids_named_arguments(self) -> bool {
        self.contains(Self::NO_NAMED_ARGUMENTS)
    }

    #[inline]
    #[must_use]
    pub const fn has_throw(self) -> bool {
        self.contains(Self::HAS_THROW)
    }

    #[inline]
    #[must_use]
    pub const fn is_backed_enum_case(self) -> bool {
        self.contains(Self::BACKED_ENUM_CASE)
    }

    #[inline]
    #[must_use]
    pub const fn is_unit_enum_case(self) -> bool {
        self.contains(Self::UNIT_ENUM_CASE)
    }

    #[inline]
    #[must_use]
    pub const fn is_by_reference(self) -> bool {
        self.contains(Self::BY_REFERENCE)
    }

    #[inline]
    #[must_use]
    pub const fn is_variadic(self) -> bool {
        self.contains(Self::VARIADIC)
    }

    #[inline]
    #[must_use]
    pub const fn is_promoted_property(self) -> bool {
        self.contains(Self::PROMOTED_PROPERTY)
    }

    #[inline]
    #[must_use]
    pub const fn has_default(self) -> bool {
        self.contains(Self::HAS_DEFAULT)
    }

    #[inline]
    #[must_use]
    pub const fn is_virtual_property(self) -> bool {
        self.contains(Self::VIRTUAL_PROPERTY)
    }

    #[inline]
    #[must_use]
    pub const fn is_magic_method(self) -> bool {
        self.contains(Self::MAGIC_METHOD)
    }

    #[inline]
    #[must_use]
    pub const fn is_asymmetric_property(self) -> bool {
        self.contains(Self::ASYMMETRIC_PROPERTY)
    }

    #[inline]
    #[must_use]
    pub const fn is_static(self) -> bool {
        self.contains(Self::STATIC)
    }

    #[inline]
    #[must_use]
    pub const fn is_public_api(self) -> bool {
        self.contains(Self::API)
    }

    #[inline]
    #[must_use]
    pub const fn is_experimental(self) -> bool {
        self.contains(Self::EXPERIMENTAL)
    }

    #[inline]
    #[must_use]
    pub const fn is_mutation_free(self) -> bool {
        self.contains(Self::MUTATION_FREE)
    }

    #[inline]
    #[must_use]
    pub const fn is_external_mutation_free(self) -> bool {
        self.contains(Self::EXTERNAL_MUTATION_FREE)
    }

    #[inline]
    #[must_use]
    pub const fn suspends_fiber(self) -> bool {
        self.contains(Self::SUSPENDS_FIBER)
    }

    #[inline]
    #[must_use]
    pub const fn is_polyfill(self) -> bool {
        self.contains(Self::POLYFILL)
    }

    #[inline]
    #[must_use]
    pub const fn is_patch(self) -> bool {
        self.contains(Self::PATCH)
    }

    #[inline]
    #[must_use]
    pub const fn is_external(self) -> bool {
        self.contains(Self::EXTERNAL)
    }

    #[inline]
    #[must_use]
    pub const fn origin_flags(file_type: FileType) -> Self {
        match file_type {
            FileType::Host => Self::USER_DEFINED,
            FileType::Builtin => Self::BUILTIN,
            FileType::Patch => Self::PATCH,
            FileType::External => Self::EXTERNAL,
            FileType::Vendored => Self::empty(),
        }
    }
}

impl std::ops::BitOr for MetadataFlags {
    type Output = Self;

    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        MetadataFlags(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for MetadataFlags {
    type Output = Self;

    #[inline]
    fn bitand(self, rhs: Self) -> Self::Output {
        MetadataFlags(self.0 & rhs.0)
    }
}

impl std::ops::BitXor for MetadataFlags {
    type Output = Self;

    #[inline]
    fn bitxor(self, rhs: Self) -> Self::Output {
        MetadataFlags(self.0 ^ rhs.0)
    }
}

impl std::ops::Not for MetadataFlags {
    type Output = Self;

    #[inline]
    fn not(self) -> Self::Output {
        MetadataFlags(!self.0)
    }
}

impl std::ops::BitOrAssign for MetadataFlags {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAndAssign for MetadataFlags {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl std::ops::BitXorAssign for MetadataFlags {
    #[inline]
    fn bitxor_assign(&mut self, rhs: Self) {
        self.0 ^= rhs.0;
    }
}

#[cfg(test)]
mod tests {
    use super::MetadataFlags;

    #[test]
    fn each_flag_is_one_bit_that_no_other_flag_shares() {
        let flags = [
            ("ABSTRACT", MetadataFlags::ABSTRACT),
            ("FINAL", MetadataFlags::FINAL),
            ("READONLY", MetadataFlags::READONLY),
            ("DEPRECATED", MetadataFlags::DEPRECATED),
            ("ENUM_INTERFACE", MetadataFlags::ENUM_INTERFACE),
            ("POPULATED", MetadataFlags::POPULATED),
            ("INTERNAL", MetadataFlags::INTERNAL),
            ("CONSISTENT_CONSTRUCTOR", MetadataFlags::CONSISTENT_CONSTRUCTOR),
            ("CONSISTENT_TEMPLATES", MetadataFlags::CONSISTENT_TEMPLATES),
            ("UNCHECKED", MetadataFlags::UNCHECKED),
            ("USER_DEFINED", MetadataFlags::USER_DEFINED),
            ("BUILTIN", MetadataFlags::BUILTIN),
            ("HAS_YIELD", MetadataFlags::HAS_YIELD),
            ("MUST_USE", MetadataFlags::MUST_USE),
            ("HAS_THROW", MetadataFlags::HAS_THROW),
            ("PURE", MetadataFlags::PURE),
            ("IGNORE_NULLABLE_RETURN", MetadataFlags::IGNORE_NULLABLE_RETURN),
            ("IGNORE_FALSABLE_RETURN", MetadataFlags::IGNORE_FALSABLE_RETURN),
            ("INHERITS_DOCS", MetadataFlags::INHERITS_DOCS),
            ("NO_NAMED_ARGUMENTS", MetadataFlags::NO_NAMED_ARGUMENTS),
            ("BACKED_ENUM_CASE", MetadataFlags::BACKED_ENUM_CASE),
            ("UNIT_ENUM_CASE", MetadataFlags::UNIT_ENUM_CASE),
            ("BY_REFERENCE", MetadataFlags::BY_REFERENCE),
            ("VARIADIC", MetadataFlags::VARIADIC),
            ("PROMOTED_PROPERTY", MetadataFlags::PROMOTED_PROPERTY),
            ("HAS_DEFAULT", MetadataFlags::HAS_DEFAULT),
            ("VIRTUAL_PROPERTY", MetadataFlags::VIRTUAL_PROPERTY),
            ("ASYMMETRIC_PROPERTY", MetadataFlags::ASYMMETRIC_PROPERTY),
            ("STATIC", MetadataFlags::STATIC),
            ("WRITEONLY", MetadataFlags::WRITEONLY),
            ("MAGIC_METHOD", MetadataFlags::MAGIC_METHOD),
            ("API", MetadataFlags::API),
            ("MUTATION_FREE", MetadataFlags::MUTATION_FREE),
            ("EXTERNAL_MUTATION_FREE", MetadataFlags::EXTERNAL_MUTATION_FREE),
            ("SUSPENDS_FIBER", MetadataFlags::SUSPENDS_FIBER),
            ("EXPERIMENTAL", MetadataFlags::EXPERIMENTAL),
            ("POLYFILL", MetadataFlags::POLYFILL),
            ("PATCH", MetadataFlags::PATCH),
            ("EXTERNAL", MetadataFlags::EXTERNAL),
            ("SHARP", MetadataFlags::SHARP),
            ("MODULE", MetadataFlags::MODULE),
            ("NAMED_CONSTRUCTOR", MetadataFlags::NAMED_CONSTRUCTOR),
            ("INIT", MetadataFlags::INIT),
            ("EVENT", MetadataFlags::EVENT),
            ("EXTENSION_METHOD", MetadataFlags::EXTENSION_METHOD),
            ("EXTENSION_PROPERTY", MetadataFlags::EXTENSION_PROPERTY),
            ("FOREIGN", MetadataFlags::FOREIGN),
            ("STRUCT", MetadataFlags::STRUCT),
            ("REQUIRED", MetadataFlags::REQUIRED),
            ("OPERATOR", MetadataFlags::OPERATOR),
            ("ENUM_CASE_CLASS", MetadataFlags::ENUM_CASE_CLASS),
            ("SEALED", MetadataFlags::SEALED),
            ("INTERNAL_SET", MetadataFlags::INTERNAL_SET),
            ("PROJECT", MetadataFlags::PROJECT),
            ("INTERNAL_REACH", MetadataFlags::INTERNAL_REACH),
            ("FIELD", MetadataFlags::FIELD),
            ("RUNTIME_INITIAL_VALUE", MetadataFlags::RUNTIME_INITIAL_VALUE),
            ("TAKES_ON_EFFECTS", MetadataFlags::TAKES_ON_EFFECTS),
        ];

        let mut seen = MetadataFlags::empty();
        for (name, flag) in flags {
            assert_eq!(flag.bits().count_ones(), 1, "{name} is not one bit");
            assert!((seen & flag).bits() == 0, "{name} shares a bit with an earlier flag");
            seen |= flag;
        }
    }
}
