<?php

declare(strict_types=1);

namespace App;

// `isset` and `empty` read a place PHP may hold no value at without an error: a typed property with no default
// before anything sets it, a property `__isset` answers for, and a `List` index past its end. PHP# reads the place,
// so those refuse, and a property with a default or a promoted one converts.
final class Holder
{
    public int $total;

    public ?string $label = null;

    /** @var list<int> */
    public array $items = [];

    public function __construct(public ?string $code)
    {
        $this->total = 0;
    }

    public function check(Plain $plain, Magic $magic): int
    {
        $count = 0;
        if (isset($plain->name)) {
            $count++;
        }
        if (empty($this->total)) {
            $count++;
        }
        if (isset($this->label, $this->code, $plain->note)) {
            $count++;
        }
        if (empty($this->items[0])) {
            $count++;
        }
        if (isset($magic->value)) {
            $count++;
        }

        return $count;
    }
}
