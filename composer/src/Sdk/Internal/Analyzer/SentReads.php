<?php

declare(strict_types=1);

namespace Mago\Sdk\Internal\Analyzer;

use Mago\Sdk\Internal\Protocol\ValueWriter;

/**
 * The lists of codebase reads this interpreter sent Mago in the analysis generation it serves now, each by the number
 * it sent it under. A provider or a file's hooks often read the same symbols on every call, so a list goes whole once a
 * generation and by its number after, and Mago keeps each list once.
 *
 * A number is never sent for two lists: the numbering goes on across generations, so a list a worker sends again after
 * serving another generation gets a new one.
 *
 * @internal
 */
final class SentReads
{
    private int $generation = -1;

    /** @var array<string, int> each list's number, by its key */
    private array $numbers = [];

    private int $next = 0;

    /**
     * Writes `$reads` for `$generation`: its number, whether the list follows, and the list when Mago has not had it,
     * a count then each read's codebase query operation, argument, and name.
     */
    public function write(ValueWriter $writer, int $generation, CodebaseReads $reads): void
    {
        if ($generation !== $this->generation) {
            $this->generation = $generation;
            $this->numbers = [];
        }

        $key = $reads->key();
        $number = $this->numbers[$key] ?? null;
        if ($number !== null) {
            $writer->writeU32($number);
            $writer->writeBoolean(false);

            return;
        }

        $number = $this->next++;
        $this->numbers[$key] = $number;
        $writer->writeU32($number);
        $writer->writeBoolean(true);
        $list = $reads->reads();
        $writer->writeCount($list);
        foreach ($list as [$operation, $argument, $name]) {
            $writer->writeU8($operation);
            $writer->writeU8($argument);
            $writer->writeBytes($name);
        }
    }
}
