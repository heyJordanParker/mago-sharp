<?php

declare(strict_types=1);

// `insteadof` picks the trait whose method the class takes, a qualified `as` names one trait's method, a method may
// have two aliases, and a visibility change applies only to the trait method the class takes, as PHP's
// zend_traits_copy_functions applies them.

trait AdaptedNumbers
{
    public function m(): int
    {
        return 1;
    }

    public function n(): int
    {
        return 1;
    }

    public function k(): int
    {
        return 1;
    }
}

trait AdaptedTexts
{
    public function m(): string
    {
        return 'b';
    }

    public function n(): string
    {
        return 'b';
    }
}

final class Adapted
{
    use AdaptedNumbers, AdaptedTexts {
        AdaptedTexts::m insteadof AdaptedNumbers;
        AdaptedNumbers::n insteadof AdaptedTexts;
        AdaptedNumbers::m as am;
        AdaptedNumbers::m as amAgain;
        AdaptedTexts::n as tn;
        AdaptedTexts::n as protected;
        AdaptedNumbers::m as protected pm;
        k as protected;
    }

    public function k(): int
    {
        return 2;
    }
}

final class AdaptedProbe
{
    public static function run(Adapted $adapted): void
    {
        self::text($adapted->m());
        self::number($adapted->n());
        self::number($adapted->k());
        self::number($adapted->am());
        self::number($adapted->amAgain());
        self::text($adapted->tn());
        // @mago-expect analysis:invalid-method-access
        $adapted->pm();
    }

    private static function text(string $text): void
    {
        echo $text;
    }

    private static function number(int $number): void
    {
        echo $number;
    }
}
