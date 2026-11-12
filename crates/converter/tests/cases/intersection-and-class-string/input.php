<?php

declare(strict_types=1);

namespace App;

// An intersection of classes is PHP#'s `A & B`, and a `class-string` the class value of any class, `Class<Object>`,
// which a string that names no class is not, as PHPDoc says (spec section 21).
final class Sharing
{
    public function link(Item&Shareable $item): string
    {
        return $item->link();
    }

    public function maybe((Item&Shareable)|null $item): ?string
    {
        return $item?->link();
    }

    /**
     * @param class-string $type
     * @return class-string
     */
    public function same(string $type): string
    {
        return $type;
    }
}
