<?php

declare(strict_types=1);

final class Foo
{
    public static function bar(): void {}
}

/**
 * @return list<string>
 */
function lowercase_pair(string $a, string $b): array
{
    return [strtolower($a), strtolower($b)];
}

function is_identifier(string $kind): bool
{
    return in_array($kind, ['simpleIdentifier', 'fieldIdentifier'], true);
}

/**
 * @return list<string>
 */
function class_constant_callable(): array
{
    return [Foo::class, 'bar'];
}

/**
 * @return list<string>
 */
function string_callable(): array
{
    return ['Foo', 'bar'];
}
