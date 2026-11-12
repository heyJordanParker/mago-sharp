<?php

declare(strict_types=1);

namespace App;

abstract class Report
{
    abstract public function totals(): array;
}
