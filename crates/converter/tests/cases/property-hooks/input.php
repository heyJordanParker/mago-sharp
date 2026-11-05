<?php

declare(strict_types=1);

namespace App;

// A property's hooks are its accessors: `$this->name` in them is the storage `field`, `$value` is `value`, and a
// short `set` stores its value. A property with storage gains the accessor it declares no hook for, and its initial
// value follows the accessors (spec section 6).
final class Badge
{
    public string $label = 'guest' {
        get => $this->label . '!';
        set => $value . '?';
    }

    public private(set) int $level = 1 {
        set(int $value) {
            $this->level = $value < 1 ? 1 : $value;
        }
    }

    public string $title {
        get {
            return $this->label . ' ' . $this->level;
        }
    }

    public function promote(): void
    {
        $this->level = $this->level + 1;
    }
}
