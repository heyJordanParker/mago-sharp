<?php

declare(strict_types=1);

namespace App;

// A parameter's attributes keep their arguments, a list holding a `new` included, before a type in parentheses
// (spec section 1).
final class Entry
{
    public function __construct(
        #[Tag(list: 'key')]
        public string $key,
        #[Tag(list: 'value')]
        #[Tag(anyOf: [Tag::Text, new Tag(list: Tag::Text)])]
        public string|int|null $value = null,
    ) {
    }
}
