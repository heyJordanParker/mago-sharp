<?php

declare(strict_types=1);

namespace App;

// A `switch` with a `case` on a value that is no constant, which no pattern takes, is a chain of PHP's loose `==`
// tests. Cases with no statements of their own join the next with `||`, and `default` runs where no case holds: last,
// it is the `else`, and before other cases, the branch where none of them holds (spec sections 19 and 21).
final class Limits
{
    public function label(int $code, int $limit): string
    {
        $label = 'none';
        switch ($code) {
            case $limit:
                return 'at the limit';
            case 0:
            case $limit + 1:
                $label = 'edge';
                if ($limit > 9) {
                    $label = 'past the limit';
                }
                break;
            default:
                $label = 'other';
        }

        return $label;
    }

    public function level(int $code, int $low, int $high): string
    {
        switch ($code) {
            default:
                return 'middle';
            case $low:
                return 'low';
            case $high:
                return 'high';
        }
    }
}
