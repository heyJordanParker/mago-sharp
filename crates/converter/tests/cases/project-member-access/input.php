<?php

declare(strict_types=1);

namespace App;

use App\Routing\Route;
use Countable;

// In a project, each member PHP makes public takes the narrowest access level its uses need, as the checker's
// `too-wide-member` reads them (spec section 5): `private` for the class's own code, `protected` for a subclass's,
// `internal` for its namespace's or for no code the analysis reads, and `public` otherwise. A member that implements
// another keeps its access, as does one a marked attribute says a framework reaches by reflection.
class Cart implements Countable
{
    public const int LIMIT = 3;

    public int $items = 0;

    public string $owner = '';

    public function add(): void
    {
        if ($this->items < self::LIMIT) {
            $this->items++;
        }
    }

    public function total(): int
    {
        return $this->items * 10;
    }

    public function label(): string
    {
        return 'cart';
    }

    public function unused(): void
    {
    }

    #[Route('/cart')]
    public function show(): string
    {
        return 'cart';
    }

    public function count(): int
    {
        return $this->items;
    }
}
