<?php

declare(strict_types=1);

// A function in PHP's root namespace, in a library with no class.
function str_shout(string $text): string
{
    return $text . '!';
}
