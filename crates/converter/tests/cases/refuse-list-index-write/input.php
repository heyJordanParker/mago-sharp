<?php

declare(strict_types=1);

namespace App;

// A `List` written at an index refuses: PHP adds the key past a list's end, where a PHP# `List` throws. So does
// `unset` at a `List` index: PHP leaves the keys after it as they are, where a PHP# `List` renumbers its elements.
final class Slots
{
    /** @var list<string> */
    private array $names = [];

    public function put(int $at, string $name): void
    {
        $this->names[$at] = $name;
    }

    public function drop(int $at): void
    {
        unset($this->names[$at]);
    }
}
