<?php

declare(strict_types=1);

namespace App;

final class Task
{
    public function run(): string
    {
        return 'ran';
    }

    public function close(): void
    {
    }
}
