<?php

declare(strict_types=1);

namespace App;

interface Shareable
{
    public function link(): string;
}
