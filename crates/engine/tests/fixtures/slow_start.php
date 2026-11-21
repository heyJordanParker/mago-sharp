<?php

declare(strict_types=1);

$rounds = 0;
$until = hrtime(true) + 1_500_000_000;
while (hrtime(true) < $until) {
    $rounds++;
}
