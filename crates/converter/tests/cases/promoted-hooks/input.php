<?php

declare(strict_types=1);

namespace App;

// A promoted parameter's hooks are its property's accessors, as a property's in the class body are, and its default
// follows them. PHP runs the `set` hook on the argument, as PHP# runs `set` on it (spec sections 6 and 9).
final class Badge
{
    public function __construct(
        public string $label = 'guest' {
            set => $value . '!';
        },
        public int $level = 1 {
            get => $this->level * 10;
        },
    ) {
    }
}
