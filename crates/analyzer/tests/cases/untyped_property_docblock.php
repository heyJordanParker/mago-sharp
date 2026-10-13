<?php

class A
{
}

/**
 * @property-read  $x
 * @property  $y
 * @property-write      $z
 */
class T
{
    public function __get(string $name): mixed
    {
        return $this->__get($name);
    }

    public function __set(string $name, mixed $value): void
    {
    }
}

/**
 * @mago-expect analysis:invalid-property-write
 * @mago-expect analysis:mixed-assignment
 */
function foo(): void
{
    $t = new T();
    $c = $t->x;
    $t->x = 10;
}

/**
 * @mago-expect analysis:mixed-assignment
 */
function bar(): void
{
    $t = new T();
    $c = $t->y;
    $t->y = 10;
}

/**
 * @mago-expect analysis:invalid-property-read
 * @mago-expect analysis:impossible-assignment
 */
function baz(): void
{
    $t = new T();
    $c = $t->z;
    $t->z = 10;
}
