<?php

namespace Sharp;

/**
 * Where code sits in its source: its file, directory, line and column, and the function it is in. PHP# reads it here
 * instead of from PHP's magic constants. `$function` is the fully qualified dotted name.
 */
final class Position
{
    public readonly string $file;
    public readonly string $directory;
    public readonly int $line;
    public readonly int $column;
    public readonly string $function;

    public function __construct(string $file, int $line, int $column, string $function) {}

    /**
     * The position where it is written, or the caller's position as a parameter's default. The engine declares no
     * `current()`: the bridge lowers each call to a `new Position(…)`.
     */
    public static function current(): Position {}
}

/**
 * The process environment: its command-line arguments, its current directory and its environment variables. PHP#
 * reads them here instead of from PHP's superglobals.
 */
final class Environment
{
    /**
     * @var list<string> the command-line arguments, starting with the script's name.
     */
    public readonly array $arguments;

    /**
     * @var string the directory the process runs in.
     */
    public readonly string $currentDirectory;

    public function __construct() {}

    /**
     * @return string|null the environment variable `$name`, or null when it is not set.
     */
    public function variable(string $name): ?string {}
}

final class List
{
    /**
     * The list itself, or a list holding the one value. PHP# writes this instead of PHP's `(array)` cast. The analyzer
     * refuses a `.sharp` call when `T` could itself be a list, because a `List` and a `Map` both run as PHP arrays.
     *
     * @template T
     *
     * @param T|list<T> $value
     *
     * @return list<T>
     */
    public static function wrap(mixed $value): array {}
}

/**
 * The methods a PHP# `List<T>` has. The analyzer checks a call on a list against them, and the engine runs them on
 * `Sharp\Collection`. A method without `@mutation-free` changes the list.
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
 * The methods a PHP# `Map<K, V>` has. The analyzer checks a call on a map against them, and the engine runs them on
 * `Sharp\Collection`. A method without `@mutation-free` changes the map.
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
    public function delete(int|string $key): void {}

    /**
     * @param K $key
     *
     * @return V|null the value of `$key`, or null when the map has no entry for it.
     *
     * @mutation-free
     */
    public function get(int|string $key): mixed {}

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

/**
 * The methods a PHP# `Set<T>` has. The analyzer checks a call on a set against them. A method without
 * `@mutation-free` changes the set.
 *
 * @template T
 */
final class SetMethods
{
    /**
     * Adds `$value`, if the set does not hold it.
     *
     * @param T $value
     *
     * @return bool whether the set did not hold `$value`.
     */
    public function add(mixed $value): bool {}

    /**
     * Removes `$value`, if the set holds it.
     *
     * @param T $value
     *
     * @return bool whether the set held `$value`.
     */
    public function remove(mixed $value): bool {}

    /**
     * Removes every element.
     */
    public function clear(): void {}

    /**
     * @param T $value
     *
     * @return bool whether the set holds `$value`.
     *
     * @mutation-free
     */
    public function contains(mixed $value): bool {}

    /**
     * @return int how many elements the set holds.
     *
     * @mutation-free
     */
    public function count(): int {}

    /**
     * @return list<T> the elements, in the order they were added.
     *
     * @mutation-free
     */
    public function toList(): array {}

    /**
     * The analyzer gives a call the type `Set<T>`, which a docblock cannot write.
     *
     * @param \Closure(T): bool $predicate
     *
     * @return array<array-key, T> the set of the elements `$predicate` keeps, in order.
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
     * @return R the sum of what `$selector` gives for each element, 0 for an empty set.
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
