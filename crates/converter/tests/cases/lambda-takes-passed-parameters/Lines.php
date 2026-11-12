<?php

declare(strict_types=1);

namespace App;

final class Lines
{
    /** @var list<string> */
    private array $lines = ['a', 'b'];

    /**
     * @param \Closure(string, int): string $callback
     * @return list<string>
     */
    public function map(\Closure $callback): array
    {
        $mapped = [];
        foreach ($this->lines as $index => $line) {
            $mapped[] = $callback($line, $index);
        }

        return $mapped;
    }

    /** @param \Closure(Lines, int): void $callback */
    public function each(\Closure $callback): void
    {
        $callback($this, 0);
    }
}
