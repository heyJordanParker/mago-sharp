<?php

declare(strict_types=1);

namespace Foo;

use function json_encode;
// @mago-expect analysis:incorrect-function-casing
use function Json_Decode;
use function Missing_Function;

json_encode('a');
