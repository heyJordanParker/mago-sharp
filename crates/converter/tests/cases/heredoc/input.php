<?php

declare(strict_types=1);

namespace App;

// A heredoc and a nowdoc are templates, without the closing label's indentation and the line break before it (spec
// section 18).
final class Page
{
    public function render(string $title): string
    {
        $raw = <<<'TXT'
            cost: $5 `code`
            TXT;

        return <<<HTML
            <h1>$title</h1>
              <p>{$raw}</p>
            HTML;
    }
}
