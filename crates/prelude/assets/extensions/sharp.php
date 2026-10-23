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

final class List
{
    /**
     * The list itself, or a list holding the one value. PHP# writes this instead of PHP's `(array)` cast. The analyzer
     * refuses a `.sharp` call when `T` could itself be a list, because a `List` is returned as it is.
     *
     * @template T
     *
     * @param T|list<T> $value
     *
     * @return list<T>
     */
    public static function wrap(mixed $value): array {}
}

final class Class
{
    /**
     * The class of the object `$value` holds, which may be a subclass of its type. PHP# writes this instead of PHP's
     * `$value::class`, because `typeof` takes a type only. Plain PHP receives it as the class name.
     *
     * @template T of object
     *
     * @param T $value
     *
     * @return class-string<T>
     */
    public static function of(object $value): string {}
}

/**
 * A PHP# `Map<K, V>`, as plain PHP receives one: a value with each key kept as itself, an object included, and found by
 * its `==` and `hash()`, in insertion order. Plain PHP reads it, writes it and removes from it through its index, which
 * copies a shared `Map` first, and loops over its entries.
 *
 * @template K
 * @template V
 *
 * @implements \ArrayAccess<K, V>
 * @implements \IteratorAggregate<K, V>
 */
final class Map implements \ArrayAccess, \IteratorAggregate, \Countable, \JsonSerializable
{
    /**
     * @param list<mixed> $keysAndValues each key, then its value.
     */
    public function __construct(array $keysAndValues = []) {}

    /**
     * @return list<mixed> each key, then its value.
     */
    public function __serialize(): array {}

    /**
     * Throws `ValueError` for data that is not each key followed by its value, and `Error` on a `Map` that holds entries.
     *
     * @param list<mixed> $data each key, then its value.
     */
    public function __unserialize(array $data): void {}

    /**
     * @template TKey of array-key
     * @template TValue
     *
     * @param array<TKey, TValue> $entries
     *
     * @return Map<TKey, TValue>
     */
    public static function from(array $entries): Map {}

    /**
     * @param K $offset
     */
    public function offsetExists(mixed $offset): bool {}

    /**
     * @param K $offset
     *
     * @return V|null the value, or null when the key is missing.
     */
    public function offsetGet(mixed $offset): mixed {}

    /**
     * Throws: write through the index, `$map[$key] = $value`, which copies a shared `Map` first.
     *
     * @param K $offset
     * @param V $value
     */
    public function offsetSet(mixed $offset, mixed $value): void {}

    /**
     * Throws: remove through the index, `unset($map[$key])`, which copies a shared `Map` first.
     *
     * @param K $offset
     */
    public function offsetUnset(mixed $offset): void {}

    /**
     * @return \Iterator<K, V>
     */
    public function getIterator(): \Iterator {}

    public function count(): int {}

    public function jsonSerialize(): mixed {}

    /**
     * Throws `ValueError` when a key is no `int` or `string`.
     *
     * @return array<K, V>
     */
    public function toArray(): array {}

    /**
     * @return list<K>
     */
    public function keys(): array {}

    /**
     * @return list<V>
     */
    public function values(): array {}

    /**
     * @return list<array{K, V}> each entry, `[key, value]`, in insertion order.
     */
    public function toList(): array {}
}

/**
 * A PHP# `Set<T>`, as plain PHP receives one: a value with each element once, found by its `==` and `hash()`, in
 * insertion order. Plain PHP tests for an element with `isset($set[$element])`, adds one with `$set[] = $element`,
 * removes one with `unset($set[$element])`, each of which copies a shared `Set` first, and loops over its elements.
 *
 * @template T
 *
 * @implements \ArrayAccess<T, T>
 * @implements \IteratorAggregate<int, T>
 */
final class Set implements \ArrayAccess, \IteratorAggregate, \Countable, \JsonSerializable
{
    /**
     * @param list<T> $values
     */
    public function __construct(array $values = []) {}

    /**
     * @return list<T>
     */
    public function __serialize(): array {}

    /**
     * Throws `ValueError` for data that is not a list, and `Error` on a `Set` that holds elements.
     *
     * @param list<T> $data
     */
    public function __unserialize(array $data): void {}

    /**
     * @param T $offset
     */
    public function offsetExists(mixed $offset): bool {}

    /**
     * Throws: a `Set` has no index.
     *
     * @param T $offset
     *
     * @return never
     */
    public function offsetGet(mixed $offset): mixed {}

    /**
     * Throws: add through the index, `$set[] = $element`, which copies a shared `Set` first.
     *
     * @param T|null $offset
     * @param T $value
     */
    public function offsetSet(mixed $offset, mixed $value): void {}

    /**
     * Throws: remove through the index, `unset($set[$element])`, which copies a shared `Set` first.
     *
     * @param T $offset
     */
    public function offsetUnset(mixed $offset): void {}

    /**
     * @return \Iterator<int, T>
     */
    public function getIterator(): \Iterator {}

    public function count(): int {}

    public function jsonSerialize(): mixed {}

    /**
     * @return list<T>
     */
    public function toArray(): array {}

    /**
     * @return list<T>
     */
    public function toList(): array {}
}

/**
 * The engine's container, which builds the services the app's modules bind. Plain PHP reaches it by name, as a
 * framework's service provider does.
 */
final class Container
{
    /**
     * The service `$id` names: a class name or a service id. Throws `ContainerError` when the container can't build it.
     */
    public function get(string $id): mixed {}

    /**
     * Whether a module binds `$id`.
     */
    public function has(string $id): bool {}
}

/**
 * A failure of the container itself: the service no module binds, the class that needs itself, the parameter with no
 * class type, or the bound class that isn't there.
 */
final class ContainerError extends \Error {}

/**
 * The handle of a listener `Events.on` starts: the engine stops the listener when the handle is destroyed.
 */
final class Listening
{
    private function __construct() {}
}

namespace Sharp\Internal;

/**
 * Marks a method of `ListMethods` that the bridge lowers to PHP on the array, from the list's type, instead of a call.
 * A `List` is a PHP array, which has no methods, so the checker refuses a marked method on a value that can also be an
 * object, whose own method would not run.
 */
#[\Attribute(\Attribute::TARGET_METHOD)]
final class Inlined {}

/**
 * Marks the parameter of a standard library method that writes its argument as JSON text, as `Json.encode`'s, so the
 * checker refuses an argument holding a `Map` key with no JSON text.
 */
#[\Attribute(\Attribute::TARGET_PARAMETER)]
final class EncodedAsJson {}

/**
 * Marks the standard library class that is the effect of `emit`, as `Events` is. `just regen-sharp-classes` reads the
 * mark, so the checker copies no library name.
 */
#[\Attribute(\Attribute::TARGET_CLASS)]
final class EffectOfEmit {}

/**
 * Marks the standard library interface a property behavior implements, as `Behavior<T>` is. `just regen-sharp-classes`
 * reads the mark, so the checker copies no library name.
 */
#[\Attribute(\Attribute::TARGET_CLASS)]
final class PropertyBehavior {}

/**
 * Marks a standard library method without a body whose implementations may have any effect, because the code that
 * calls it carries the implementation's effects, as `Dispatcher.dispatch` and `Behavior<T>`'s `get` and `set` are.
 */
#[\Attribute(\Attribute::TARGET_METHOD)]
final class AcceptsAnyEffect {}

/**
 * Marks the standard library method whose call starts a listener of its first type argument, as `Events.on` does, so
 * the checker refuses a type argument no event can be.
 */
#[\Attribute(\Attribute::TARGET_METHOD)]
final class StartsListener {}

/**
 * Marks a standard library generic method that parses a value into its type argument with the type's `parse`, as
 * `Json.decode<T>` does, so the checker gives each call's type argument the struct parse check.
 */
#[\Attribute(\Attribute::TARGET_METHOD)]
final class ParsesTypeArgument {}

/**
 * The class a type parameter with no class bound stands for in a method marked `ParsesTypeArgument`, as `Json.decode<T>`
 * is: each struct and PHP# class, whose `parse` and `tryParse` it declares, so `T.parse($value)` gives a `T`. Each call
 * of the method gets the struct parse check for its type argument.
 */
abstract class Parsed
{
    public static function parse(mixed $value): static {}

    public static function tryParse(mixed $value): ?static {}
}

/**
 * Marks a standard library method that a law's proof reads as the Lean function `$function` folded over its
 * arguments, as `Math.max` is Lean's `max`, so a law reasons about it as it does about `<`.
 */
#[\Attribute(\Attribute::TARGET_METHOD)]
final class LeanFunction
{
    public function __construct(public string $function) {}
}

/**
 * The methods a PHP# `List<T>` has. The analyzer checks a call on a list against them, and the engine runs them on
 * `Sharp\Collection`, except the methods marked `Inlined`, which the bridge lowers from the list's type. A method
 * without `@mutation-free` changes the list.
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
     * @return T|null the element at `$index`, or null when `$index` is not an index of the list.
     *
     * @mutation-free
     */
    #[Inlined]
    public function getOrNull(int $index): mixed {}

    /**
     * The analyzer types the result `Set<T>`, which a docblock cannot write.
     *
     * @return list<T> the set of the elements, each once, in the order they first appear.
     *
     * @mutation-free
     */
    #[Inlined]
    public function toSet(): array {}

    /**
     * @return array<int, T> each index with its element, for `for (const [i, x] of list.entries())`.
     *
     * @mutation-free
     */
    public function entries(): array {}

    /**
     * @return int how many elements the list holds.
     *
     * @mutation-free
     */
    #[Inlined]
    public function count(): int {}

    /**
     * @param T $value
     *
     * @return bool whether an element equals `$value` by `==`.
     *
     * @mutation-free
     */
    #[Inlined]
    public function contains(mixed $value): bool {}

    /**
     * @param T $value
     *
     * @return int|null the index of the first element that equals `$value` by `==`, or null when none does.
     *
     * @mutation-free
     */
    #[Inlined]
    public function indexOf(mixed $value): ?int {}

    /**
     * @return T the last element.
     *
     * @throws \OutOfRangeException when the list is empty.
     *
     * @mutation-free
     */
    #[Inlined]
    public function last(): mixed {}

    /**
     * Puts `$value` at `$index`, after the elements before it, and renumbers the ones after it.
     *
     * @param T $value
     *
     * @throws \OutOfRangeException when `$index` is below 0 or above the number of elements.
     */
    public function insert(int $index, mixed $value): void {}

    /**
     * Removes the first element that equals `$value` by `==`, and renumbers the elements after it.
     *
     * @param T $value
     *
     * @return bool whether an element equalled `$value`.
     */
    #[Inlined]
    public function remove(mixed $value): bool {}

    /**
     * Removes every element.
     */
    #[Inlined]
    public function clear(): void {}

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
     * @template R
     *
     * @param \Closure(T): iterable<R> $transform
     *
     * @return list<R> the elements of what `$transform` gives for each element, in order.
     *
     * @mutation-free
     */
    public function flatMap(\Closure $transform): array {}

    /**
     * @return list<T> the elements, in order.
     *
     * @mutation-free
     */
    #[Inlined]
    public function toList(): array {}

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
     * @return T the largest element.
     *
     * @throws \OutOfRangeException when the list is empty.
     *
     * @where T is int
     *
     * @mutation-free
     */
    #[Inlined]
    public function max(): mixed {}

    /**
     * @return T the smallest element.
     *
     * @throws \OutOfRangeException when the list is empty.
     *
     * @where T is int
     *
     * @mutation-free
     */
    #[Inlined]
    public function min(): mixed {}

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
 * The methods a PHP# `Map<K, V>` has. The analyzer checks a call on a map against them, and the engine runs each on
 * `Sharp\Collection`, over the `Sharp\Map` object. A method without `@mutation-free` changes the map.
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
     *
     * @return bool whether the map had an entry for `$key`.
     */
    public function remove(int|string $key): bool {}

    /**
     * @param K $key
     *
     * @return V|null the value of `$key`, or null when the map has no entry for it.
     *
     * @mutation-free
     */
    public function get(int|string $key): mixed {}

    /**
     * @param K $key
     *
     * @return bool whether the map has an entry for `$key`, which may hold null.
     *
     * @mutation-free
     */
    public function has(int|string $key): bool {}

    /**
     * @return list<K> the keys, in order.
     *
     * @mutation-free
     */
    public function keys(): array {}

    /**
     * @return list<V> the values, in the order of their keys.
     *
     * @mutation-free
     */
    public function values(): array {}

    /**
     * The analyzer types the result `List<[K, V]>`, which a docblock cannot write.
     *
     * @return array<K, V> the entries, each the tuple `[key, value]`, in order.
     *
     * @mutation-free
     */
    public function toList(): array {}

    /**
     * @return int how many entries the map holds.
     *
     * @mutation-free
     */
    public function count(): int {}

    /**
     * @param \Closure(V): bool $predicate
     *
     * @return array<K, V> the entries whose values `$predicate` keeps, with their keys.
     *
     * @mutation-free
     */
    public function filter(\Closure $predicate): array {}

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
 * The methods a PHP# `Set<T>` has. The analyzer checks a call on a set against them, and the engine runs each on
 * `Sharp\Collection`, over the `Sharp\Set` object. A method without `@mutation-free` changes the set.
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

/**
 * The lazy operations of a PHP# `Iterable<T>`. The analyzer checks a call on an iterable against them, and the engine
 * runs them on `Sharp\Sequence`. `filter`, `map` and `take` read nothing until `toList()` or a loop reads the result.
 *
 * @template T
 */
final class IterableMethods
{
    /**
     * @param \Closure(T): bool $predicate
     *
     * @return iterable<T> the elements `$predicate` keeps, in order.
     *
     * @mutation-free
     */
    public function filter(\Closure $predicate): iterable {}

    /**
     * @template R
     *
     * @param \Closure(T): R $transform
     *
     * @return iterable<R> what `$transform` gives for each element, in order.
     *
     * @mutation-free
     */
    public function map(\Closure $transform): iterable {}

    /**
     * A negative `$count` throws `ValueError`.
     *
     * @return iterable<T> the first `$count` elements, reading no element after them.
     *
     * @mutation-free
     */
    public function take(int $count): iterable {}

    /**
     * @return list<T> every element, read now and numbered from 0.
     *
     * @mutation-free
     */
    public function toList(): array {}
}

/**
 * Starts a listener on every event of the class or interface named `$event`, until its handle is destroyed. The
 * standard library's `Events.on` runs it, and only the standard library reaches `Sharp.Internal`.
 *
 * @template T of object
 *
 * @param class-string<T> $event
 * @param \Closure(T): void $listener
 */
function listen(string $event, \Closure $listener): \Sharp\Listening {}

/**
 * The listeners of `$event`, one function per listener that takes the event, as `emit` gives them to the dispatcher.
 * The standard library's `Sharp\Psr\ListenerProvider` returns them to a PSR-14 dispatcher.
 *
 * @return list<\Closure(object): void>
 */
function listenersFor(object $event): array {}

/**
 * Runs `$body` in a new scope, which keeps each scoped service `$body` builds until it returns or throws, and returns
 * what `$body` returns. The standard library's `Scope.run` runs it.
 *
 * @template T
 *
 * @param \Closure(): T $body
 *
 * @return T
 */
function runScoped(\Closure $body): mixed {}
