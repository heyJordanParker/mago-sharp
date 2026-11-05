<?php

declare(strict_types=1);

namespace App;

// PHP# destructures a tuple and reads a `Map` variable key by key. A `List` by position, a `Map` a call gives, and
// destructuring inside another expression have no form (spec sections 12 and 30).
final class Pairs
{
    /**
     * @param list<int> $numbers
     *
     * @return array<string, mixed>
     */
    public function run(array $numbers): array
    {
        [$first, $second] = $numbers;
        ['size' => $size] = $this->sizes();
        while (['size' => $next] = $this->sizes()) {
            break;
        }

        return ['first' => $first, 'second' => $second, 'size' => $size, 'next' => $next];
    }

    /** @return array<string, int> */
    public function sizes(): array
    {
        return ['size' => 1];
    }
}
