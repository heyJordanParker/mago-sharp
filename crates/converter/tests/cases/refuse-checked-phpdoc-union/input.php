<?php

declare(strict_types=1);

namespace App;

// A value of no known type that lands in a nullable or a union type only PHPDoc gives, which PHP never checks, so it
// lets through any value a check would throw for (spec section 21).
final class Router
{
    public function go(): void
    {
        $this->redirect($this->raw());
    }

    private function raw(): mixed
    {
        return null;
    }

    /** @param string|null $to */
    private function redirect($to = null): void
    {
    }
}
