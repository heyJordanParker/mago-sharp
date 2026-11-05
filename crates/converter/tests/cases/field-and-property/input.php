<?php

declare(strict_types=1);

namespace App;

// A private or protected property is a field, and a public one a property with accessors (spec section 6).
final class Account
{
    private int $count = 0;

    protected string $label = 'none';

    public string $name = '';

    public readonly int $id;

    private $loose;

    public int $first = 1, $second = 2;

    private float $rate = 1;

    // PHP reads `017` as octal and `0x1F` as hex, so a `float` takes the value each writes.
    private float $mask = 017;

    private float $flags = 0x1F;

    public function __construct(int $id)
    {
        $this->id = $id;
    }
}
