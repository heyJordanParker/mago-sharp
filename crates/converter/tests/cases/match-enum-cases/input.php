<?php

declare(strict_types=1);

namespace App;

// A PHP# `match` over a project's enum lists its cases, so a `default` on one is the cases it stands for, and a
// `switch` without `default` gains an empty arm for the cases no `case` names, `null` included (spec section 21).
final class Board
{
    public function label(Status $status): string
    {
        return match ($status) {
            Status::Open => 'open',
            default => 'done',
        };
    }

    public function count(Status $status): int
    {
        $total = 0;
        switch ($status) {
            case Status::Open:
                $total = 1;
                break;
            default:
                $total = 2;
        }

        return $total;
    }

    public function mark(?Status $status): int
    {
        $total = 0;
        switch ($status) {
            case Status::Paused:
                $total = 1;
                break;
        }

        return $total;
    }
}
