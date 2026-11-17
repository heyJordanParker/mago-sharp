<?php

declare(strict_types=1);

namespace App;

// A property never narrows, since each read may give another value (spec section 21). A condition that tests a
// property its body reads again binds it, by the property's own name, or `label2` where the method has a `$label`,
// and the body reads the name, which holds the value the test read.
final class Shelf
{
    private ?string $label = null;

    public function __construct(private ?Shelf $next = null)
    {
    }

    public function title(int $label): string
    {
        if ($this->label !== null && $this->label !== '') {
            return "{$this->label} {$label}";
        }

        return 'shelf';
    }

    public function last(): Shelf
    {
        if ($this->next instanceof Shelf) {
            return $this->next->last();
        }

        return $this;
    }
}
