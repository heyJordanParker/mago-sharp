<?php

declare(strict_types=1);

interface Greeter
{
    public function greet(): string;
}

trait Greets
{
    public function hello(): string
    {
        return 'hello';
    }
}

final class Host implements Greeter
{
    use Greets {
        hello as greet;
    }
}

echo (new Host())->greet();
