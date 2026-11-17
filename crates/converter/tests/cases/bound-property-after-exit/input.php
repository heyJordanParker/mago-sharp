<?php

declare(strict_types=1);

namespace App;

// A property never narrows, since each read may give another value (spec section 21). An `if` that always leaves
// where a property holds no value binds it with `is not`, and the statements after it read the name.
final class Job
{
    public function __construct(private ?Logger $logger = null, private ?Job $next = null)
    {
    }

    public function log(string $message): string
    {
        if ($this->logger === null) {
            return '';
        }

        return $this->logger->info($message);
    }

    public function after(): Job
    {
        if (!$this->next instanceof Job) {
            throw new \LogicException('no job after this one');
        }

        return $this->next;
    }
}
