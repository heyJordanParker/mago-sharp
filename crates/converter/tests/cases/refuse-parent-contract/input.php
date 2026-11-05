<?php

declare(strict_types=1);

namespace App;

// PHP lets a class extend a deprecated one, an override take less than the method it replaces, and a call reach a
// method its object's type doesn't declare. PHP# warns on the first and refuses the others (spec sections 22 and 27).
final class Store extends Base
{
    public function path(string $path = ''): string
    {
        return '/store/' . $path;
    }

    public function find(Source $source): string
    {
        return $source->lookup();
    }
}
