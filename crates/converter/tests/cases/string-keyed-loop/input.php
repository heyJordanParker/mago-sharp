<?php

declare(strict_types=1);

namespace App;

// A keyed loop over a `string`-keyed `Map` converts where the body reads the key only as text or as a key. PHP turns
// the key `'42'` into the `int` 42 and a PHP# `Map` keeps the string `"42"`, and both show as `42` and find the same
// entry (spec section 12).
final class Prices
{
    /** @param array<string, int> $prices */
    public function shown(array $prices): string
    {
        $shown = '';
        foreach ($prices as $sku => $price) {
            $shown .= $sku . '=' . $price . ';';
        }

        return $shown;
    }

    /**
     * @param array<string, int> $prices
     * @param array<string, bool> $wanted
     * @return array<string, int>
     */
    public function kept(array $prices, array $wanted): array
    {
        $kept = [];
        foreach ($prices as $sku => $price) {
            if (isset($wanted[$sku])) {
                $kept[$sku] = $price;
            }
        }

        return $kept;
    }
}
