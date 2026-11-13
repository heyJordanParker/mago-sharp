<?php

declare(strict_types=1);

// A function in PHP's root namespace, which Externs.sharp gives the namespace Acme\Text, one of whose classes takes its
// name.
function str(string $text): string
{
    return $text;
}
