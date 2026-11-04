<?php

declare(strict_types=1);

// A function in PHP's root namespace, beside the classes of Acme\Format.
function shout(string $text): string
{
    return $text . '!';
}
