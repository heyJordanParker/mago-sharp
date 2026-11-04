<?php

declare(strict_types=1);

namespace App;

// `.` between strings is `+`, and with a number it is a template, which shows the number as PHP does (spec section 18).
final class Label
{
    private string $text = 'a';

    public function joined(string $other): string
    {
        return $this->text . ' and ' . $other;
    }

    public function counted(int $count): string
    {
        return 'count ' . $count . ' of ' . $this->text;
    }

    public function priced(float $price): string
    {
        return 'price ' . $price;
    }

    public function grow(string $more): void
    {
        $this->text .= $more;
    }
}
