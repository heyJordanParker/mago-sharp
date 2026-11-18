<?php

namespace Fixture;

class Model
{
    /** @mago-expect analysis:missing-property-type */
    public $table;
}

trait HasConnection
{
    /** @mago-expect analysis:missing-property-type */
    public $connection;
}

class User extends Model
{
    use HasConnection;

    public $table = 'users';

    public $connection;

    /** @mago-expect analysis:missing-property-type */
    public $name;
}
