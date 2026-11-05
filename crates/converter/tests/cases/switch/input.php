<?php

declare(strict_types=1);

namespace App;

// A `switch` whose cases never fall through is a `match` statement with block arms. Cases with no statements of their
// own join the next with `or`, and a `switch` without `default` gains an empty one (spec section 21).
final class Router
{
    public function label(int $code): string
    {
        $label = 'unknown';
        switch ($code) {
            case 200:
            case 201:
                $label = 'ok';
                break;
            case 404:
                return 'missing';
            default:
                $label = 'other';
        }

        return $label;
    }

    public function kind(string $method): int
    {
        switch ($method) {
            case 'get':
                return 1;
            case 'post':
                return 2;
        }

        return 0;
    }

    // PHP's `null == 'get'` doesn't hold, so a `null` subject meets no case, as it matches no pattern.
    public function verb(?string $method): int
    {
        switch ($method) {
            case 'get':
                return 1;
        }

        return 0;
    }
}
