<?php

declare(strict_types=1);

namespace Proof\Shop;

use Proof\Spec;

/**
 * Every type is in PHPDoc.
 *
 * @template T of Spec
 */
class Shelf
{
    /** @var list<T> */
    private $items = [];

    /** @var array<int, T> */
    private $byNumber = [];

    /** @var int */
    private $count = 0;

    /**
     * @param T $item
     * @param int $number
     *
     * @return void
     */
    public function add($item, $number)
    {
        $this->items[] = $item;
        $this->byNumber[$number] = $item;
        $this->count += 1;
    }

    /**
     * @return list<T>
     */
    public function all()
    {
        return $this->items;
    }

    /**
     * @return array<int, T>
     */
    public function numbered()
    {
        return $this->byNumber;
    }

    /**
     * @return int
     */
    public function size()
    {
        return $this->count;
    }
}
