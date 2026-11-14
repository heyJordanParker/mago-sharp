<?php

declare(strict_types=1);

// A function in PHP's root namespace, in a folder of the project with no class.
function shout_all(string $text): string
{
    return $text . '!';
}
