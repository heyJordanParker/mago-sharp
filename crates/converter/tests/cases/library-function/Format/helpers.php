<?php

declare(strict_types=1);

// A function in PHP's root namespace, which Externs.sharp gives the namespace Acme\Format.
function shout(string $text): string
{
    return $text . '!';
}
