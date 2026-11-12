<?php

declare(strict_types=1);

namespace App;

// PHP numbers the elements of `['a', 'b']` from 0, so where a `Map` is declared they are its keys, since `[a, b]` is a
// `List` literal. Each element of a literal lands in the value type its place declares, at any depth (spec section 12).
final class Menu
{
    /** @return array<int, string> */
    public function names(): array
    {
        return ['soup', 'bread'];
    }

    /** @return array<string, array<int, string>> */
    public function courses(): array
    {
        return ['starters' => ['soup', 'bread'], 'mains' => []];
    }

    /** @return array<int, class-string> */
    public function kinds(): array
    {
        return [Menu::class, self::class];
    }

    /** @return array<string, list<string>> */
    public function grouped(bool $vegan): array
    {
        return ['starters' => $vegan ? ['soup'] : []];
    }
}
