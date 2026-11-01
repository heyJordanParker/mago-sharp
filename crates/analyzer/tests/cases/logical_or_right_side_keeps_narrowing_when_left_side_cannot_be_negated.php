<?php

final class Door
{
    public function isOpen(): bool
    {
        return true;
    }
}

function door_is_open(mixed $a, mixed $b, int|string|Door $door): bool
{
    return ((is_string($a) || is_int($a)) && (is_string($b) || is_int($b)))
        || ((is_int($door) || is_string($door)) || $door->isOpen());
}
