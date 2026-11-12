<?php

declare(strict_types=1);

namespace Proof\Shop;

use Acme\Format\Lines;
use Acme\Format\Record;

// Values the engine checks where they enter PHP#: an `(int)` of a property only PHPDoc types `int` stays, since the
// property may hold `"5"`, and an `iterable` the body loops over by value alone takes any keys and class. A keyed
// loop over a `string`-keyed array reads the key `'42'`, which PHP holds as the `int` 42, only as text and as a key.
final class Stock
{
    /** @param array<string, int> $prices */
    public function listed(array $prices): string
    {
        $listed = '';
        foreach ($prices as $sku => $price) {
            $listed .= $sku . '=' . $price . ';';
        }

        return $listed;
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

    public function quantity(Record $record): int
    {
        return (int) $record->quantity;
    }

    // A `(string)` of a property only PHPDoc types `string` converts what it holds, `5` or `null`, to text.
    public function coded(Record $record): string
    {
        return (string) $record->code . '/' . (string) $record->note;
    }

    // A loop over the values of a plain PHP collection is PHP's `foreach` over the object.
    public function counted(Lines $lines): string
    {
        $joined = '';
        foreach ($lines as $line) {
            $joined .= $line;
        }

        return $joined;
    }

    // A plain `array` is a `Map` keyed by `int|string`, whose keys a loop gives as PHP holds them: `5` stays an `int`.
    /** @return list<array-key> */
    public function given(array $input): array
    {
        $input['source'] = 'form';
        $keys = [];
        foreach ($input as $key => $value) {
            $keys[] = $key;
        }
        if (isset($input['email'])) {
            $keys[] = 'email given';
        }

        return $keys;
    }

    // Elements PHP numbers from 0, where an `int`-keyed array is declared, at two depths, and class values among them.
    /** @return array<string, array<int, string>> */
    public function shelves(): array
    {
        return ['top' => ['jam', 'tea'], 'bottom' => [], 'kinds' => [self::class, Coupon::class]];
    }

    // A `List` index that may be missing, behind `isset` and `??`, past the end and below 0.
    /** @param list<string> $jobs */
    public function nth(array $jobs, int $at): string
    {
        return (isset($jobs[$at]) ? 'has ' : 'no ') . ($jobs[$at] ?? 'none');
    }

    // `=` and `unset` at an index of a plain PHP `ArrayAccess` collection, which call its `offsetSet` and `offsetUnset`.
    public function stocked(\Acme\Format\Bag $bag, string $sku): int
    {
        $bag[$sku] = 3;
        unset($bag['old']);

        return $bag->writes;
    }

    // A lambda that ignores the index plain PHP passes it, which its PHP# form takes as `unused`.
    /** @return list<string> */
    public function marked(Lines $lines): array
    {
        return $lines->mapped(fn (string $line): string => $line . '!');
    }

    /** @param iterable<string> $names */
    public function joined(iterable $names): string
    {
        $joined = '';
        foreach ($names as $name) {
            $joined .= $name;
        }

        return $joined;
    }
}
