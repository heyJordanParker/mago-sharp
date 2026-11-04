<?php

declare(strict_types=1);

namespace App;

use RuntimeException;

// `try`, `catch` and `finally` stay, and the caught value is named without `$` (spec section 24).
final class Guarded
{
    public function run(Task $task): string
    {
        try {
            $result = $task->run();

            return $result;
        } catch (RuntimeException $e) {
            return $e->getMessage();
        } finally {
            $task->close();
        }
    }
}
