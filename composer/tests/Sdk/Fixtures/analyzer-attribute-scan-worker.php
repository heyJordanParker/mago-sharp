<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Fixtures;

use Mago\Sdk\Analyzer\CodebaseScanContext;
use Mago\Sdk\Analyzer\CodebaseScanHook;
use Mago\Sdk\Analyzer\Metadata\ConstantExpression;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;
use RuntimeException;

use function array_column;
use function array_merge;
use function dirname;
use function file_put_contents;
use function getenv;
use function getmypid;
use function is_string;
use function json_encode;

use const FILE_APPEND;
use const JSON_THROW_ON_ERROR;
use const LOCK_EX;

require_once dirname(__DIR__, 4) . '/vendor/autoload.php';

/**
 * Records each argument of a class's or a property's attribute a host receives as its name, its type and its value's
 * kind and parts, without evaluating it, so a test compares what a PHP# file and its PHP twin send.
 *
 * @mago-expect lint:cyclomatic-complexity
 * @mago-expect lint:file-name
 */
final class AttributeScanProofPlugin implements CodebaseScanHook, Plugin
{
    /** @var array<string, list<string>> */
    private array $arguments = [];

    public function __construct(
        private readonly string $auditLog,
    ) {}

    public function getTargets(): array
    {
        return ['src/**'];
    }

    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('attribute-scan-proof', 'Attribute scan proof', 'Records attribute arguments.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerCodebaseScanHook($this);
    }

    public function scan(CodebaseScanContext $context): void
    {
        foreach ($context->files as $file) {
            $arguments = [];
            foreach ($file->classLikes as $classLike) {
                $attributes = array_merge(
                    $classLike->attributes,
                    ...array_column($file->getProperties($classLike), 'attributes'),
                );
                foreach ($attributes as $attribute) {
                    foreach ($attribute->arguments as $argument) {
                        $arguments[] = json_encode([
                            $argument->name,
                            $argument->valueType?->__toString(),
                            self::parts($argument->value),
                        ], JSON_THROW_ON_ERROR);
                    }
                }
            }
            $this->arguments[$file->path] = $arguments;
        }

        if (!$context->lastBatch) {
            return;
        }

        $record = json_encode([getmypid(), $this->arguments], JSON_THROW_ON_ERROR);
        if (file_put_contents($this->auditLog, $record . "\n", FILE_APPEND | LOCK_EX) === false) {
            throw new RuntimeException('Unable to append to the attribute-scan audit log.');
        }
    }

    /**
     * @return array<string, mixed>|null
     */
    private static function parts(?ConstantExpression $expression): ?array
    {
        if ($expression === null) {
            return null;
        }

        $items = [];
        foreach ($expression->items as [$key, $item]) {
            $items[] = [self::parts($key), self::parts($item)];
        }

        $arguments = [];
        foreach ($expression->arguments as [$name, $argument]) {
            $arguments[] = [$name, self::parts($argument)];
        }

        return [
            'kind' => $expression->kind->name,
            'value' => $expression->value,
            'constant' => $expression->constant,
            'items' => $items,
            'arguments' => $arguments,
        ];
    }
}

$auditLog = getenv('MAGO_SCAN_AUDIT_LOG');
if (!is_string($auditLog) || $auditLog === '') {
    throw new RuntimeException('MAGO_SCAN_AUDIT_LOG must name the attribute-scan audit file.');
}

(new Worker(new Extension(
    identifier: 'mago/attribute-scan-proof',
    name: 'Mago attribute-scan proof',
    version: '1.0.0',
    analyzerPlugins: [new AttributeScanProofPlugin($auditLog)],
)))->run();
