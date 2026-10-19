<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Metadata;

/**
 * Immutable semantic flags attached to scanned metadata.
 *
 * `$bits` holds all 64 bits the analyzer sets, so bit 63 is the sign bit of PHP's `int`, as a Java `long` holds a
 * protobuf `fixed64`. Each flag is named as the analyzer's codex names it, the PHP# ones from `SHARP` on included.
 *
 * @api
 */
final class MetadataFlags
{
    public const ABSTRACT = 1;
    public const FINAL = 1 << 1;
    public const READONLY = 1 << 3;
    public const DEPRECATED = 1 << 4;
    public const ENUM_INTERFACE = 1 << 5;
    public const POPULATED = 1 << 6;
    public const INTERNAL = 1 << 7;
    public const CONSISTENT_CONSTRUCTOR = 1 << 11;
    public const CONSISTENT_TEMPLATES = 1 << 12;
    public const UNCHECKED = 1 << 13;
    public const USER_DEFINED = 1 << 14;
    public const BUILTIN = 1 << 15;
    public const HAS_YIELD = 1 << 16;
    public const MUST_USE = 1 << 17;
    public const HAS_THROW = 1 << 18;
    public const PURE = 1 << 19;
    public const IGNORE_NULLABLE_RETURN = 1 << 20;
    public const IGNORE_FALSABLE_RETURN = 1 << 21;
    public const INHERITS_DOCS = 1 << 22;
    public const NO_NAMED_ARGUMENTS = 1 << 23;
    public const BACKED_ENUM_CASE = 1 << 24;
    public const UNIT_ENUM_CASE = 1 << 25;
    public const BY_REFERENCE = 1 << 26;
    public const VARIADIC = 1 << 27;
    public const PROMOTED_PROPERTY = 1 << 28;
    public const HAS_DEFAULT = 1 << 29;
    public const VIRTUAL_PROPERTY = 1 << 30;
    public const ASYMMETRIC_PROPERTY = 1 << 31;
    public const STATIC = 1 << 32;
    public const WRITEONLY = 1 << 33;
    public const MAGIC_METHOD = 1 << 35;
    public const API = 1 << 36;
    public const MUTATION_FREE = 1 << 37;
    public const EXTERNAL_MUTATION_FREE = 1 << 38;
    public const SUSPENDS_FIBER = 1 << 39;
    public const EXPERIMENTAL = 1 << 40;
    public const POLYFILL = 1 << 41;
    public const PATCH = 1 << 42;
    public const EXTERNAL = 1 << 43;
    /** Declared in a PHP# file. */
    public const SHARP = 1 << 44;
    /** A PHP# module, spec section 32, which holds only laws and never runs. */
    public const MODULE = 1 << 45;
    /** A PHP# named constructor, spec section 9.1, called as `new Class.name(…)`. */
    public const NAMED_CONSTRUCTOR = 1 << 46;
    /** A PHP# property declared with `init`, spec section 6.1, set only while its object is created. */
    public const INIT = 1 << 47;
    /** A PHP# event, spec section 15. */
    public const EVENT = 1 << 48;
    /** A PHP# extension method, spec section 26. */
    public const EXTENSION_METHOD = 1 << 49;
    /** A PHP# extension's computed property, spec section 26. */
    public const EXTENSION_PROPERTY = 1 << 50;
    /** A PHP# `foreign` class, spec section 29: an effect. */
    public const FOREIGN = 1 << 51;
    /** A PHP# struct, spec section 10. */
    public const STRUCT = 1 << 52;
    /** A PHP# property each creation of its object must set, spec sections 6.1 and 10. */
    public const REQUIRED = 1 << 53;
    /** A PHP# operator, spec section 19. */
    public const OPERATOR = 1 << 54;
    /** A PHP# enum case that runs as a class, spec section 20. */
    public const ENUM_CASE_CLASS = 1 << 55;
    /** A PHP# sealed interface, spec section 20.1. */
    public const SEALED = 1 << 56;
    /** A PHP# property whose `set` or `init` is `internal`, spec section 5. */
    public const INTERNAL_SET = 1 << 57;
    /** A PHP# class-like of a project, whose `composer.json` says `"type": "project"`. */
    public const PROJECT = 1 << 58;
    /** A PHP# declaration or member written `internal`, spec section 5. `INTERNAL` stays PHPDoc's `@internal`. */
    public const INTERNAL_REACH = 1 << 59;
    /** A PHP# field, spec section 6: storage that is never a property. */
    public const FIELD = 1 << 60;
    /** A PHP# static whose initial value runs as code when its class's statics are first used, spec section 6. */
    public const RUNTIME_INITIAL_VALUE = 1 << 61;
    /** A PHP# parameter whose default is `Position.current()`, spec section 27. */
    public const CALLER_POSITION = 1 << 62;
    /** A PHP# function parameter whose type states no effects, spec section 29. It is bit 63, PHP's `PHP_INT_MIN`. */
    public const TAKES_ON_EFFECTS = 1 << 63;

    public function __construct(
        public readonly int $bits,
    ) {}

    public function contains(int $flag): bool
    {
        return ($this->bits & $flag) === $flag;
    }
}
