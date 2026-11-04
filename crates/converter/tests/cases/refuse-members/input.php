<?php

declare(strict_types=1);

namespace App;

// A later family converts a trait in the header and property hooks. A static needs an initial value, a constant's type
// must be one PHP# writes, `__invoke` waits for the language, and a property and a method of one name have no PHP#
// form (spec sections 6 and 14).
final class Members
{
    use Greets;

    public const LIMITS = ['low' => 3];

    public static int $total;

    public string $label {
        get => 'label';
    }

    public private(set) int $count = 0;

    private string $name = '';

    public static function make(): self
    {
        return new self();
    }

    public function __invoke(): int
    {
        return 1;
    }

    public function name(): string
    {
        return $this->name;
    }
}
