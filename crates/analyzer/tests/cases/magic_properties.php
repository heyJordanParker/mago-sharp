<?php

function get_val(): mixed
{
    return 'some_value';
}

function i_take_string(string $_): void
{
}

function i_take_mixed(mixed $_): void
{
}

/**
 * @property $untyped_rw
 * @property string $typed_rw
 * @property-read $untyped_ro
 * @property-read string $typed_ro
 * @property-write $untyped_wo
 * @property-write string $typed_wo
 */
class ExampleWithNoMagicMethods
{
}

/**
 * @property $untyped_rw
 * @property string $typed_rw
 * @property-read $untyped_ro
 * @property-read string $typed_ro
 * @property-write $untyped_wo
 * @property-write string $typed_wo
 */
class ExampleWithGetOnly
{
    public function __get(string $name): mixed
    {
        return get_val();
    }
}

/**
 * @property $untyped_rw
 * @property string $typed_rw
 * @property-read $untyped_ro
 * @property-read string $typed_ro
 * @property-write $untyped_wo
 * @property-write string $typed_wo
 */
class ExampleWithSetOnly
{
    public function __set(string $name, mixed $value): void
    {
    }
}

/**
 * @property $untyped_rw
 * @property string $typed_rw
 * @property-read $untyped_ro
 * @property-read string $typed_ro
 * @property-write $untyped_wo
 * @property-write string $typed_wo
 */
class ExampleWithBothMagicMethods
{
    public function __get(string $name): mixed
    {
        return get_val();
    }

    public function __set(string $name, mixed $value): void
    {
    }
}

class ExampleWithMagicGetOnlyNoDocs
{
    public function __get(string $name): mixed
    {
        return get_val();
    }
}

class ExampleWithMagicSetOnlyNoDocs
{
    public function __set(string $name, mixed $value): void
    {
    }
}

class ExampleWithMagicBothNoDocs
{
    public function __get(string $name): mixed
    {
        return get_val();
    }

    public function __set(string $name, mixed $value): void
    {
    }
}

/**
 * Contains all test cases that should produce an ERROR.
 *
 * @mago-expect analysis:impossible-assignment
 */
function err(): void
{
    $obj_no_magic = new ExampleWithNoMagicMethods();
    $obj_get_only = new ExampleWithGetOnly();
    $obj_set_only = new ExampleWithSetOnly();
    $obj_both = new ExampleWithBothMagicMethods();
    $obj_get_only_no_docs = new ExampleWithMagicGetOnlyNoDocs();
    $obj_set_only_no_docs = new ExampleWithMagicSetOnlyNoDocs();

    // Errors for reading a write-only property
    $_ = $obj_no_magic->untyped_wo; // @mago-expect analysis:invalid-property-read
    $_ = $obj_no_magic->typed_wo; // @mago-expect analysis:invalid-property-read
    $_ = $obj_get_only->untyped_wo; // @mago-expect analysis:invalid-property-read
    $_ = $obj_get_only->typed_wo; // @mago-expect analysis:invalid-property-read
    $_ = $obj_set_only->untyped_wo; // @mago-expect analysis:invalid-property-read
    $_ = $obj_set_only->typed_wo; // @mago-expect analysis:invalid-property-read
    $_ = $obj_both->untyped_wo; // @mago-expect analysis:invalid-property-read
    $_ = $obj_both->typed_wo; // @mago-expect analysis:invalid-property-read

    $obj_no_magic->untyped_ro = 1; // @mago-expect analysis:invalid-property-write,missing-magic-method
    $obj_no_magic->typed_ro = 'a'; // @mago-expect analysis:invalid-property-write,missing-magic-method
    $obj_get_only->untyped_ro = 1; // @mago-expect analysis:invalid-property-write,missing-magic-method
    $obj_get_only->typed_ro = 'a'; // @mago-expect analysis:invalid-property-write,missing-magic-method
    $obj_set_only->untyped_ro = 1; // @mago-expect analysis:invalid-property-write
    $obj_set_only->typed_ro = 'a'; // @mago-expect analysis:invalid-property-write
    $obj_both->untyped_ro = 1; // @mago-expect analysis:invalid-property-write
    $obj_both->typed_ro = 'a'; // @mago-expect analysis:invalid-property-write

    // Errors for mismatched types on write
    $obj_set_only->typed_rw = 1; // @mago-expect analysis:invalid-property-assignment-value
    $obj_set_only->typed_wo = 1; // @mago-expect analysis:invalid-property-assignment-value
    $obj_both->typed_rw = 1; // @mago-expect analysis:invalid-property-assignment-value
    $obj_both->typed_wo = 1; // @mago-expect analysis:invalid-property-assignment-value

    // Errors for accessing properties on classes with missing magic methods
    $_ = $obj_set_only_no_docs->any_prop; // @mago-expect analysis:non-existent-property
    $obj_get_only_no_docs->any_prop = 1; // @mago-expect analysis:non-existent-property

    $obj_no_magic = new ExampleWithNoMagicMethods();
    $obj_get_only = new ExampleWithGetOnly();
    $obj_set_only = new ExampleWithSetOnly();
    $obj_get_only_no_docs = new ExampleWithMagicGetOnlyNoDocs();
    $obj_set_only_no_docs = new ExampleWithMagicSetOnlyNoDocs();
    $obj_both_no_docs = new ExampleWithMagicBothNoDocs();

    $_ = $obj_no_magic->untyped_rw; // @mago-expect analysis:missing-magic-method
    $_ = $obj_no_magic->typed_rw; // @mago-expect analysis:missing-magic-method
    $_ = $obj_no_magic->untyped_ro; // @mago-expect analysis:missing-magic-method
    $_ = $obj_no_magic->typed_ro; // @mago-expect analysis:missing-magic-method
    $_ = $obj_set_only->untyped_rw; // @mago-expect analysis:missing-magic-method
    $_ = $obj_set_only->typed_rw; // @mago-expect analysis:missing-magic-method
    $_ = $obj_set_only->untyped_ro; // @mago-expect analysis:missing-magic-method
    $_ = $obj_set_only->typed_ro; // @mago-expect analysis:missing-magic-method
    $obj_no_magic->untyped_rw = 1; // @mago-expect analysis:missing-magic-method
    $obj_no_magic->typed_rw = 'a'; // @mago-expect analysis:missing-magic-method
    $obj_no_magic->untyped_wo = 1; // @mago-expect analysis:missing-magic-method
    $obj_no_magic->typed_wo = 'a'; // @mago-expect analysis:missing-magic-method
    $obj_get_only->untyped_rw = 1; // @mago-expect analysis:missing-magic-method
    $obj_get_only->typed_rw = 'a'; // @mago-expect analysis:missing-magic-method
    $obj_get_only->untyped_wo = 1; // @mago-expect analysis:missing-magic-method
    $obj_get_only->typed_wo = 'a'; // @mago-expect analysis:missing-magic-method
}

/**
 * Contains all test cases that should produce a WARNING.
 */
function warn(): void
{
    $obj_get_only_no_docs = new ExampleWithMagicGetOnlyNoDocs();
    $obj_set_only_no_docs = new ExampleWithMagicSetOnlyNoDocs();
    $obj_both_no_docs = new ExampleWithMagicBothNoDocs();

    $_ = $obj_get_only_no_docs->any_prop; // @mago-expect analysis:non-documented-property
    $_ = $obj_both_no_docs->any_prop; // @mago-expect analysis:non-documented-property
    $obj_set_only_no_docs->any_prop = 1; // @mago-expect analysis:non-documented-property
    $obj_both_no_docs->any_prop = 1; // @mago-expect analysis:non-documented-property
}

/**
 * Contains all test cases that should pass with no issues.
 */
function ok(): void
{
    $obj_get_only = new ExampleWithGetOnly();
    $obj_set_only = new ExampleWithSetOnly();
    $obj_both = new ExampleWithBothMagicMethods();

    i_take_mixed($obj_get_only->untyped_rw);
    i_take_string($obj_get_only->typed_rw);
    i_take_mixed($obj_get_only->untyped_ro);
    i_take_string($obj_get_only->typed_ro);
    i_take_mixed($obj_both->untyped_rw);
    i_take_string($obj_both->typed_rw);
    i_take_mixed($obj_both->untyped_ro);
    i_take_string($obj_both->typed_ro);

    $obj_set_only->untyped_rw = 1;
    $obj_set_only->typed_rw = 'a';
    $obj_set_only->untyped_wo = 1;
    $obj_set_only->typed_wo = 'a';
    $obj_both->untyped_rw = 1;
    $obj_both->typed_rw = 'a';
    $obj_both->untyped_wo = 1;
    $obj_both->typed_wo = 'a';
}
