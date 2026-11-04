<?php

declare(strict_types=1);

namespace App;

/**
 * Every type is in PHPDoc, which PHP# writes in the declarations (spec sections 11 and 12).
 *
 * @template T of Spec
 * @template-covariant TLabel
 */
class Shelf
{
    /** @var list<T> */
    private $items = [];

    /** @var array<int, T> */
    private $byNumber = [];

    /**
     * Adds an item under its number.
     *
     * @param T $item
     * @param int $number
     *
     * @return void
     */
    public function add($item, $number)
    {
        $this->items[] = $item;
        $this->byNumber[$number] = $item;
    }

    /**
     * @return list<T>
     */
    public function all()
    {
        return $this->items;
    }

    /** @return array<int, T> */
    public function numbered(): array
    {
        return $this->byNumber;
    }

    /** @return list<TLabel> */
    public function labels(): array
    {
        return [];
    }
}
