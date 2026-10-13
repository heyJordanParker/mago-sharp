<?php

declare(strict_types=1);

namespace Mago\Sdk\Syntax;

/**
 * What a bare PHP# name with a resolved name refers to, as Mago's binder decided from its file alone. A PHP name has
 * none. Locals and `this` have no resolved name, so they have no binding here.
 *
 * @api
 */
enum Binding: string
{
    /** A class, written before `.`, such as `Calc` in the static call `Calc.make()`. */
    case ClassName = 'ClassName';
    /** A constant. */
    case Constant = 'Constant';
    /** A member of the enclosing class, read without `this.`. A bare call is never one: it calls the global function. */
    case Member = 'Member';
}
