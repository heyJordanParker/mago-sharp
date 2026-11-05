<?php

declare(strict_types=1);

namespace App;

use Lib\Pdf as Doc;
use Lib\Mail\Message;
use RuntimeException;

// A class prints by the name the file reaches it by: its import, its short name in the file's namespace, bare as a
// root class of `Builtins.sharp`, or a new import, renamed when its short name is taken (spec section 23).
final class Mailer
{
    public function document(): Doc
    {
        return new Doc();
    }

    public function message(): Message
    {
        return new Message();
    }

    public function chat(): \Lib\Chat\Message
    {
        return new \Lib\Chat\Message();
    }

    public function draft(): Draft
    {
        return new Draft();
    }

    public function failure(): RuntimeException
    {
        return new \UnexpectedValueException('no');
    }
}
