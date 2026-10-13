<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Unit\Analyzer;

use ArrayObject;
use Mago\Sdk\Analyzer\Metadata\AttributeArgumentMetadata;
use Mago\Sdk\Analyzer\Metadata\AttributeMetadata;
use Mago\Sdk\Analyzer\Metadata\ConstantExpression;
use Mago\Sdk\Analyzer\Metadata\ConstantExpressionKind;
use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Exception\InvalidArgumentException;
use Mago\Sdk\SourceLocation;
use Mago\Sdk\Span;
use PHPUnit\Framework\TestCase;

use const PHP_INT_MAX;

final class AttributeMetadataTest extends TestCase
{
    public function testArgumentsCanBeReadByPositionOrName(): void
    {
        $location = new SourceLocation('example.php', new Span(10, 20));
        $positional = new AttributeArgumentMetadata(
            null,
            $location,
            null,
            $location,
            Type::literalString('provider'),
            null,
        );
        $named = new AttributeArgumentMetadata('enabled', $location, $location, $location, Type::false(), null);
        $attribute = new AttributeMetadata('Example', $location, [$positional, $named]);

        self::assertSame($positional, $attribute->getArgument(0, 'methodName'));
        self::assertSame($named, $attribute->getArgument(1, 'enabled'));
        self::assertNull($attribute->getArgument(1));
        self::assertSame($positional, $attribute->getArgument(0, 'missing'));
        self::assertNull($attribute->getArgument(1, 'missing'));
    }

    public function testArgumentsEvaluateAsPhpReflectionDoes(): void
    {
        $location = new SourceLocation('example.php', new Span(10, 20));
        $list = new ConstantExpression(ConstantExpressionKind::Array_, items: [
            [null, self::literal('first')],
            [self::literal('1'), self::literal(1.5)],
            [new ConstantExpression(ConstantExpressionKind::Constant, 'PHP_INT_MAX'), self::literal(null)],
        ]);
        $nested = new ConstantExpression(ConstantExpressionKind::New_, ArrayObject::class, arguments: [
            [null, new ConstantExpression(ConstantExpressionKind::Array_, items: [[null, self::literal(true)]])],
        ]);
        $attribute = new AttributeMetadata(ArrayObject::class, $location, [
            new AttributeArgumentMetadata(null, $location, null, $location, null, $list),
            new AttributeArgumentMetadata(
                'flags',
                $location,
                $location,
                $location,
                null,
                new ConstantExpression(ConstantExpressionKind::ClassConstant, ArrayObject::class, 'ARRAY_AS_PROPS'),
            ),
            new AttributeArgumentMetadata(
                'iteratorClass',
                $location,
                $location,
                $location,
                null,
                new ConstantExpression(ConstantExpressionKind::ClassName, 'ArrayIterator'),
            ),
        ]);

        self::assertSame(
            [
                ['first', 1 => 1.5, PHP_INT_MAX => null],
                'flags' => ArrayObject::ARRAY_AS_PROPS,
                'iteratorClass' => 'ArrayIterator',
            ],
            $attribute->getArguments(),
        );
        self::assertEquals(new ArrayObject([true]), $nested->evaluate());

        $instance = $attribute->newInstance();
        self::assertInstanceOf(ArrayObject::class, $instance);
        self::assertSame(ArrayObject::ARRAY_AS_PROPS, $instance->getFlags());
        self::assertSame('ArrayIterator', $instance->getIteratorClass());
    }

    public function testUnsupportedExpressionsAndPlaceholdersFailLoudly(): void
    {
        $location = new SourceLocation('example.php', new Span(10, 20));
        $unsupported = new AttributeMetadata('Example', $location, [
            new AttributeArgumentMetadata(
                null,
                $location,
                null,
                $location,
                null,
                new ConstantExpression(ConstantExpressionKind::Unsupported, location: $location),
            ),
        ]);
        $placeholder = new AttributeMetadata('Example', $location, [
            new AttributeArgumentMetadata(null, $location, null, null, null, null),
        ]);

        self::assertThrows($unsupported->getArguments(...), 'at 10 in example.php');
        self::assertThrows($placeholder->getArguments(...), 'is a placeholder');
    }

    private static function literal(bool|int|float|string|null $value): ConstantExpression
    {
        return new ConstantExpression(ConstantExpressionKind::Literal, $value);
    }

    /** @param callable(): mixed $callback */
    private static function assertThrows(callable $callback, string $message): void
    {
        try {
            $callback();
        } catch (InvalidArgumentException $exception) {
            self::assertStringContainsString($message, $exception->getMessage());

            return;
        }

        self::fail("Expected an exception containing `{$message}`.");
    }
}
