<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer\Metadata;

/**
 * @api
 */
enum ConstantExpressionKind
{
    case Literal;
    case Array_;
    case ClassName;
    case ClassConstant;
    case Constant;
    case New_;
    case Unsupported;
}
