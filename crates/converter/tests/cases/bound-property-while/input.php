<?php

declare(strict_types=1);

namespace App;

// A property never narrows, since each read may give another value (spec section 21). A `while` that tests a
// property its body reads binds it, and PHP tests it again before each pass, which binds it again. A write of the
// property after the body's reads writes the property itself.
final class Walker
{
    public function __construct(private ?Node $node = null)
    {
    }

    public function sum(): int
    {
        $sum = 0;
        while ($this->node !== null) {
            $sum += $this->node->value;
            $this->node = $this->node->next;
        }

        return $sum;
    }
}
