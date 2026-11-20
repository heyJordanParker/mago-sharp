<?php

declare(strict_types=1);

namespace Mago\Tests\Engine;

use RuntimeException;

use function Mago\Engine\ask;

final class Calls
{
    public static int $count = 0;
}

enum Carrier
{
    case Post;
    case Courier;
}

final class Parcel
{
    public function __construct(
        public readonly int $grams,
        private readonly ?string $label,
        public readonly Carrier $carrier,
    ) {
        throw new RuntimeException('a parcel Mago builds runs no constructor');
    }

    public function label(): ?string
    {
        return $this->label;
    }
}

function open_parcel(Parcel $parcel): array
{
    return [$parcel->grams, $parcel->label(), $parcel->carrier->name];
}

function count_call(): int
{
    return ++Calls::$count;
}

function echo_back(mixed ...$values): array
{
    return $values;
}

function ask_mago(string $question): mixed
{
    return ask($question, 2);
}

function ask_mago_and_catch(string $question): string
{
    try {
        return (string) ask($question);
    } catch (RuntimeException $exception) {
        return 'caught: ' . $exception->getMessage();
    }
}

function throw_exception(): never
{
    throw new RuntimeException('the provider refused');
}

function fail(): never
{
    eval('namespace Mago\\Tests\\Engine; function count_call(): int { return 0; }');
}

function spin(): never
{
    while (true) {
        Calls::$count++;
    }
}

function spin_asking(int $rounds): int
{
    for ($round = 0; $round < $rounds; $round++) {
        usleep(40_000);
        ask('progress');
    }

    return $rounds;
}

function variable(string $name): array
{
    return [getenv($name), $_ENV[$name] ?? false, $_SERVER[$name] ?? false];
}

function setting(string $name): string|false
{
    return ini_get($name);
}

function settings(): array
{
    return [
        'memory_limit' => ini_get('memory_limit'),
        'php.ini' => php_ini_loaded_file(),
        'max_execution_time' => ini_get('max_execution_time'),
    ];
}

function an_object(): object
{
    return new \stdClass();
}

function write_variable_at_shutdown(string $name, string $file): void
{
    register_shutdown_function(static function () use ($name, $file): void {
        file_put_contents($file, (string) getenv($name));
    });
}

function class_count_around_a_declaration(): array
{
    $before = \Mago\Engine\class_count();
    eval('namespace Mago\\Tests\\Engine; interface Declared {}');

    return [$before, \Mago\Engine\class_count()];
}
