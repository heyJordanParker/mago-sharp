<?php

declare(strict_types=1);

namespace App;

// A PHPDoc type that narrows the type PHP# writes, as `non-empty-string` narrows `string`, holds the callers' checks
// in PHP, and the analysis of a `.sharp` file reads no PHPDoc, so converting would drop it. A tag PHP# writes whole
// converts.
final class Label
{
    /** @var non-empty-string */
    public string $label = 'label';

    /** @return non-empty-string */
    public function text(): string
    {
        return $this->label;
    }

    /** @param positive-int $count */
    public function repeat(int $count): int
    {
        return $count;
    }

    /** @return list<string> */
    public function words(): array
    {
        return [];
    }
}
