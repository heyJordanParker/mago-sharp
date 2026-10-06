<?php

namespace Sharp;

final class Int
{
    /**
     * Parses a string that holds only an int, as C#'s `int.Parse` does: `[ws][sign]digits[ws]`.
     *
     * @throws \ValueError when `$value` is not a string or does not hold an int.
     * @throws \ArithmeticError when the int is outside PHP_INT_MIN to PHP_INT_MAX.
     */
    public static function parse(mixed $value): int {}

    /**
     * Gives null where `parse` throws.
     *
     * @pure
     */
    public static function tryParse(mixed $value): ?int {}
}

final class Float
{
    /**
     * Parses a string that holds only a float: `[ws][sign](digits[.digits] | .digits)([eE][sign]digits)?[ws]`.
     *
     * @throws \ValueError when `$value` is not a string or does not hold a float.
     * @throws \ArithmeticError when the float is outside -PHP_FLOAT_MAX to PHP_FLOAT_MAX.
     */
    public static function parse(mixed $value): float {}

    /**
     * Gives null where `parse` throws.
     *
     * @pure
     */
    public static function tryParse(mixed $value): ?float {}
}

/**
 * The methods of a PHP# `List<T>`, as spec section 12 writes them. The analyzer checks a call on a list against
 * them, and the engine runs them on `Sharp\Collection`. A method without `@mutation-free` changes the list.
 *
 * @template T
 */
final class ListMethods
{
    /**
     * Appends `$value` after the last element.
     *
     * @param T $value
     */
    public function add(mixed $value): void {}

    /**
     * Replaces the element at `$index`.
     *
     * @param T $value
     *
     * @throws \OutOfRangeException when `$index` is not an index of the list.
     */
    public function set(int $index, mixed $value): void {}

    /**
     * @return T|null the element at `$index`, or null when `$index` is not an index of the list.
     *
     * @mutation-free
     */
    public function get(int $index): mixed {}

    /**
     * @return array<int, T> each index with its element, for `for (const [i, x] of list.entries())`.
     *
     * @mutation-free
     */
    public function entries(): array {}

    /**
     * @param \Closure(T): bool $predicate
     *
     * @return list<T> the elements `$predicate` keeps, in order.
     *
     * @mutation-free
     */
    public function filter(\Closure $predicate): array {}

    /**
     * @template R
     *
     * @param \Closure(T): R $transform
     *
     * @return list<R> what `$transform` gives for each element, in order.
     *
     * @mutation-free
     */
    public function map(\Closure $transform): array {}

    /**
     * @template R of int|float
     *
     * @param \Closure(T): R $selector
     *
     * @return R the sum of what `$selector` gives for each element, 0 for an empty list.
     *
     * @mutation-free
     */
    public function sumOf(\Closure $selector): int|float {}

    /**
     * @param \Closure(T): bool $predicate
     *
     * @return T the first element `$predicate` keeps.
     *
     * @throws \OutOfRangeException when `$predicate` keeps no element.
     *
     * @mutation-free
     */
    public function first(\Closure $predicate): mixed {}

    /**
     * @param \Closure(T): bool $predicate
     *
     * @return bool whether `$predicate` keeps any element.
     *
     * @mutation-free
     */
    public function any(\Closure $predicate): bool {}

    /**
     * @template G of array-key
     *
     * @param \Closure(T): G $key
     *
     * @return array<G, list<T>> the elements under the key `$key` gives each, in order.
     *
     * @mutation-free
     */
    public function groupBy(\Closure $key): array {}

    /**
     * @template G of array-key
     *
     * @param \Closure(T): G $key
     *
     * @return array<G, T> each element under the key `$key` gives it, the last one where two share a key.
     *
     * @mutation-free
     */
    public function associateBy(\Closure $key): array {}

    /**
     * @template R
     *
     * @param \Closure(T): R $selector
     *
     * @return list<T> the elements in ascending order of what `$selector` gives, equal ones in their order.
     *
     * @mutation-free
     */
    public function sortedBy(\Closure $selector): array {}
}

/**
 * The methods of a PHP# `Map<K, V>`, as spec section 12 writes them. The analyzer checks a call on a map against
 * them, and the engine runs them on `Sharp\Collection`. A method without `@mutation-free` changes the map.
 *
 * @template K of array-key
 * @template V
 */
final class MapMethods
{
    /**
     * Removes the entry of `$key`, if the map has one.
     *
     * @param K $key
     */
    public function delete(int|string|\BackedEnum $key): void {}

    /**
     * @param K $key
     *
     * @return V|null the value of `$key`, or null when the map has no entry for it.
     *
     * @mutation-free
     */
    public function get(int|string|\BackedEnum $key): mixed {}

    /**
     * @param \Closure(V): bool $predicate
     *
     * @return list<V> the values `$predicate` keeps, in order, without their keys.
     *
     * @mutation-free
     */
    public function filter(\Closure $predicate): array {}

    /**
     * @param \Closure(V): bool $predicate
     *
     * @return array<K, V> the entries whose values `$predicate` keeps, with their keys.
     *
     * @mutation-free
     */
    public function filterValues(\Closure $predicate): array {}

    /**
     * @template R
     *
     * @param \Closure(V): R $transform
     *
     * @return list<R> what `$transform` gives for each value, in order.
     *
     * @mutation-free
     */
    public function map(\Closure $transform): array {}

    /**
     * @template R of int|float
     *
     * @param \Closure(V): R $selector
     *
     * @return R the sum of what `$selector` gives for each value, 0 for an empty map.
     *
     * @mutation-free
     */
    public function sumOf(\Closure $selector): int|float {}

    /**
     * @param \Closure(V): bool $predicate
     *
     * @return V the first value `$predicate` keeps.
     *
     * @throws \OutOfRangeException when `$predicate` keeps no value.
     *
     * @mutation-free
     */
    public function first(\Closure $predicate): mixed {}

    /**
     * @param \Closure(V): bool $predicate
     *
     * @return bool whether `$predicate` keeps any value.
     *
     * @mutation-free
     */
    public function any(\Closure $predicate): bool {}

    /**
     * @template G of array-key
     *
     * @param \Closure(V): G $key
     *
     * @return array<G, list<V>> the values under the key `$key` gives each, in order.
     *
     * @mutation-free
     */
    public function groupBy(\Closure $key): array {}

    /**
     * @template G of array-key
     *
     * @param \Closure(V): G $key
     *
     * @return array<G, V> each value under the key `$key` gives it, the last one where two share a key.
     *
     * @mutation-free
     */
    public function associateBy(\Closure $key): array {}

    /**
     * @template R
     *
     * @param \Closure(V): R $selector
     *
     * @return list<V> the values in ascending order of what `$selector` gives, equal ones in their order.
     *
     * @mutation-free
     */
    public function sortedBy(\Closure $selector): array {}
}
