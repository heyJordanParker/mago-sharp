<?php

declare(strict_types=1);

namespace Proof\Shop;

// Text: an interpolated string, a heredoc and a nowdoc as templates, and `__DIR__` as `Position`.
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
