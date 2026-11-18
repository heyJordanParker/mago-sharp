<?php

declare(strict_types=1);

class Entity
{
    public function __construct(public readonly string $id)
    {
    }
}

class Record extends Entity
{
    public function __construct(string $id)
    {
        parent::__construct($id);
    }
}

final class Invoice extends Record
{
    public function __construct(
        // @mago-expect analysis:invalid-property-write
        public readonly string $id,
    ) {
        parent::__construct($id);
    }
}

new Invoice('a');
