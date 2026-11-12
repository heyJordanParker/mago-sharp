<?php

declare(strict_types=1);

namespace App;

// PHP's `array-key` is `int|string`, so a plain `array`, as a request's input, is a `Map` keyed by `int|string`, whose
// keys a PHP array gives as PHP holds them (spec section 12). A key the body reads or writes by is an `int`, or a
// string PHP keeps a string.
final class Input
{
    /** @param array<array-key, string> $fields */
    public function count(array $fields): int
    {
        $count = 0;
        foreach ($fields as $name => $value) {
            $count++;
        }

        return $count;
    }

    public function email(array $input): string
    {
        $input['source'] = 'form';

        return isset($input['email']) ? 'given' : 'missing';
    }

    /**
     * @param array-key $id
     * @return array-key
     */
    public function same($id)
    {
        return $id;
    }
}
