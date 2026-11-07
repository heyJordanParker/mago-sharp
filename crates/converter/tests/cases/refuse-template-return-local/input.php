<?php

declare(strict_types=1);

namespace App;

// A call whose declared return names a type parameter, as `@return T|null`, takes the type the analysis infers for
// that call, so a local declared first in the body takes no type from it (decision 2).
final class Lookup
{
    public function first(Finder $finder, bool $wide): string
    {
        $found = null;
        if ($wide) {
            $found = $finder->find(Lookup::class);
        }

        return $found === null ? 'none' : 'found';
    }
}
