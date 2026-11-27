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
 * A value that holds a resource, such as an open archive, a transaction or a listener. Its one owner releases it with
 * `dispose()`: a `using` local when its block ends, newest first, or the container when the value's scope ends. An
 * implementation may have any effect, and a `using` local carries its value's.
 */
interface Disposable
{
    #[\Sharp\Internal\AcceptsAnyEffect]
    public function dispose(): void;
}

/**
 * The handle of a listener `Events.on` starts. It is `Disposable`: `dispose()` stops the listener, and the engine stops
 * it too when the handle is destroyed.
 */
final class Listening implements Disposable
{
    private function __construct() {}

    public function dispose(): void {}
}

/**
 * The bound of every type that has `==`, as in `<T : Equatable>`. A type satisfies it with no declaration, and it is
 * never a value's type, so the engine declares nothing for it.
 */
interface Equatable {}

namespace Sharp\Internal;

/**
 * Whether `$value` and `$other` are equal as PHP# `==` compares two values of `Any?`, at every depth: a number with a
 * number, a `List`, a `Map` and a `Set` by their elements, and a struct or a class through its `operator ==`. The
 * standard library's `JsonValue` compares by it.
 */
function equals(mixed $value, mixed $other): bool {}

/**
 * The stored form of `$value` as a value, what `jsonEncode` writes as text: each JSON object a `Map`, each JSON array a
 * PHP list and each scalar itself. The standard library's `Class<T>.encode` gives it, which `parseAs` reads back.
 *
 * @throws \JsonException
 */
function storedValue(mixed $value): mixed {}

/**
 * The JSON text of `$value`, as `json_encode` writes it with `$flags`. An `$indent` above 0 writes each nested value on
 * its own line, `$indent` spaces deeper than its parent, as `JSON_PRETTY_PRINT` does with 4. The standard library's
 * `Json.encode` writes by it.
 */
function jsonEncode(mixed $value, int $flags, int $indent): string {}

/**
 * The value the JSON text `$json` holds, as `json_decode` reads it with `JSON_OBJECT_AS_ARRAY`, each object a PHP
 * array, but an empty object an empty `Map`, which `jsonEncode` writes as `{}` again. The standard library's
 * `Json.parse` and `Json.decode` read by it.
 *
 * @throws \JsonException
 */
function jsonDecode(string $json): mixed {}

/**
 * The value of the type the class value `$type` holds that `$value` builds, as a struct's `parse` reads a field of that
 * type, each number and bool read from its text with `$text`. A struct's and a class's `parse`, and the standard
 * library's `Json.decode` and `Query.decode`, read by it.
 *
 * @template T
 *
 * @param class-string<T> $type
 *
 * @return T
 *
 * @throws \Sharp\InvalidFields
 */
function parseAs(string $type, mixed $value, bool $text): mixed {}

/**
 * The variant of the family `$family` that `$value` names under the family's key, read as `parseAs` reads it. With
 * `$throws` false, null wherever that throws a ValueError, as `tryParse` gives.
 *
 * @throws \Sharp\InvalidFields
 */
function parseVariant(string $family, mixed $value, bool $throws = true): ?object {}

/**
 * The class name of the variant of the family `$family` whose `[Key]` is `$tag`, or null. The standard library's
 * `Class<T>.withTag` reads by it.
 */
function withTag(string $family, string $tag): ?string {}

/**
 * The name the family of the class `$variant` stores for it, its `[Key]` or its value as a backed machine's case, or
 * null for a class that is no variant of a family. The standard library's `Class<T>.tag` reads by it.
 */
function tag(string $variant): ?string {}

/**
 * The text of the warning the last silenced built-in call of this request raised, as "preg_match(): No ending
 * matching delimiter ')' found", or null. The standard library's `Regex.parse` names a pattern's compile error by it.
 */
function silencedWarning(): ?string {}

/**
 * The stored form of the field `$path`, as `App.SmtpAccount.password`, whose value is `$value`, with each `Secret` in
 * it encrypted for that field. Without `$path`, the stored form of the `Secret` or `Hashed` `$value`, which a class's
 * `#json` writes.
 */
function seal(mixed $value, ?string $path = null): mixed {}

/**
 * The Argon2id hash, as a PHC string, of the stored text of `$value`, which `Hashed` keeps.
 */
function hashValue(mixed $value): string {}

/**
 * Whether `$hash`, a `Hashed`'s PHC string, is that of the stored text of `$value`.
 */
function verifyHash(string $hash, mixed $value): bool {}

/**
 * Each address the system resolver gives for `$host`, as text and once, in the order getaddrinfo gives them, the hosts
 * file included, and throws when the resolver cannot find the host. The standard library's `Dns.addresses` resolves
 * by it.
 *
 * @return list<non-empty-string>
 *
 * @throws \RuntimeException
 */
function resolve(string $host): array {}

/**
 * How many processors the machine has online, as sysconf's `_SC_NPROCESSORS_ONLN` counts them, at least 1. The
 * standard library's `Environment.processorCount` reads it.
 *
 * @return positive-int
 */
function processorCount(): int {}

/**
 * How `$left` orders against `$right`, -1, 0 or 1, as PHP#'s `<` orders two values of their type: an object by the
 * `operator <=>` its class declares, two tuples element by element, two strings by their bytes, and two numbers as
 * numbers. The standard library's `SortedMap`, `SortedSet` and `PriorityQueue` order by it when they take no comparison.
 *
 * @return -1|0|1
 *
 * @throws \ValueError for an object whose class declares no `operator <=>`.
 */
function compare(mixed $left, mixed $right): int {}

/**
 * The global variable `$name`, or its element at `$key`, as `$GLOBALS[$name][$key]` reads it, or null when either is
 * not set. The standard library's `PhpGlobals.get` and `PhpGlobals.read` run it.
 */
function readGlobal(string $name, ?string $key = null): mixed {}

/**
 * Sets the global variable `$name`, or its element at `$key`, to `$value`, as `$GLOBALS[$name][$key] = $value` does.
 * The standard library's `PhpGlobals.set` and `PhpGlobals.write` run it.
 *
 * @throws \ValueError for a `$key` of a global that holds no array.
 */
function writeGlobal(string $name, mixed $value, ?string $key = null): void {}

/**
 * Runs the PHP file at `$path` in the global scope, so each global variable is in its scope, and each one it sets is a
 * global after it. The standard library's `PhpGlobals.include` runs it.
 *
 * @throws \RuntimeException for a file that cannot be read.
 */
function includeGlobal(string $path): void {}

/**
 * Marks the parameter of a standard library collection member that the member compares with the collection's elements
 * by `==`, as `List.contains`'s, so the checker refuses a value whose `==` against an element is no comparison.
 */
#[\Attribute(\Attribute::TARGET_PARAMETER)]
final class ComparedByEquality {}

/**
 * Marks a standard library collection member that searches its receiver for the value of its parameter marked
 * `ComparedByEquality`, as `List.contains` does and `Set.add` does not, so the checker refuses a search for a string
 * fixed at compile time in an application.
 */
#[\Attribute(\Attribute::TARGET_METHOD)]
final class SearchesElements {}

/**
 * Marks a standard library member that tests its receiver and its first parameter for equality, as `==` does, as
 * `string.equals` does, so the checker refuses a test against a string fixed at compile time in an application.
 */
#[\Attribute(\Attribute::TARGET_METHOD)]
final class TestsEquality {}

/**
 * Marks the parameter of a standard library collection member whose function gives the keys the member orders the
 * elements by with `<=>`, as `sortedBy`'s, so the checker refuses a key type `<` cannot order.
 */
#[\Attribute(\Attribute::TARGET_PARAMETER)]
final class ComparedByOrder {}

/**
 * Marks the parameter of an engine function that makes a `Map` or a `Set`, which takes the type arguments of the
 * collection the call gives, as `entries`'s, so the bridge passes them from the type the checker gave the call.
 */
#[\Attribute(\Attribute::TARGET_PARAMETER)]
final class TakesResultType {}

/**
 * Marks the parameter of an engine function that orders by the keys a function gives, as `sortedBy`'s, which takes the
 * `operator <=>` the checker chose for the key type, so the bridge passes it from the call.
 */
#[\Attribute(\Attribute::TARGET_PARAMETER)]
final class TakesKeyOrder {}

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
 * Marks an engine function that gives the result of the PHP function `$function`, as `size` gives `count`'s for a
 * `Map` or a `Set`, so the converter turns PHP's `$function` into the standard library member whose body is one call
 * of the engine function.
 */
#[\Attribute(\Attribute::TARGET_FUNCTION)]
final class RunsAs
{
    public function __construct(public string $function) {}
}

/**
 * Marks the standard library method whose call starts a listener of its first type argument, as `Events.on` does, so
 * the checker refuses a type argument no event can be.
 */
#[\Attribute(\Attribute::TARGET_METHOD)]
final class StartsListener {}

/**
 * Marks a standard library generic method that creates the attributes of its type argument, as `attributes<T>()` of
 * `Class<T>` and `Property` does, so a call whose type argument is a PHP# class is pure: the checker holds every PHP#
 * attribute's constructor and initial values to no effects, since an attribute is data.
 */
#[\Attribute(\Attribute::TARGET_METHOD)]
final class CreatesAttributes {}

/**
 * Marks a standard library property whose value is a member's name from the code, as the `name` of `Method`,
 * `Property`, `Field`, `Parameter`, `Constant` and `EnumCase` is, so the checker holds a comparison of it with a string
 * fixed at compile time to no selection from data: no enum can declare a member's name.
 */
#[\Attribute(\Attribute::TARGET_PROPERTY | \Attribute::TARGET_PARAMETER)]
final class NamesMember {}

/**
 * Marks a standard library method that runs the initial value of a property, as `Property.defaultValue()` and
 * `Field.defaultValue()` do, so a call has the effects of every initial value the program declares that is not
 * constant, and none for a constant one.
 */
#[\Attribute(\Attribute::TARGET_METHOD)]
final class RunsInitialValues {}

/**
 * Marks the standard library interface every `emit` hands its event to, as `Dispatcher` is, so the app's binding of it
 * is its dispatcher. The checker, the compile command and the lowering of `emit` read the mark, so none copies the
 * library's name.
 */
#[\Attribute(\Attribute::TARGET_CLASS)]
final class DispatchesEmits {}

/**
 * Marks a standard library generic method that parses a value into its type argument with the type's `parse`, as
 * `Json.decode<T>` does, so the checker gives each call's type argument the struct parse check.
 */
#[\Attribute(\Attribute::TARGET_METHOD)]
final class ParsesTypeArgument {}

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
 * The engine's lazy `Iterable<T>`, which the standard library's `IterableMembers` run. `filter`, `map` and `take` read
 * nothing until `toList()` or a loop reads the result.
 *
 * @template T
 *
 * @implements \IteratorAggregate<mixed, T>
 */
final class Sequence implements \IteratorAggregate
{
    private function __construct() {}

    /**
     * @template S
     *
     * @param iterable<S> $source
     *
     * @return Sequence<S> the elements of `$source`, read only as the result is read, or `$source` itself when it is a
     *   `Sequence`.
     *
     * @pure
     */
    public static function from(iterable $source): Sequence {}

    /**
     * @param \Closure(T): bool $predicate
     *
     * @return Sequence<T> the elements `$predicate` keeps, in order.
     *
     * @mutation-free
     */
    public function filter(\Closure $predicate): Sequence {}

    /**
     * @template R
     *
     * @param \Closure(T): R $transform
     *
     * @return Sequence<R> what `$transform` gives for each element, in order.
     *
     * @mutation-free
     */
    public function map(\Closure $transform): Sequence {}

    /**
     * A negative `$count` throws `ValueError`.
     *
     * @return Sequence<T> the first `$count` elements, reading no element after them.
     *
     * @mutation-free
     */
    public function take(int $count): Sequence {}

    /**
     * @return list<T> every element, read now and numbered from 0.
     *
     * @mutation-free
     */
    public function toList(): array {}

    /**
     * @return \Iterator<mixed, T>
     */
    public function getIterator(): \Iterator {}
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
 * The properties and fields of the class the class value `$type` names, its statics included, its own first and then
 * those it inherits, in the order they are declared. The standard library's `Class<T>.properties()` makes a `Property`
 * of each.
 *
 * @return list<list<string>> the class that declares each, its name, the class value of its type as declared, the
 * access modifier that declares it, "public", "internal", "protected" or "private", and its words, each after a
 * space: what writes it, "set" or "init" and the access modifier that reaches it, as "set private", none for a
 * get-only property, then "static", "abstract", "virtual" and "override" as the declaration states them.
 */
function properties(string $type): array {}

/**
 * The instance and static methods of the class the class value `$type` names, its own first and then those it
 * inherits, in the order they are declared, without its constructor and the magic methods PHP names with `__`. The
 * standard library's `Class<T>.methods()` makes a `Method` of each, and a `Parameter` of each of its parameters.
 *
 * @return list<list<list<string>>> for each, first the class that declares it, its name, the class value of its result
 * type as declared, its words, "static", "abstract", "virtual" and "override" as the declaration states them, each
 * after a space, and the access modifier that declares it, as `properties()` gives a property's, then one list per
 * parameter: its name, the class value of its type as declared, "optional" for
 * one a call may leave out or "", and "variadic" for the one that takes the rest of a call's arguments or "".
 */
function methods(string $type): array {}

/**
 * The constructor of the class the class value `$type` names, its own or the one it inherits, as `methods()` gives a
 * method, or null for a class that has none. The standard library's `Class<T>.constructor()` makes a `Method` of it.
 *
 * @return list<list<string>>|null
 */
function constructor(string $type): ?array {}

/**
 * The method `$name` of the class `$class` that the compile made, whose name holds `#`, as a function, or null for a
 * class without it. PHP's reflection finds no such method. The standard library's `Property.defaultValue()` and
 * `Field.defaultValue()` run a member's `name#initial`, and `Method.attributes<T>()` reads a static accessor's
 * `name#get` and `name#set`.
 *
 * @return (\Closure(): mixed)|null
 *
 * @throws \ValueError
 */
function hiddenMethod(string $class, string $name): ?\Closure {}

/**
 * The class values of the type arguments of the type the class value `$type` holds without its `?`, in order, each
 * whole. The standard library's `Class<T>.typeArguments()` returns them.
 *
 * @return list<string>
 */
function typeArguments(string $type): array {}

/**
 * The kind of the type the class value `$type` holds without its `?`: "Class", "Union", "Tuple", "Function" or
 * "TypeParameter". The standard library's `Class<T>.kind()` makes a `TypeKind` of it.
 */
function typeKind(string $type): string {}

/**
 * Whether the type the class value `$type` holds takes null. The standard library's `Class<T>.isNullable()` returns it.
 */
function isNullable(string $type): bool {}

/**
 * The class value of the type the class value `$type` holds without null. The standard library's
 * `Class<T>.nonNullable()` returns it.
 */
function nonNullable(string $type): string {}

/**
 * `$bytes` read as UTF-8, each byte sequence that is not UTF-8 replaced by U+FFFD, as the engine makes a string that
 * crosses into PHP# text. The standard library's `Bytes.toText()` returns it.
 */
function text(string $bytes): string {}

/**
 * The class values the type the class value `$type` holds without its `?` is made of: a union's members, a tuple's
 * element types, a function type's result type then its parameter types, a type parameter's bound, and none for a
 * class. The standard library's `Class<T>` members of each kind read them.
 *
 * @return list<string>
 */
function typeParts(string $type): array {}

/**
 * The name of each element of the tuple the class value `$type` holds without its `?`, or null for one without.
 *
 * @return list<string|null>
 */
function tupleNames(string $type): array {}

/**
 * The class value of the generic type the class value `$type` holds without its type arguments, or null for a type
 * that declares no type parameter. The standard library's `Class<T>.genericDefinition()` returns it.
 */
function genericDefinition(string $type): ?string {}

/**
 * The class values of the type parameters the generic type the class value `$type` holds declares. The standard
 * library's `Class<T>.typeParameters()` returns them.
 *
 * @return list<string>
 */
function typeParameters(string $type): array {}

/**
 * The class value of the class the class `$type` names extends, with the type arguments it gives it, or null. The
 * standard library's `Class<T>.baseType()` returns it.
 */
function baseType(string $type): ?string {}

/**
 * The class values of the interfaces the class `$type` names implements, each with the type arguments it gives it.
 *
 * @return list<string>
 */
function interfaces(string $type): array {}

/**
 * Whether the class `$type` names is abstract, an interface, a struct and an enum, in that order.
 *
 * @return list<bool>
 */
function classFacts(string $type): array {}

/**
 * "int" or "string" for a backed enum the class `$type` names, or null.
 */
function enumBackingType(string $type): ?string {}

/**
 * The class value of the generic type the class value `$type` holds with `$arguments`, class values, as its type
 * arguments. The standard library's `Class<T>.makeGenericType()` returns it.
 *
 * @param list<string> $arguments
 *
 * @throws \InvalidArgumentException
 */
function makeGenericType(string $type, array $arguments): string {}

/**
 * Whether a value of the type the class value `$type` holds fits the type the class value `$other` holds, as `is`
 * answers it for such a value. The standard library's `Class<T>.isAssignableTo()` returns it.
 */
function isAssignable(string $type, string $other): bool {}

/**
 * An object of the class the class value `$case` names, the standard library's `EnumCase`, for each case of the enum the
 * class value `$type` names, in the order they are declared, none for a class value that names no enum, which the
 * standard library's `Class<T>.enumCases()` gives.
 *
 * @template T
 *
 * @param class-string<T> $case
 *
 * @return list<T>
 */
function enumCases(string $type, string $case): array {}

/**
 * A new object of each attribute of the class the class value `$attribute` names, or of a class that extends or
 * implements it, on the declaration `$kind` of the class value `$type`: "class", the method `$name` for "method", its
 * parameter at `$position` for "parameter", the property or field `$name` for "property", and the constant or enum
 * case `$name` for "constant". Each member's `attributes<T>()` of the standard library gives them.
 *
 * @template T
 *
 * @param class-string<T> $attribute
 *
 * @return list<T>
 *
 * @throws \Error
 */
function attributes(string $type, string $kind, string $name, int $position, string $attribute): array {}

/**
 * An object of the class the class value `$data` names, the standard library's `AttributeData`, for each attribute
 * `attributes()` reads with the same arguments, in the order they are declared: its class, and the call of its
 * constructor, one value for each parameter in parameter order, a named argument at its parameter's place and a
 * left-out parameter's default filled in. It builds no attribute. Each member's `attributeData<T>()` of the standard
 * library gives them.
 *
 * @template T
 *
 * @param class-string<T> $data
 *
 * @return list<T>
 *
 * @throws \Error
 */
function attributeData(string $type, string $kind, string $name, int $position, string $attribute, string $data): array {}

/**
 * The constants of the class the class value `$type` names, its own first and then those it inherits, in the order they
 * are declared, without an enum's cases. The standard library's `Class<T>.constants()` makes a `Constant` of each.
 *
 * @return list<list<string>> the class that declares each, its name, and the class value of its type as declared.
 */
function constants(string $type): array {}

/**
 * The value of the constant `$name` that `$class` declares, as a direct read from the caller's code gives it, a `Map` or
 * `Set` constant's as the object a direct read makes. The standard library's `Constant.value()` returns it.
 */
function constant(string $class, string $name): mixed {}

/**
 * A new object of the class the class value `$type` names, with its type arguments, created by its constructor with
 * `$arguments`, as a direct `new` from the caller's code would. The standard library's `Class<T>.createInstance()`
 * returns it.
 *
 * @template T of object
 *
 * @param class-string<T> $type
 * @param list<mixed> $arguments
 *
 * @return T
 *
 * @throws \InvalidArgumentException
 */
function createInstance(string $type, array $arguments): object {}

/**
 * The accessors of the property `$name` of the class the class value `$type` names, as `ReflectionProperty::getHook`
 * gives a hook: the row of its `get` and the row of its `set`, each as `methods()` gives a method's, named `$name::get`
 * and `$name::set`, or null for one it runs none of. The standard library's `Property.getter()` and `setter()` read
 * them.
 *
 * @return array{0: list<list<string>>|null, 1: list<list<string>>|null}
 */
function accessors(string $type, string $name): array {}

/**
 * A value of the function type the class value `$type` names that calls the method `$method` that `$class` declares, or
 * the accessor `$name::get` or `$name::set`, as a direct call from the caller's code would: an instance one on the
 * receiver of the class `$readOn` names it takes first, the one its `Method` was read from, and a static one on that
 * class. The standard library's `Method.createDelegate` returns it.
 *
 * @template T
 *
 * @param class-string<T> $type
 *
 * @return T
 *
 * @throws \ValueError
 */
function createDelegate(string $readOn, string $class, string $method, string $type): \Closure {}

/**
 * A value of the function type the class value `$type` names that calls the method `$method` that `$class` declares, or
 * the accessor `$name::get` or `$name::set`, with a list of arguments, as a direct call from the caller's code would: an
 * instance one on the receiver of the class `$readOn` names it takes first, the one its `Method` was read from, and a
 * static one on that class. The standard library's `Method.createInvoker` returns it.
 *
 * @template T
 *
 * @param class-string<T> $type
 *
 * @return T
 *
 * @throws \ValueError
 */
function createInvoker(string $readOn, string $class, string $method, string $type): \Closure {}

/**
 * The value of the property `$property` that `$class` declares, read on `$receiver`, or of the static `$property`, as a
 * direct read from the caller's code would be. The standard library's `Property.get` returns it.
 *
 * @throws \InvalidArgumentException
 */
function read(?object $receiver, string $class, string $property): mixed {}

/**
 * Writes `$value` into the property `$property` that `$class` declares on `$receiver`, or into the static `$property`,
 * as a direct write from the caller's code would. The standard library's `Property.set` runs it.
 *
 * @throws \InvalidArgumentException
 */
function write(?object $receiver, string $class, string $property, mixed $value): void {}

/**
 * The value the storage of the field or auto-property `$field` that `$class` declares holds on `$receiver`, or the
 * static `$field`'s, reached as a direct read from the caller's code would reach it, with no accessor, no initializer
 * of a lazy object and no initial value of statics run. The standard library's `Field.get` returns it.
 *
 * @throws \InvalidArgumentException
 */
function readField(?object $receiver, string $class, string $field): mixed {}

/**
 * Writes `$value` into the storage of the field or auto-property `$field` that `$class` declares on `$receiver`, or
 * into the static `$field`, as a direct write from the caller's code would reach it, with no accessor, no initializer of
 * a lazy object and no initial value of statics run, so a lazy object that has not loaded takes it without loading. The
 * standard library's `Field.set` runs it.
 *
 * @throws \InvalidArgumentException
 */
function writeField(?object $receiver, string $class, string $field, mixed $value): void {}

/**
 * The default of the parameter at `$position` of the method `$method` that `$class` declares, or of the accessor
 * `$name::get` or `$name::set`, as `ReflectionParameter::getDefaultValue` evaluates it. A default that runs code, a
 * `new`, a call or a closure, throws `ValueError` and runs none of it, and a parameter with no default throws
 * `OutOfRangeException`. The standard library's `Parameter.defaultValue()` returns it.
 *
 * @throws \ValueError
 * @throws \OutOfRangeException
 */
function defaultValue(string $class, string $method, int $position): mixed {}

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

/**
 * Opens a hold, inside any hold already open, in which each `emit` waits instead of reaching the app's dispatcher.
 * The standard library's `Database.transaction` opens one for each transaction it begins.
 */
function holdEmits(): void {}

/**
 * Closes the newest hold once its transaction commits: each emit it held joins the hold around it, or, when none is
 * open, reaches the app's dispatcher in emit order.
 */
function releaseEmits(): void {}

/**
 * Closes the newest hold once its transaction rolls back, so no dispatcher hears the emits it held.
 */
function dropEmits(): void {}

/*
 * The engine functions the standard library's `List`, `Map` and `Set` members run, each with the collection first. A
 * function that changes the collection takes it by reference, from the place a `ref` receiver passes, and only the
 * standard library reaches `Sharp.Internal`. A function that takes `$collection` takes any of the three, a `Map`'s
 * values and a `Set`'s elements as its elements, and one that takes `$table` takes a `Map` or a `Set`, whose elements
 * are its table's keys.
 */

/**
 * Appends `$value` after the last element.
 *
 * @template T
 *
 * @param list<T> $list
 * @param T $value
 */
function add(array &$list, mixed $value): void {}

/**
 * Puts `$value` at `$index`, after the elements before it, and renumbers the ones after it.
 *
 * @template T
 *
 * @param list<T> $list
 * @param T $value
 *
 * @throws \OutOfRangeException when `$index` is below 0 or above the number of elements.
 */
function insert(array &$list, int $index, mixed $value): void {}

/**
 * Replaces the element at `$index` with `$value`, as `list[i] = v` does.
 *
 * @template T
 *
 * @param list<T> $list
 * @param T $value
 *
 * @return T the element it replaced, as Kotlin's `MutableList.set` gives it.
 *
 * @throws \OutOfRangeException when `$index` is not an index of the list.
 */
function set(array &$list, int $index, mixed $value): mixed {}

/**
 * Removes the first element that equals `$value` by `==`, and renumbers the elements after it.
 *
 * @template T
 *
 * @param list<T> $list
 * @param T $value
 *
 * @return bool whether an element equalled `$value`.
 */
function remove(array &$list, mixed $value): bool {}

/**
 * Removes the element at `$index`, and renumbers the elements after it.
 *
 * @template T
 *
 * @param list<T> $list
 *
 * @return T the removed element.
 *
 * @throws \OutOfRangeException when the list is empty, or `$index` is not an index of it.
 */
function removeAt(array &$list, int $index): mixed {}

/**
 * Removes the last element.
 *
 * @template T
 *
 * @param list<T> $list
 *
 * @return T the removed element.
 *
 * @throws \OutOfRangeException when the list is empty.
 */
function removeLast(array &$list): mixed {}

/**
 * Removes every element.
 *
 * @template C of array
 *
 * @param C $collection
 * @param-out C $collection
 */
function clear(array|\Sharp\Map|\Sharp\Set &$collection): void {}

/**
 * @template K
 * @template T
 *
 * @param iterable<K, T> $collection
 * @param K $key
 *
 * @return T|null the element at `$key`, a `List`'s index or a `Map`'s key, or null when the collection has none.
 *
 * @pure
 */
function get(array|\Sharp\Map $collection, mixed $key): mixed {}

/**
 * @template T
 *
 * @param list<T> $list
 * @param T $value
 *
 * @return bool whether an element equals `$value` by `==`.
 */
function contains(array $list, mixed $value): bool {}

/**
 * @template T
 *
 * @param list<T> $list
 * @param T $value
 *
 * @return int|null the index of the first element that equals `$value` by `==`, or null when none does.
 */
function indexOf(array $list, mixed $value): ?int {}

/**
 * @template T
 *
 * @param list<T> $list
 *
 * @return non-empty-list<T> the list.
 *
 * @throws \OutOfRangeException when the list is empty.
 *
 * @pure
 */
function nonEmpty(array $list): array {}

/**
 * @template T
 *
 * @param list<T> $list
 *
 * @return array<int, T> each index with its element.
 *
 * @pure
 */
function entries(array $list, #[TakesResultType] ?string $type = null): array {}

/**
 * The analyzer types the result `Set<T>`, which a docblock cannot write.
 *
 * @template T
 *
 * @param list<T> $list
 *
 * @return list<T> the set of the elements, each once, in the order they first appear.
 *
 * @pure
 */
function toSet(array $list, #[TakesResultType] ?string $type = null): array {}

/**
 * @template T
 *
 * @param list<T> $list
 * @param \Closure(T): bool $predicate
 *
 * @return list<T> the elements `$predicate` keeps, in order.
 */
function filter(array $list, \Closure $predicate): array {}

/**
 * @template K
 * @template T
 * @template R
 *
 * @param iterable<K, T> $collection
 * @param \Closure(T): R $transform
 *
 * @return list<R> what `$transform` gives for each element, in order.
 */
function map(array|\Sharp\Map|\Sharp\Set $collection, \Closure $transform): array {}

/**
 * @template T
 * @template R
 *
 * @param list<T> $list
 * @param \Closure(T): iterable<R> $transform
 *
 * @return list<R> the elements of what `$transform` gives for each element, in order.
 */
function flatMap(array $list, \Closure $transform): array {}

/**
 * @template K
 * @template T
 * @template R of int|float
 *
 * @param iterable<K, T> $collection
 * @param \Closure(T): R $selector
 *
 * @return R the sum of what `$selector` gives for each element, 0 for an empty collection.
 */
function sumOf(array|\Sharp\Map|\Sharp\Set $collection, \Closure $selector): int|float {}

/**
 * @template K
 * @template T
 *
 * @param iterable<K, T> $collection
 * @param \Closure(T): bool $predicate
 *
 * @return T the first element `$predicate` keeps.
 *
 * @throws \OutOfRangeException when `$predicate` keeps no element.
 */
function first(array|\Sharp\Map|\Sharp\Set $collection, \Closure $predicate): mixed {}

/**
 * @template K
 * @template T
 *
 * @param iterable<K, T> $collection
 * @param \Closure(T): bool $predicate
 *
 * @return bool whether `$predicate` keeps any element.
 */
function any(array|\Sharp\Map|\Sharp\Set $collection, \Closure $predicate): bool {}

/**
 * @template K
 * @template T
 * @template G of array-key
 *
 * @param iterable<K, T> $collection
 * @param \Closure(T): G $key
 *
 * @return array<G, list<T>> the elements under the key `$key` gives each, in order.
 */
function groupBy(array|\Sharp\Map|\Sharp\Set $collection, \Closure $key, #[TakesResultType] ?string $type = null): array {}

/**
 * @template K
 * @template T
 * @template G of array-key
 *
 * @param iterable<K, T> $collection
 * @param \Closure(T): G $key
 *
 * @return array<G, T> each element under the key `$key` gives it, the last one where two share a key.
 */
function associateBy(array|\Sharp\Map|\Sharp\Set $collection, \Closure $key, #[TakesResultType] ?string $type = null): array {}

/**
 * @template K
 * @template T
 *
 * @param iterable<K, T> $collection
 * @param \Closure(T): bool $predicate
 *
 * @return T|null the first element `$predicate` keeps, or null when it keeps none.
 */
function firstOrNull(array|\Sharp\Map|\Sharp\Set $collection, \Closure $predicate): mixed {}

/*
 * A function that orders by the keys `$selector` gives takes `$order` last, the bridge's: the `operator <=>` the checker
 * chose for the key type, a list of each element's for a tuple key, which orders element by element, or null for keys
 * PHP# orders itself, as numbers and strings. A `null` key sorts before every other key.
 */

/**
 * @template K
 * @template T
 * @template R
 *
 * @param iterable<K, T> $collection
 * @param \Closure(T): R $selector
 *
 * @return list<T> the elements in ascending order of what `$selector` gives, equal ones in their order.
 */
function sortedBy(array|\Sharp\Map|\Sharp\Set $collection, \Closure $selector, #[TakesKeyOrder] \Closure|array|null $order = null): array {}

/**
 * @template K
 * @template T
 * @template R
 *
 * @param iterable<K, T> $collection
 * @param \Closure(T): R $selector
 *
 * @return list<T> the elements in descending order of what `$selector` gives, equal ones in their order.
 */
function sortedByDescending(array|\Sharp\Map|\Sharp\Set $collection, \Closure $selector, #[TakesKeyOrder] \Closure|array|null $order = null): array {}

/**
 * @template T
 *
 * @param list<T> $list
 * @param \Closure(T, T): int $comparison
 *
 * @return list<T> the elements in the order `$comparison` gives two of them, below 0 for the first before the second,
 *   equal ones in their order.
 */
function sortedWith(array $list, \Closure $comparison): array {}

/**
 * @template K
 * @template T
 * @template R
 *
 * @param iterable<K, T> $collection
 * @param \Closure(T): R $selector
 *
 * @return T the first element of the smallest key `$selector` gives.
 *
 * @throws \OutOfRangeException when the collection is empty.
 */
function minBy(array|\Sharp\Map|\Sharp\Set $collection, \Closure $selector, #[TakesKeyOrder] \Closure|array|null $order = null): mixed {}

/**
 * @template K
 * @template T
 * @template R
 *
 * @param iterable<K, T> $collection
 * @param \Closure(T): R $selector
 *
 * @return T the first element of the largest key `$selector` gives.
 *
 * @throws \OutOfRangeException when the collection is empty.
 */
function maxBy(array|\Sharp\Map|\Sharp\Set $collection, \Closure $selector, #[TakesKeyOrder] \Closure|array|null $order = null): mixed {}

/**
 * @return int how many entries the table holds, read in constant time.
 */
#[RunsAs('count')]
function size(\Sharp\Map|\Sharp\Set $table): int {}

/**
 * @template K
 * @template V
 *
 * @param iterable<K, V> $table
 * @param K $key
 *
 * @return bool whether the table has an entry for `$key`, which may hold null.
 */
function containsKey(\Sharp\Map|\Sharp\Set $table, mixed $key): bool {}

/**
 * Removes the entry of `$key`, if the table has one.
 *
 * @template C of iterable
 *
 * @param C $table
 * @param-out C $table
 *
 * @return bool whether the table had an entry for `$key`.
 */
function removeKey(\Sharp\Map|\Sharp\Set &$table, mixed $key): bool {}

/**
 * Adds `$key`, if the table does not hold it.
 *
 * @template C of iterable
 *
 * @param C $table
 * @param-out C $table
 *
 * @return bool whether the table did not hold `$key`.
 */
function addKey(\Sharp\Set &$table, mixed $key): bool {}

/**
 * Inserts `$key` with `$value`, or replaces the value of an equal key, as `map[key] = value` does.
 *
 * @template C of iterable
 *
 * @param C $map
 * @param-out C $map
 */
function put(\Sharp\Map &$map, mixed $key, mixed $value): void {}

/**
 * The value of `$key`, or, when the map holds no equal key, what `$defaultValue` gives, inserted with `$key` first, as
 * Kotlin's `getOrPut` does.
 *
 * @template C of iterable
 * @template V
 *
 * @param C $map
 * @param \Closure(): V $defaultValue
 * @param-out C $map
 *
 * @return V
 */
function getOrPut(\Sharp\Map &$map, mixed $key, \Closure $defaultValue): mixed {}

/**
 * @template K
 * @template V
 *
 * @param iterable<K, V> $table
 *
 * @return list<K> the keys, in order, which are a `Set`'s elements.
 */
function keys(\Sharp\Map|\Sharp\Set $table): array {}

/**
 * @template K
 * @template V
 *
 * @param iterable<K, V> $map
 *
 * @return list<V> the values, in the order of their keys.
 */
function values(\Sharp\Map $map): array {}

/**
 * @template K
 * @template V
 *
 * @param iterable<K, V> $map
 *
 * @return list<list{K, V}> the entries, each the tuple `[key, value]`, in order.
 */
function pairs(\Sharp\Map $map): array {}

/**
 * @template K
 * @template V
 *
 * @param iterable<K, V> $map
 * @param \Closure(V): bool $predicate
 *
 * @return array<K, V> the entries whose values `$predicate` keeps, with their keys.
 */
function filterEntries(\Sharp\Map $map, \Closure $predicate): \Sharp\Map {}

/**
 * The analyzer types the result `Set<T>`, which a docblock cannot write.
 *
 * @template K
 * @template T
 *
 * @param iterable<K, T> $set
 * @param \Closure(T): bool $predicate
 *
 * @return list<T> the set of the elements `$predicate` keeps, in order.
 */
function filterElements(\Sharp\Set $set, \Closure $predicate): \Sharp\Set {}
