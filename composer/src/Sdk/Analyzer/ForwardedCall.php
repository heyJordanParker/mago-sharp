<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer;

use Mago\Sdk\Exception\InvalidArgumentException;

/**
 * The methods a magic access runs, in order, starting on the receiver.
 *
 * @api
 */
final class ForwardedCall
{
    /** @var non-empty-list<non-empty-string> */
    public readonly array $methods;

    /**
     * @param list<string> $methods
     */
    public function __construct(
        public readonly Type $receiver,
        array $methods,
    ) {
        $validated = [];
        foreach ($methods as $method) {
            if ($method === '') {
                throw new InvalidArgumentException('A forwarded call method name cannot be empty.');
            }

            $validated[] = $method;
        }

        if ($validated === []) {
            throw new InvalidArgumentException('A forwarded call names the methods it runs, in order.');
        }

        $this->methods = $validated;
    }
}
