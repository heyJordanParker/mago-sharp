<?php

declare(strict_types=1);

namespace App;

class Job
{
    public int $timeout = 60;

    protected string $queue = 'default';

    public function limit(): int
    {
        return $this->timeout;
    }
}
