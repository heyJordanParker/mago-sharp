<?php

declare(strict_types=1);

class Base
{
}

class Vault
{
    protected function open(): string
    {
        return 'open';
    }
}

/**
 * @require-extends Base
 */
trait OpensVaults
{
    public function peek(): string
    {
        // @mago-expect analysis:invalid-method-access
        // @mago-expect analysis:mixed-return-statement
        return (new Vault())->open();
    }
}

final class Teller extends Base
{
    use OpensVaults;
}

echo (new Teller())->peek();
