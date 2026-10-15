<?php

declare(strict_types=1);

/** @psalm-assert non-empty-list<int|bool> $value */
function assert_non_empty_ints_or_bools(mixed $value): void {}

/** @psalm-assert list<int|bool> $value */
function assert_ints_or_bools(mixed $value): void {}

/** @psalm-assert list{int|bool, int|bool} $value */
function assert_pair_of_ints_or_bools(mixed $value): void {}

/** @param non-empty-list<int> $value */
function take_ints(array $value): void {}

/** @param list{int, int} $value */
function take_pair(array $value): void {}

/** @param list<int|string> $value */
function neither_known(array $value): void
{
    assert_non_empty_ints_or_bools($value);
    take_ints($value);
}

/** @param list{int|string, int|string} $value */
function existing_known(array $value): void
{
    assert_ints_or_bools($value);
    take_pair($value);
}

/** @param list<int|string> $value */
function asserted_known(array $value): void
{
    assert_pair_of_ints_or_bools($value);
    take_pair($value);
}

/** @param list{int|string, int|string} $value */
function both_known(array $value): void
{
    assert_pair_of_ints_or_bools($value);
    take_pair($value);
}
