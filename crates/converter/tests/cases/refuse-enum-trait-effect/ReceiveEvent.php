<?php

declare(strict_types=1);

namespace App\Events;

use App\Bus\Dispatchable;

final class ReceiveEvent
{
    use Dispatchable;
}
