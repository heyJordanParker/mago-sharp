<?php

declare(strict_types=1);

namespace App;

// `?:` of a call reads the call's value once, as PHP does, so the call runs once: through `match`, whose `when` is
// PHP's truthiness of the value, and through `??` for an object, which is truthy where it is not null (spec section 21).
final class Header
{
    public function rate(): float
    {
        return $this->find() ?: 1.5;
    }

    public function counted(): int
    {
        return $this->count() ?: 1;
    }

    /** @return list<string> */
    public function names(): array
    {
        return $this->all() ?: ['none'];
    }

    public function owner(): Header
    {
        return $this->parent() ?: $this;
    }

    private function find(): ?float
    {
        return null;
    }

    private function count(): ?int
    {
        return null;
    }

    /** @return list<string> */
    private function all(): array
    {
        return [];
    }

    private function parent(): ?Header
    {
        return null;
    }
}
