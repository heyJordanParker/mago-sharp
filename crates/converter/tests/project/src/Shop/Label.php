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
