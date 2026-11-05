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

    /**
     * Conditions, loops, `isset`, `match`, a built-in's call and a local read after the block that assigns it.
     *
     * @return string
     */
    public function report()
    {
        $lines = 0;
        $text = '';
        foreach ($this->byNumber as $number => $item) {
            if (!isset($this->byNumber[$number + 1])) $text = $text . $item->label() . ';';
            $lines++;
        }
        if ($lines > 1) {
            $kind = 'many';
        } else {
            $kind = 'few';
        }
        $size = match (count($this->items)) {
            0 => 'empty',
            1, 2 => 'small',
            default => 'large',
        };

        return $text . ' ' . $kind . ' ' . $size . ($this->items ? ' stocked' : ' bare');
    }
}
