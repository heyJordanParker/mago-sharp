<?php

declare(strict_types=1);

namespace App;

// An operator converts only where PHP# gives it PHP's meaning for its operands' types (spec sections 18, 19 and 24).
final class Operators
{
    /** @param list<int> $list */
    public function run(string $text, int $count, float $rate, bool $flag, array $list): void
    {
        $loose = $text == 'a';
        $low = $flag and $flag;
        $added = $text + $count;
        $divided = $count / 2;
        $modulo = $rate % 2;
        $masked = $flag & $flag;
        $power = $count ** $count;
        $before = $text < 'b';
        $shown = 'flag ' . $flag;
        $text .= $count;
        $text++;
        $negative = -$text;
        $inverted = ~$rate;
        $same = $count === $rate;
        $lists = $list === $list;
        $mixed = $text === $count;
        $rate /= 2;
    }
}
