<?php

declare(strict_types=1);

namespace Mago\Sdk\Analyzer;

/**
 * Names the call a magic method or property forwards to.
 *
 * Mago asks only after real methods, pseudo methods, and mixins miss on a class
 * with `__call`, `__callStatic`, or `__get`. The answer resolves as a mixin would:
 * every forwarded method but the last runs with no arguments, and the last takes
 * the call's own arguments, so they are checked once. A property forwards with
 * every method taking no arguments. A forwarded call never asks a forwarding
 * provider again.
 *
 * @api
 * @extends TargetedProvider<MethodTarget>
 */
interface CallForwardingProvider extends TargetedProvider
{
    public function getForwardedCall(CallForwardingProviderContext $context): ?ForwardedCall;
}
