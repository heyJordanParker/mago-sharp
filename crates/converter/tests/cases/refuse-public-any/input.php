<?php

declare(strict_types=1);

namespace App\Shop;

// A public member of an application names its types, so its signature holds no `Any`, `Any?` or `Object`, as a PHP
// `mixed`, an `array` no PHPDoc types or an `object` would be. A narrower PHPDoc type, and a private member, convert.
final class Store
{
    public function __construct(public object $handle, private mixed $state = null)
    {
    }

    public function checkoutPayload(): mixed
    {
        return $this->state;
    }

    public function take(array $items): void
    {
    }

    /** @param array<string, int> $counts */
    public function counted(array $counts): int
    {
        return 0;
    }

    private function kept(mixed $value): mixed
    {
        return $value;
    }
}
