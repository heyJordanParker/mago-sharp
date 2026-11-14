<?php

declare(strict_types=1);

namespace App;

// A method PHP declares no return type for returns `null` where its body runs on past its last statement, and a PHP#
// method returns on every path, so its body ends with `return null;` there (spec section 7).
final class Inviter
{
    public function send(string $name)
    {
        try {
            $this->notify($name);
        } catch (\LogicException $exception) {
            throw new \RuntimeException($exception->getMessage(), 0, $exception);
        }
    }

    public function some(string $name)
    {
        if ($name === '') {
            return 1;
        }
        $this->notify($name);
    }

    public function nothing()
    {
    }

    /** @param list<string> $names */
    public function each(array $names)
    {
        foreach ($names as $name) {
            $this->notify($name);
        }
    }

    // A body that returns or throws on every path ends as it is.
    public function either(string $name)
    {
        if ($name === '') {
            return 1;
        } else {
            return 2;
        }
    }

    public function fail()
    {
        throw new \RuntimeException('no');
    }

    private function notify(string $name): void
    {
    }
}
