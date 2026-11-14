<?php

declare(strict_types=1);

namespace App;

// A body PHP writes between `:` and an end keyword is a body in braces, as PHP# writes every body (spec section 17).
final class Tally
{
    /** @param list<int> $numbers */
    public function sum(array $numbers): int
    {
        $total = 0;
        foreach ($numbers as $number):
            $total += $number;
        endforeach;
        for ($i = 0; $i < 2; $i++):
            $total += $i;
        endfor;
        while ($total > 100):
            $total -= 100;
        endwhile;

        return $total;
    }

    public function size(int $count): string
    {
        if ($count > 10):
            return 'large';
        elseif ($count > 1):
            return 'small';
        else:
            return 'one';
        endif;
    }
}
