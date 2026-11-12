<?php

declare(strict_types=1);

namespace App;

// Plain PHP's `new Page(...)` gives a page its bound, `Page<Item>`, so the page of orders `Pages::of` returns is no
// `Page<Order>` once it enters PHP#, which throws where PHP went on (spec section 11). So is a page plain PHP passes or
// sets. A page of its bound converts.
final class Orders
{
    /** @var Page<Order>|null */
    public ?Page $last = null;

    /** @var Page<Item>|null */
    public ?Page $any = null;

    public function __construct(private Pages $pages)
    {
    }

    /** @return Page<Order> */
    public function page(Order $order): Page
    {
        return $this->pages->of([$order]);
    }

    /** @return Page<Item> */
    public function item(Item $item): Page
    {
        return $this->pages->of([$item]);
    }

    /** @param Page<Order> $page */
    public function count(Page $page): int
    {
        return \count($page->items);
    }

    /** @param Page<Item> $page */
    public function size(Page $page): int
    {
        return \count($page->items);
    }
}
