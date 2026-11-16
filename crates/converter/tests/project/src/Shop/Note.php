<?php

declare(strict_types=1);

namespace Proof\Shop;

final class Note
{
    private ?int $pages = null;

    // `?:` and the truthiness of a property, whose PHP# tests read it again: `0` and `null` are falsy, as in PHP.
    public function headed(?int $pages): string
    {
        $this->pages = $pages;
        if ($this->pages) {
            return 'headed ' . $this->pages;
        }

        return 'pages ' . ($pages ?: 1);
    }

    // A string that may be null, which PHP's `.` and interpolation show as nothing.
    public function signed(?string $name): string
    {
        return "Dear {$name}," . $name . '.';
    }

    // `(bool)`, which the truthiness test of the value's type replaces: `0`, `0.0` and `null` are false, as in PHP.
    /** @return list<bool> */
    public function flagged(int $count, ?float $share): array
    {
        return [(bool) $count, (bool) $share];
    }

    // `.=` of a number and of a string that may be null, and `===` of a value of a union type with a string.
    public function tagged(string|int|bool $answer, string $agreed, int $count, ?string $tag): string
    {
        $text = 'note';
        $text .= $count;
        $text .= $tag;

        return $text . ($answer === $agreed ? ' agreed' : ' not');
    }

    // `(string)`, `.` and interpolation of a plain PHP object, which PHP shows by its `__toString`.
    public function labelled(\Acme\Format\Sku $sku): string
    {
        return (string) $sku . ' ' . $sku . " {$sku}";
    }

    // `===` of two class values of a class no PHP# generic class extends, and of an `array-key` with a string.
    /**
     * @param class-string<\Proof\Spec> $kind
     * @param class-string<\Proof\Spec>|null $other
     * @param array-key $key
     * @return list<bool>
     */
    public function matched(string $kind, ?string $other, int|string $key, string $field): array
    {
        return [$kind === $other, $key === $field];
    }

    public function describe(string|int $value): string
    {
        if (is_string($value)) {
            return 'text ' . $value;
        }

        return 'other';
    }
}
