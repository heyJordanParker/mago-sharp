<?php

declare(strict_types=1);

namespace App;

/**
 * A plain PHP generic class whose type parameters are covariant.
 *
 * @template-covariant TRelated of Model
 * @template-covariant TDeclaring of Model
 */
class Relation
{
}
