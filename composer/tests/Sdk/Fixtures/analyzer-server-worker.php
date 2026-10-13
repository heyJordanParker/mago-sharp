<?php

declare(strict_types=1);

namespace Mago\Tests\Sdk\Fixtures;

use Mago\Sdk\Analyzer\AfterAnalysisContext;
use Mago\Sdk\Analyzer\AfterAnalysisHook;
use Mago\Sdk\Analyzer\AfterFileAnalysisContext;
use Mago\Sdk\Analyzer\AfterFileAnalysisHook;
use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\BeforeAnalysisHook;
use Mago\Sdk\Analyzer\FileAnalysis;
use Mago\Sdk\Analyzer\MethodReturnTypeProvider;
use Mago\Sdk\Analyzer\MethodTarget;
use Mago\Sdk\Analyzer\NodeAnalysisContext;
use Mago\Sdk\Analyzer\NodeAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Analyzer\ReturnTypeProviderContext;
use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Extension;
use Mago\Sdk\Reporting\Issue;
use Mago\Sdk\Reporting\Level;
use Mago\Sdk\SourceLocation;
use Mago\Sdk\Span;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Worker;
use RuntimeException;

use function dirname;
use function explode;
use function implode;
use function sort;
use function str_contains;
use function strcspn;
use function strlen;
use function strpos;
use function substr;
use function usort;

require_once dirname(__DIR__, 4) . '/vendor/autoload.php';

/**
 * The hooks the analysis server tests drive: a before-analysis issue, a node hook that reports
 * each function and fails in a file marked `node hook: fail`, a failing after-analysis hook, and a
 * cross-file rule that reports a route declared by two analyzed files.
 *
 * @mago-expect lint:cyclomatic-complexity
 */
final class ServerProofPlugin implements Plugin, BeforeAnalysisHook, NodeAnalysisHook, AfterAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('server-proof', 'Server proof', 'Hooks the analysis server tests drive.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerBeforeAnalysisHook($this);
        $registry->registerNodeAnalysisHook($this);
        $registry->registerAfterAnalysisHook($this);
    }

    public function getTargets(): array
    {
        return [NodeKind::Function];
    }

    public function getRequirements(): array
    {
        return [];
    }

    public function analyze(NodeAnalysisContext $context): void
    {
        if (str_contains($context->analysis->getSourceFile()->contents, 'node hook: fail')) {
            throw new RuntimeException("The node hook ran on `{$context->analysis->file}`.");
        }

        $context->report(Level::Warning, 'node', Issue::new('Node hook ran.', $context->node->span));
    }

    public function beforeAnalysis(BeforeAnalysisContext $context): void
    {
        $marker = $context->codebase->getConstant('PROOF_BEFORE');
        if ($marker !== null) {
            $context->report(Level::Warning, 'before', Issue::at('Before-analysis hook ran.', $marker->location));
        }
    }

    public function afterAnalysis(AfterAnalysisContext $context): void
    {
        if ($context->codebase->getConstant('PROOF_FAIL') !== null) {
            throw new RuntimeException('The after-analysis hook was told to fail.');
        }

        $files = $context->analysis->files;
        usort($files, static fn(FileAnalysis $left, FileAnalysis $right): int => $left->file <=> $right->file);

        $owners = [];
        foreach ($files as $file) {
            $contents = $file->getSourceFile()->contents;
            $marker = strpos($contents, 'route: ');
            while ($marker !== false) {
                $start = $marker + strlen('route: ');
                $end = $start + strcspn($contents, " \t\n\v\f\r", $start);
                $marker = strpos($contents, 'route: ', $end);

                $route = substr($contents, $start, $end - $start);
                $owner = $owners[$route] ?? null;
                if ($owner === null) {
                    $owners[$route] = $file->file;
                    continue;
                }

                $location = new SourceLocation($file->file, new Span($start, $end));
                $context->report(
                    Level::Error,
                    'duplicate-route',
                    Issue::at("Route `{$route}` is also declared in `{$owner}`.", $location),
                );
            }
        }
    }
}

/**
 * An after-file hook that reads the codebase: in a file marked `reads: Class::method` it reports
 * the method's visibility, in a file marked `lists: classes` every class name, and in a file
 * marked `compares: Class` whether the class is a `Base`. Each report counts the hook's runs in
 * this worker, so a test sees whether Mago ran it again.
 *
 * @mago-expect lint:single-class-per-file
 * @mago-expect lint:cyclomatic-complexity
 */
final class ServerReadsPlugin implements Plugin, AfterFileAnalysisHook
{
    private int $runs = 0;

    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('server-reads', 'Server reads', 'An after-file hook that reads the codebase.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerAfterFileAnalysisHook($this);
    }

    public function getRequirements(): array
    {
        return [];
    }

    public function afterFileAnalysis(AfterFileAnalysisContext $context): void
    {
        $contents = $context->analysis->getSourceFile()->contents;
        $read = strpos($contents, 'reads: ');
        $listing = strpos($contents, 'lists: classes');
        $comparison = strpos($contents, 'compares: ');
        if ($read === false && $listing === false && $comparison === false) {
            return;
        }

        ++$this->runs;
        if ($read !== false) {
            $start = $read + strlen('reads: ');
            $end = $start + strcspn($contents, " \t\n\v\f\r", $start);
            [$class, $method] = explode('::', substr($contents, $start, $end - $start));
            $visibility = $context->codebase->getMethod($class, $method)->visibility->name ?? 'missing';
            $context->report(
                Level::Warning,
                'read',
                Issue::at(
                    "Run {$this->runs}: `{$class}::{$method}` is {$visibility}.",
                    new SourceLocation($context->analysis->file, new Span($start, $end)),
                ),
            );
        }

        if ($listing !== false) {
            $classes = $context->codebase->getClassNames();
            sort($classes);
            $context->report(
                Level::Warning,
                'listing',
                Issue::at(
                    "Run {$this->runs}: classes " . implode(', ', $classes) . '.',
                    new SourceLocation(
                        $context->analysis->file,
                        new Span($listing, $listing + strlen('lists: classes')),
                    ),
                ),
            );
        }

        if ($comparison !== false) {
            $start = $comparison + strlen('compares: ');
            $end = $start + strcspn($contents, " \t\n\v\f\r", $start);
            $class = substr($contents, $start, $end - $start);
            $relation = $context->types->isContainedBy(Type::namedObject($class), Type::namedObject('Base'))
                ? 'is'
                : 'is not';
            $context->report(
                Level::Warning,
                'comparison',
                Issue::at(
                    "Run {$this->runs}: `{$class}` {$relation} a `Base`.",
                    new SourceLocation($context->analysis->file, new Span($start, $end)),
                ),
            );
        }
    }
}

/**
 * A return-type provider that answers `App\Models\Query::total()` with the return type
 * `App\Models\Order::total()` declares, read from the codebase, `App\Models\Query::models()`
 * with `int` once it listed every class, and `App\Models\Query::billable()` with `int` when
 * `App\Models\Order` is an `App\Models\Model` and `0` otherwise.
 *
 * @mago-expect lint:single-class-per-file
 */
final class ServerModelPlugin implements Plugin, MethodReturnTypeProvider
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('server-model', 'Server model', 'A return-type provider that reads a model.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerMethodReturnTypeProvider($this);
    }

    public function getTargets(): array
    {
        return [
            MethodTarget::exact('App\\Models\\Query', 'total'),
            MethodTarget::exact('App\\Models\\Query', 'models'),
            MethodTarget::exact('App\\Models\\Query', 'billable'),
        ];
    }

    public function getReturnType(ReturnTypeProviderContext $context): ?Type
    {
        if ($context->invocation->name === 'models') {
            return $context->codebase->getClassNames() === [] ? null : Type::int();
        }

        if ($context->invocation->name === 'billable') {
            return $context->types->isContainedBy(
                Type::namedObject('App\\Models\\Order'),
                Type::namedObject('App\\Models\\Model'),
            )
                ? Type::int()
                : Type::literalInt(0);
        }

        return $context->codebase->getMethod('App\\Models\\Order', 'total')?->returnType?->type;
    }
}

(new Worker(new Extension(
    identifier: 'mago/server-proof',
    name: 'Mago analysis server proof',
    version: '1.0.0',
    analyzerPlugins: [new ServerProofPlugin(), new ServerReadsPlugin(), new ServerModelPlugin()],
)))->run();
