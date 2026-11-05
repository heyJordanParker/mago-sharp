<?php

declare(strict_types=1);

namespace App;

// A static needs an initial value, a constant's type must be one PHP# writes, a `set` accessor's `value` has the
// property's type, a property needs a `get`, `__invoke` waits for the language, and a property and a method of one
// name have no PHP# form (spec sections 6 and 14).
final class Members
{
    use Greets;

    public const LIMITS = ['low' => 3];

    public static int $total;

    public string $label = '' {
        set(string|int $text) => (string) $text;
    }

    public string $sink {
        set {
            $this->name = $value;
        }
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
