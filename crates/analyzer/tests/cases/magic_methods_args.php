<?php

/**
 * Class for testing validation against the @method tag.
 * The __call signature is generic and should not produce errors.
 *
 * @method void specific(int $a, string $b)
 * @method static void staticSpecific(int $a, string $b)
 */
class TestAgainstMethodTag
{
    /** @param array<mixed> $arguments */
    public function __call(string $name, array $arguments): void
    {
    }

    /** @param array<mixed> $arguments */
    public static function __callStatic(string $name, array $arguments): void
    {
    }
}

/**
 * Class for testing validation against the __call signature.
 * The @method tag is generic, so validation should fall back to __call.
 *
 * @method void generic(mixed ...$args)
 * @method static void staticGeneric(mixed ...$args)
 */
class TestAgainstMagicMethod
{
    /**
     * @param 'foo'|'bar' $name
     * @param array<int, string> $arguments
     */
    public function __call(string $name, array $arguments): void
    {
    }

    /**
     * @param 'staticFoo'|'staticBar' $name
     * @param array<int, string> $arguments
     */
    public static function __callStatic(string $name, array $arguments): void
    {
    }
}

/**
 * Class for testing validation against both the @method and __call signatures.
 * Both are specific and can conflict.
 *
 * @method void specific(int|string $a)
 * @method static void staticSpecific(int|string $a)
 */
class TestAgainstBoth
{
    /**
     * @param 'specific' $name
     * @param array{0: int} $arguments
     */
    public function __call(string $name, array $arguments): void
    {
    }

    /**
     * @param 'staticSpecific' $name
     * @param array{0: int} $arguments
     */
    public static function __callStatic(string $name, array $arguments): void
    {
    }
}

function test(): void
{
    $obj = new TestAgainstMethodTag();
    $obj->specific(1, 'hello');
    $obj->specific(1, 'hello', 3); // @mago-expect analysis:too-many-arguments
    $obj->specific(1); // @mago-expect analysis:too-few-arguments
    $obj->specific('hello', 1); // @mago-expect analysis:invalid-argument,invalid-argument

    $obj = new TestAgainstMagicMethod();
    $obj->foo('hello', 'world'); // @mago-expect analysis:non-documented-method
    $obj->bar('another', 'test'); // @mago-expect analysis:non-documented-method
    $obj->baz(); // @mago-expect analysis:non-documented-method
    $obj->foo(123); // @mago-expect analysis:non-documented-method
    $obj->__call('foo', ['1', '2']);
    $obj->__call('bar', [123]); // @mago-expect analysis:invalid-argument

    $obj = new TestAgainstBoth();
    $obj->specific(123);
    $obj->specific([]); // @mago-expect analysis:invalid-argument
    $obj->specific(null); // @mago-expect analysis:null-argument
    $obj->__call('specific', [123]);
    $obj->__call('specific', ['string']); // @mago-expect analysis:invalid-argument
    $obj->__call('specific', [null]); // @mago-expect analysis:invalid-argument
}

function testStatic(): void
{
    TestAgainstMethodTag::staticSpecific(1, 'hello');
    TestAgainstMethodTag::staticSpecific(1, 'hello', 3); // @mago-expect analysis:too-many-arguments
    TestAgainstMethodTag::staticSpecific(1); // @mago-expect analysis:too-few-arguments
    TestAgainstMethodTag::staticSpecific('hello', 1); // @mago-expect analysis:invalid-argument,invalid-argument

    TestAgainstMagicMethod::staticFoo('hello', 'world'); // @mago-expect analysis:non-documented-method
    TestAgainstMagicMethod::staticBar('another', 'test'); // @mago-expect analysis:non-documented-method
    TestAgainstMagicMethod::staticBaz(); // @mago-expect analysis:non-documented-method
    TestAgainstMagicMethod::staticFoo(123); // @mago-expect analysis:non-documented-method
    TestAgainstMagicMethod::__callStatic('staticFoo', ['1', '2']);
    TestAgainstMagicMethod::__callStatic('staticBar', [123]); // @mago-expect analysis:invalid-argument

    TestAgainstBoth::staticSpecific(123);
    TestAgainstBoth::staticSpecific([]); // @mago-expect analysis:invalid-argument
    TestAgainstBoth::staticSpecific(null); // @mago-expect analysis:null-argument
    TestAgainstBoth::__callStatic('staticSpecific', [123]);
    TestAgainstBoth::__callStatic('staticSpecific', ['string']); // @mago-expect analysis:invalid-argument
    TestAgainstBoth::__callStatic('staticSpecific', [null]); // @mago-expect analysis:invalid-argument
}
