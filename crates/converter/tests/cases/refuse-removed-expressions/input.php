<?php

declare(strict_types=1);

namespace App;

// An anonymous class, `@`, `print`, `exit`, `include`, a member named by a variable, a call of a value and `?:` of a
// call, which its PHP# test would make twice, have no PHP# form (spec sections 2, 8, 14, 21, 25 and 27).
final class Removed
{
    public string $name = '';

    public function run(string $member, ?string $text): mixed
    {
        $object = new class {
        };
        $quiet = @$this->name;
        print 'out';
        include 'other.php';
        $read = $this->$member;
        $called = $member();
        $either = $this->title() ?: 'none';
        exit(1);
    }

    private function title(): ?string
    {
        return null;
    }
}
