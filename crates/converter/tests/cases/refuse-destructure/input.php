<?php

declare(strict_types=1);

namespace App;

// A plain PHP array is no tuple, so destructuring by position has no PHP# form. Destructuring by key reads keys the
// array may lack, where PHP warns and an error handler may throw, and a PHP# `Map` read gives `null`. Destructuring
// inside another expression has no form either (spec sections 12 and 30).
final class Pairs
{
    /**
     * @param list<int> $numbers
     * @param array<string, int> $row
     *
     * @return array<string, mixed>
     */
    public function run(array $numbers, array $row): array
    {
        [$first, $second] = $numbers;
        ['price' => $price, 'count' => $count] = $row;
        ['size' => $size] = $this->sizes();
        while (['size' => $next] = $this->sizes()) {
            break;
        }

        return ['first' => $first, 'second' => $second, 'price' => $price, 'count' => $count, 'size' => $size, 'next' => $next];
    }

    /** @return array<string, int> */
    public function sizes(): array
    {
        return ['size' => 1];
    }
}
