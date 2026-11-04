<?php

declare(strict_types=1);

namespace App;

// A later slice converts class constants, statics, a trait in the header, property hooks and a `set` of its own
// visibility. `__invoke` waits for the language, and a property and a method of one name have no PHP# form (spec
// section 14).
final class Members
{
    use Greets;

    public const int LIMIT = 3;

    public static int $total = 0;

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
