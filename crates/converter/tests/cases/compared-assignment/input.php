<?php

declare(strict_types=1);

namespace App;

// An `if` whose condition first assigns a local that a comparison reads: the assignment runs on its own line before
// the `if`, as PHP runs it before the rest of the condition, and the comparison reads the local.
final class Orders
{
    public function total(int $id): int
    {
        $total = 0;
        if (($count = $this->count($id)) !== 0 && $count > 1) {
            $total = $count * 2;
        }

        return $total;
    }

    private function count(int $id): int
    {
        return $id;
    }
}
