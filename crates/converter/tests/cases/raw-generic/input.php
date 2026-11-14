<?php

declare(strict_types=1);

namespace App;

// A generic class written without type arguments, which PHP takes as any object of the class, takes its bounds where
// each type parameter is covariant, since those hold every such object (spec section 11).
final class Post extends Model
{
    public function author(): Relation
    {
        return $this->belongsTo(User::class);
    }

    public function describe(Relation $relation): string
    {
        return 'relation';
    }
}
