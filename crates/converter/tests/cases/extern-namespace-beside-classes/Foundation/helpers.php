<?php

declare(strict_types=1);

// A function and a constant in PHP's root namespace, beside the classes of Acme\Foundation, which no
// `extern namespace` line of the project gives a namespace yet.
const ACME_VERSION = '1.0';

function abort(int $code): never
{
    throw new RuntimeException('Aborted.', $code);
}
