<?php

declare(strict_types=1);

namespace App;

// A value of no known type that lands in a nullable or a union type PHP declares is checked where it lands, and PHP
// throws a `TypeError` for one the type doesn't hold. One read of the value through `match` does the same, so a call
// runs once, as PHP runs it (spec section 21). The arms name a variable no other code of the file names.
final class Ledger
{
    public function note(): ?string
    {
        return $this->raw();
    }

    public function record(): void
    {
        $this->log($this->raw());
    }

    public function limited(): int
    {
        return $this->limit($this->raw());
    }

    // PHP returns an `int` where a `float` is declared as that number.
    public function ratio(): float
    {
        return $this->raw();
    }

    public function entry(?Loggable $item): ?Entry
    {
        return $item;
    }

    private function raw(): mixed
    {
        return null;
    }

    private function log(string|Loggable $detail): void
    {
    }

    private function limit(?int $value): int
    {
        return $value ?? 0;
    }
}
