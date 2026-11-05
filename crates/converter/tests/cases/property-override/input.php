<?php

declare(strict_types=1);

namespace App;

// A property that redeclares its parent's to change its initial value replaces it, which PHP# writes as a property
// with accessors marked `override`, even where PHP's is protected (spec section 22).
final class SlowJob extends Job
{
    public int $timeout = 300;

    protected string $queue = 'slow';
}
