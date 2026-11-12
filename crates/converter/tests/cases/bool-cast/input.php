<?php

declare(strict_types=1);

namespace App;

// PHP# has no `(bool)`, and the comparison a condition of the value's type gives replaces it (spec sections 21 and 24).
final class Basket
{
    /**
     * @param list<string> $items
     * @return list<bool>
     */
    public function flags(int $count, ?string $note, array $items): array
    {
        return [(bool) $count, (bool) $note, (bool) $items];
    }
}
