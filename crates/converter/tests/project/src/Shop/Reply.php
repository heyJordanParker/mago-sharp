<?php

declare(strict_types=1);

namespace Proof\Shop;

// `===` of a string with a string fixed at compile time selects one of a closed set of values, which an application
// declares as a backed enum, so the file stays plain PHP and the report names the comparison.
final class Reply
{
    public function agreed(string $answer): bool
    {
        return $answer === 'yes';
    }
}
