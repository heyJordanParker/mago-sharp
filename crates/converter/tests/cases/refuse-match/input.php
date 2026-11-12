<?php

declare(strict_types=1);

namespace App;

// A PHP `match` with no `default` arm throws where no arm fits, a PHP# pattern takes constants only, where `match (true)`
// takes a guard, and each arm compares by PHP's `===` (spec sections 19 and 21).
final class Picker
{
    public function strict(int $code): string
    {
        return match ($code) {
            200 => 'ok',
        };
    }

    public function computed(int $code, int $limit): string
    {
        return match ($code) {
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
}
