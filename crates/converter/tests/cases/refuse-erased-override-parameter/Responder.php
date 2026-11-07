<?php

declare(strict_types=1);

namespace App;

interface Responder
{
    /** @param Request $request */
    public function respond($request): string;
}
