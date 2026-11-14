<?php

declare(strict_types=1);

namespace App;

abstract class Model
{
    /**
     * @template T of Model
     *
     * @param class-string<T> $related
     *
     * @return Relation<T, $this>
     */
    public function belongsTo(string $related): Relation
    {
        return new Relation();
    }
}
