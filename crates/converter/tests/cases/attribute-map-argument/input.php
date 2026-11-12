<?php

declare(strict_types=1);

namespace App;

// A list literal an attribute passes where a plain `array`, a `Map`, is declared, in the attribute and in a `new` it
// holds, on a property and on a parameter.
final class Order
{
    #[Rule(anyOf: ['paid', 'shipped'])]
    #[Rule(of: new Rule(anyOf: ['paid', 'shipped']))]
    public string $status = 'paid';

    public function __construct(
        #[Rule(of: new Rule(anyOf: [Order::class, Rule::class]))]
        public string $payment = 'card',
    ) {
    }
}
