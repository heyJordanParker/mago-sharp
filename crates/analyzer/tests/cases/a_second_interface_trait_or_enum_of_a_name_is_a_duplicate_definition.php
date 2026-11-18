<?php

declare(strict_types=1);

if (PHP_VERSION_ID >= 80100) {
    interface Shape {}

    trait Sides {}

    enum Suit {}
} else {
    // @mago-expect analysis:duplicate-definition
    interface Shape {}

    // @mago-expect analysis:duplicate-definition
    trait Sides {}

    // @mago-expect analysis:duplicate-definition
    enum Suit {}
}
