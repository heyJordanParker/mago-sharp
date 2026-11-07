<?php

declare(strict_types=1);

// A function in PHP's root namespace, beside the classes of Acme\Text, one of which takes its name.
function str(string $text): string
{
    return $text;
}
