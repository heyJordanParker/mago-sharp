<?php

declare(strict_types=1);

namespace App;

/**
 * A method's own type parameters are in PHPDoc, which PHP# writes after the method's name (spec section 11).
 */
class Repository
{
    /**
     * Saves an entity, and gives it back as its own type.
     *
     * @template TEntity of Entity
     *
     * @param TEntity $entity
     *
     * @return TEntity
     */
    public function save(Entity $entity): Entity
    {
        return $entity;
    }

    /**
     * The analysis types the value as wider than `TEntity`, which only a `TEntity` is.
     *
     * @template TEntity of Entity
     *
     * @param TEntity $entity
     *
     * @return TEntity
     */
    public function saved(Entity $entity): Entity
    {
        return $entity->saved ? $entity : new Entity();
    }

    /**
     * @template TFound
     *
     * @param TFound $found
     *
     * @return list<TFound>
     */
    public function listed($found): array
    {
        return [$found];
    }
}
