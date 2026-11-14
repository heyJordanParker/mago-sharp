<?php

declare(strict_types=1);

namespace App;

// A PHP `match` with no `default` arm throws a message PHP's settings shape for a value other than an `int`, an arm on
// a value that is no constant tests the value again, which may run code, and each arm compares by PHP's `===` (spec
// sections 19 and 21).
final class Picker
{
    public function strict(string $code): string
    {
        return match ($code) {
            'ok' => 'fine',
        };
    }

    public function computed(int $limit): string
    {
        return match ($this->code()) {
            $limit => 'at the limit',
            default => 'under',
        };
    }

    public function mixedTypes(int|float $code): string
    {
        return match ($code) {
            200 => 'ok',
            default => 'other',
        };
    }

    private function code(): int
    {
        return 200;
    }
}
