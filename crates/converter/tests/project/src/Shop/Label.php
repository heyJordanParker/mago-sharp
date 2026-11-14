<?php

declare(strict_types=1);

namespace Proof\Shop;

// Text: an interpolated string, a heredoc and a nowdoc as templates, `__DIR__` as `Position`, and the class, method
// and namespace names PHP compiles in as their text.
final class Label
{
    public function render(string $title, int $count): string
    {
        $raw = <<<'TXT'
            cost: $5 `code`
            TXT;
        $line = "Title $title has {$count} items";

        return <<<HTML
            <h1>$line</h1>
              <p>{$raw}</p>
            HTML;
    }

    public function folder(): string
    {
        return __DIR__ === '' ? 'nowhere' : 'somewhere';
    }

    public function traced(): string
    {
        return __METHOD__ . ' ' . __FUNCTION__ . ' ' . __NAMESPACE__;
    }

    /** @return class-string<self> */
    public function kind(): string
    {
        return __CLASS__;
    }

    // `match (true)` as guards on the pattern `true`.
    public function size(int $count, bool $bulk): string
    {
        return match (true) {
            $count > 100 => 'huge',
            $bulk, $count > 10 => 'large',
            default => 'small',
        };
    }

    // A value of no known type, as an argument and as a return, checked as the declared type it lands in.
    public function titled(mixed $title): string
    {
        return $this->exclaimed($title);
    }

    public function counted(mixed $count): int
    {
        return $count;
    }

    // A read of a `Map` whose values are never null, at a key it may lack.
    /** @param array<string, int> $sizes */
    public function sized(array $sizes): int
    {
        return $sizes['small'];
    }

    // A condition of a `bool` only PHPDoc types, which reads PHP's truthiness of what it holds.
    public function flagged(Counter $counter, mixed $value): string
    {
        if (!$counter->enabled($value)) {
            return 'off';
        }

        return $counter->enabled($value) ? 'on' : 'off';
    }

    private function exclaimed(string $title): string
    {
        return $title . '!';
    }

    // A `switch` as a `match` statement.
    public function status(int $code): string
    {
        $status = 'other';
        switch ($code) {
            case 200:
            case 201:
                $status = 'ok';
                break;
            case 404:
                return 'missing';
        }

        return $status;
    }
}
