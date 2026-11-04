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
}
