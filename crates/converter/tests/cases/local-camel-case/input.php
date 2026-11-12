<?php

declare(strict_types=1);

namespace App;

use RuntimeException;

// PHP# names a local and a `catch` variable in camel case (spec section 24), and no code outside the method reaches
// one by its name, so each takes its camel case name wherever the method names it, its lambdas included.
final class Invoice
{
    /** @param list<int> $prices */
    public function total(array $prices, int $tax): string
    {
        $sum_so_far = 0;
        foreach ($prices as $unit_price) {
            $sum_so_far += $unit_price;
        }
        $tax_rate = $tax;
        $with_tax = fn (int $amount): int => $amount + $tax_rate;
        try {
            $grand_total = $with_tax($sum_so_far);

            return "total: {$grand_total}";
        } catch (RuntimeException $failed_sum) {
            return $failed_sum->getMessage();
        }
    }
}
