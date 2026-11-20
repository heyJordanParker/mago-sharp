<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit;

use Closure;
use RuntimeException;

/**
 * Plays mago for a worker these tests call: answers what it asks through `Mago\Engine\ask()`.
 */
final class Mago
{
    /**
     * @var null|Closure(string, string|list<null|bool|int|float|string|list<mixed>>): ?list<null|bool|int|float|string|list<mixed>>
     */
    public static ?Closure $answer = null;

    /**
     * @param string|list<null|bool|int|float|string|list<mixed>> $payload
     *
     * @return null|list<null|bool|int|float|string|list<mixed>>
     */
    public static function ask(string $question, string|array $payload): ?array
    {
        $answer = self::$answer ?? throw new RuntimeException("The worker asked `{$question}` where it asks nothing.");

        return $answer($question, $payload);
    }
}
