<?php

declare(strict_types=1);

namespace App;

// A type that names a class the project doesn't declare keeps the file plain PHP, with the type named (spec section 24).
final class Shipping
{
    /** @param list<Parcel> $parcels */
    public function ship(array $parcels): void
    {
    }
}
