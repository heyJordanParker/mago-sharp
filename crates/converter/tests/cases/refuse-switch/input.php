<?php

declare(strict_types=1);

namespace App;

// A case that runs on into the next, a `break` before its case ends, and a numeric string that `==` compares as a
// number have no PHP# form (spec sections 19 and 21).
final class Branches
{
    public function run(int $code, string $text, bool $early): int
    {
        $total = 0;
        switch ($code) {
            case 1:
                $total = 1;
            case 2:
                $total = 2;
                break;
        }
        switch ($code) {
            case 3:
                if ($early) {
                    break;
                }
                $total = 3;
                break;
        }
        switch ($text) {
            case '10':
                $total = 10;
                break;
        }

        return $total;
    }

    // PHP's `null == ''` and `null == 0` hold, so a case a `null` subject meets runs, where a pattern doesn't match
    // `null`. A constant's value counts as a literal's does.
    public const string CODE = '7';

    public function nullable(?string $text, ?int $count): int
    {
        switch ($text) {
            case '':
                return 1;
        }
        switch ($count) {
            case 0:
                return 2;
        }
        switch ($text) {
            case self::CODE:
                return 3;
        }

        return 0;
    }
}
