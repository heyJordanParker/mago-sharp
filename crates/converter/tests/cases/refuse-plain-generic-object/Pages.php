<?php

declare(strict_types=1);

namespace App;

final class Pages
{
    /**
     * @template T of Item
     * @param list<T> $items
     * @return Page<T>
     */
    public function of(array $items): Page
    {
        return new Page(...$items);
    }
}
