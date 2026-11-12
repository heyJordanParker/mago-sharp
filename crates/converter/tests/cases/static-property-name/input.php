<?php

declare(strict_types=1);

namespace App;

// A static property keeps the name its class declares, which code outside the method reaches it by, beside a local
// the method renames to camel case.
final class Catalog
{
    public function pages(int $count): int
    {
        $page_size = Settings::$page_size;

        return $count * $page_size;
    }
}
